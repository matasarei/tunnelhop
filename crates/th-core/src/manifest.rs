//! The bundle manifest (`manifest.json`): its model and every rule from SPEC §3.2.
//!
//! Parsing rejects unknown fields; [`parse`] then runs the rules serde cannot express and
//! reports all violations at once, so a bundle author fixes them in one pass.

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: &str = "1";
pub const MAX_PROFILES: usize = 500;
/// The most characters in a bundle's name or a profile's.
pub const MAX_NAME_CHARS: usize = 100;
const MAX_ID_CHARS: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Wireguard,
    Openvpn,
}

impl Protocol {
    /// The top-level archive directory this protocol's configs must live in.
    pub fn dir(self) -> &'static str {
        match self {
            Protocol::Wireguard => "wireguard",
            Protocol::Openvpn => "openvpn",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnDisconnect {
    ReconnectSame,
    ReconnectRandom,
    StayDown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: String,
    pub bundle_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocols: Option<Vec<Protocol>>,
    #[serde(default)]
    pub rotation: Rotation,
    pub profiles: Vec<Profile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rotation {
    pub enabled: bool,
    pub strategy: Strategy,
    pub interval_minutes: u32,
    pub on_disconnect: OnDisconnect,
    pub reconnect_grace_seconds: u32,
}

impl Default for Rotation {
    fn default() -> Self {
        Rotation {
            enabled: true,
            strategy: Strategy::Random,
            interval_minutes: 60,
            on_disconnect: OnDisconnect::ReconnectRandom,
            reconnect_grace_seconds: 20,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub file: String,
}

impl Manifest {
    /// Protocols rotation may pick from: the declared list, or every protocol a profile uses.
    pub fn rotation_protocols(&self) -> Vec<Protocol> {
        if let Some(declared) = &self.protocols {
            return declared.clone();
        }
        let mut used = Vec::new();
        for profile in &self.profiles {
            if !used.contains(&profile.protocol) {
                used.push(profile.protocol);
            }
        }
        used
    }
}

/// One rule violation, located by a JSON-path-like `field` such as `profiles[2].id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError {
    pub field: String,
    pub message: String,
}

impl ManifestError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        ManifestError {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

/// Parse and validate `manifest.json`. Returns every violation found, not just the first.
pub fn parse(bytes: &[u8]) -> Result<Manifest, Vec<ManifestError>> {
    let manifest: Manifest = serde_json::from_slice(bytes)
        .map_err(|e| vec![ManifestError::new("manifest.json", e.to_string())])?;
    let errors = validate(&manifest);
    if errors.is_empty() {
        Ok(manifest)
    } else {
        Err(errors)
    }
}

fn validate(m: &Manifest) -> Vec<ManifestError> {
    let mut errors = Vec::new();

    if m.schema_version != SCHEMA_VERSION {
        errors.push(ManifestError::new(
            "schema_version",
            format!("must be \"{SCHEMA_VERSION}\""),
        ));
    }
    check_name("bundle_name", &m.bundle_name, &mut errors);

    if let Some(protocols) = &m.protocols {
        if protocols.is_empty() {
            errors.push(ManifestError::new("protocols", "must not be empty"));
        }
        let unique: HashSet<_> = protocols.iter().collect();
        if unique.len() != protocols.len() {
            errors.push(ManifestError::new(
                "protocols",
                "must not repeat a protocol",
            ));
        }
    }

    let r = &m.rotation;
    if !(1..=1440).contains(&r.interval_minutes) {
        errors.push(ManifestError::new(
            "rotation.interval_minutes",
            "must be between 1 and 1440",
        ));
    }
    if !(5..=300).contains(&r.reconnect_grace_seconds) {
        errors.push(ManifestError::new(
            "rotation.reconnect_grace_seconds",
            "must be between 5 and 300",
        ));
    }

    if m.profiles.is_empty() {
        errors.push(ManifestError::new(
            "profiles",
            "must have at least one profile",
        ));
    }
    if m.profiles.len() > MAX_PROFILES {
        errors.push(ManifestError::new(
            "profiles",
            format!("must have at most {MAX_PROFILES} profiles"),
        ));
    }

    let mut seen_ids = HashSet::new();
    for (i, p) in m.profiles.iter().enumerate() {
        let at = |field: &str| format!("profiles[{i}].{field}");
        if !is_valid_id(&p.id) {
            errors.push(ManifestError::new(at("id"), "must match ^[a-z0-9-]{1,40}$"));
        } else if !seen_ids.insert(p.id.as_str()) {
            errors.push(ManifestError::new(
                at("id"),
                format!("duplicate id \"{}\"", p.id),
            ));
        }
        check_name(&at("name"), &p.name, &mut errors);
        if let Err(message) = check_profile_file(&p.file, p.protocol) {
            errors.push(ManifestError::new(at("file"), message));
        }
    }

    if !m.profiles.is_empty() {
        let allowed = m.rotation_protocols();
        if !m.profiles.iter().any(|p| allowed.contains(&p.protocol)) {
            errors.push(ManifestError::new(
                "protocols",
                "no profile uses any of the listed protocols",
            ));
        }
    }

    errors
}

fn check_name(field: &str, value: &str, errors: &mut Vec<ManifestError>) {
    let chars = value.chars().count();
    if chars == 0 || chars > MAX_NAME_CHARS {
        errors.push(ManifestError::new(
            field,
            format!("must be 1 to {MAX_NAME_CHARS} characters"),
        ));
    }
}

/// A profile id: `^[a-z0-9-]{1,40}$`. Ids become file names in the service, so nothing else
/// is let in.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_CHARS
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A profile's `file`: a relative, `/`-separated path inside the protocol's own directory.
fn check_profile_file(file: &str, protocol: Protocol) -> Result<(), String> {
    if file.contains('\\') || file.contains(':') || file.contains('\0') {
        return Err("must be a relative path using '/' only".into());
    }
    let segments: Vec<&str> = file.split('/').collect();
    if segments
        .iter()
        .any(|s| s.is_empty() || *s == "." || *s == "..")
    {
        return Err("must not contain empty, '.' or '..' segments".into());
    }
    if segments.len() < 2 || segments[0] != protocol.dir() {
        return Err(format!("must be inside {}/", protocol.dir()));
    }
    Ok(())
}
