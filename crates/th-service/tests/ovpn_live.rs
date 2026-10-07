//! The OpenVPN driver against the real `openvpn.exe` and the lab server. Never on the host PC:
//! only inside the `th-test` VM, with the test servers up (DEVELOPMENT.md §4–5):
//!
//! ```powershell
//! scripts\vm.ps1 test ovpn_live
//! ```
//!
//! The lab server takes `th-test-user` with a password holding a quote and a backslash, and
//! also lets clients in on their certificate alone (test-infra/gen.sh).
#![cfg(windows)]

use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use th_core::bundle::BundleId;
use th_core::driver::{ResolvedEndpoint, ResolvedProfile, TunnelStatus, VpnDriver};
use th_core::manifest::Protocol;
use th_core::sanitize::{Credentials, openvpn};
use th_core::tunnel_name::tunnel_name;
use th_service::drivers::openvpn::{CredentialLookup, OpenvpnDriver};
use th_service::runtimes;
use th_service::store::Layout;

const TUNNEL_GATEWAY: &str = "10.77.3.1";
const SERVER: &str = "192.168.77.1";

fn in_test_vm() {
    assert_eq!(
        std::env::var("TH_TEST_VM").as_deref(),
        Ok("1"),
        "ovpn_live runs only in th-test, through scripts\\vm.ps1"
    );
}

fn configs() -> PathBuf {
    std::env::var_os("TH_CONFIGS").map_or_else(|| PathBuf::from(r"C:\th\configs"), PathBuf::from)
}

/// A client config from `test-infra/out/`, resolved the way the orchestrator will.
fn profile(file: &str, profile_id: &str) -> ResolvedProfile {
    let config = configs().join(file);
    let text = std::fs::read_to_string(&config)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", config.display()));
    let sanitized = openvpn::sanitize(file, &text, &|_| None).expect("a valid test config");
    let endpoints = sanitized
        .endpoints
        .iter()
        .map(|e| ResolvedEndpoint {
            addr: (e.host.as_str(), e.port)
                .to_socket_addrs()
                .unwrap()
                .next()
                .unwrap(),
            transport: e.transport,
        })
        .collect();
    let bundle = BundleId::parse("0123456789abcdef").unwrap();
    ResolvedProfile {
        tunnel_name: tunnel_name(&bundle, profile_id),
        bundle,
        profile_id: profile_id.into(),
        protocol: Protocol::Openvpn,
        config_path: config,
        endpoints,
    }
}

/// The service's directories for one test, emptied first and kept afterwards, so OpenVPN's log
/// is there to read when a test fails.
fn layout(test: &str) -> Layout {
    let root = PathBuf::from(r"C:\th\ovpn-live").join(test);
    let _ = std::fs::remove_dir_all(&root);
    let layout = Layout::new(root);
    for dir in [layout.tunnels(), layout.secrets(), layout.logs()] {
        std::fs::create_dir_all(dir).unwrap();
    }
    layout
}

fn driver(layout: &Layout, credentials: Option<(&'static str, &'static str)>) -> OpenvpnDriver {
    let runtime = runtimes::discover()
        .into_iter()
        .find(|r| r.protocol == Protocol::Openvpn)
        .unwrap();
    let lookup: CredentialLookup = Arc::new(move |_, _| {
        Ok(credentials.map(|(username, password)| Credentials {
            username: username.into(),
            password: password.into(),
        }))
    });
    OpenvpnDriver::new(&runtime, layout, lookup).expect("OpenVPN is installed in th-test")
}

fn routes() -> String {
    let output = std::process::Command::new("route")
        .args(["print", "-4"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn openvpn_processes() -> usize {
    let output = std::process::Command::new("tasklist")
        .args(["/fi", "imagename eq openvpn.exe", "/fo", "csv", "/nh"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| l.starts_with("\"openvpn.exe\""))
        .count()
}

fn files_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

/// Whether the table has OpenVPN's host route to the lab server. A clean table reaches it
/// through the lab subnet's on-link route only.
fn has_server_route(routes: &str) -> bool {
    routes.lines().any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        fields.starts_with(&[SERVER, "255.255.255.255"])
    })
}

/// Nothing of a tunnel is left: no process, no route through it or to its server, no files
/// but OpenVPN's log.
fn assert_gone(layout: &Layout) {
    assert_eq!(openvpn_processes(), 0, "openvpn.exe is still running");
    let routes = routes();
    assert!(
        !routes.contains(TUNNEL_GATEWAY),
        "a route through the tunnel is left:\n{routes}"
    );
    assert!(
        !has_server_route(&routes),
        "the host route to the server is left:\n{routes}"
    );
    assert_eq!(files_in(&layout.tunnels()), Vec::<String>::new());
    assert_eq!(files_in(&layout.secrets()), Vec::<String>::new());
}

#[tokio::test]
#[ignore = "starts a real OpenVPN tunnel: th-test VM only"]
async fn a_profile_connects_reports_up_and_disconnects_cleanly() {
    in_test_vm();
    let layout = layout("connect");
    let driver = driver(&layout, None);
    let profile = profile("ovpn.ovpn", "live-ovpn");

    let handle = driver.connect(&profile).await.expect("connected");
    assert_ne!(handle.interface.0, 0, "the tunnel's interface was found");
    assert_eq!(driver.status(&handle).await.unwrap(), TunnelStatus::Up);
    let routes = routes();
    assert!(
        routes.contains(TUNNEL_GATEWAY),
        "no route through the tunnel:\n{routes}"
    );
    // The copy points at the resolved address, and the management password is gone.
    let copy = std::fs::read_to_string(
        layout
            .tunnels()
            .join(format!("{}.ovpn", profile.tunnel_name)),
    )
    .unwrap();
    assert!(copy.contains("remote 192.168.77.1 1194 udp\n"), "{copy}");
    assert_eq!(files_in(&layout.secrets()), Vec::<String>::new());

    driver.disconnect(&handle).await.expect("disconnected");
    assert_eq!(driver.status(&handle).await.unwrap(), TunnelStatus::Down);
    assert_gone(&layout);
}

#[tokio::test]
#[ignore = "starts a real OpenVPN tunnel: th-test VM only"]
async fn credentials_are_answered_from_the_store_and_a_refusal_is_reported() {
    in_test_vm();
    let layout = layout("credentials");
    let profile = profile("ovpn-auth.ovpn", "live-auth");

    let none = driver(&layout, None).connect(&profile).await.unwrap_err();
    assert!(none.to_string().contains("needs credentials"), "{none}");
    assert_gone(&layout);

    let wrong = driver(&layout, Some(("th-test-user", "not the password")))
        .connect(&profile)
        .await
        .unwrap_err();
    assert!(wrong.to_string().contains("refused"), "{wrong}");
    assert_gone(&layout);

    // The right password has a quote and a backslash in it.
    let driver = driver(&layout, Some(("th-test-user", r#"th-test "pass\word"#)));
    let handle = driver
        .connect(&profile)
        .await
        .expect("connected with credentials");
    assert_eq!(driver.status(&handle).await.unwrap(), TunnelStatus::Up);
    driver.disconnect(&handle).await.unwrap();
    assert_gone(&layout);
}

#[tokio::test]
#[ignore = "starts a real OpenVPN tunnel: th-test VM only"]
async fn a_tunnel_killed_with_the_service_is_cleared_on_the_next_start() {
    in_test_vm();
    let layout = layout("crash");
    let profile = profile("ovpn.ovpn", "live-crash");
    {
        let driver = driver(&layout, None);
        driver.connect(&profile).await.expect("connected");
        // The service dies: the job's handle closes and takes openvpn.exe with it.
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while openvpn_processes() > 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "openvpn.exe outlived the job"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // The offload adapter takes the routes through the tunnel with it; the host route to the
    // server, on the physical interface, stays.
    let routes = routes();
    assert!(
        has_server_route(&routes),
        "a killed OpenVPN is expected to leave its route to the server:\n{routes}"
    );

    let restarted = driver(&layout, None);
    assert_eq!(restarted.reconcile().await.unwrap(), Vec::new());
    assert_gone(&layout);
}
