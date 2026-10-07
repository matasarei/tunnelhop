//! OpenVPN `.ovpn` sanitizer.
//!
//! Lines are split into tokens the way OpenVPN itself does — whitespace-separated, `"…"` with
//! backslash escapes, `'…'` literal, a backslash escaping the next character outside single
//! quotes, `#`/`;` starting a comment between tokens, and a leading `--` on the directive
//! dropped — so a directive cannot hide from the allowlist behind quoting.

use super::{Credentials, Endpoint, SanitizedConfig, Transport, is_valid_host, parse_port};
use zeroize::Zeroizing;

use crate::bundle::ImportError;

const DEFAULT_PORT: u16 = 1194;
/// OpenVPN reads a config in chunks of this many bytes, so a longer line would be split into
/// two directives that this parser never saw separately.
const MAX_LINE_BYTES: usize = 255;

/// Directives allowed with any arguments.
const ALLOWED: &[&str] = &[
    "client",
    "dev",
    "dev-type",
    "resolv-retry",
    "nobind",
    "persist-key",
    "persist-tun",
    "remote-random",
    "remote-cert-tls",
    "cipher",
    "data-ciphers",
    "data-ciphers-fallback",
    "auth",
    "tls-version-min",
    "tls-cipher",
    "tls-ciphersuites",
    "key-direction",
    "verb",
    "mute",
    "auth-nocache",
    "pull-filter",
    "redirect-gateway",
    "block-ipv6",
    "block-outside-dns",
    "ping",
    "ping-restart",
    "connect-retry",
    "connect-retry-max",
    "server-poll-timeout",
    "float",
    "mssfix",
    "tun-mtu",
    "fragment",
    "sndbuf",
    "rcvbuf",
    "verify-x509-name",
    "explicit-exit-notify",
    "windows-driver",
];

const INLINE_BLOCKS: &[&str] = &["ca", "cert", "key", "tls-auth", "tls-crypt", "tls-crypt-v2"];

pub fn sanitize(
    file: &str,
    text: &str,
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<SanitizedConfig, Vec<ImportError>> {
    let mut errors = Vec::new();
    let mut output = Vec::new();
    let mut remotes: Vec<(usize, Vec<String>)> = Vec::new();
    let mut proto = Transport::Udp;
    let mut port = DEFAULT_PORT;
    let mut credentials = None;
    let mut asks_credentials = false;
    let mut inline_until: Option<(String, usize)> = None;

    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let mut emitted = line.to_string();
        let err = |message: String| ImportError::at_line(file, number, message);

        // Checked inside inline blocks too: OpenVPN reads block content in the same chunks and
        // looks for the closing tag in each, so a long line could end a block for OpenVPN while
        // this parser still treats what follows as data.
        if line.len() > MAX_LINE_BYTES {
            errors.push(err(format!("line is longer than {MAX_LINE_BYTES} bytes")));
            continue;
        }
        if line.contains('\0') {
            errors.push(err("line contains a NUL byte".into()));
            continue;
        }

        if let Some((tag, _)) = &inline_until {
            if line.trim_start().starts_with(&format!("</{tag}>")) {
                inline_until = None;
            }
            output.push(emitted);
            continue;
        }

        let tokens = match tokenize(line) {
            Ok(tokens) => tokens,
            Err(message) => {
                errors.push(err(message));
                continue;
            }
        };
        let Some(first) = tokens.first() else {
            output.push(emitted);
            continue;
        };
        let name = first.strip_prefix("--").unwrap_or(first);
        let args = &tokens[1..];

        if let Some(tag) = name.strip_prefix('<').and_then(|t| t.strip_suffix('>')) {
            if INLINE_BLOCKS.contains(&tag) {
                inline_until = Some((tag.to_string(), number));
            } else {
                errors.push(err(format!("inline block <{tag}> is not allowed")));
            }
            output.push(emitted);
            continue;
        }

        let verdict: Result<(), String> = match name {
            n if ALLOWED.contains(&n) => Ok(()),
            "remote" => {
                if (1..=3).contains(&args.len()) {
                    remotes.push((number, args.to_vec()));
                    Ok(())
                } else {
                    Err("remote needs a host, and optionally a port and a protocol".into())
                }
            }
            "proto" => match args {
                [value] => parse_transport(value).map(|t| proto = t),
                _ => Err("proto needs exactly one value".into()),
            },
            "port" => match args {
                [value] => parse_port(value)
                    .map(|p| port = p)
                    .ok_or_else(|| format!("invalid port \"{value}\"")),
                _ => Err("port needs exactly one value".into()),
            },
            "lport" => match args {
                [value] if parse_port(value).is_some() => Ok(()),
                _ => Err("lport needs one valid port".into()),
            },
            "tls-auth" | "tls-crypt" | "tls-crypt-v2" => match args.first().map(String::as_str) {
                None | Some("[inline]") => Ok(()),
                Some(_) => Err(format!("{name} must use an inline <{name}> block")),
            },
            "comp-lzo" => match args {
                [value] if value == "no" => Ok(()),
                _ => Err("only \"comp-lzo no\" is allowed".into()),
            },
            "allow-compression" => match args {
                [value] if value == "no" || value == "asym" => Ok(()),
                _ => Err("only \"allow-compression no\" or \"asym\" is allowed".into()),
            },
            // `setenv` only feeds scripts, which are refused, but stays an exact-match list:
            // the two forms providers actually ship.
            "setenv" => {
                if args == ["opt", "block-outside-dns"] || args == ["CLIENT_CERT", "0"] {
                    Ok(())
                } else {
                    Err(
                        "only \"setenv opt block-outside-dns\" and \"setenv CLIENT_CERT 0\" are allowed"
                            .into(),
                    )
                }
            }
            "reneg-sec" => match args {
                [value] if value.parse::<u32>().is_ok() => Ok(()),
                _ => Err("reneg-sec needs one number of seconds".into()),
            },
            "auth-user-pass" => {
                asks_credentials = true;
                match args {
                    [] => Ok(()),
                    [path] => read_credentials(file, path, read).map(|c| {
                        credentials = Some(c);
                        emitted = "auth-user-pass".to_string();
                    }),
                    _ => Err("auth-user-pass takes at most one file".into()),
                }
            }
            other => Err(format!("directive \"{other}\" is not allowed")),
        };
        if let Err(message) = verdict {
            errors.push(err(message));
        }
        output.push(emitted);
    }

    if let Some((tag, opened_at)) = inline_until {
        errors.push(ImportError::at_line(
            file,
            opened_at,
            format!("inline block <{tag}> is never closed"),
        ));
    }

    let mut endpoints = Vec::new();
    for (number, args) in &remotes {
        match remote_endpoint(args, port, proto) {
            Ok(endpoint) => endpoints.push(endpoint),
            Err(message) => errors.push(ImportError::at_line(file, *number, message)),
        }
    }
    if remotes.is_empty() {
        errors.push(ImportError::in_file(file, "has no remote"));
    }

    if !errors.is_empty() {
        return Err(errors);
    }
    let mut text = output.join("\n");
    text.push('\n');
    Ok(SanitizedConfig {
        text,
        endpoints,
        credentials,
        asks_credentials,
    })
}

#[derive(PartialEq)]
enum State {
    Between,
    Unquoted,
    DoubleQuoted,
    SingleQuoted,
}

/// Split one line into tokens following OpenVPN's own rules.
pub(crate) fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut state = State::Between;
    let mut backslash = false;

    for c in line.chars() {
        if !backslash && c == '\\' && state != State::SingleQuoted {
            backslash = true;
            continue;
        }
        let mut done = false;
        match state {
            State::Between => {
                if !c.is_whitespace() {
                    if c == '#' || c == ';' {
                        break;
                    }
                    if !backslash && c == '"' {
                        state = State::DoubleQuoted;
                    } else if !backslash && c == '\'' {
                        state = State::SingleQuoted;
                    } else {
                        current.push(c);
                        state = State::Unquoted;
                    }
                }
            }
            State::Unquoted => {
                if !backslash && c.is_whitespace() {
                    done = true;
                } else {
                    current.push(c);
                }
            }
            State::DoubleQuoted => {
                if !backslash && c == '"' {
                    done = true;
                } else {
                    current.push(c);
                }
            }
            State::SingleQuoted => {
                if c == '\'' {
                    done = true;
                } else {
                    current.push(c);
                }
            }
        }
        if done {
            tokens.push(std::mem::take(&mut current));
            state = State::Between;
        }
        backslash = false;
    }

    match state {
        State::DoubleQuoted | State::SingleQuoted => Err("quotation is never closed".into()),
        State::Unquoted => {
            tokens.push(current);
            Ok(tokens)
        }
        State::Between => Ok(tokens),
    }
}

fn parse_transport(value: &str) -> Result<Transport, String> {
    match value {
        "udp" | "udp4" | "udp6" => Ok(Transport::Udp),
        "tcp" | "tcp4" | "tcp6" | "tcp-client" | "tcp4-client" | "tcp6-client" => {
            Ok(Transport::Tcp)
        }
        other => Err(format!("unsupported protocol \"{other}\"")),
    }
}

fn remote_endpoint(
    args: &[String],
    default_port: u16,
    default_proto: Transport,
) -> Result<Endpoint, String> {
    let host = &args[0];
    if !is_valid_host(host) {
        return Err(format!("invalid remote host \"{host}\""));
    }
    let port = match args.get(1) {
        Some(value) => parse_port(value).ok_or_else(|| format!("invalid port \"{value}\""))?,
        None => default_port,
    };
    let transport = match args.get(2) {
        Some(value) => parse_transport(value)?,
        None => default_proto,
    };
    Ok(Endpoint {
        host: host.clone(),
        port,
        transport,
    })
}

/// Read `auth-user-pass <path>`, which must name a file inside the bundle, relative to the
/// config's own directory.
fn read_credentials(
    file: &str,
    path: &str,
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<Credentials, String> {
    let segments: Vec<&str> = path.split('/').collect();
    if path.contains(['\\', ':', '\0'])
        || segments
            .iter()
            .any(|s| s.is_empty() || *s == "." || *s == "..")
    {
        return Err(format!(
            "auth-user-pass file \"{path}\" must be a plain relative path inside the bundle"
        ));
    }
    let dir = file.rsplit_once('/').map_or("", |(dir, _)| dir);
    let full = if dir.is_empty() {
        path.to_string()
    } else {
        format!("{dir}/{path}")
    };
    let bytes = read(&full)
        .ok_or_else(|| format!("auth-user-pass file \"{full}\" is not in the archive"))?;
    let text = Zeroizing::new(
        String::from_utf8(bytes)
            .map_err(|_| format!("auth-user-pass file \"{full}\" is not valid UTF-8"))?,
    );
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r'));
    match (lines.next(), lines.next()) {
        (Some(username), Some(password)) if !username.is_empty() && !password.is_empty() => {
            Ok(Credentials {
                username: username.to_string(),
                password: password.to_string(),
            })
        }
        _ => Err(format!(
            "auth-user-pass file \"{full}\" must hold a username and a password on two lines"
        )),
    }
}
