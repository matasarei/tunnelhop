//! WireGuard `.conf` sanitizer.
//!
//! Keys are matched case-insensitively, as WireGuard for Windows does, against an allowlist
//! per section. The hook keys (`PreUp`, `PostUp`, `PreDown`, `PostDown`) and `Table` are
//! refused by name so the error says why.

use super::{Endpoint, SanitizedConfig, Transport, is_valid_host, parse_port};
use crate::bundle::ImportError;

const INTERFACE_KEYS: &[&str] = &["privatekey", "listenport", "address", "dns", "mtu"];
const PEER_KEYS: &[&str] = &[
    "publickey",
    "presharedkey",
    "allowedips",
    "endpoint",
    "persistentkeepalive",
];
const REFUSED_KEYS: &[&str] = &["preup", "postup", "predown", "postdown", "table"];

#[derive(Clone, Copy, PartialEq)]
enum Section {
    None,
    Interface,
    Peer,
}

#[derive(Default)]
struct Peer {
    line: usize,
    has_public_key: bool,
    endpoint: Option<Endpoint>,
}

pub fn sanitize(file: &str, text: &str) -> Result<SanitizedConfig, Vec<ImportError>> {
    let mut errors = Vec::new();
    let mut section = Section::None;
    let mut interfaces = 0;
    let mut has_private_key = false;
    let mut peers: Vec<Peer> = Vec::new();

    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let err = |message: String| ImportError::at_line(file, number, message);
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with('[') {
            match line.to_ascii_lowercase().as_str() {
                "[interface]" => {
                    section = Section::Interface;
                    interfaces += 1;
                }
                "[peer]" => {
                    section = Section::Peer;
                    peers.push(Peer {
                        line: number,
                        ..Peer::default()
                    });
                }
                _ => errors.push(err(format!("unknown section {line}"))),
            }
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            errors.push(err("expected \"Key = Value\"".into()));
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        let lower = key.to_ascii_lowercase();

        if REFUSED_KEYS.contains(&lower.as_str()) {
            errors.push(err(format!("{key} is not allowed")));
            continue;
        }
        match section {
            Section::None => errors.push(err(format!("{key} is outside any section"))),
            Section::Interface if !INTERFACE_KEYS.contains(&lower.as_str()) => {
                errors.push(err(format!("{key} is not allowed in [Interface]")))
            }
            Section::Peer if !PEER_KEYS.contains(&lower.as_str()) => {
                errors.push(err(format!("{key} is not allowed in [Peer]")))
            }
            Section::Peer => {
                let peer = peers.last_mut().expect("a [Peer] line opened this section");
                match lower.as_str() {
                    "publickey" => peer.has_public_key = !value.is_empty(),
                    "endpoint" => match parse_endpoint(value) {
                        Ok(endpoint) => peer.endpoint = Some(endpoint),
                        Err(message) => errors.push(err(message)),
                    },
                    _ => {}
                }
            }
            Section::Interface => {
                if lower == "privatekey" && !value.is_empty() {
                    has_private_key = true;
                }
            }
        }
    }

    if interfaces != 1 {
        errors.push(ImportError::in_file(
            file,
            "must have exactly one [Interface] section",
        ));
    } else if !has_private_key {
        errors.push(ImportError::in_file(file, "[Interface] has no PrivateKey"));
    }
    if peers.is_empty() {
        errors.push(ImportError::in_file(file, "must have at least one [Peer]"));
    }
    let mut endpoints = Vec::new();
    for peer in peers {
        if !peer.has_public_key {
            errors.push(ImportError::at_line(
                file,
                peer.line,
                "[Peer] has no PublicKey",
            ));
        }
        match peer.endpoint {
            Some(endpoint) => endpoints.push(endpoint),
            None => errors.push(ImportError::at_line(
                file,
                peer.line,
                "[Peer] has no Endpoint",
            )),
        }
    }

    if errors.is_empty() {
        Ok(SanitizedConfig {
            text: text.to_string(),
            endpoints,
            credentials: None,
        })
    } else {
        Err(errors)
    }
}

/// `host:port` or `[ipv6]:port`.
fn parse_endpoint(value: &str) -> Result<Endpoint, String> {
    let invalid = || format!("invalid Endpoint \"{value}\"");
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let (host, port) = rest.split_once("]:").ok_or_else(invalid)?;
        (host, port)
    } else {
        value.rsplit_once(':').ok_or_else(invalid)?
    };
    if !is_valid_host(host) || (host.contains(':') && !value.starts_with('[')) {
        return Err(invalid());
    }
    Ok(Endpoint {
        host: host.to_string(),
        port: parse_port(port).ok_or_else(invalid)?,
        transport: Transport::Udp,
    })
}
