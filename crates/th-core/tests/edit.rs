mod common;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::PathBuf;

use common::{Entry, OVPN_CONF, WG_CONF, valid_bundle, zip};
use serde_json::{Value, json};
use th_core::bundle::{self, BundleId, Unlock};
use th_core::edit::{self, Edit, MAX_CONFIG_BYTES, NewProfile, profile_id_from, protocol_of};
use th_core::manifest::{MAX_PROFILES, Protocol, SignIns};
use th_core::sanitize::Credentials;
use zip::unstable::write::FileOptionsExt;
use zip::write::SimpleFileOptions;
use zip::{AesMode, ZipArchive, ZipWriter};

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

/// A fresh, empty store.
fn store() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("bundles");
    (tmp, root)
}

#[test]
fn loose_config_files_become_a_bundle_that_imports() {
    let (_tmp, root) = store();
    let files = [
        new("Kyiv", "wg-UA-83.conf", WG_CONF),
        new("", "nl free.OVPN", OVPN_CONF),
    ];

    let zip = edit::create("  Proton UA ", &files).unwrap();
    let imported = bundle::import(&zip, &root).unwrap();

    assert_eq!(imported.manifest.bundle_name, "Proton UA");
    let profiles: Vec<_> = imported
        .manifest
        .profiles
        .iter()
        .map(|p| (p.id.as_str(), p.name.as_str(), p.protocol, p.file.as_str()))
        .collect();
    assert_eq!(
        profiles,
        [
            (
                "wg-ua-83",
                "Kyiv",
                Protocol::Wireguard,
                "wireguard/wg-ua-83.conf"
            ),
            (
                "nl-free",
                "nl free",
                Protocol::Openvpn,
                "openvpn/nl-free.ovpn"
            ),
        ]
    );
    assert_eq!(
        zip,
        edit::create("Proton UA", &files).unwrap(),
        "the same files and name give the same bundle"
    );
}

#[test]
fn a_bundle_from_one_file_or_with_no_name_still_imports() {
    let (_tmp, root) = store();
    let zip = edit::create(" ", &[new("", "only.conf", WG_CONF)]).unwrap();
    let imported = bundle::import(&zip, &root).unwrap();
    assert_eq!(imported.manifest.bundle_name, edit::DEFAULT_BUNDLE_NAME);
    assert_eq!(imported.manifest.profiles.len(), 1);
}

#[test]
fn files_with_the_same_name_each_become_a_server() {
    let (_tmp, root) = store();
    let files = [new("", "ua.conf", WG_CONF), new("", "UA.conf", WG_CONF)];
    let imported = bundle::import(&edit::create("Twins", &files).unwrap(), &root).unwrap();
    let ids: Vec<_> = imported.manifest.profiles.iter().map(|p| &p.id).collect();
    assert_eq!(ids, ["ua", "ua-2"]);
}

#[test]
fn what_cannot_become_a_bundle_is_refused_before_a_zip_is_built() {
    let errors = errors_of(edit::create(
        "Mixed",
        &[
            new("", "notes.txt", "hello"),
            new("", "huge.ovpn", &"x".repeat(MAX_CONFIG_BYTES + 1)),
        ],
    ));
    assert!(errors[0].starts_with("notes.txt"), "{errors:?}");
    assert!(errors[1].contains("1 MB"), "{errors:?}");

    let nothing = errors_of(edit::create("Empty", &[]));
    assert!(nothing[0].contains("no config file"), "{nothing:?}");

    let too_many: Vec<_> = (0..=MAX_PROFILES)
        .map(|n| new("", &format!("s{n}.conf"), WG_CONF))
        .collect();
    let errors = errors_of(edit::create("Crowd", &too_many));
    assert!(errors[0].contains("at most 500"), "{errors:?}");
}

#[test]
fn a_hostile_loose_file_is_refused_by_the_import_and_leaves_nothing() {
    let (_tmp, root) = store();
    let hostile = OVPN_CONF.replace("client\n", "client\nup \"calc.exe\"\n");
    let zip = edit::create("Bad", &[new("", "evil.ovpn", &hostile)]).unwrap();
    let errors: Vec<String> = bundle::import(&zip, &root)
        .unwrap_err()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(
        errors.iter().any(|e| e.starts_with("openvpn/evil.ovpn:2")),
        "{errors:?}"
    );
    assert!(bundle::installed(&root).unwrap().is_empty());

    // A name longer than a manifest allows is the import's to refuse as well.
    let zip = edit::create(&"n".repeat(101), &[new("", "a.conf", WG_CONF)]).unwrap();
    assert!(bundle::import(&zip, &root).is_err());
    assert!(bundle::installed(&root).unwrap().is_empty());
}

#[test]
fn a_zip_of_bare_configs_becomes_a_bundle() {
    let (_tmp, root) = store();
    // As Explorer zips a selection, as a tool that writes backslashes does, and in a folder.
    let plain = zip(&[
        Entry::File("wg-UA-83.conf", WG_CONF.as_bytes().to_vec()),
        Entry::Dir("more/"),
        Entry::File("more/nl.ovpn", OVPN_CONF.as_bytes().to_vec()),
        Entry::File(r"other\wg-UA-21.conf", WG_CONF.as_bytes().to_vec()),
    ]);

    let built = edit::from_config_zip(&plain)
        .expect("a zip of configs")
        .unwrap();
    let imported = bundle::import(&built, &root).unwrap();

    assert_eq!(imported.manifest.bundle_name, edit::DEFAULT_BUNDLE_NAME);
    let mut ids: Vec<_> = imported
        .manifest
        .profiles
        .iter()
        .map(|p| p.id.as_str())
        .collect();
    ids.sort();
    assert_eq!(ids, ["nl", "wg-ua-21", "wg-ua-83"]);
    assert_eq!(
        built,
        edit::from_config_zip(&plain).unwrap().unwrap(),
        "the same zip again is the same bundle"
    );
}

#[test]
fn only_a_zip_of_bare_configs_is_converted() {
    // A real bundle, something that is not a zip, and a zip with no config at all are the
    // import's to judge.
    assert!(edit::from_config_zip(&valid_bundle(vec![])).is_none());
    assert!(edit::from_config_zip(b"not a zip").is_none());
    let notes = zip(&[Entry::File("notes.txt", b"hello".to_vec())]);
    assert!(edit::from_config_zip(&notes).is_none());
    // So is a bundle zipped one folder too deep: a manifest anywhere means a bundle was meant.
    let nested = zip(&[
        Entry::File("lab/manifest.json", b"{}".to_vec()),
        Entry::File("lab/wireguard/a.conf", WG_CONF.as_bytes().to_vec()),
    ]);
    assert!(edit::from_config_zip(&nested).is_none());

    // Configs with anything else beside them are refused, naming the other file.
    let mixed = zip(&[
        Entry::File("a.conf", WG_CONF.as_bytes().to_vec()),
        Entry::File("readme.txt", b"hello".to_vec()),
    ]);
    let errors = errors_of(edit::from_config_zip(&mixed).unwrap());
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].starts_with("readme.txt") && errors[0].contains("only .conf and .ovpn"),
        "{errors:?}"
    );

    // The archive rules of an import hold here too.
    let link = zip(&[
        Entry::File("a.conf", WG_CONF.as_bytes().to_vec()),
        Entry::Symlink("b.conf", "a.conf"),
    ]);
    let errors = errors_of(edit::from_config_zip(&link).unwrap());
    assert!(errors[0].contains("symbolic links"), "{errors:?}");
}

// Export

fn sign_in(username: &str, password: &str) -> Credentials {
    Credentials {
        username: username.to_string(),
        password: password.to_string(),
    }
}

/// The entries of a zip, by name: read with `password` where they are encrypted.
fn entries_of(zip_bytes: &[u8], password: Option<&str>) -> BTreeMap<String, Vec<u8>> {
    let mut archive = ZipArchive::new(Cursor::new(zip_bytes)).unwrap();
    let mut entries = BTreeMap::new();
    for i in 0..archive.len() {
        let mut entry = match password {
            Some(password) => archive.by_index_decrypt(i, password.as_bytes()).unwrap(),
            None => archive.by_index(i).unwrap(),
        };
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        entries.insert(entry.name().to_string(), content);
    }
    entries
}

/// A bundle of one OpenVPN server whose config asks for a sign-in, and one WireGuard server.
fn asking_bundle(manifest_extra: Value, auth_txt: bool) -> Vec<u8> {
    let mut manifest: Value = serde_json::from_str(common::MANIFEST).unwrap();
    if let Value::Object(extra) = manifest_extra {
        manifest.as_object_mut().unwrap().extend(extra);
    }
    let directive = if auth_txt {
        "auth-user-pass auth.txt\n"
    } else {
        "auth-user-pass\n"
    };
    let mut entries = vec![
        Entry::File("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        Entry::File("wireguard/one.conf", WG_CONF.as_bytes().to_vec()),
        Entry::File(
            "openvpn/one.ovpn",
            format!("{OVPN_CONF}{directive}").into_bytes(),
        ),
    ];
    if auth_txt {
        entries.push(Entry::File(
            "openvpn/auth.txt",
            b"file-user\nfile-pass\n".to_vec(),
        ));
    }
    zip(&entries)
}

#[test]
fn an_exported_bundle_imports_again_with_the_same_servers_and_configs() {
    let (_tmp, root, id) = installed();
    let exported = edit::export(&root, &id, None, None).unwrap();

    let (_other, elsewhere) = store();
    let again = bundle::import(&exported, &elsewhere).unwrap();
    let original = bundle::open(&root, &id).unwrap();
    assert_eq!(again.manifest, original.manifest);
    for profile in &original.manifest.profiles {
        assert_eq!(
            fs::read(again.dir.join(&profile.file)).unwrap(),
            fs::read(original.dir.join(&profile.file)).unwrap(),
            "{}",
            profile.file
        );
    }
}

#[test]
fn a_bundle_made_here_exports_to_the_bytes_it_was_installed_from() {
    let (_tmp, root) = store();
    let made = edit::create("Mine", &[new("", "a.conf", WG_CONF)]).unwrap();
    let id = bundle::import(&made, &root).unwrap().id;

    let exported = edit::export(&root, &id, None, None).unwrap();
    assert_eq!(exported, made);
    assert!(bundle::import(&exported, &root).unwrap().already_present);
}

#[test]
fn sign_ins_travel_in_the_manifest_and_never_stay_in_the_installed_one() {
    let (_tmp, root) = store();
    let id = bundle::import(&asking_bundle(json!({}), false), &root)
        .unwrap()
        .id;
    let sign_ins = SignIns {
        default: Some(sign_in("everyone", "pw-default")),
        profiles: [("ovpn-1".to_string(), sign_in("just-one", "pw-own"))].into(),
    };

    let exported = edit::export(&root, &id, Some(sign_ins), None).unwrap();
    let manifest = String::from_utf8(entries_of(&exported, None)["manifest.json"].clone()).unwrap();
    assert!(
        manifest.contains("pw-default") && manifest.contains("pw-own"),
        "{manifest}"
    );

    let (_other, elsewhere) = store();
    let imported = bundle::import(&exported, &elsewhere).unwrap();
    assert_eq!(
        imported.default_credentials,
        Some(sign_in("everyone", "pw-default"))
    );
    assert_eq!(
        imported.credentials.get("ovpn-1"),
        Some(&sign_in("just-one", "pw-own"))
    );
    assert!(imported.needs_credentials.is_empty());
    assert_eq!(imported.manifest.credentials, None);

    let stored = fs::read_to_string(imported.dir.join("manifest.json")).unwrap();
    assert!(
        !stored.contains("credentials") && !stored.contains("pw-"),
        "{stored}"
    );
    // What was stored is still a manifest the service reads back.
    assert_eq!(
        bundle::open(&elsewhere, &imported.id).unwrap().manifest,
        imported.manifest
    );
}

#[test]
fn a_default_sign_in_alone_answers_every_server() {
    let sign_ins = json!({ "credentials": { "default": { "username": "u", "password": "p" } } });
    let (_tmp, root) = store();
    let imported = bundle::import(&asking_bundle(sign_ins, false), &root).unwrap();
    assert_eq!(imported.default_credentials, Some(sign_in("u", "p")));
    assert!(imported.credentials.is_empty());
    assert!(imported.needs_credentials.is_empty());
}

#[test]
fn a_server_signed_in_by_its_auth_file_and_the_manifest_is_refused() {
    let both = json!({ "credentials": { "profiles": { "ovpn-1": { "username": "u", "password": "p" } } } });
    let (_tmp, root) = store();
    let errors: Vec<String> = bundle::import(&asking_bundle(both, true), &root)
        .unwrap_err()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("credentials.profiles.ovpn-1")),
        "{errors:?}"
    );
    assert!(!root.exists() || fs::read_dir(&root).unwrap().next().is_none());

    // The auth file alone still works as before.
    let imported = bundle::import(&asking_bundle(json!({}), true), &root).unwrap();
    assert_eq!(
        imported.credentials.get("ovpn-1"),
        Some(&sign_in("file-user", "file-pass"))
    );
}

#[test]
fn a_password_encrypts_every_entry_with_aes_and_the_import_asks_for_it() {
    let (_tmp, root, id) = installed();
    let exported = edit::export(&root, &id, None, Some("correct horse")).unwrap();

    let mut archive = ZipArchive::new(Cursor::new(&exported)).unwrap();
    for i in 0..archive.len() {
        assert!(archive.by_index_raw(i).unwrap().encrypted());
        let aes = archive
            .get_aes_verification_key_and_salt(i)
            .unwrap()
            .unwrap();
        assert_eq!(aes.aes_mode, AesMode::Aes256);
    }

    assert_eq!(bundle::unlock(&exported, None), Unlock::NeedsPassword);
    assert_eq!(
        bundle::unlock(&exported, Some("wrong")),
        Unlock::WrongPassword
    );
    assert_eq!(
        bundle::unlock(&exported, Some("correct horse")),
        Unlock::Readable
    );
    assert_eq!(
        bundle::unlock(&valid_bundle(vec![]), None),
        Unlock::Readable
    );

    let (_other, elsewhere) = store();
    let errors = bundle::import(&exported, &elsewhere).unwrap_err();
    assert!(
        errors[0].to_string().contains("a password is needed"),
        "{errors:?}"
    );
    assert!(bundle::import_with(&exported, &elsewhere, Some("wrong")).is_err());
    let imported = bundle::import_with(&exported, &elsewhere, Some("correct horse")).unwrap();
    assert_eq!(
        imported.manifest,
        bundle::open(&root, &id).unwrap().manifest
    );
}

#[test]
fn entries_encrypted_with_zipcrypto_are_refused_whatever_the_password() {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let weak = SimpleFileOptions::default()
        .with_deprecated_encryption(b"pw")
        .unwrap();
    for (name, content) in [
        ("manifest.json", common::MANIFEST),
        ("wireguard/one.conf", WG_CONF),
        ("openvpn/one.ovpn", OVPN_CONF),
    ] {
        writer.start_file(name, weak).unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    let weak = writer.finish().unwrap().into_inner();
    // No password is asked for: none would make it acceptable.
    assert_eq!(bundle::unlock(&weak, None), Unlock::Readable);

    let (_tmp, root) = store();
    let errors = bundle::import_with(&weak, &root, Some("pw")).unwrap_err();
    assert_eq!(errors.len(), 3, "{errors:?}");
    assert!(
        errors.iter().all(|e| e.to_string().contains("ZipCrypto")),
        "{errors:?}"
    );
}

#[test]
fn a_protected_zip_of_bare_configs_becomes_a_bundle_with_its_password() {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().with_aes_encryption(AesMode::Aes256, "pw");
    writer.start_file("a.conf", options).unwrap();
    writer.write_all(WG_CONF.as_bytes()).unwrap();
    let locked = writer.finish().unwrap().into_inner();

    assert_eq!(bundle::unlock(&locked, None), Unlock::NeedsPassword);
    let built = edit::from_config_zip_with(&locked, Some("pw"))
        .unwrap()
        .unwrap();
    let (_tmp, root) = store();
    assert_eq!(
        bundle::import(&built, &root).unwrap().manifest.profiles[0].id,
        "a"
    );
}

#[test]
fn exporting_a_bundle_that_is_not_installed_is_an_error() {
    let (_tmp, root) = store();
    let missing = BundleId::parse("0123456789abcdef").unwrap();
    assert!(edit::export(&root, &missing, None, None).is_err());
}

#[test]
fn an_exported_file_is_named_after_its_bundle() {
    assert_eq!(edit::export_file_name("Proton UA"), "Proton UA.zip");
    assert_eq!(edit::export_file_name("a/b:c*?"), "a_b_c__.zip");
    assert_eq!(edit::export_file_name("  trailing. . "), "trailing.zip");
    assert_eq!(edit::export_file_name("Мої сервери"), "Мої сервери.zip");
    assert_eq!(edit::export_file_name(""), "bundle.zip");
    assert_eq!(edit::export_file_name("..."), "bundle.zip");
    assert_eq!(edit::export_file_name("CON"), "bundle.zip");
}
