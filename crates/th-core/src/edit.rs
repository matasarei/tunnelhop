//! Adding and removing a bundle's servers — SPEC §3.5.
//!
//! A bundle is immutable: its id is the hash of its zip. An edit therefore builds a new zip
//! from the installed manifest and configs plus the change, and the caller imports that zip
//! like any other, so an edited bundle passes every check an imported one does. The rebuild is
//! deterministic: the same edit of the same bundle gives the same bytes, and so the same id.

use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter};

use crate::bundle::{self, BundleId, ImportError, MANIFEST_FILE, MAX_ZIP_BYTES};
use crate::manifest::{MAX_PROFILES, Manifest, Profile, Protocol, Rotation, SCHEMA_VERSION};
use crate::tunnel_name::is_dos_device_name;

/// The largest config file an edit may add. Real ones are a few kilobytes.
pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_ID_CHARS: usize = 40;
/// What a bundle made from config files is called when the user gives no name.
pub const DEFAULT_BUNDLE_NAME: &str = "My servers";

/// One config file to add as a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProfile {
    /// The display name; the file name without its extension when empty.
    pub name: String,
    /// The file's own name, such as `nl-free-12.conf`. Its extension gives the protocol and
    /// the rest gives the profile id; it is never used as a path.
    pub file_name: String,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Add(Vec<NewProfile>),
    /// By profile id.
    Remove(Vec<String>),
}

/// The protocol a config file's extension stands for: `.conf` or `.ovpn`, in any case.
pub fn protocol_of(file_name: &str) -> Option<Protocol> {
    let (_, extension) = file_name.rsplit_once('.')?;
    if extension.eq_ignore_ascii_case("conf") {
        Some(Protocol::Wireguard)
    } else if extension.eq_ignore_ascii_case("ovpn") {
        Some(Protocol::Openvpn)
    } else {
        None
    }
}

/// A file name without its last extension.
fn stem(file_name: &str) -> &str {
    file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _)| stem)
}

/// A profile id made from a config's file name: lowercase, every run of other characters one
/// `-`, at most 40 characters, and `-2`, `-3`, … appended until `is_free` accepts it.
pub fn profile_id_from(file_name: &str, is_free: impl Fn(&str) -> bool) -> String {
    let mut base = String::new();
    for c in stem(file_name).chars() {
        if c.is_ascii_alphanumeric() {
            base.push(c.to_ascii_lowercase());
        } else if !base.is_empty() && !base.ends_with('-') {
            base.push('-');
        }
    }
    let mut base = base.trim_end_matches('-').to_string();
    // The id becomes a file name: `nul.conf` would be the null device.
    if base.is_empty() || is_dos_device_name(&base) {
        base = format!("server-{base}").trim_end_matches('-').to_string();
    }
    base.truncate(MAX_ID_CHARS);
    let base = base.trim_end_matches('-');
    if is_free(base) {
        return base.to_string();
    }
    (2u32..)
        .map(|n| {
            let suffix = format!("-{n}");
            let kept = &base[..base.len().min(MAX_ID_CHARS - suffix.len())];
            format!("{}{suffix}", kept.trim_end_matches('-'))
        })
        .find(|id| is_free(id))
        .expect("some suffix is always free")
}

/// The zip of the installed bundle `id` with `edit` applied, for the caller to import.
///
/// Refuses what cannot become a bundle at all (an unknown extension, a profile that is not
/// there, removing every server); the import then judges the configs themselves.
pub fn rebuild(root: &Path, id: &BundleId, edit: &Edit) -> Result<Vec<u8>, Vec<ImportError>> {
    let dir = root.join(id.as_str());
    let mut manifest = bundle::read_installed_manifest(&dir)?;
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for profile in &manifest.profiles {
        let content = fs::read(dir.join(&profile.file)).map_err(|e| {
            vec![ImportError::in_file(
                &profile.file,
                format!("installed config is unreadable: {e}"),
            )]
        })?;
        files.push((profile.file.clone(), content));
    }

    match edit {
        Edit::Add(new) => add(&mut manifest, &mut files, new)?,
        Edit::Remove(ids) => remove(&mut manifest, &mut files, ids)?,
    }

    pack(&manifest, files)
}

/// The zip of a new bundle called `name` ("My servers" when empty) made of these config
/// files, for the caller to import: how a user whose provider hands out single files, not a
/// bundle, gets one. The same checks, ids and bytes as an added server gets.
pub fn create(name: &str, new: &[NewProfile]) -> Result<Vec<u8>, Vec<ImportError>> {
    let name = match name.trim() {
        "" => DEFAULT_BUNDLE_NAME,
        name => name,
    };
    let mut manifest = Manifest {
        schema_version: SCHEMA_VERSION.to_string(),
        bundle_name: name.to_string(),
        protocols: None,
        rotation: Rotation::default(),
        profiles: Vec::new(),
    };
    let mut files = Vec::new();
    add(&mut manifest, &mut files, new)?;
    pack(&manifest, files)
}

/// When `zip_bytes` is a zip of bare config files (no `manifest.json` anywhere, at least one
/// `.conf` or `.ovpn`), the bundle zip [`create`] makes of them; `None` for anything else, which is
/// the import's to judge. The files may sit in folders: only their own names are used.
pub fn from_config_zip(zip_bytes: &[u8]) -> Option<Result<Vec<u8>, Vec<ImportError>>> {
    if zip_bytes.len() > MAX_ZIP_BYTES {
        return None;
    }
    let archive = ZipArchive::new(Cursor::new(zip_bytes)).ok()?;
    // A manifest anywhere means a bundle was meant (zipped one folder too deep, say): the
    // import's own errors describe that better than "only config files" would.
    if archive
        .file_names()
        .any(|name| base_name(name).eq_ignore_ascii_case(MANIFEST_FILE))
        || !archive
            .file_names()
            .any(|name| protocol_of(base_name(name)).is_some())
    {
        return None;
    }
    Some(
        bundle::read_entries(zip_bytes, check_config_entry).and_then(|entries| {
            // In the archive's name order, which a `BTreeMap` already gives.
            let new: Vec<NewProfile> = entries
                .into_iter()
                .map(|(path, content)| NewProfile {
                    name: String::new(),
                    file_name: base_name(&path).to_string(),
                    content,
                })
                .collect();
            create(DEFAULT_BUNDLE_NAME, &new)
        }),
    )
}

/// An entry's own name, whichever separator the tool that made the zip wrote.
fn base_name(entry: &str) -> &str {
    entry.rsplit(['/', '\\']).next().unwrap_or(entry)
}

/// In a zip of bare configs every file is one; folders are only passed through.
fn check_config_entry(name: &str) -> Result<(), &'static str> {
    if name.ends_with(['/', '\\']) || protocol_of(base_name(name)).is_some() {
        Ok(())
    } else {
        Err("a zip without manifest.json may hold only .conf and .ovpn files")
    }
}

fn pack(
    manifest: &Manifest,
    mut files: Vec<(String, Vec<u8>)>,
) -> Result<Vec<u8>, Vec<ImportError>> {
    let mut entries = vec![(
        MANIFEST_FILE.to_string(),
        serde_json::to_vec_pretty(manifest).expect("a manifest always serializes"),
    )];
    files.sort();
    entries.extend(files);
    write_zip(&entries).map_err(|e| {
        vec![ImportError::archive(format!(
            "could not build the bundle: {e}"
        ))]
    })
}

fn add(
    manifest: &mut Manifest,
    files: &mut Vec<(String, Vec<u8>)>,
    new: &[NewProfile],
) -> Result<(), Vec<ImportError>> {
    let mut errors = Vec::new();
    if new.is_empty() {
        errors.push(ImportError::archive("no config file was given"));
    }
    if manifest.profiles.len() + new.len() > MAX_PROFILES {
        errors.push(ImportError::archive(format!(
            "a bundle holds at most {MAX_PROFILES} servers"
        )));
    }
    for profile in new {
        let Some(protocol) = protocol_of(&profile.file_name) else {
            errors.push(ImportError::in_file(
                &profile.file_name,
                "only .conf (WireGuard) and .ovpn (OpenVPN) files can be added",
            ));
            continue;
        };
        if profile.content.len() > MAX_CONFIG_BYTES {
            errors.push(ImportError::in_file(
                &profile.file_name,
                "config file is larger than 1 MB",
            ));
            continue;
        }
        let extension = match protocol {
            Protocol::Wireguard => "conf",
            Protocol::Openvpn => "ovpn",
        };
        let path_of = |id: &str| format!("{}/{id}.{extension}", protocol.dir());
        // Free as an id, and as a file name whatever the case: Windows would fold them.
        let taken_ids: HashSet<&str> = manifest.profiles.iter().map(|p| p.id.as_str()).collect();
        let taken_files: HashSet<String> = files.iter().map(|(f, _)| f.to_lowercase()).collect();
        let id = profile_id_from(&profile.file_name, |id| {
            !taken_ids.contains(id) && !taken_files.contains(&path_of(id))
        });
        let file = path_of(&id);
        // The given name, else the file's, else (a file called just `.conf`) the id.
        let name = [profile.name.trim(), stem(&profile.file_name).trim(), &id]
            .into_iter()
            .find(|name| !name.is_empty())
            .expect("an id is never empty")
            .to_string();
        if let Some(listed) = &mut manifest.protocols
            && !listed.contains(&protocol)
        {
            listed.push(protocol);
        }
        files.push((file.clone(), profile.content.clone()));
        manifest.profiles.push(Profile {
            id,
            name,
            protocol,
            file,
        });
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn remove(
    manifest: &mut Manifest,
    files: &mut Vec<(String, Vec<u8>)>,
    ids: &[String],
) -> Result<(), Vec<ImportError>> {
    let mut errors = Vec::new();
    if ids.is_empty() {
        errors.push(ImportError::archive("no server was named"));
    }
    for id in ids {
        if !manifest.profiles.iter().any(|p| &p.id == id) {
            errors.push(ImportError::archive(format!(
                "the bundle has no server \"{id}\""
            )));
        }
    }
    let removed: HashSet<&String> = ids.iter().collect();
    if errors.is_empty() && manifest.profiles.iter().all(|p| removed.contains(&p.id)) {
        errors.push(ImportError::archive(
            "a bundle keeps at least one server: delete the bundle instead",
        ));
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    manifest.profiles.retain(|p| !removed.contains(&p.id));
    let kept: HashSet<&str> = manifest.profiles.iter().map(|p| p.file.as_str()).collect();
    files.retain(|(file, _)| kept.contains(file.as_str()));
    Ok(())
}

/// The entries in the order given, with fixed timestamps and compression: nothing in the
/// bytes depends on when or where the zip was written.
fn write_zip(entries: &[(String, Vec<u8>)]) -> zip::result::ZipResult<Vec<u8>> {
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(DateTime::default())
        .unix_permissions(0o600);
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in entries {
        writer.start_file(name, options)?;
        writer.write_all(content)?;
    }
    Ok(writer.finish()?.into_inner())
}
