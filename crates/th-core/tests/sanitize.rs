mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{Entry, MANIFEST, WG_CONF, zip};
use th_core::bundle;
use th_core::manifest::Protocol;
use th_core::sanitize::{self, Endpoint, Transport};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/configs")
            .join(name),
    )
    .unwrap()
}

fn files(pairs: &[(&str, Vec<u8>)]) -> BTreeMap<String, Vec<u8>> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn ovpn(text: &str) -> Result<sanitize::SanitizedConfig, Vec<String>> {
    let bundle = files(&[("openvpn/auth.txt", b"user\npass\n".to_vec())]);
    sanitize::sanitize(Protocol::Openvpn, "openvpn/x.ovpn", text.as_bytes(), &|p| {
        bundle.get(p).cloned()
    })
    .map_err(|errors| errors.iter().map(|e| e.to_string()).collect())
}

fn wg(text: &str) -> Result<sanitize::SanitizedConfig, Vec<String>> {
    sanitize::sanitize(
        Protocol::Wireguard,
        "wireguard/x.conf",
        text.as_bytes(),
        &|_| None,
    )
    .map_err(|errors| errors.iter().map(|e| e.to_string()).collect())
}

fn assert_err(result: Result<sanitize::SanitizedConfig, Vec<String>>, needle: &str) {
    let errors = result.expect_err(&format!("expected a refusal containing {needle:?}"));
    assert!(
        errors.iter().any(|e| e.contains(needle)),
        "expected an error containing {needle:?}, got {errors:?}"
    );
}

const BASE: &str = "client\ndev tun\nremote 198.51.100.8 1194\n";

// ---- OpenVPN -------------------------------------------------------------------------------

#[test]
fn provider_ovpn_is_accepted_with_endpoints_and_credentials() {
    let bundle = files(&[("openvpn/auth.txt", fixture("auth.txt"))]);
    let config = sanitize::sanitize(
        Protocol::Openvpn,
        "openvpn/provider.ovpn",
        &fixture("provider.ovpn"),
        &|p| bundle.get(p).cloned(),
    )
    .unwrap();

    assert_eq!(
        config.endpoints,
        vec![
            Endpoint {
                host: "kyiv-1.vpn.example.net".into(),
                port: 1194,
                transport: Transport::Udp
            },
            Endpoint {
                host: "198.51.100.9".into(),
                port: 443,
                transport: Transport::Tcp
            },
        ]
    );
    let credentials = config.credentials.unwrap();
    assert_eq!(credentials.username, "alice");
    assert_eq!(credentials.password, "s3cret pass");
    assert!(
        !format!("{credentials:?}").contains("s3cret"),
        "Debug must not print the password"
    );
    assert!(config.text.contains("\nauth-user-pass\n"));
    assert!(!config.text.contains("auth.txt"));
    assert!(config.text.contains("<ca>\n-----BEGIN CERTIFICATE-----"));
}

#[test]
fn every_dangerous_directive_is_refused() {
    for directive in [
        "up /tmp/x.sh",
        "down /tmp/x.sh",
        "route-up x",
        "route-pre-down x",
        "ipchange x",
        "tls-verify x",
        "auth-user-pass-verify x via-env",
        "learn-address x",
        "client-connect x",
        "plugin evil.dll",
        "script-security 2",
        "setenv-safe X y",
        "management 0.0.0.0 7505",
        "config other.ovpn",
        "log C:\\Windows\\x.log",
        "log-append x",
        "status x",
        "writepid x",
        "cd C:\\",
        "ca ca.crt",
        "ignore-unknown-option up",
        "daemon",
    ] {
        let name = directive.split_whitespace().next().unwrap();
        assert_err(
            ovpn(&format!("{BASE}{directive}\n")),
            &format!("x.ovpn:4: directive \"{name}\" is not allowed"),
        );
    }
}

#[test]
fn quoting_and_escaping_cannot_hide_a_directive() {
    for line in [
        "\"up\" \"cmd\"",
        "'up' cmd",
        "u\\p cmd",
        "--up cmd",
        "  \t up cmd",
        "\"plugin\" x.dll",
    ] {
        let result = ovpn(&format!("{BASE}{line}\n"));
        let errors = result.expect_err(line);
        assert!(
            errors.iter().any(|e| e.contains("is not allowed")),
            "{line}: {errors:?}"
        );
    }
}

#[test]
fn tokenizer_matches_openvpn_rules() {
    // A comment mid-line ends the directive; an inline `#` inside a token does not.
    ovpn(&format!("{BASE}verb 3 # up cmd\n")).unwrap();
    ovpn(&format!("{BASE}pull-filter ignore \"route # x\"\n")).unwrap();
    assert_err(
        ovpn(&format!("{BASE}verb \"3\n")),
        "quotation is never closed",
    );
}

#[test]
fn smuggling_through_long_lines_and_nul_is_refused() {
    let long = format!("verb 3{}up cmd", " ".repeat(260));
    assert_err(ovpn(&format!("{BASE}{long}\n")), "longer than 255 bytes");
    assert_err(ovpn(&format!("{BASE}verb 3\0up cmd\n")), "NUL byte");
}

#[test]
fn long_lines_cannot_close_an_inline_block_early() {
    // OpenVPN reads block content in 255-byte chunks and checks each chunk for the closing
    // tag, so here it would leave <ca> on line 2 and run `up` as a directive.
    let config = format!(
        "{BASE}<ca>\n{}</ca>\nscript-security 2\nup evil.bat\n<cert>\n</ca>{}</cert>\n",
        "A".repeat(255),
        " ".repeat(250),
    );
    let errors = ovpn(&config).expect_err("must be refused");
    assert!(
        errors
            .iter()
            .any(|e| e == "openvpn/x.ovpn:5: line is longer than 255 bytes"),
        "{errors:?}"
    );
    assert_err(
        ovpn(&format!("{BASE}<ca>\nMII\0B\n</ca>\n")),
        "x.ovpn:5: line contains a NUL byte",
    );
}

#[test]
fn restricted_arguments_are_enforced() {
    assert_err(
        ovpn(&format!("{BASE}setenv opt up\n")),
        "only \"setenv opt block-outside-dns\" and \"setenv CLIENT_CERT 0\"",
    );
    assert_err(
        ovpn(&format!("{BASE}setenv PATH C:\\evil\n")),
        "only \"setenv opt block-outside-dns\" and \"setenv CLIENT_CERT 0\"",
    );
    assert_err(
        ovpn(&format!("{BASE}comp-lzo yes\n")),
        "only \"comp-lzo no\"",
    );
    assert_err(
        ovpn(&format!("{BASE}allow-compression yes\n")),
        "allow-compression",
    );
    assert_err(
        ovpn(&format!("{BASE}tls-auth ta.key 1\n")),
        "must use an inline <tls-auth> block",
    );
    ovpn(&format!("{BASE}tls-auth [inline] 1\n")).unwrap();
    assert_err(
        ovpn(&format!("{BASE}proto sctp\n")),
        "unsupported protocol \"sctp\"",
    );
    assert_err(ovpn(&format!("{BASE}port 0\n")), "invalid port");
}

#[test]
fn proton_style_lines_are_accepted_with_exact_arguments() {
    ovpn(&format!(
        "{BASE}setenv CLIENT_CERT 0\nreneg-sec 0\nreneg-sec 3600\n"
    ))
    .unwrap();
    assert_err(
        ovpn(&format!("{BASE}setenv CLIENT_CERT 1\n")),
        "only \"setenv opt block-outside-dns\"",
    );
    assert_err(
        ovpn(&format!("{BASE}reneg-sec\n")),
        "reneg-sec needs one number of seconds",
    );
    assert_err(
        ovpn(&format!("{BASE}reneg-sec 60 /tmp/x\n")),
        "reneg-sec needs one number of seconds",
    );
}

#[test]
fn inline_blocks_are_checked() {
    assert_err(
        ovpn(&format!(
            "{BASE}<connection>\nremote 1.2.3.4\n</connection>\n"
        )),
        "inline block <connection> is not allowed",
    );
    assert_err(
        ovpn(&format!(
            "{BASE}<auth-user-pass>\nu\np\n</auth-user-pass>\n"
        )),
        "not allowed",
    );
    assert_err(
        ovpn(&format!("{BASE}<ca>\nMIIB\n")),
        "x.ovpn:4: inline block <ca> is never closed",
    );
    // Text inside an allowed block is data, not directives.
    ovpn(&format!("{BASE}<ca>\nup cmd\n</ca>\n")).unwrap();
}

#[test]
fn remotes_are_validated_and_defaults_applied() {
    let config =
        ovpn("proto tcp\nport 443\nremote a.example.net\nremote b.example.net 80 udp\n").unwrap();
    assert_eq!(
        config.endpoints,
        vec![
            Endpoint {
                host: "a.example.net".into(),
                port: 443,
                transport: Transport::Tcp
            },
            Endpoint {
                host: "b.example.net".into(),
                port: 80,
                transport: Transport::Udp
            },
        ]
    );
    assert_err(ovpn("client\n"), "x.ovpn: has no remote");
    assert_err(ovpn("remote bad_host!\n"), "invalid remote host");
    assert_err(ovpn("remote a.example.net 99999\n"), "invalid port");
}

#[test]
fn auth_user_pass_file_must_be_inside_the_bundle() {
    assert_err(
        ovpn(&format!("{BASE}auth-user-pass ../secret.txt\n")),
        "plain relative path",
    );
    assert_err(
        ovpn(&format!("{BASE}auth-user-pass C:\\creds.txt\n")),
        "plain relative path",
    );
    assert_err(
        ovpn(&format!("{BASE}auth-user-pass missing.txt\n")),
        "\"openvpn/missing.txt\" is not in the archive",
    );
    let config = ovpn(&format!("{BASE}auth-user-pass\n")).unwrap();
    assert!(config.credentials.is_none());
}

#[test]
fn incomplete_credentials_are_refused() {
    let bundle = files(&[("openvpn/auth.txt", b"only-user\n".to_vec())]);
    let result = sanitize::sanitize(
        Protocol::Openvpn,
        "openvpn/x.ovpn",
        format!("{BASE}auth-user-pass auth.txt\n").as_bytes(),
        &|p| bundle.get(p).cloned(),
    );
    assert!(
        result.unwrap_err()[0]
            .to_string()
            .contains("username and a password")
    );
}

#[test]
fn every_error_is_reported_with_its_line() {
    let errors = ovpn("up a\nclient\ndown b\nplugin c\nremote 1.2.3.4\n").unwrap_err();
    assert_eq!(errors.len(), 3);
    assert!(errors[0].starts_with("openvpn/x.ovpn:1:"));
    assert!(errors[2].starts_with("openvpn/x.ovpn:4:"));
}

// ---- WireGuard -----------------------------------------------------------------------------

#[test]
fn provider_conf_is_accepted_with_endpoints() {
    let config = sanitize::sanitize(
        Protocol::Wireguard,
        "wireguard/p.conf",
        &fixture("provider.conf"),
        &|_| None,
    )
    .unwrap();
    assert_eq!(
        config.endpoints,
        vec![
            Endpoint {
                host: "kyiv-1.vpn.example.net".into(),
                port: 51820,
                transport: Transport::Udp
            },
            Endpoint {
                host: "2001:db8::7".into(),
                port: 51821,
                transport: Transport::Udp
            },
        ]
    );
    assert_eq!(config.text.as_bytes(), fixture("provider.conf"));
}

#[test]
fn hook_keys_and_table_are_refused_in_any_case() {
    for key in [
        "PreUp", "PostUp", "PreDown", "PostDown", "postup", "POSTDOWN", "Table",
    ] {
        let text = WG_CONF.replace("DNS = 10.0.0.1", &format!("{key} = calc.exe"));
        assert_err(wg(&text), &format!("{key} is not allowed"));
    }
}

#[test]
fn unknown_keys_and_sections_are_refused() {
    assert_err(
        wg(&WG_CONF.replace("DNS = 10.0.0.1", "SaveConfig = true")),
        "SaveConfig is not allowed in [Interface]",
    );
    assert_err(
        wg(&format!("{WG_CONF}\n[Script]\nRun = x\n")),
        "unknown section [Script]",
    );
    assert_err(
        wg(&format!("PrivateKey = x\n{WG_CONF}")),
        "PrivateKey is outside any section",
    );
    assert_err(
        wg(&format!("{WG_CONF}garbage line\n")),
        "expected \"Key = Value\"",
    );
}

#[test]
fn structure_is_enforced() {
    assert_err(
        wg(&format!("{WG_CONF}[Interface]\nPrivateKey = x\n")),
        "exactly one [Interface]",
    );
    assert_err(wg("[Interface]\nPrivateKey = x\n"), "at least one [Peer]");
    assert_err(
        wg(&WG_CONF.replace("Endpoint = 198.51.100.7:51820\n", "")),
        "[Peer] has no Endpoint",
    );
    assert_err(
        wg(&WG_CONF.replace("PublicKey", "# PublicKey")),
        "[Peer] has no PublicKey",
    );
    let no_private_key = WG_CONF.replace(
        "PrivateKey = cGxhY2Vob2xkZXJwbGFjZWhvbGRlcnBsYWNlaG9sZGU=\n",
        "",
    );
    assert_err(wg(&no_private_key), "[Interface] has no PrivateKey");
    assert_err(
        wg(&WG_CONF.replace(
            "PrivateKey = cGxhY2Vob2xkZXJwbGFjZWhvbGRlcnBsYWNlaG9sZGU=",
            "PrivateKey =",
        )),
        "[Interface] has no PrivateKey",
    );
}

#[test]
fn endpoints_are_validated() {
    for bad in [
        "198.51.100.7",
        "host:0",
        "2001:db8::1:51820",
        "[2001:db8::1]51820",
        "bad_host:51820",
    ] {
        assert_err(
            wg(&WG_CONF.replace("198.51.100.7:51820", bad)),
            "invalid Endpoint",
        );
    }
}

#[test]
fn non_utf8_config_is_refused() {
    let result = sanitize::sanitize(
        Protocol::Wireguard,
        "wireguard/x.conf",
        &[0xff, 0xfe, 0x00],
        &|_| None,
    );
    assert!(
        result.unwrap_err()[0]
            .to_string()
            .contains("not valid UTF-8")
    );
}

// ---- Through the import --------------------------------------------------------------------

#[test]
fn import_moves_credentials_out_and_never_writes_auth_file() {
    let tmp = tempfile::tempdir().unwrap();
    let bytes = zip(&[
        Entry::File("manifest.json", MANIFEST.as_bytes().to_vec()),
        Entry::File("wireguard/one.conf", WG_CONF.as_bytes().to_vec()),
        Entry::File(
            "openvpn/one.ovpn",
            format!("{BASE}auth-user-pass auth.txt\n").into_bytes(),
        ),
        Entry::File("openvpn/auth.txt", b"bob\nhunter2\n".to_vec()),
    ]);

    let imported = bundle::import(&bytes, tmp.path()).unwrap();

    assert_eq!(imported.credentials["ovpn-1"].username, "bob");
    assert_eq!(imported.endpoints["wg-1"][0].port, 51820);
    assert_eq!(imported.endpoints["ovpn-1"][0].host, "198.51.100.8");
    assert!(!imported.dir.join("openvpn/auth.txt").exists());
    let installed = std::fs::read_to_string(imported.dir.join("openvpn/one.ovpn")).unwrap();
    assert!(installed.ends_with("auth-user-pass\n"), "{installed}");

    let again = bundle::import(&bytes, tmp.path()).unwrap();
    assert!(again.already_present);
    assert!(again.credentials.is_empty());
    assert_eq!(again.endpoints, imported.endpoints);
}

#[test]
fn import_refuses_a_hostile_config_and_leaves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let bytes = zip(&[
        Entry::File("manifest.json", MANIFEST.as_bytes().to_vec()),
        Entry::File(
            "wireguard/one.conf",
            WG_CONF
                .replace("DNS = 10.0.0.1", "PostUp = calc.exe")
                .into_bytes(),
        ),
        Entry::File(
            "openvpn/one.ovpn",
            format!("{BASE}script-security 2\nup evil.bat\n").into_bytes(),
        ),
    ]);

    let errors: Vec<String> = bundle::import(&bytes, tmp.path())
        .unwrap_err()
        .iter()
        .map(|e| e.to_string())
        .collect();

    assert_eq!(errors.len(), 3, "{errors:?}");
    assert!(errors.contains(&"wireguard/one.conf:4: PostUp is not allowed".to_string()));
    assert!(
        errors.contains(
            &"openvpn/one.ovpn:4: directive \"script-security\" is not allowed".to_string()
        )
    );
    assert!(std::fs::read_dir(tmp.path()).unwrap().next().is_none());
}

// ---- Credentials the config asks for -------------------------------------------------------

#[test]
fn asks_credentials_reflects_auth_user_pass_in_either_form() {
    let bare = ovpn(&format!("{BASE}auth-user-pass\n")).unwrap();
    assert!(bare.asks_credentials);
    assert!(bare.credentials.is_none());

    let with_file = ovpn(&format!("{BASE}auth-user-pass auth.txt\n")).unwrap();
    assert!(with_file.asks_credentials);
    assert!(with_file.credentials.is_some());

    assert!(
        !ovpn(BASE).unwrap().asks_credentials,
        "certificate-only configs ask nothing"
    );
    assert!(!wg(WG_CONF).unwrap().asks_credentials);
}

#[test]
fn import_lists_openvpn_profiles_still_needing_credentials() {
    let bundle_with = |ovpn_text: String, extra: Vec<Entry<'static>>| {
        let mut entries = vec![
            Entry::File("manifest.json", MANIFEST.as_bytes().to_vec()),
            Entry::File("wireguard/one.conf", WG_CONF.as_bytes().to_vec()),
            Entry::File("openvpn/one.ovpn", ovpn_text.into_bytes()),
        ];
        entries.extend(extra);
        zip(&entries)
    };
    let tmp = tempfile::tempdir().unwrap();

    // Proton's shape: a bare auth-user-pass and no file.
    let bare = bundle::import(
        &bundle_with(format!("{BASE}auth-user-pass\n"), vec![]),
        tmp.path(),
    )
    .unwrap();
    assert_eq!(bare.needs_credentials, ["ovpn-1"]);
    assert!(bare.credentials.is_empty());

    // Credentials shipped in the bundle: nothing to ask.
    let shipped = bundle_with(
        format!("{BASE}auth-user-pass auth.txt\n"),
        vec![Entry::File("openvpn/auth.txt", b"bob\nhunter2\n".to_vec())],
    );
    let first = bundle::import(&shipped, tmp.path()).unwrap();
    assert!(first.needs_credentials.is_empty());

    // On a repeated import the credentials are not handed over again, so the profile is
    // listed; the service subtracts what it stored the first time.
    let again = bundle::import(&shipped, tmp.path()).unwrap();
    assert!(again.already_present);
    assert_eq!(again.needs_credentials, ["ovpn-1"]);

    // No auth-user-pass at all: nothing needed.
    let none = bundle::import(&bundle_with(BASE.to_string(), vec![]), tmp.path()).unwrap();
    assert!(none.needs_credentials.is_empty());
}
