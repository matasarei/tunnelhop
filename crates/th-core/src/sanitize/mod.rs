//! Config sanitizers — SPEC §3.4.
//!
//! Configs from a bundle run with LocalSystem privileges, so each one is parsed against an
//! allowlist: anything that could execute code or touch files is refused with its line number.
//! A sanitizer also extracts the server endpoints the kill-switch must let through.

use std::fmt;
use std::net::IpAddr;

use crate::bundle::ImportError;
use crate::manifest::Protocol;

pub mod openvpn;
pub mod wireguard;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Transport {
    Udp,
    Tcp,
}

/// A VPN server as written in the config. `host` may still be a name; it is resolved to an
/// address right before connecting.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub transport: Transport,
}

/// OpenVPN username and password moved out of the bundle's `auth-user-pass` file.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedConfig {
    /// The config to install. Equal to the input except where a directive was rewritten.
    pub text: String,
    pub endpoints: Vec<Endpoint>,
    pub credentials: Option<Credentials>,
}

/// Sanitize one profile's config. `file` is its path inside the bundle; `read` returns another
/// bundle file by path (used for OpenVPN's `auth-user-pass <file>`).
pub fn sanitize(
    protocol: Protocol,
    file: &str,
    bytes: &[u8],
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<SanitizedConfig, Vec<ImportError>> {
    let text = decode(file, bytes)?;
    match protocol {
        Protocol::Openvpn => openvpn::sanitize(file, &text, read),
        Protocol::Wireguard => wireguard::sanitize(file, &text),
    }
}

fn decode(file: &str, bytes: &[u8]) -> Result<String, Vec<ImportError>> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    String::from_utf8(bytes.to_vec())
        .map_err(|_| vec![ImportError::in_file(file, "is not valid UTF-8 text")])
}

/// A host name or an IP address, nothing else.
pub(crate) fn is_valid_host(host: &str) -> bool {
    if host.parse::<IpAddr>().is_ok() {
        return true;
    }
    !host.is_empty()
        && host.len() <= 253
        && !host.starts_with(['-', '.'])
        && !host.ends_with('-')
        && !host.contains("..")
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
}

pub(crate) fn parse_port(value: &str) -> Option<u16> {
    match value.parse::<u16>() {
        Ok(0) | Err(_) => None,
        Ok(port) => Some(port),
    }
}
