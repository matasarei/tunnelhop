//! Find the installed VPN runtimes and check their versions — SPEC §4.2, §4.3.
//!
//! A runtime that is missing, older than the minimum, or whose version cannot be read disables
//! its protocol; the UI shows why.

use std::fmt;
use std::path::PathBuf;

use th_core::ipc::{Availability, RuntimeInfo};
use th_core::manifest::Protocol;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Version {
            major,
            minor,
            patch,
        }
    }

    /// `2.6.12`, `0.5.3` or `2.6`. A suffix after the numbers (`2.7_rc1`, `2.6.0-git`) is
    /// ignored; anything else is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let numeric_end = text
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(text.len());
        let mut parts = text[..numeric_end].split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = match parts.next() {
            None | Some("") => 0,
            Some(p) => p.parse().ok()?,
        };
        Some(Version::new(major, minor, patch))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

pub const MIN_WIREGUARD: Version = Version::new(0, 5, 3);
pub const MIN_OPENVPN: Version = Version::new(2, 6, 0);

pub fn minimum(protocol: Protocol) -> Version {
    match protocol {
        Protocol::Wireguard => MIN_WIREGUARD,
        Protocol::Openvpn => MIN_OPENVPN,
    }
}

/// What looking for a runtime found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    Missing,
    /// The executable exists; `None` when its version could not be read.
    Installed(Option<Version>),
}

pub fn availability(found: &Found, minimum: Version) -> Availability {
    match found {
        Found::Missing => Availability::Missing,
        Found::Installed(Some(version)) if *version >= minimum => Availability::Available {
            version: version.to_string(),
        },
        Found::Installed(version) => Availability::TooOld {
            found: version.map_or_else(|| "unknown".to_string(), |v| v.to_string()),
            required: minimum.to_string(),
        },
    }
}

/// The version in the first line of `openvpn --version`:
/// `OpenVPN 2.6.12 x86_64-w64-mingw32 [SSL (OpenSSL)] … built on …`.
pub fn parse_openvpn_version(output: &str) -> Option<Version> {
    let first = output.lines().next()?;
    let mut words = first.split_whitespace();
    (words.next()? == "OpenVPN").then_some(())?;
    Version::parse(words.next()?)
}

/// One runtime as discovered on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtime {
    pub protocol: Protocol,
    /// Where the executable is, when it was found.
    pub path: Option<PathBuf>,
    pub availability: Availability,
}

impl Runtime {
    pub fn is_available(&self) -> bool {
        matches!(self.availability, Availability::Available { .. })
    }

    pub fn info(&self) -> RuntimeInfo {
        RuntimeInfo {
            protocol: self.protocol,
            availability: self.availability.clone(),
        }
    }
}

#[cfg(windows)]
pub use discover::discover;

#[cfg(windows)]
mod discover {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use th_core::manifest::Protocol;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    use super::{Found, Runtime, Version, availability, minimum, parse_openvpn_version};
    use crate::win;

    const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

    /// Look for both runtimes. Never fails: a problem shows up as that runtime's availability.
    pub fn discover() -> Vec<Runtime> {
        let program_files = win::program_files().ok();
        let wireguard = program_files
            .as_ref()
            .map(|p| p.join("WireGuard").join("wireguard.exe"));
        let openvpn = openvpn_from_registry().or_else(|| {
            program_files
                .as_ref()
                .map(|p| p.join("OpenVPN").join("bin").join("openvpn.exe"))
        });
        vec![
            runtime(Protocol::Wireguard, wireguard, |p| {
                win::file_version(p).map(|(major, minor, patch)| Version::new(major, minor, patch))
            }),
            runtime(Protocol::Openvpn, openvpn, openvpn_version),
        ]
    }

    fn runtime(
        protocol: Protocol,
        path: Option<PathBuf>,
        version: impl Fn(&Path) -> Option<Version>,
    ) -> Runtime {
        let path = path.filter(|p| p.is_file());
        let found = match &path {
            Some(p) => Found::Installed(version(p)),
            None => Found::Missing,
        };
        Runtime {
            protocol,
            availability: availability(&found, minimum(protocol)),
            path,
        }
    }

    /// `HKLM\SOFTWARE\OpenVPN\exe_path`, written by the OpenVPN installer.
    fn openvpn_from_registry() -> Option<PathBuf> {
        win::read_hklm_string(r"SOFTWARE\OpenVPN", "exe_path").map(PathBuf::from)
    }

    /// Run `openvpn --version`, giving up after a few seconds. It exits non-zero by design.
    fn openvpn_version(path: &Path) -> Option<Version> {
        let mut child = Command::new(path)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .ok()?;
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() < VERSION_TIMEOUT => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        let mut output = String::new();
        child.stdout.take()?.read_to_string(&mut output).ok()?;
        parse_openvpn_version(&output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_with_two_or_three_parts_and_ignore_suffixes() {
        assert_eq!(Version::parse("0.5.3"), Some(Version::new(0, 5, 3)));
        assert_eq!(Version::parse("2.6.12"), Some(Version::new(2, 6, 12)));
        assert_eq!(Version::parse("2.6"), Some(Version::new(2, 6, 0)));
        assert_eq!(Version::parse("2.7_rc1"), Some(Version::new(2, 7, 0)));
        assert_eq!(Version::parse("2.6.0-git"), Some(Version::new(2, 6, 0)));
        assert_eq!(Version::parse("2.6.1.0"), Some(Version::new(2, 6, 1)));
    }

    #[test]
    fn malformed_versions_are_refused() {
        for text in [
            "",
            "2",
            "v2.6.0",
            ".6.0",
            "2..0",
            "x.y.z",
            "99999999999.0.0",
        ] {
            assert_eq!(Version::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(Version::new(2, 6, 10) > Version::new(2, 6, 9));
        assert!(Version::new(0, 5, 3) >= MIN_WIREGUARD);
        assert!(Version::new(0, 5, 2) < MIN_WIREGUARD);
        assert!(Version::new(2, 5, 11) < MIN_OPENVPN);
        assert!(Version::new(2, 10, 0) > MIN_OPENVPN);
    }

    #[test]
    fn openvpn_version_comes_from_the_first_line() {
        let output = "OpenVPN 2.6.12 x86_64-w64-mingw32 [SSL (OpenSSL)] [LZO] [LZ4] [PKCS11] \
                      [AEAD] [DCO] built on Jul 18 2024\n\
                      Windows version 10.0 (Windows 10 or greater), amd64 executable\n\
                      library versions: OpenSSL 3.3.1 4 Jun 2024, LZO 2.10\n";
        assert_eq!(parse_openvpn_version(output), Some(Version::new(2, 6, 12)));
    }

    #[test]
    fn unexpected_openvpn_output_has_no_version() {
        assert_eq!(parse_openvpn_version(""), None);
        assert_eq!(parse_openvpn_version("Options error: blah"), None);
        assert_eq!(parse_openvpn_version("OpenVPN\n"), None);
        assert_eq!(parse_openvpn_version("NotOpenVPN 2.6.0"), None);
    }

    #[test]
    fn availability_reports_missing_old_unknown_and_ok() {
        assert_eq!(
            availability(&Found::Missing, MIN_OPENVPN),
            Availability::Missing
        );
        assert_eq!(
            availability(&Found::Installed(Some(Version::new(2, 5, 9))), MIN_OPENVPN),
            Availability::TooOld {
                found: "2.5.9".into(),
                required: "2.6.0".into()
            }
        );
        assert_eq!(
            availability(&Found::Installed(None), MIN_WIREGUARD),
            Availability::TooOld {
                found: "unknown".into(),
                required: "0.5.3".into()
            }
        );
        assert_eq!(
            availability(&Found::Installed(Some(MIN_WIREGUARD)), MIN_WIREGUARD),
            Availability::Available {
                version: "0.5.3".into()
            }
        );
    }
}
