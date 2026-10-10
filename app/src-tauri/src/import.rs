//! Importing a bundle from the window: the page names a file (picked, or dropped on the window),
//! and this side reads it as the signed-in user and hands the bytes to the service, which never
//! opens a path anyone chose (SPEC §3.3). The service does all the checking that matters; the
//! checks here only spare it, and the user, an upload that cannot succeed.

use std::path::Path;

use serde::Serialize;
use th_core::bundle::MAX_ZIP_BYTES;
use th_core::ipc::{self, BundleInfo, Response};

/// What an import ended in, as the page shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImportOutcome {
    Imported {
        bundle: BundleInfo,
        already_present: bool,
        /// Profiles that cannot connect until the user gives a username and password.
        needs_credentials: Vec<String>,
    },
    /// The service refused the bundle: every error it found, each with its file and line.
    Refused { errors: Vec<String> },
    /// The user closed the file picker.
    Cancelled,
}

/// The file as an `ImportBundle` payload, or why it cannot be one.
pub fn read_bundle(path: &Path) -> Result<String, String> {
    let name = path.display();
    let is_zip = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"));
    if !is_zip {
        return Err(format!(
            "{name} is not a bundle: TunnelHop bundles are .zip files"
        ));
    }
    let metadata = std::fs::metadata(path).map_err(|e| format!("cannot open {name}: {e}"))?;
    if !metadata.is_file() {
        return Err(format!("{name} is not a file"));
    }
    // Checked on the length alone, so an oversized file is never read into memory.
    if metadata.len() > MAX_ZIP_BYTES as u64 {
        return Err(format!(
            "{name} is larger than 20 MB, the most a bundle may be"
        ));
    }
    if metadata.len() == 0 {
        return Err(format!("{name} is empty"));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {name}: {e}"))?;
    Ok(ipc::encode_zip(&bytes))
}

/// The service's answer to an `ImportBundle`, as an outcome or an error to show.
pub fn outcome(response: Response) -> Result<ImportOutcome, String> {
    match response {
        Response::Imported {
            bundle,
            already_present,
            needs_credentials,
        } => Ok(ImportOutcome::Imported {
            bundle,
            already_present,
            needs_credentials,
        }),
        Response::ImportFailed { errors } => Ok(ImportOutcome::Refused { errors }),
        Response::Error { message } => Err(message),
        other => Err(format!("the service answered an import with {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use th_core::ipc::{ProfileInfo, decode_zip};
    use th_core::manifest::Protocol;

    #[test]
    fn a_zip_becomes_the_payload_the_service_decodes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Bundle.ZIP");
        std::fs::write(&path, b"PK\x03\x04 not really a zip").unwrap();
        let payload = read_bundle(&path).unwrap();
        assert_eq!(
            decode_zip(&payload).unwrap(),
            b"PK\x03\x04 not really a zip"
        );
    }

    #[test]
    fn what_cannot_be_a_bundle_is_refused_before_it_is_read() {
        let dir = tempfile::tempdir().unwrap();

        let text = dir.path().join("notes.txt");
        std::fs::write(&text, b"hello").unwrap();
        assert!(read_bundle(&text).unwrap_err().contains(".zip"));

        let missing = read_bundle(&dir.path().join("gone.zip")).unwrap_err();
        assert!(missing.contains("gone.zip"), "{missing}");

        let folder = dir.path().join("folder.zip");
        std::fs::create_dir(&folder).unwrap();
        assert!(read_bundle(&folder).is_err());

        // One byte over the limit, without writing 20 MB: the length is all that is checked.
        let big = dir.path().join("big.zip");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_ZIP_BYTES as u64 + 1)
            .unwrap();
        assert!(read_bundle(&big).unwrap_err().contains("20 MB"));

        let empty = dir.path().join("empty.zip");
        std::fs::write(&empty, b"").unwrap();
        assert!(read_bundle(&empty).unwrap_err().contains("empty"));
    }

    #[test]
    fn the_services_answers_become_outcomes() {
        let bundle = BundleInfo {
            bundle_id: "0123456789abcdef".into(),
            bundle_name: "Lab".into(),
            profiles: vec![ProfileInfo {
                id: "ovpn".into(),
                name: "Lab OpenVPN".into(),
                protocol: Protocol::Openvpn,
                has_credentials: false,
            }],
        };
        assert_eq!(
            outcome(Response::Imported {
                bundle: bundle.clone(),
                already_present: true,
                needs_credentials: vec!["ovpn".into()],
            }),
            Ok(ImportOutcome::Imported {
                bundle,
                already_present: true,
                needs_credentials: vec!["ovpn".into()],
            })
        );
        assert_eq!(
            outcome(Response::ImportFailed {
                errors: vec!["wireguard/a.conf:7: PostUp is not allowed".into()],
            }),
            Ok(ImportOutcome::Refused {
                errors: vec!["wireguard/a.conf:7: PostUp is not allowed".into()],
            })
        );
        assert_eq!(
            outcome(Response::Error {
                message: "bundle is larger than 20 MB".into()
            }),
            Err("bundle is larger than 20 MB".into())
        );
        assert!(outcome(Response::Ok).is_err(), "not an answer to an import");
    }
}
