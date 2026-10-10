mod common;

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use common::{OVPN_CONF, WG_CONF, valid_bundle};
use th_core::bundle::{self, BundleId};
use th_core::edit::{self, Edit, MAX_CONFIG_BYTES, NewProfile, profile_id_from, protocol_of};
use th_core::manifest::{MAX_PROFILES, Protocol};

/// The test bundle (`wg-1`, `ovpn-1`) installed in a fresh store.
fn installed() -> (tempfile::TempDir, PathBuf, BundleId) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("bundles");
    let id = bundle::import(&valid_bundle(vec![]), &root).unwrap().id;
    (tmp, root, id)
}

fn new(name: &str, file_name: &str, content: &str) -> NewProfile {
    NewProfile {
        name: name.to_string(),
        file_name: file_name.to_string(),
        content: content.as_bytes().to_vec(),
    }
}

fn errors_of(result: Result<Vec<u8>, Vec<bundle::ImportError>>) -> Vec<String> {
    result
        .unwrap_err()
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn ids_are_made_from_file_names() {
    let free = |_: &str| true;
    assert_eq!(profile_id_from("nl-free-12.conf", free), "nl-free-12");
    assert_eq!(
        profile_id_from("NL Free #12 (TCP).ovpn", free),
        "nl-free-12-tcp"
    );
    assert_eq!(
        profile_id_from("node.protonvpn.udp.ovpn", free),
        "node-protonvpn-udp"
    );
    assert_eq!(profile_id_from("--weird__name--.conf", free), "weird-name");
    assert_eq!(
        profile_id_from("no-extension", free),
        "no-extension",
        "the whole name"
    );
    // Nothing usable in the name, and names Windows maps to a device.
    assert_eq!(profile_id_from("сервер.conf", free), "server");
    assert_eq!(profile_id_from(".conf", free), "server");
    assert_eq!(profile_id_from("NUL.conf", free), "server-nul");
    assert_eq!(profile_id_from("com1.ovpn", free), "server-com1");

    let long = profile_id_from(&format!("{}.conf", "a".repeat(60)), free);
    assert_eq!(long, "a".repeat(40));
    let cut_at_a_dash = profile_id_from(&format!("{}-b.conf", "a".repeat(39)), free);
    assert_eq!(cut_at_a_dash, "a".repeat(39), "no trailing dash");
}

#[test]
fn a_taken_id_gets_the_next_free_suffix() {
    let taken: HashSet<&str> = ["de-1", "de-1-2"].into();
    assert_eq!(
        profile_id_from("de-1.conf", |id| !taken.contains(id)),
        "de-1-3"
    );
    let long = "a".repeat(40);
    let suffixed = profile_id_from(&format!("{long}.conf"), |id| id != long);
    assert_eq!(suffixed, format!("{}-2", "a".repeat(38)));
    assert!(th_core::manifest::is_valid_id(&suffixed));
}

#[test]
fn the_extension_gives_the_protocol() {
    assert_eq!(protocol_of("a.conf"), Some(Protocol::Wireguard));
    assert_eq!(protocol_of("a.b.OVPN"), Some(Protocol::Openvpn));
    for other in ["a.txt", "conf", "a.conf.bak", "", "a."] {
        assert_eq!(protocol_of(other), None, "{other:?}");
    }
}

#[test]
fn an_added_server_imports_as_a_new_bundle_with_the_old_ones_kept() {
    let (_tmp, root, id) = installed();
    let edit = Edit::Add(vec![
        new("", "NL Free 12.conf", WG_CONF),
        new("  Sweden, OpenVPN ", "se-1.OVPN", OVPN_CONF),
    ]);

    let zip = edit::rebuild(&root, &id, &edit).unwrap();
    let edited = bundle::import(&zip, &root).unwrap();

    assert_ne!(edited.id, id);
    assert_eq!(edited.manifest.bundle_name, "Test");
    let profiles: Vec<_> = edited
        .manifest
        .profiles
        .iter()
        .map(|p| (p.id.as_str(), p.name.as_str(), p.protocol, p.file.as_str()))
        .collect();
    assert_eq!(
        profiles,
        [
            ("wg-1", "WG 1", Protocol::Wireguard, "wireguard/one.conf"),
            ("ovpn-1", "OVPN 1", Protocol::Openvpn, "openvpn/one.ovpn"),
            (
                "nl-free-12",
                "NL Free 12",
                Protocol::Wireguard,
                "wireguard/nl-free-12.conf"
            ),
            (
                "se-1",
                "Sweden, OpenVPN",
                Protocol::Openvpn,
                "openvpn/se-1.ovpn"
            ),
        ]
    );
    // The configs that were there are carried over as installed.
    assert_eq!(
        fs::read(edited.dir.join("wireguard/one.conf")).unwrap(),
        fs::read(root.join(id.as_str()).join("wireguard/one.conf")).unwrap()
    );
    assert!(edited.dir.join("openvpn/se-1.ovpn").is_file());
    assert!(
        bundle::open(&root, &id).is_ok(),
        "the old bundle is the caller's to delete"
    );
}

#[test]
fn a_file_with_no_name_of_its_own_is_shown_under_its_id() {
    let (_tmp, root, id) = installed();
    let edit = Edit::Add(vec![new("  ", ".conf", WG_CONF)]);
    let edited = bundle::import(&edit::rebuild(&root, &id, &edit).unwrap(), &root).unwrap();
    let added = edited.manifest.profiles.last().unwrap();
    assert_eq!(
        (added.id.as_str(), added.name.as_str()),
        ("server", "server")
    );
}

#[test]
fn the_same_edit_gives_the_same_bundle() {
    let (_tmp, root, id) = installed();
    let edit = Edit::Add(vec![new("Extra", "extra.conf", WG_CONF)]);
    let first = edit::rebuild(&root, &id, &edit).unwrap();
    let second = edit::rebuild(&root, &id, &edit).unwrap();
    assert_eq!(first, second);

    let renamed = Edit::Add(vec![new("Other name", "extra.conf", WG_CONF)]);
    assert_ne!(edit::rebuild(&root, &id, &renamed).unwrap(), first);
}

#[test]
fn added_files_never_take_an_id_or_a_file_already_in_the_bundle() {
    let (_tmp, root, id) = installed();
    // "wg-1" is an id in the bundle; "one" is the file of another profile.
    let edit = Edit::Add(vec![
        new("", "WG-1.conf", WG_CONF),
        new("", "One.conf", WG_CONF),
        new("", "one.conf", WG_CONF),
    ]);
    let edited = bundle::import(&edit::rebuild(&root, &id, &edit).unwrap(), &root).unwrap();
    let added: Vec<_> = edited.manifest.profiles[2..]
        .iter()
        .map(|p| (p.id.as_str(), p.file.as_str()))
        .collect();
    assert_eq!(
        added,
        [
            ("wg-1-2", "wireguard/wg-1-2.conf"),
            ("one-2", "wireguard/one-2.conf"),
            ("one-3", "wireguard/one-3.conf"),
        ]
    );
}

#[test]
fn a_hostile_added_file_is_refused_by_the_import_with_its_line() {
    let (_tmp, root, id) = installed();
    let hostile = WG_CONF.replace("[Interface]\n", "[Interface]\nPostUp = calc.exe\n");
    let edit = Edit::Add(vec![new("Evil", "evil.conf", &hostile)]);

    let zip = edit::rebuild(&root, &id, &edit).unwrap();
    let errors: Vec<String> = bundle::import(&zip, &root)
        .unwrap_err()
        .iter()
        .map(ToString::to_string)
        .collect();

    assert!(
        errors
            .iter()
            .any(|e| e.starts_with("wireguard/evil.conf:2") && e.contains("PostUp")),
        "{errors:?}"
    );
    assert_eq!(bundle::installed(&root).unwrap(), [id], "nothing was added");
}

#[test]
fn what_cannot_be_added_is_refused_before_a_zip_is_built() {
    let (_tmp, root, id) = installed();
    let errors = errors_of(edit::rebuild(
        &root,
        &id,
        &Edit::Add(vec![
            new("Notes", "notes.txt", "hello"),
            new("Huge", "huge.conf", &"x".repeat(MAX_CONFIG_BYTES + 1)),
        ]),
    ));
    assert!(
        errors[0].starts_with("notes.txt") && errors[0].contains(".conf"),
        "{errors:?}"
    );
    assert!(
        errors[1].starts_with("huge.conf") && errors[1].contains("1 MB"),
        "{errors:?}"
    );

    let nothing = errors_of(edit::rebuild(&root, &id, &Edit::Add(Vec::new())));
    assert!(nothing[0].contains("no config file"), "{nothing:?}");

    let too_many: Vec<_> = (0..MAX_PROFILES)
        .map(|n| new("", &format!("s{n}.conf"), WG_CONF))
        .collect();
    let errors = errors_of(edit::rebuild(&root, &id, &Edit::Add(too_many)));
    assert!(errors[0].contains("at most 500"), "{errors:?}");
}

#[test]
fn a_removed_server_is_gone_with_its_config() {
    let (_tmp, root, id) = installed();
    let zip = edit::rebuild(&root, &id, &Edit::Remove(vec!["ovpn-1".into()])).unwrap();
    let edited = bundle::import(&zip, &root).unwrap();

    let ids: Vec<_> = edited
        .manifest
        .profiles
        .iter()
        .map(|p| p.id.as_str())
        .collect();
    assert_eq!(ids, ["wg-1"]);
    assert!(edited.dir.join("wireguard/one.conf").is_file());
    assert!(!edited.dir.join("openvpn").exists());
}

#[test]
fn removing_what_is_not_there_or_everything_is_refused() {
    let (_tmp, root, id) = installed();
    let unknown = errors_of(edit::rebuild(
        &root,
        &id,
        &Edit::Remove(vec!["wg-1".into(), "nope".into()]),
    ));
    assert_eq!(unknown.len(), 1);
    assert!(unknown[0].contains("no server \"nope\""), "{unknown:?}");

    let everything = errors_of(edit::rebuild(
        &root,
        &id,
        &Edit::Remove(vec!["wg-1".into(), "ovpn-1".into()]),
    ));
    assert!(
        everything[0].contains("at least one server"),
        "{everything:?}"
    );

    let nothing = errors_of(edit::rebuild(&root, &id, &Edit::Remove(Vec::new())));
    assert!(nothing[0].contains("no server was named"), "{nothing:?}");
}

#[test]
fn an_explicit_protocol_list_gains_the_protocol_of_an_added_server() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("bundles");
    let manifest = r#"{
      "schema_version": "1",
      "bundle_name": "WireGuard only",
      "protocols": ["wireguard"],
      "profiles": [
        { "id": "wg-1", "name": "WG 1", "protocol": "wireguard", "file": "wireguard/one.conf" }
      ]
    }"#;
    let zip = common::zip(&[
        common::Entry::File("manifest.json", manifest.as_bytes().to_vec()),
        common::Entry::File("wireguard/one.conf", WG_CONF.as_bytes().to_vec()),
    ]);
    let id = bundle::import(&zip, &root).unwrap().id;

    let edit = Edit::Add(vec![new("OpenVPN", "o.ovpn", OVPN_CONF)]);
    let edited = bundle::import(&edit::rebuild(&root, &id, &edit).unwrap(), &root).unwrap();
    assert_eq!(
        edited.manifest.protocols,
        Some(vec![Protocol::Wireguard, Protocol::Openvpn])
    );
}

#[test]
fn editing_a_bundle_that_is_not_installed_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = BundleId::parse("0123456789abcdef").unwrap();
    let edit = Edit::Remove(vec!["wg-1".into()]);
    assert!(edit::rebuild(tmp.path(), &missing, &edit).is_err());
}
