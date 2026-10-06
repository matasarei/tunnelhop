//! Importing a configuration bundle (`.zip`) safely — SPEC §3.3.
//!
//! The archive is untrusted. Every entry is checked and read into memory under hard limits
//! before anything touches the disk; only the manifest and the files its profiles reference
//! are written, into a staging directory that is renamed into place once everything passed.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::manifest::{self, Manifest};
use crate::sanitize::{self, Credentials, Endpoint};

pub const MAX_ZIP_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 500;
pub const MAX_UNCOMPRESSED_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_COMPRESSION_RATIO: u64 = 100;

const MANIFEST_FILE: &str = "manifest.json";
const STAGING_PREFIX: &str = ".staging-";

/// First 16 hex characters of the SHA-256 of the zip bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BundleId(String);

impl BundleId {
    pub fn of(zip_bytes: &[u8]) -> Self {
        let digest = Sha256::digest(zip_bytes);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        BundleId(hex[..16].to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BundleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One reason a bundle was refused, located as precisely as the check allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportError {
    pub file: Option<String>,
    pub line: Option<usize>,
    pub message: String,
}

impl ImportError {
    pub fn archive(message: impl Into<String>) -> Self {
        ImportError {
            file: None,
            line: None,
            message: message.into(),
        }
    }

    pub fn in_file(file: impl Into<String>, message: impl Into<String>) -> Self {
        ImportError {
            file: Some(file.into()),
            line: None,
            message: message.into(),
        }
    }

    pub fn at_line(file: impl Into<String>, line: usize, message: impl Into<String>) -> Self {
        ImportError {
            file: Some(file.into()),
            line: Some(line),
            message: message.into(),
        }
    }
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.file, self.line) {
            (Some(file), Some(line)) => write!(f, "{file}:{line}: {}", self.message),
            (Some(file), None) => write!(f, "{file}: {}", self.message),
            _ => f.write_str(&self.message),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedBundle {
    pub id: BundleId,
    pub dir: PathBuf,
    pub manifest: Manifest,
    /// Server endpoints per profile id, for the kill-switch.
    pub endpoints: BTreeMap<String, Vec<Endpoint>>,
    /// OpenVPN credentials per profile id, taken out of the archive for the caller to store
    /// securely. Empty when `already_present`: they were handed over on the first import.
    pub credentials: BTreeMap<String, Credentials>,
    /// The same zip was imported before; nothing was written this time.
    pub already_present: bool,
}

/// Validate `zip_bytes` and install the bundle under `root/<bundle_id>/`.
///
/// On any error nothing is left under `root` and every problem found is returned.
pub fn import(zip_bytes: &[u8], root: &Path) -> Result<ImportedBundle, Vec<ImportError>> {
    if zip_bytes.len() > MAX_ZIP_BYTES {
        return Err(vec![ImportError::archive(format!(
            "archive is larger than {} MB",
            MAX_ZIP_BYTES / (1024 * 1024)
        ))]);
    }

    let id = BundleId::of(zip_bytes);
    let dir = root.join(id.as_str());
    if dir.is_dir() {
        let manifest = read_installed_manifest(&dir)?;
        let prepared = prepare_profiles(&manifest, &|path| fs::read(dir.join(path)).ok())?;
        return Ok(ImportedBundle {
            id,
            dir,
            manifest,
            endpoints: prepared.endpoints,
            credentials: BTreeMap::new(),
            already_present: true,
        });
    }

    let entries = read_entries(zip_bytes)?;
    let manifest_bytes = entries
        .get(MANIFEST_FILE)
        .ok_or_else(|| vec![ImportError::archive("manifest.json is missing")])?;
    let manifest = manifest::parse(manifest_bytes).map_err(|errors| {
        errors
            .iter()
            .map(|e| ImportError::in_file(MANIFEST_FILE, e.to_string()))
            .collect::<Vec<_>>()
    })?;
    let prepared = prepare_profiles(&manifest, &|path| entries.get(path).cloned())?;

    install(root, &dir, manifest_bytes, &prepared.files).map_err(|e| {
        vec![ImportError::archive(format!(
            "could not write the bundle: {e}"
        ))]
    })?;

    Ok(ImportedBundle {
        id,
        dir,
        manifest,
        endpoints: prepared.endpoints,
        credentials: prepared.credentials,
        already_present: false,
    })
}

fn read_installed_manifest(dir: &Path) -> Result<Manifest, Vec<ImportError>> {
    let bytes = fs::read(dir.join(MANIFEST_FILE)).map_err(|e| {
        vec![ImportError::archive(format!(
            "installed bundle is unreadable: {e}"
        ))]
    })?;
    manifest::parse(&bytes).map_err(|errors| {
        errors
            .iter()
            .map(|e| ImportError::in_file(MANIFEST_FILE, e.to_string()))
            .collect()
    })
}

/// Read every regular file of the archive into memory, enforcing the entry rules and limits.
fn read_entries(zip_bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, Vec<ImportError>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes)).map_err(|e| {
        vec![ImportError::archive(format!(
            "not a valid zip archive: {e}"
        ))]
    })?;
    if archive.len() > MAX_ENTRIES {
        return Err(vec![ImportError::archive(format!(
            "archive has more than {MAX_ENTRIES} entries"
        ))]);
    }

    let mut errors = Vec::new();
    let mut files = BTreeMap::new();
    let mut folded_names = HashSet::new();
    let mut total: u64 = 0;

    for index in 0..archive.len() {
        let mut entry = match archive.by_index(index) {
            Ok(entry) => entry,
            Err(e) => {
                errors.push(ImportError::archive(format!("entry #{index}: {e}")));
                continue;
            }
        };
        let name = match std::str::from_utf8(entry.name_raw()) {
            Ok(name) => name.to_string(),
            Err(_) => {
                errors.push(ImportError::archive(format!(
                    "entry #{index}: name is not valid UTF-8"
                )));
                continue;
            }
        };
        if let Err(message) = check_entry_name(&name) {
            errors.push(ImportError::in_file(&name, message));
            continue;
        }
        if entry.is_symlink() {
            errors.push(ImportError::in_file(
                &name,
                "symbolic links are not allowed",
            ));
            continue;
        }
        if entry.encrypted() {
            errors.push(ImportError::in_file(
                &name,
                "encrypted entries are not allowed",
            ));
            continue;
        }
        if !folded_names.insert(name.trim_end_matches('/').to_lowercase()) {
            errors.push(ImportError::in_file(
                &name,
                "another entry has the same name when case is ignored",
            ));
            continue;
        }
        if entry.is_dir() {
            continue;
        }

        let compressed = entry.compressed_size();
        let declared = entry.size();
        if declared > 0 && (compressed == 0 || declared / compressed > MAX_COMPRESSION_RATIO) {
            errors.push(ImportError::in_file(
                &name,
                format!("compression ratio is above {MAX_COMPRESSION_RATIO}"),
            ));
            continue;
        }

        // Never trust the declared size: read at most what the remaining budget allows.
        let budget = MAX_UNCOMPRESSED_BYTES - total;
        let mut content = Vec::new();
        if let Err(e) = (&mut entry).take(budget + 1).read_to_end(&mut content) {
            errors.push(ImportError::in_file(
                &name,
                format!("could not be read: {e}"),
            ));
            continue;
        }
        total += content.len() as u64;
        if total > MAX_UNCOMPRESSED_BYTES {
            return Err(vec![ImportError::archive(format!(
                "archive expands to more than {} MB",
                MAX_UNCOMPRESSED_BYTES / (1024 * 1024)
            ))]);
        }
        if content.len() as u64 != declared {
            errors.push(ImportError::in_file(
                &name,
                "actual size differs from the size the archive declares",
            ));
            continue;
        }
        files.insert(name, content);
    }

    if errors.is_empty() {
        Ok(files)
    } else {
        Err(errors)
    }
}

/// An entry name must be `manifest.json` or a relative path under `wireguard/` or `openvpn/`.
fn check_entry_name(name: &str) -> Result<(), &'static str> {
    if name.contains('\\') || name.contains(':') || name.contains('\0') {
        return Err("name must be a relative path using '/' only");
    }
    let path = name.strip_suffix('/').unwrap_or(name);
    let segments: Vec<&str> = path.split('/').collect();
    if segments
        .iter()
        .any(|s| s.is_empty() || *s == "." || *s == "..")
    {
        return Err("name must not be absolute or contain empty, '.' or '..' segments");
    }
    match segments.as_slice() {
        [MANIFEST_FILE] if !name.ends_with('/') => Ok(()),
        ["wireguard" | "openvpn", ..] => Ok(()),
        _ => Err("only manifest.json, wireguard/ and openvpn/ are allowed"),
    }
}

struct Prepared {
    /// Sanitized config text keyed by its path in the bundle.
    files: BTreeMap<String, Vec<u8>>,
    endpoints: BTreeMap<String, Vec<Endpoint>>,
    credentials: BTreeMap<String, Credentials>,
}

/// Sanitize every profile's config; `read` returns a bundle file by its path.
fn prepare_profiles(
    manifest: &Manifest,
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<Prepared, Vec<ImportError>> {
    let mut errors = Vec::new();
    let mut prepared = Prepared {
        files: BTreeMap::new(),
        endpoints: BTreeMap::new(),
        credentials: BTreeMap::new(),
    };
    for (i, profile) in manifest.profiles.iter().enumerate() {
        let Some(content) = read(&profile.file) else {
            errors.push(ImportError::in_file(
                MANIFEST_FILE,
                format!(
                    "profiles[{i}].file: \"{}\" is not in the archive",
                    profile.file
                ),
            ));
            continue;
        };
        match sanitize::sanitize(profile.protocol, &profile.file, &content, read) {
            Ok(config) => {
                prepared
                    .files
                    .insert(profile.file.clone(), config.text.into_bytes());
                prepared
                    .endpoints
                    .insert(profile.id.clone(), config.endpoints);
                if let Some(credentials) = config.credentials {
                    prepared.credentials.insert(profile.id.clone(), credentials);
                }
            }
            Err(found) => errors.extend(found),
        }
    }
    if errors.is_empty() {
        Ok(prepared)
    } else {
        Err(errors)
    }
}

fn install(
    root: &Path,
    dir: &Path,
    manifest_bytes: &[u8],
    files: &BTreeMap<String, Vec<u8>>,
) -> std::io::Result<()> {
    fs::create_dir_all(root)?;
    let mut suffix = [0u8; 8];
    rand::fill(&mut suffix);
    let suffix: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
    let staging = root.join(format!("{STAGING_PREFIX}{suffix}"));

    let result = write_tree(&staging, manifest_bytes, files).and_then(|()| {
        match fs::rename(&staging, dir) {
            // Another import of the same zip won the race; its copy is identical.
            Err(_) if dir.is_dir() => fs::remove_dir_all(&staging),
            other => other,
        }
    });
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn write_tree(
    staging: &Path,
    manifest_bytes: &[u8],
    files: &BTreeMap<String, Vec<u8>>,
) -> std::io::Result<()> {
    fs::create_dir(staging)?;
    fs::write(staging.join(MANIFEST_FILE), manifest_bytes)?;
    for (name, content) in files {
        let path = staging.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)?;
    }
    Ok(())
}
