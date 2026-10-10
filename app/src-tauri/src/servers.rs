//! Adding servers to a bundle from the window (SPEC §3.5): the page names `.conf` and `.ovpn`
//! files (picked, or dropped on the window) and a display name for each; this side reads them
//! as the signed-in user and hands the bytes to the service. As with a bundle, the service
//! does the checking that matters; the checks here spare it an upload that cannot succeed.

use std::path::Path;

use serde::{Deserialize, Serialize};
use th_core::edit::{MAX_CONFIG_BYTES, protocol_of};
use th_core::ipc::ProfileFile;

use crate::lang::text;

/// A config file the user chose, with the name its server will show under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChosenFile {
    pub path: String,
    pub name: String,
}

fn file_name(path: &Path) -> Result<&str, String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| protocol_of(name).is_some())
        .ok_or_else(|| text("file.not_config", &[("name", &path.display().to_string())]))
}

/// The file as the page offers it: its path, and its name without the extension as the
/// server's name until the user types another.
pub fn describe(path: &Path) -> Result<ChosenFile, String> {
    let file_name = file_name(path)?;
    let name = file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _)| stem);
    Ok(ChosenFile {
        path: path.display().to_string(),
        name: name.trim().to_string(),
    })
}

/// The file as an `AddProfiles` entry, or why it cannot be one.
pub fn read(chosen: &ChosenFile) -> Result<ProfileFile, String> {
    let path = Path::new(&chosen.path);
    let file_name = file_name(path)?;
    let shown = path.display().to_string();
    let named = [("name", shown.as_str())];
    let metadata = std::fs::metadata(path)
        .map_err(|e| text("file.cannot_open", &[named[0], ("reason", &e.to_string())]))?;
    if !metadata.is_file() {
        return Err(text("file.not_file", &named));
    }
    // Checked on the length alone, so an oversized file is never read into memory.
    if metadata.len() > MAX_CONFIG_BYTES as u64 {
        return Err(text("file.config_too_large", &named));
    }
    if metadata.len() == 0 {
        return Err(text("file.empty", &named));
    }
    let bytes = std::fs::read(path)
        .map_err(|e| text("file.cannot_read", &[named[0], ("reason", &e.to_string())]))?;
    Ok(ProfileFile::new(chosen.name.trim(), file_name, &bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chosen(path: &Path, name: &str) -> ChosenFile {
        ChosenFile {
            path: path.display().to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn a_config_is_offered_under_its_file_name() {
        let described = describe(Path::new(r"C:\Users\me\Downloads\NL Free 12.OVPN")).unwrap();
        assert_eq!(described.name, "NL Free 12");
        assert_eq!(described.path, r"C:\Users\me\Downloads\NL Free 12.OVPN");
        for other in [r"C:\x\bundle.zip", r"C:\x\conf", r"C:\x\"] {
            let error = describe(Path::new(other)).unwrap_err();
            assert!(error.contains(".conf"), "{error}");
        }
    }

    #[test]
    fn a_config_becomes_the_entry_the_service_decodes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nl-1.conf");
        std::fs::write(&path, b"[Interface]\n").unwrap();
        let entry = read(&chosen(&path, "  Netherlands 1 ")).unwrap();
        assert_eq!(entry.name, "Netherlands 1");
        assert_eq!(
            entry.file_name, "nl-1.conf",
            "the name alone, never the path"
        );
        assert_eq!(entry.content().unwrap(), b"[Interface]\n");
    }

    #[test]
    fn what_cannot_be_a_server_is_refused_before_it_is_read() {
        let dir = tempfile::tempdir().unwrap();

        let text = dir.path().join("notes.txt");
        std::fs::write(&text, b"hello").unwrap();
        assert!(read(&chosen(&text, "")).unwrap_err().contains(".ovpn"));

        let missing = read(&chosen(&dir.path().join("gone.conf"), "")).unwrap_err();
        assert!(missing.contains("gone.conf"), "{missing}");

        let folder = dir.path().join("folder.conf");
        std::fs::create_dir(&folder).unwrap();
        assert!(read(&chosen(&folder, "")).is_err());

        // One byte over the limit, without writing it: the length is all that is checked.
        let big = dir.path().join("big.ovpn");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_CONFIG_BYTES as u64 + 1)
            .unwrap();
        assert!(read(&chosen(&big, "")).unwrap_err().contains("1 MB"));

        let empty = dir.path().join("empty.conf");
        std::fs::write(&empty, b"").unwrap();
        assert!(read(&chosen(&empty, "")).unwrap_err().contains("empty"));
    }
}
