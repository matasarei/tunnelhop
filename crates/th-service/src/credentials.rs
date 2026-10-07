//! OpenVPN credentials, sealed at rest — SPEC §3.4.
//!
//! One file per bundle-wide default and one per profile, under `secrets\credentials\`:
//!
//! ```text
//! <bundle_id>\bundle.cred         every profile of the bundle that has no file of its own
//! <bundle_id>\profile.<id>.cred   one profile (from the bundle's auth.txt, or set by the user)
//! ```
//!
//! A profile's own file wins over the bundle default. Ids are validated again before they
//! become file names, whatever the caller already checked.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use th_core::bundle::{BundleId, ImportedBundle};
use th_core::ipc::{BundleInfo, ProfileInfo, Secret};
use th_core::manifest::{Manifest, is_valid_id};
use th_core::sanitize::Credentials;
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::store::write_atomic;

/// Encrypts credentials at rest. DPAPI in the service; tests use a stand-in.
pub trait Vault {
    fn seal(&self, plain: &[u8]) -> io::Result<Vec<u8>>;
    fn open(&self, sealed: &[u8]) -> io::Result<Zeroizing<Vec<u8>>>;
}

/// DPAPI for the service's own account: only LocalSystem on this machine can open the files.
#[cfg(windows)]
pub struct Dpapi;

#[cfg(windows)]
impl Vault for Dpapi {
    fn seal(&self, plain: &[u8]) -> io::Result<Vec<u8>> {
        crate::win::dpapi_protect(plain)
    }

    fn open(&self, sealed: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
        crate::win::dpapi_unprotect(sealed)
    }
}

/// Which credentials a file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    Bundle,
    Profile(&'a str),
}

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("\"{0}\" is not a valid profile id")]
    InvalidProfile(String),
    #[error("stored credentials could not be read: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Serialize)]
struct StoredRef<'a> {
    username: &'a str,
    password: &'a str,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
struct Stored {
    username: String,
    password: String,
}

pub struct CredentialStore<V> {
    dir: PathBuf,
    vault: V,
}

impl<V: Vault> CredentialStore<V> {
    /// `dir` is `secrets\credentials\`; it must already exist.
    pub fn new(dir: impl Into<PathBuf>, vault: V) -> Self {
        CredentialStore {
            dir: dir.into(),
            vault,
        }
    }

    pub fn path(&self, bundle: &BundleId, scope: Scope<'_>) -> Result<PathBuf, CredentialError> {
        let name = match scope {
            Scope::Bundle => "bundle.cred".to_string(),
            Scope::Profile(id) if is_valid_id(id) => format!("profile.{id}.cred"),
            Scope::Profile(id) => return Err(CredentialError::InvalidProfile(id.to_string())),
        };
        Ok(self.dir.join(bundle.as_str()).join(name))
    }

    pub fn set(
        &self,
        bundle: &BundleId,
        scope: Scope<'_>,
        credentials: &Credentials,
    ) -> Result<(), CredentialError> {
        let path = self.path(bundle, scope)?;
        let stored = StoredRef {
            username: &credentials.username,
            password: &credentials.password,
        };
        // Sized up front so serializing never reallocates and leaves a copy behind.
        let mut plain = Zeroizing::new(Vec::with_capacity(
            credentials.username.len() * 6 + credentials.password.len() * 6 + 64,
        ));
        serde_json::to_writer(&mut *plain, &stored).expect("credentials always serialize");
        let sealed = self.vault.seal(&plain)?;
        fs::create_dir_all(path.parent().expect("credential files have a parent"))?;
        write_atomic(&path, &sealed)?;
        Ok(())
    }

    /// The credentials `profile` connects with: its own, else the bundle default.
    pub fn get(
        &self,
        bundle: &BundleId,
        profile: &str,
    ) -> Result<Option<Credentials>, CredentialError> {
        for scope in [Scope::Profile(profile), Scope::Bundle] {
            let sealed = match fs::read(self.path(bundle, scope)?) {
                Ok(sealed) => sealed,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            let plain = self
                .vault
                .open(&sealed)
                .map_err(|e| CredentialError::Corrupt(e.to_string()))?;
            let mut stored: Stored = serde_json::from_slice(&plain)
                .map_err(|_| CredentialError::Corrupt("not a credential record".into()))?;
            return Ok(Some(Credentials {
                username: std::mem::take(&mut stored.username),
                password: std::mem::take(&mut stored.password),
            }));
        }
        Ok(None)
    }

    /// Whether `profile` has credentials, without decrypting them.
    pub fn has(&self, bundle: &BundleId, profile: &str) -> bool {
        [Scope::Profile(profile), Scope::Bundle]
            .into_iter()
            .any(|scope| self.path(bundle, scope).is_ok_and(|p| p.is_file()))
    }

    /// Store what a `SetCredentials` request carries: for the listed profiles, or as the
    /// bundle default when the list is empty.
    pub fn apply(
        &self,
        bundle: &BundleId,
        profile_ids: &[String],
        username: &str,
        password: &Secret,
    ) -> Result<(), CredentialError> {
        // All ids first, so a bad one cannot leave the request half applied.
        if let Some(bad) = profile_ids.iter().find(|id| !is_valid_id(id)) {
            return Err(CredentialError::InvalidProfile(bad.clone()));
        }
        let credentials = Credentials {
            username: username.to_string(),
            password: password.expose().to_string(),
        };
        if profile_ids.is_empty() {
            return self.set(bundle, Scope::Bundle, &credentials);
        }
        for id in profile_ids {
            self.set(bundle, Scope::Profile(id), &credentials)?;
        }
        Ok(())
    }

    /// Keep the credentials an import took out of the bundle's `auth.txt` files.
    pub fn store_imported(&self, imported: &ImportedBundle) -> Result<(), CredentialError> {
        for (profile, credentials) in &imported.credentials {
            self.set(&imported.id, Scope::Profile(profile), credentials)?;
        }
        Ok(())
    }

    /// Forget every credential of a bundle.
    pub fn remove_bundle(&self, bundle: &BundleId) -> io::Result<()> {
        match fs::remove_dir_all(self.dir.join(bundle.as_str())) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

/// The bundle as the UI sees it. `asks_credentials` lists the profiles whose runtime will ask
/// for a username and password (`ImportedBundle::needs_credentials`); each of them reports
/// whether the store can answer.
pub fn bundle_info(
    bundle: &BundleId,
    manifest: &Manifest,
    asks_credentials: &[String],
    has_credentials: impl Fn(&str) -> bool,
) -> BundleInfo {
    BundleInfo {
        bundle_id: bundle.as_str().to_string(),
        bundle_name: manifest.bundle_name.clone(),
        profiles: manifest
            .profiles
            .iter()
            .map(|p| ProfileInfo {
                id: p.id.clone(),
                name: p.name.clone(),
                protocol: p.protocol,
                has_credentials: !asks_credentials.contains(&p.id) || has_credentials(&p.id),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reversible and obviously not encryption: enough to prove nothing is written in clear.
    struct XorVault;

    impl Vault for XorVault {
        fn seal(&self, plain: &[u8]) -> io::Result<Vec<u8>> {
            Ok(plain.iter().map(|b| b ^ 0x5A).collect())
        }

        fn open(&self, sealed: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
            Ok(Zeroizing::new(sealed.iter().map(|b| b ^ 0x5A).collect()))
        }
    }

    fn bundle() -> BundleId {
        BundleId::parse("0123456789abcdef").unwrap()
    }

    fn credentials(username: &str, password: &str) -> Credentials {
        Credentials {
            username: username.into(),
            password: password.into(),
        }
    }

    fn store() -> (tempfile::TempDir, CredentialStore<XorVault>) {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("credentials"), XorVault);
        (dir, store)
    }

    #[test]
    fn files_are_keyed_by_bundle_then_scope() {
        let (dir, store) = store();
        let root = dir.path().join("credentials").join("0123456789abcdef");
        assert_eq!(
            store.path(&bundle(), Scope::Bundle).unwrap(),
            root.join("bundle.cred")
        );
        assert_eq!(
            store.path(&bundle(), Scope::Profile("ua-ovpn-01")).unwrap(),
            root.join("profile.ua-ovpn-01.cred")
        );
    }

    #[test]
    fn profile_ids_that_could_escape_the_directory_are_refused() {
        let (_dir, store) = store();
        for id in ["..", "../x", r"..\x", "a/b", "", "UPPER", "a.cred"] {
            assert!(
                matches!(
                    store.path(&bundle(), Scope::Profile(id)),
                    Err(CredentialError::InvalidProfile(_))
                ),
                "{id:?}"
            );
        }
    }

    #[test]
    fn a_profile_falls_back_to_the_bundle_default() {
        let (_dir, store) = store();
        assert!(store.get(&bundle(), "p1").unwrap().is_none());
        assert!(!store.has(&bundle(), "p1"));

        store
            .set(&bundle(), Scope::Bundle, &credentials("all", "pw-all"))
            .unwrap();
        store
            .set(
                &bundle(),
                Scope::Profile("p2"),
                &credentials("own", "pw-own"),
            )
            .unwrap();

        assert_eq!(
            store.get(&bundle(), "p1").unwrap(),
            Some(credentials("all", "pw-all"))
        );
        assert_eq!(
            store.get(&bundle(), "p2").unwrap(),
            Some(credentials("own", "pw-own"))
        );
        assert!(store.has(&bundle(), "p1") && store.has(&bundle(), "p2"));
    }

    #[test]
    fn nothing_is_written_in_clear() {
        let (dir, store) = store();
        store
            .set(&bundle(), Scope::Bundle, &credentials("alice", "hunter2"))
            .unwrap();
        let path = store.path(&bundle(), Scope::Bundle).unwrap();
        let bytes = fs::read(path).unwrap();
        assert!(!bytes.windows(7).any(|w| w == b"hunter2"));
        let leftovers: Vec<_> =
            fs::read_dir(dir.path().join("credentials").join(bundle().as_str()))
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
        assert_eq!(leftovers, ["bundle.cred"]);
    }

    #[test]
    fn set_credentials_with_no_profiles_sets_the_bundle_default() {
        let (_dir, store) = store();
        store
            .apply(&bundle(), &[], "u", &Secret::new("p\"w\\"))
            .unwrap();
        assert!(store.path(&bundle(), Scope::Bundle).unwrap().is_file());
        assert_eq!(
            store.get(&bundle(), "anything").unwrap(),
            Some(credentials("u", "p\"w\\"))
        );
    }

    #[test]
    fn set_credentials_for_profiles_writes_only_those() {
        let (_dir, store) = store();
        let ids = vec!["p1".to_string(), "p2".to_string()];
        store
            .apply(&bundle(), &ids, "u", &Secret::new("pw"))
            .unwrap();
        assert!(store.has(&bundle(), "p1") && store.has(&bundle(), "p2"));
        assert!(!store.has(&bundle(), "p3"));
    }

    #[test]
    fn a_bad_profile_id_applies_nothing() {
        let (_dir, store) = store();
        let ids = vec!["p1".to_string(), "../p2".to_string()];
        assert!(
            store
                .apply(&bundle(), &ids, "u", &Secret::new("pw"))
                .is_err()
        );
        assert!(!store.has(&bundle(), "p1"));
    }

    #[test]
    fn a_damaged_file_is_reported_not_returned() {
        let (_dir, store) = store();
        let path = store.path(&bundle(), Scope::Bundle).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"garbage").unwrap();
        assert!(matches!(
            store.get(&bundle(), "p1"),
            Err(CredentialError::Corrupt(_))
        ));
    }

    #[test]
    fn removing_a_bundle_forgets_its_credentials_only() {
        let (_dir, store) = store();
        let other = BundleId::parse("fedcba9876543210").unwrap();
        store
            .set(&bundle(), Scope::Bundle, &credentials("a", "b"))
            .unwrap();
        store
            .set(&other, Scope::Bundle, &credentials("c", "d"))
            .unwrap();
        store.remove_bundle(&bundle()).unwrap();
        store.remove_bundle(&bundle()).unwrap();
        assert!(!store.has(&bundle(), "p1"));
        assert!(store.has(&other, "p1"));
    }

    fn manifest() -> Manifest {
        th_core::manifest::parse(
            br#"{"schema_version":"1","bundle_name":"B","profiles":[
                {"id":"wg","name":"WG","protocol":"wireguard","file":"wireguard/a.conf"},
                {"id":"ov1","name":"OV1","protocol":"openvpn","file":"openvpn/a.ovpn"},
                {"id":"ov2","name":"OV2","protocol":"openvpn","file":"openvpn/b.ovpn"}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn credentials_from_an_import_are_kept_per_profile() {
        let (_dir, store) = store();
        let imported = ImportedBundle {
            id: bundle(),
            dir: PathBuf::from("unused"),
            manifest: manifest(),
            endpoints: Default::default(),
            credentials: [("ov1".to_string(), credentials("from-auth-txt", "pw1"))].into(),
            needs_credentials: vec!["ov2".to_string()],
            already_present: false,
        };
        store.store_imported(&imported).unwrap();

        assert!(
            store
                .path(&bundle(), Scope::Profile("ov1"))
                .unwrap()
                .is_file()
        );
        assert!(!store.path(&bundle(), Scope::Bundle).unwrap().exists());
        assert_eq!(
            store.get(&bundle(), "ov1").unwrap(),
            Some(credentials("from-auth-txt", "pw1"))
        );
        assert!(!store.has(&bundle(), "ov2"));
    }

    #[test]
    fn has_credentials_is_false_only_for_profiles_still_waiting() {
        let manifest = manifest();
        let asks = vec!["ov1".to_string(), "ov2".to_string()];
        let info = bundle_info(&bundle(), &manifest, &asks, |id| id == "ov2");
        let flags: Vec<_> = info
            .profiles
            .iter()
            .map(|p| (p.id.as_str(), p.has_credentials))
            .collect();
        assert_eq!(flags, [("wg", true), ("ov1", false), ("ov2", true)]);
        assert_eq!(info.bundle_id, "0123456789abcdef");
        assert_eq!(info.bundle_name, "B");
    }
}
