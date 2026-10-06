mod common;

use std::fs;
use std::path::Path;

use common::{Entry, MANIFEST, OVPN_CONF, valid_bundle, zip};
use th_core::bundle::{self, BundleId, MAX_ENTRIES};

fn root_is_empty(root: &Path) -> bool {
    !root.exists() || fs::read_dir(root).unwrap().next().is_none()
}

fn import_err(bytes: &[u8]) -> (Vec<String>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("bundles");
    let errors = bundle::import(bytes, &root).unwrap_err();
    assert!(
        root_is_empty(&root),
        "a refused import must leave nothing behind"
    );
    (errors.iter().map(|e| e.to_string()).collect(), tmp)
}

fn assert_refused(bytes: &[u8], needle: &str) {
    let (errors, _tmp) = import_err(bytes);
    assert!(
        errors.iter().any(|e| e.contains(needle)),
        "expected an error containing {needle:?}, got {errors:?}"
    );
}

#[test]
fn valid_bundle_installs_manifest_and_profile_files_only() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("bundles");
    let bytes = valid_bundle(vec![
        Entry::Dir("openvpn/"),
        Entry::File("openvpn/unused.ovpn", b"up /bin/sh\n".to_vec()),
    ]);

    let imported = bundle::import(&bytes, &root).unwrap();

    assert_eq!(imported.id, BundleId::of(&bytes));
    assert_eq!(imported.id.as_str().len(), 16);
    assert!(!imported.already_present);
    assert_eq!(imported.manifest.profiles.len(), 2);
    assert_eq!(imported.dir, root.join(imported.id.as_str()));
    assert_eq!(
        fs::read_to_string(imported.dir.join("manifest.json")).unwrap(),
        MANIFEST
    );
    assert_eq!(
        fs::read_to_string(imported.dir.join("openvpn/one.ovpn")).unwrap(),
        OVPN_CONF
    );
    assert!(imported.dir.join("wireguard/one.conf").is_file());
    assert!(
        !imported.dir.join("openvpn/unused.ovpn").exists(),
        "files no profile references must not be installed"
    );
    let leftovers: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        leftovers.len(),
        1,
        "no staging directory may remain: {leftovers:?}"
    );
}

#[test]
fn reimporting_the_same_zip_is_a_no_op() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("bundles");
    let bytes = valid_bundle(vec![]);

    let first = bundle::import(&bytes, &root).unwrap();
    let second = bundle::import(&bytes, &root).unwrap();

    assert_eq!(first.id, second.id);
    assert!(second.already_present);
    assert_eq!(first.manifest, second.manifest);
}

#[test]
fn different_zips_get_different_ids() {
    let a = valid_bundle(vec![]);
    let b = valid_bundle(vec![Entry::File("openvpn/extra.txt", b"x".to_vec())]);
    assert_ne!(BundleId::of(&a), BundleId::of(&b));
}

#[test]
fn garbage_is_not_a_zip() {
    assert_refused(b"definitely not a zip", "not a valid zip archive");
}

#[test]
fn oversized_archive_is_refused_before_reading() {
    assert_refused(&vec![0u8; bundle::MAX_ZIP_BYTES + 1], "larger than 20 MB");
}

#[test]
fn traversal_and_absolute_names_are_refused() {
    for name in [
        "../evil.ovpn",
        "openvpn/../../evil.ovpn",
        "/etc/passwd",
        "openvpn//a.ovpn",
        "openvpn/./a.ovpn",
        "openvpn\\..\\evil.ovpn",
        "C:/Windows/evil.ovpn",
    ] {
        let bytes = valid_bundle(vec![Entry::File(name, b"x".to_vec())]);
        let (errors, _tmp) = import_err(&bytes);
        assert!(
            errors.iter().any(|e| e.starts_with(name)),
            "{name} should be refused by name, got {errors:?}"
        );
    }
}

#[test]
fn names_windows_would_alter_or_map_to_a_device_are_refused() {
    for name in [
        "openvpn/one.ovpn.",
        "openvpn/one.ovpn ",
        "openvpn/nul.ovpn",
        "openvpn/CON",
        "openvpn/com1.txt",
        "openvpn/LPT9 .ovpn",
        "openvpn/aux./x.ovpn",
        "openvpn/a?.ovpn",
        "openvpn/a\u{1}.ovpn",
    ] {
        let bytes = valid_bundle(vec![Entry::File(name, b"x".to_vec())]);
        let (errors, _tmp) = import_err(&bytes);
        assert!(
            errors.contains(&format!("{name}: name is not a valid Windows file name")),
            "{name:?} should be refused, got {errors:?}"
        );
    }
    // Look-alikes that are fine on Windows.
    let bytes = valid_bundle(vec![
        Entry::File("openvpn/console.ovpn", b"x".to_vec()),
        Entry::File("openvpn/com10.txt", b"x".to_vec()),
        Entry::File("openvpn/.hidden", b"x".to_vec()),
    ]);
    bundle::import(&bytes, tempfile::tempdir().unwrap().path()).unwrap();
}

#[test]
fn entries_outside_the_allowed_tree_are_refused() {
    let bytes = valid_bundle(vec![Entry::File("scripts/run.bat", b"x".to_vec())]);
    assert_refused(
        &bytes,
        "only manifest.json, wireguard/ and openvpn/ are allowed",
    );
}

#[test]
fn symlinks_are_refused() {
    let bytes = valid_bundle(vec![Entry::Symlink("openvpn/link.ovpn", "/etc/passwd")]);
    assert_refused(&bytes, "symbolic links are not allowed");
}

#[test]
fn names_colliding_case_insensitively_are_refused() {
    let bytes = valid_bundle(vec![Entry::File("openvpn/ONE.ovpn", b"x".to_vec())]);
    assert_refused(&bytes, "same name when case is ignored");
}

#[test]
fn too_many_entries_are_refused() {
    let extra: Vec<Entry> = (0..MAX_ENTRIES)
        .map(|i| {
            Entry::Stored(
                Box::leak(format!("openvpn/f{i}.txt").into_boxed_str()),
                vec![],
            )
        })
        .collect();
    assert_refused(&valid_bundle(extra), "more than 500 entries");
}

#[test]
fn zip_bomb_entry_is_refused_by_ratio() {
    let bytes = valid_bundle(vec![Entry::File(
        "openvpn/bomb.txt",
        vec![0u8; 4 * 1024 * 1024],
    )]);
    assert_refused(&bytes, "compression ratio is above 100");
}

#[test]
fn total_uncompressed_size_is_capped() {
    // Moderately compressible data: passes the per-entry ratio, but six entries exceed 50 MB.
    let mut state: u32 = 12345;
    let chunk: Vec<u8> = (0..10 * 1024 * 1024)
        .map(|i| {
            if i % 16 == 0 {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (state >> 16) as u8
            } else {
                0
            }
        })
        .collect();
    let names = [
        "openvpn/a",
        "openvpn/b",
        "openvpn/c",
        "openvpn/d",
        "openvpn/e",
        "openvpn/f",
    ];
    let extra = names
        .iter()
        .map(|n| Entry::File(n, chunk.clone()))
        .collect();
    assert_refused(&valid_bundle(extra), "expands to more than 50 MB");
}

#[test]
fn missing_manifest_is_refused() {
    let bytes = zip(&[Entry::File(
        "openvpn/one.ovpn",
        OVPN_CONF.as_bytes().to_vec(),
    )]);
    assert_refused(&bytes, "manifest.json is missing");
}

#[test]
fn invalid_manifest_errors_are_reported_against_the_file() {
    let bytes = zip(&[Entry::File(
        "manifest.json",
        br#"{"schema_version":"2"}"#.to_vec(),
    )]);
    assert_refused(&bytes, "manifest.json: ");
}

#[test]
fn profile_file_missing_from_the_archive_is_refused() {
    let bytes = zip(&[
        Entry::File("manifest.json", MANIFEST.as_bytes().to_vec()),
        Entry::File("openvpn/one.ovpn", OVPN_CONF.as_bytes().to_vec()),
    ]);
    assert_refused(&bytes, "\"wireguard/one.conf\" is not in the archive");
}
