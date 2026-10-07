//! The service's data under `%ProgramData%\TunnelHop` — SPEC §2.3.
//!
//! The root grants SYSTEM and Administrators full control and nothing else; `secrets\` grants
//! SYSTEM alone. Both DACLs are protected, so nothing is inherited from `ProgramData`, where
//! standard users may create folders.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use th_core::bundle::BundleId;
use th_core::manifest::is_valid_id;

/// The root and everything under it except `secrets\`.
pub const ROOT_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
/// `secrets\`: DPAPI blobs and management passwords, readable by the service alone.
/// Administrators may list folders and delete what is in them, so an uninstall (which runs as an
/// administrator, not as SYSTEM) can remove the store, but they may not read a file's contents:
/// - folders: `0x110081` = list, read attributes, delete, synchronize;
/// - files (`OIIO`): `0x110080` = read attributes, delete, synchronize. No read-data.
pub const SECRETS_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;CI;0x110081;;;BA)(A;OIIO;0x110080;;;BA)";

/// Where everything lives. Paths only; [`prepare`] creates and secures them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Layout { root: root.into() }
    }

    /// `%ProgramData%\TunnelHop`.
    #[cfg(windows)]
    pub fn system() -> io::Result<Self> {
        Ok(Layout::new(crate::win::program_data()?.join("TunnelHop")))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn bundles(&self) -> PathBuf {
        self.root.join("bundles")
    }

    pub fn tunnels(&self) -> PathBuf {
        self.root.join("tunnels")
    }

    pub fn secrets(&self) -> PathBuf {
        self.root.join("secrets")
    }

    pub fn credentials(&self) -> PathBuf {
        self.secrets().join("credentials")
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn settings_file(&self) -> PathBuf {
        self.root.join("settings.json")
    }
}

/// Create the store, or make an existing one safe to use.
///
/// A root that is a reparse point, or that someone other than SYSTEM, Administrators or this
/// account owns, may have been planted by a standard user before the service first ran: it is
/// deleted and created again. Every start then re-applies the DACLs.
#[cfg(windows)]
pub fn prepare(layout: &Layout) -> io::Result<()> {
    use crate::win;

    let planted = |root: &Path| -> io::Result<bool> {
        Ok(win::is_reparse_point(root)? || !win::has_trusted_owner(root)?)
    };

    let root = layout.root();
    if fs::symlink_metadata(root).is_ok() && planted(root)? {
        log::warn!(
            "{} has an untrusted owner or is a link; recreating it",
            root.display()
        );
        if win::is_reparse_point(root)? {
            fs::remove_dir(root).or_else(|_| fs::remove_file(root))?;
        } else {
            fs::remove_dir_all(root)?;
        }
    }
    if !win::create_dir_with_sddl(root, ROOT_SDDL)? {
        // It existed, or was recreated since the removal above. An owner keeps the right to
        // rewrite the DACL, so a root someone else owns is never used, whatever DACL we set.
        if planted(root)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} was created by an untrusted account; refusing to use it",
                    root.display()
                ),
            ));
        }
        win::set_dacl(root, ROOT_SDDL)?;
    }
    for dir in [layout.bundles(), layout.tunnels(), layout.logs()] {
        fs::create_dir_all(dir)?;
    }
    let secrets = layout.secrets();
    if !win::create_dir_with_sddl(&secrets, SECRETS_SDDL)? {
        win::set_dacl(&secrets, SECRETS_SDDL)?;
    }
    fs::create_dir_all(layout.credentials())
}

/// Write `bytes` to a temporary file next to `path` and rename it over `path`, so a crash
/// leaves either the old file or the new one.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let mut file = fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)
}

/// The user's choices, kept across restarts. `None` means "use the manifest's default".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub active_bundle: Option<String>,
    pub selected_profile: Option<String>,
    pub auto_rotate: Option<bool>,
    pub interval_minutes: Option<u32>,
    /// Where the public-IP probe asks, in order; `None` means ipify, then ipinfo. Tests point
    /// it at the lab's own endpoint. Read when the service starts.
    pub probe_urls: Option<Vec<String>>,
}

impl Settings {
    /// Read the settings; a missing file is the defaults. Values that fail validation are
    /// dropped one by one rather than discarding the whole file.
    pub fn load(path: &Path) -> io::Result<Settings> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
            Err(e) => return Err(e),
        };
        let settings: Settings = serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(settings.validated())
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(self).expect("settings always serialize");
        write_atomic(path, &json)
    }

    fn validated(mut self) -> Settings {
        if self
            .active_bundle
            .as_deref()
            .is_some_and(|id| BundleId::parse(id).is_none())
        {
            self.active_bundle = None;
        }
        if self
            .selected_profile
            .as_deref()
            .is_some_and(|id| !is_valid_id(id))
        {
            self.selected_profile = None;
        }
        if self
            .interval_minutes
            .is_some_and(|m| !(1..=1440).contains(&m))
        {
            self.interval_minutes = None;
        }
        if let Some(urls) = &mut self.probe_urls {
            urls.retain(|u| crate::probe::is_valid_url(u));
            urls.truncate(crate::probe::MAX_URLS);
            if urls.is_empty() {
                self.probe_urls = None;
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_the_spec() {
        let layout = Layout::new(r"C:\ProgramData\TunnelHop");
        let root = Path::new(r"C:\ProgramData\TunnelHop");
        assert_eq!(layout.bundles(), root.join("bundles"));
        assert_eq!(layout.tunnels(), root.join("tunnels"));
        assert_eq!(layout.secrets(), root.join("secrets"));
        assert_eq!(
            layout.credentials(),
            root.join("secrets").join("credentials")
        );
        assert_eq!(layout.logs(), root.join("logs"));
        assert_eq!(layout.settings_file(), root.join("settings.json"));
    }

    #[test]
    fn missing_settings_are_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings::load(&dir.path().join("settings.json")).unwrap();
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let settings = Settings {
            active_bundle: Some("0123456789abcdef".into()),
            selected_profile: Some("ua-wg-01".into()),
            auto_rotate: Some(false),
            interval_minutes: Some(60),
            probe_urls: Some(vec!["http://198.18.77.2:8080/".into()]),
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);
        assert!(!dir.path().join("settings.json.tmp").exists());
    }

    #[test]
    fn invalid_values_are_dropped_individually() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            r#"{"active_bundle":"..\\..\\x","selected_profile":"UPPER","auto_rotate":true,
                "interval_minutes":0,"field_from_a_newer_version":1}"#,
        )
        .unwrap();
        let settings = Settings::load(&path).unwrap();
        assert_eq!(
            settings,
            Settings {
                auto_rotate: Some(true),
                ..Settings::default()
            }
        );
    }

    #[test]
    fn probe_urls_keep_only_valid_ones_and_at_most_four() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            r#"{"probe_urls":["file:///C:/x","http://a/","https://b/","http://c/",
                "http://d/","http://e/"]}"#,
        )
        .unwrap();
        let urls = Settings::load(&path).unwrap().probe_urls.unwrap();
        assert_eq!(urls, ["http://a/", "https://b/", "http://c/", "http://d/"]);

        fs::write(&path, r#"{"probe_urls":["ftp://x/"]}"#).unwrap();
        assert_eq!(Settings::load(&path).unwrap().probe_urls, None);
    }

    #[test]
    fn corrupt_settings_are_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"{not json").unwrap();
        let error = Settings::load(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    /// The entries of an SDDL DACL, without its flags (`P`, `AI`), which depend on how it
    /// was applied.
    #[cfg(windows)]
    fn aces(sddl: &str) -> &str {
        &sddl[sddl.find('(').unwrap_or(sddl.len())..]
    }

    /// `secrets\` admits SYSTEM alone, so this runs as SYSTEM in the test VM, as the service
    /// does: `scripts\vm.ps1 test lib -AsSystem`.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs LocalSystem; run it in the th-test VM with vm.ps1 test lib -AsSystem"]
    fn prepare_replaces_a_planted_link_and_secures_the_store() {
        use crate::win;

        let parent = tempfile::tempdir().unwrap();
        let layout = Layout::new(parent.path().join("TunnelHop"));
        let elsewhere = parent.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        let junction = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(layout.root())
            .arg(&elsewhere)
            .status()
            .unwrap();
        assert!(junction.success());

        prepare(&layout).unwrap();
        assert!(!win::is_reparse_point(layout.root()).unwrap());
        assert_eq!(
            fs::read_dir(&elsewhere).unwrap().count(),
            0,
            "followed the link"
        );
        for dir in [
            layout.bundles(),
            layout.tunnels(),
            layout.logs(),
            layout.credentials(),
        ] {
            assert!(dir.is_dir(), "{}", dir.display());
        }

        // A second start keeps the store and re-applies the same DACLs.
        fs::write(layout.settings_file(), b"{}").unwrap();
        prepare(&layout).unwrap();
        assert!(layout.settings_file().is_file());
        let root = win::dacl_sddl(layout.root()).unwrap();
        let secrets = win::dacl_sddl(&layout.secrets()).unwrap();
        assert!(root.starts_with("D:P") && secrets.starts_with("D:P"));
        assert_eq!(aces(&root), "(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");
        assert_eq!(
            aces(&secrets),
            "(A;OICI;FA;;;SY)(A;CI;0x110081;;;BA)(A;OIIO;0x110080;;;BA)"
        );

        // A file in secrets\ inherits the file entry: administrators may delete it, not read it.
        let secret = layout.credentials().join("probe.cred");
        fs::write(&secret, b"sealed").unwrap();
        assert_eq!(
            aces(&win::dacl_sddl(&secret).unwrap()),
            "(A;ID;FA;;;SY)(A;ID;0x110080;;;BA)"
        );
        fs::remove_dir_all(layout.root()).unwrap();
    }
}
