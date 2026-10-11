use std::path::Path;

use serde_json::{Value, json};
use th_core::manifest::{self, OnDisconnect, Protocol, Rotation};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/manifests")
            .join(name),
    )
    .unwrap()
}

fn minimal() -> Value {
    serde_json::from_slice(&fixture("valid-minimal.json")).unwrap()
}

fn errors_for(value: &Value) -> Vec<String> {
    match manifest::parse(&serde_json::to_vec(value).unwrap()) {
        Ok(_) => vec![],
        Err(errors) => errors.iter().map(|e| e.to_string()).collect(),
    }
}

fn assert_rejected(value: &Value, needle: &str) {
    let errors = errors_for(value);
    assert!(
        errors.iter().any(|e| e.contains(needle)),
        "expected an error containing {needle:?}, got {errors:?}"
    );
}

#[test]
fn full_fixture_parses() {
    let m = manifest::parse(&fixture("valid-full.json")).unwrap();
    assert_eq!(m.bundle_name, "Ukraine Server Cluster");
    assert_eq!(m.profiles.len(), 3);
    assert_eq!(m.profiles[2].protocol, Protocol::Openvpn);
    assert_eq!(m.rotation.on_disconnect, OnDisconnect::ReconnectRandom);
}

#[test]
fn minimal_fixture_gets_defaults() {
    let m = manifest::parse(&fixture("valid-minimal.json")).unwrap();
    assert_eq!(m.rotation, Rotation::default());
    assert_eq!(m.rotation_protocols(), vec![Protocol::Openvpn]);
}

#[test]
fn partial_rotation_keeps_other_defaults() {
    let mut v = minimal();
    v["rotation"] = json!({ "interval_minutes": 5 });
    let m = manifest::parse(&serde_json::to_vec(&v).unwrap()).unwrap();
    assert_eq!(m.rotation.interval_minutes, 5);
    assert_eq!(m.rotation.reconnect_grace_seconds, 20);
}

#[test]
fn unknown_fields_are_rejected() {
    let mut v = minimal();
    v["default_protocol"] = json!("wireguard");
    assert_rejected(&v, "unknown field `default_protocol`");

    let mut v = minimal();
    v["rotation"] = json!({ "cooldown_seconds": 2 });
    assert_rejected(&v, "unknown field `cooldown_seconds`");

    let mut v = minimal();
    v["profiles"][0]["auth_file"] = json!("openvpn/auth.txt");
    assert_rejected(&v, "unknown field `auth_file`");
}

#[test]
fn bad_enum_values_are_rejected() {
    let mut v = minimal();
    v["rotation"] = json!({ "strategy": "round_robin" });
    assert_rejected(&v, "unknown variant `round_robin`");

    let mut v = minimal();
    v["profiles"][0]["protocol"] = json!("ipsec");
    assert_rejected(&v, "unknown variant `ipsec`");
}

#[test]
fn wrong_schema_version_is_rejected() {
    let mut v = minimal();
    v["schema_version"] = json!("1.0");
    assert_rejected(&v, "schema_version: must be \"1\"");
}

#[test]
fn empty_profiles_is_rejected() {
    let mut v = minimal();
    v["profiles"] = json!([]);
    assert_rejected(&v, "profiles: must have at least one profile");
}

#[test]
fn too_many_profiles_is_rejected() {
    let mut v = minimal();
    let profiles: Vec<Value> = (0..=manifest::MAX_PROFILES)
        .map(|i| json!({ "id": format!("p{i}"), "name": "n", "protocol": "openvpn", "file": "openvpn/a.ovpn" }))
        .collect();
    v["profiles"] = Value::Array(profiles);
    assert_rejected(&v, "must have at most 500 profiles");
}

#[test]
fn bad_and_duplicate_ids_are_rejected() {
    let mut v = minimal();
    v["profiles"][0]["id"] = json!("Upper_Case");
    assert_rejected(&v, "profiles[0].id: must match");

    let mut v = minimal();
    let p = v["profiles"][0].clone();
    v["profiles"] = json!([p.clone(), p]);
    assert_rejected(&v, "profiles[1].id: duplicate id \"only\"");
}

#[test]
fn names_must_be_bounded() {
    let mut v = minimal();
    v["bundle_name"] = json!("");
    assert_rejected(&v, "bundle_name: must be 1 to 100 characters");

    let mut v = minimal();
    v["profiles"][0]["name"] = json!("x".repeat(101));
    assert_rejected(&v, "profiles[0].name: must be 1 to 100 characters");
}

#[test]
fn files_outside_the_protocol_dir_are_rejected() {
    for (file, needle) in [
        ("../evil.ovpn", "'..'"),
        ("openvpn/../manifest.json", "'..'"),
        ("/etc/openvpn/a.ovpn", "empty"),
        ("openvpn//a.ovpn", "empty"),
        ("openvpn\\a.ovpn", "'/' only"),
        ("C:/a.ovpn", "'/' only"),
        ("wireguard/a.conf", "must be inside openvpn/"),
        ("openvpn", "must be inside openvpn/"),
        ("a.ovpn", "must be inside openvpn/"),
    ] {
        let mut v = minimal();
        v["profiles"][0]["file"] = json!(file);
        assert_rejected(&v, needle);
    }
}

#[test]
fn rotation_ranges_are_enforced() {
    let mut v = minimal();
    v["rotation"] = json!({ "interval_minutes": 0, "reconnect_grace_seconds": 301 });
    let errors = errors_for(&v);
    assert!(
        errors
            .iter()
            .any(|e| e.starts_with("rotation.interval_minutes"))
    );
    assert!(
        errors
            .iter()
            .any(|e| e.starts_with("rotation.reconnect_grace_seconds"))
    );
}

#[test]
fn protocols_must_be_non_empty_unique_and_used() {
    let mut v = minimal();
    v["protocols"] = json!([]);
    assert_rejected(&v, "protocols: must not be empty");

    let mut v = minimal();
    v["protocols"] = json!(["openvpn", "openvpn"]);
    assert_rejected(&v, "protocols: must not repeat a protocol");

    let mut v = minimal();
    v["protocols"] = json!(["wireguard"]);
    assert_rejected(&v, "no profile uses any of the listed protocols");
}

#[test]
fn all_errors_are_reported_together() {
    let mut v = minimal();
    v["schema_version"] = json!("2");
    v["bundle_name"] = json!("");
    v["profiles"][0]["id"] = json!("BAD");
    assert_eq!(errors_for(&v).len(), 3);
}

#[test]
fn published_schema_agrees_with_the_fixtures() {
    let schema: Value =
        serde_json::from_str(include_str!("../../../docs/manifest.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();

    for name in ["valid-full.json", "valid-minimal.json"] {
        let instance: Value = serde_json::from_slice(&fixture(name)).unwrap();
        assert!(
            validator.is_valid(&instance),
            "{name} should match the schema"
        );
    }

    let signed_in = with_sign_ins(json!({
        "default": { "username": "u", "password": "p" },
        "profiles": { "only": { "username": "u2", "password": "p2" } }
    }));
    assert!(
        validator.is_valid(&signed_in),
        "sign-ins should match the schema"
    );

    let mut unknown = minimal();
    unknown["extra"] = json!(1);
    let mut traversal = minimal();
    traversal["profiles"][0]["file"] = json!("openvpn\\..\\x.ovpn");
    let mut bad_id = minimal();
    bad_id["profiles"][0]["id"] = json!("BAD");
    let wrapped = with_sign_ins(json!({ "default": { "username": "u", "password": "p\n" } }));
    for instance in [unknown, traversal, bad_id, wrapped] {
        assert!(
            !validator.is_valid(&instance),
            "schema should reject {instance}"
        );
        assert!(manifest::parse(&serde_json::to_vec(&instance).unwrap()).is_err());
    }
}

/// The minimal manifest with a WireGuard profile beside its OpenVPN one, and `credentials`.
fn with_sign_ins(credentials: Value) -> Value {
    let mut v = minimal();
    v["profiles"].as_array_mut().unwrap().push(json!({
        "id": "wg", "name": "WG", "protocol": "wireguard", "file": "wireguard/wg.conf"
    }));
    v["credentials"] = credentials;
    v
}

#[test]
fn sign_ins_name_openvpn_profiles_and_hold_usable_values() {
    let good = with_sign_ins(json!({
        "default": { "username": "everyone", "password": "pw" },
        "profiles": { "only": { "username": "one", "password": "pw2" } }
    }));
    let m = manifest::parse(&serde_json::to_vec(&good).unwrap()).unwrap();
    let sign_ins = m.credentials.unwrap();
    assert_eq!(sign_ins.default.unwrap().username, "everyone");
    assert_eq!(sign_ins.profiles["only"].password, "pw2");

    assert_rejected(&with_sign_ins(json!({})), "credentials: must hold");
    assert_rejected(
        &with_sign_ins(json!({ "profiles": { "gone": { "username": "u", "password": "p" } } })),
        "credentials.profiles.gone: no profile has this id",
    );
    assert_rejected(
        &with_sign_ins(json!({ "profiles": { "wg": { "username": "u", "password": "p" } } })),
        "credentials.profiles.wg: only OpenVPN profiles sign in",
    );
    assert_rejected(
        &with_sign_ins(json!({ "default": { "username": "", "password": "p" } })),
        "credentials.default.username: must be 1 to 256 characters",
    );
    assert_rejected(
        &with_sign_ins(json!({ "default": { "username": "u", "password": "p\nmore" } })),
        "credentials.default.password: must not contain line breaks",
    );
    assert_rejected(
        &with_sign_ins(json!({ "default": { "username": "u", "password": "p", "otp": "1" } })),
        "unknown field",
    );
}

#[test]
fn a_manifest_without_sign_ins_is_written_without_them() {
    let m = manifest::parse(&fixture("valid-minimal.json")).unwrap();
    let written = serde_json::to_string(&m).unwrap();
    assert!(!written.contains("credentials"), "{written}");
}
