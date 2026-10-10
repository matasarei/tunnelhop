//! The protocol between the UI and the service over `\\.\pipe\tunnelhop` — SPEC §2.2.
//!
//! One JSON object per line. The client opens with `Hello` carrying [`PROTOCOL_VERSION`]; the
//! service answers `Hello` or an error and closes. After that the client sends numbered
//! requests and the service answers each by number, interleaved with unsolicited events.
//!
//! Everything arriving here is untrusted: frames are size-limited before they are parsed, and
//! every request is validated before the service acts on it.

use std::fmt;
use std::net::IpAddr;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::bundle::MAX_ZIP_BYTES;
use crate::edit::MAX_CONFIG_BYTES;
use crate::manifest::Protocol;
use crate::rotation::State;

/// Raised whenever a request, response or event is added or changed: a client and a service
/// from different versions then refuse each other at `Hello` instead of failing later on a
/// message one of them cannot parse.
pub const PROTOCOL_VERSION: u32 = 3;
/// Largest line either side accepts: a maximal bundle in base64 plus the envelope.
pub const MAX_FRAME_BYTES: usize = MAX_ZIP_BYTES / 3 * 4 + 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Hello {
        version: u32,
    },
    Ping,
    GetStatus,
    /// The zip itself, base64-encoded: the service never opens a path the user chose.
    ImportBundle {
        zip_base64: String,
    },
    SelectBundle {
        bundle_id: String,
    },
    /// Every installed bundle, and which one is active.
    ListBundles,
    /// Remove an installed bundle and its stored credentials. The active one is disconnected
    /// first, and no bundle is active afterwards.
    DeleteBundle {
        bundle_id: String,
    },
    /// Add servers to a bundle from config files (SPEC §3.5). The bundle is replaced by a new
    /// one with a new id; answered like an import.
    AddProfiles {
        bundle_id: String,
        files: Vec<ProfileFile>,
    },
    /// Make a new bundle of these config files (SPEC §3.5), for a user whose provider hands
    /// out single files. An empty `name` means "My servers". Answered like an import; the new
    /// bundle also becomes the active one when none is and nothing is blocked.
    CreateBundle {
        name: String,
        files: Vec<ProfileFile>,
    },
    /// Remove servers from a bundle; it is replaced the same way.
    RemoveProfiles {
        bundle_id: String,
        profile_ids: Vec<String>,
    },
    /// Connect to this profile, or to a random one when `None`.
    Connect {
        profile_id: Option<String>,
    },
    RotateNow,
    SetAutoRotate {
        enabled: bool,
    },
    SetInterval {
        minutes: u32,
    },
    /// "Disconnect & unblock".
    Disconnect,
    /// The UI is exiting; with `disconnect`, also bring everything down.
    Shutdown {
        disconnect: bool,
    },
    /// OpenVPN credentials for profiles in a bundle; an empty `profile_ids` means every
    /// OpenVPN profile in it.
    SetCredentials {
        bundle_id: String,
        profile_ids: Vec<String>,
        username: String,
        password: Secret,
    },
}

/// One config file to add as a server. Like a bundle, it travels as bytes: the service never
/// opens a path the user chose.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileFile {
    /// The display name; empty means "the file name without its extension".
    pub name: String,
    /// The file's own name, without any directory: `.conf` is WireGuard, `.ovpn` OpenVPN.
    pub file_name: String,
    pub content_base64: String,
}

/// A config holds a private key: never printed.
impl fmt::Debug for ProfileFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProfileFile")
            .field("name", &self.name)
            .field("file_name", &self.file_name)
            .field("content_base64", &"<redacted>")
            .finish()
    }
}

impl ProfileFile {
    pub fn new(name: &str, file_name: &str, content: &[u8]) -> Self {
        ProfileFile {
            name: name.to_string(),
            file_name: file_name.to_string(),
            content_base64: BASE64.encode(content),
        }
    }

    /// The file's bytes, enforcing the size limit.
    pub fn content(&self) -> Result<Vec<u8>, IpcError> {
        let too_large = || IpcError::Invalid(format!("{} is larger than 1 MB", self.file_name));
        if self.content_base64.len() > MAX_CONFIG_BYTES / 3 * 4 + 4 {
            return Err(too_large());
        }
        let content = BASE64.decode(&self.content_base64).map_err(|e| {
            IpcError::Invalid(format!("{} is not valid base64: {e}", self.file_name))
        })?;
        if content.len() > MAX_CONFIG_BYTES {
            return Err(too_large());
        }
        Ok(content)
    }

    fn validate(&self) -> Result<(), IpcError> {
        let name = &self.file_name;
        let chars = name.chars().count();
        if chars == 0 || chars > MAX_FILE_NAME_CHARS {
            return Err(IpcError::Invalid(format!(
                "a file name must be 1 to {MAX_FILE_NAME_CHARS} characters"
            )));
        }
        if name.contains(['/', '\\', ':']) || name.chars().any(char::is_control) {
            return Err(IpcError::Invalid(
                "a file name must not contain a path or control characters".into(),
            ));
        }
        if crate::edit::protocol_of(name).is_none() {
            return Err(IpcError::Invalid(format!(
                "{name}: only .conf (WireGuard) and .ovpn (OpenVPN) files can be added"
            )));
        }
        if self.name.chars().count() > MAX_NAME_CHARS {
            return Err(IpcError::Invalid(format!(
                "a server name must be at most {MAX_NAME_CHARS} characters"
            )));
        }
        self.content().map(|_| ())
    }
}

const MAX_FILE_NAME_CHARS: usize = 255;
/// The manifest's limit for a profile name.
const MAX_NAME_CHARS: usize = 100;

/// A value that must never reach a log: `Debug` prints a placeholder, and the memory is wiped
/// when the value is dropped.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

const MAX_CREDENTIAL_CHARS: usize = 256;

impl Request {
    /// Check the request's own values; the service still checks them against its state.
    pub fn validate(&self) -> Result<(), IpcError> {
        match self {
            Request::ImportBundle { zip_base64 } => decode_zip(zip_base64).map(|_| ()),
            Request::SetInterval { minutes } if !(1..=1440).contains(minutes) => Err(
                IpcError::Invalid("interval must be between 1 and 1440 minutes".into()),
            ),
            Request::SelectBundle { bundle_id } | Request::DeleteBundle { bundle_id } => {
                check_bundle_id(bundle_id)
            }
            Request::AddProfiles { bundle_id, files } => {
                check_bundle_id(bundle_id)?;
                check_files(files)
            }
            Request::CreateBundle { name, files } => {
                if name.chars().count() > MAX_BUNDLE_NAME_CHARS {
                    return Err(IpcError::Invalid(format!(
                        "a bundle's name is at most {MAX_BUNDLE_NAME_CHARS} characters"
                    )));
                }
                check_files(files)
            }
            Request::RemoveProfiles {
                bundle_id,
                profile_ids,
            } => {
                check_bundle_id(bundle_id)?;
                if profile_ids.is_empty() || profile_ids.len() > crate::manifest::MAX_PROFILES {
                    return Err(IpcError::Invalid(
                        "name at least one server to remove".into(),
                    ));
                }
                profile_ids.iter().try_for_each(|id| check_profile_id(id))
            }
            Request::Connect {
                profile_id: Some(id),
            } => check_profile_id(id),
            Request::SetCredentials {
                bundle_id,
                profile_ids,
                username,
                password,
            } => {
                check_bundle_id(bundle_id)?;
                if profile_ids.len() > crate::manifest::MAX_PROFILES {
                    return Err(IpcError::Invalid("too many profile ids".into()));
                }
                profile_ids.iter().try_for_each(|id| check_profile_id(id))?;
                check_credential("username", username)?;
                check_credential("password", password.expose())
            }
            _ => Ok(()),
        }
    }
}

fn check_bundle_id(id: &str) -> Result<(), IpcError> {
    if crate::bundle::BundleId::parse(id).is_some() {
        Ok(())
    } else {
        Err(IpcError::Invalid(
            "bundle id must be 16 lowercase hex characters".into(),
        ))
    }
}

/// The manifest's limit on `bundle_name`, so a longer one is refused before a zip is built.
const MAX_BUNDLE_NAME_CHARS: usize = 100;

fn check_files(files: &[ProfileFile]) -> Result<(), IpcError> {
    if files.is_empty() || files.len() > crate::manifest::MAX_PROFILES {
        return Err(IpcError::Invalid(format!(
            "between 1 and {} files can be added at once",
            crate::manifest::MAX_PROFILES
        )));
    }
    files.iter().try_for_each(ProfileFile::validate)
}

/// The manifest's own rule: ids become storage keys in the service, so nothing else is let in.
fn check_profile_id(id: &str) -> Result<(), IpcError> {
    if crate::manifest::is_valid_id(id) {
        Ok(())
    } else {
        Err(IpcError::Invalid(
            "profile id must match ^[a-z0-9-]{1,40}$".into(),
        ))
    }
}

/// Credentials end up as one line each in OpenVPN's management protocol: no line breaks or
/// NUL, and a bounded length.
fn check_credential(what: &str, value: &str) -> Result<(), IpcError> {
    let chars = value.chars().count();
    if chars == 0 || chars > MAX_CREDENTIAL_CHARS {
        return Err(IpcError::Invalid(format!(
            "{what} must be 1 to {MAX_CREDENTIAL_CHARS} characters"
        )));
    }
    if value.contains(['\r', '\n', '\0']) {
        return Err(IpcError::Invalid(format!(
            "{what} must not contain line breaks or NUL"
        )));
    }
    Ok(())
}

/// Decode an `ImportBundle` payload, enforcing the archive size limit.
pub fn decode_zip(zip_base64: &str) -> Result<Vec<u8>, IpcError> {
    if zip_base64.len() > MAX_ZIP_BYTES / 3 * 4 + 4 {
        return Err(IpcError::Invalid("bundle is larger than 20 MB".into()));
    }
    BASE64
        .decode(zip_base64)
        .map_err(|e| IpcError::Invalid(format!("bundle is not valid base64: {e}")))
}

pub fn encode_zip(zip: &[u8]) -> String {
    BASE64.encode(zip)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Hello {
        version: u32,
    },
    Pong,
    Status {
        report: StatusReport,
    },
    Imported {
        bundle: BundleInfo,
        already_present: bool,
        /// OpenVPN profiles that cannot connect until the user gives credentials.
        needs_credentials: Vec<String>,
    },
    ImportFailed {
        errors: Vec<String>,
    },
    /// The installed bundles, ordered by name, and the id of the active one.
    Bundles {
        bundles: Vec<BundleInfo>,
        active: Option<String>,
    },
    Ok,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Status {
        report: StatusReport,
    },
    /// Something the user must see, such as "every attempt failed — traffic is blocked".
    Alert {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientMessage {
    pub id: u64,
    pub request: Request,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerMessage {
    Response { id: u64, response: Response },
    Event { event: Event },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusReport {
    pub state: State,
    pub profile_id: Option<String>,
    pub public_ip: Option<IpAddr>,
    pub killswitch_engaged: bool,
    pub bundle: Option<BundleInfo>,
    pub auto_rotate: bool,
    pub interval_minutes: u32,
    pub runtimes: Vec<RuntimeInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleInfo {
    pub bundle_id: String,
    pub bundle_name: String,
    pub profiles: Vec<ProfileInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileInfo {
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    /// False for an OpenVPN profile still waiting for credentials.
    pub has_credentials: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeInfo {
    pub protocol: Protocol,
    pub availability: Availability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Availability {
    Available { version: String },
    Missing,
    TooOld { found: String, required: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IpcError {
    #[error("message is larger than {MAX_FRAME_BYTES} bytes")]
    TooLarge,
    #[error("malformed message: {0}")]
    Malformed(String),
    #[error("protocol version {client} is not supported; the service speaks {PROTOCOL_VERSION}")]
    VersionMismatch { client: u32 },
    #[error("{0}")]
    Invalid(String),
}

/// Answer a client's `Hello`.
pub fn check_version(client: u32) -> Result<Response, IpcError> {
    if client == PROTOCOL_VERSION {
        Ok(Response::Hello {
            version: PROTOCOL_VERSION,
        })
    } else {
        Err(IpcError::VersionMismatch { client })
    }
}

/// One message as a line, ready to write to the pipe.
pub fn encode<T: Serialize>(message: &T) -> Vec<u8> {
    let mut line = serde_json::to_vec(message).expect("IPC types always serialize");
    line.push(b'\n');
    line
}

pub fn decode<T: DeserializeOwned>(line: &[u8]) -> Result<T, IpcError> {
    if line.len() > MAX_FRAME_BYTES {
        return Err(IpcError::TooLarge);
    }
    serde_json::from_slice(line).map_err(|e| IpcError::Malformed(e.to_string()))
}

/// Accumulates bytes read from the pipe and yields complete lines.
#[derive(Debug, Default)]
pub struct FrameBuffer {
    buffer: Vec<u8>,
}

impl FrameBuffer {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The next complete line without its newline, `None` until one has arrived. A peer that
    /// sends more than [`MAX_FRAME_BYTES`] without a newline is an error: drop the connection.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, IpcError> {
        match self.buffer.iter().position(|&b| b == b'\n') {
            Some(end) if end > MAX_FRAME_BYTES => Err(IpcError::TooLarge),
            Some(end) => {
                let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                Ok(Some(line))
            }
            None if self.buffer.len() > MAX_FRAME_BYTES => Err(IpcError::TooLarge),
            None => Ok(None),
        }
    }
}
