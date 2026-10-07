//! The management client against the real `openvpn.exe`, so the protocol as the fake server
//! plays it (tests/ovpn_mgmt.rs) is checked against the real thing. Only in the `th-test` VM,
//! with the test servers up:
//!
//! ```powershell
//! scripts\vm.ps1 test ovpn_mgmt_live -Env @{ TH_OVPN_CONFIG = 'C:\th\configs\ovpn.ovpn' }
//! ```
#![cfg(windows)]

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use th_core::ipc::Secret;
use th_service::drivers::ovpn_mgmt::{Management, Notification, State};
use tokio::time::timeout;

fn in_test_vm() {
    assert_eq!(
        std::env::var("TH_TEST_VM").as_deref(),
        Ok("1"),
        "ovpn_mgmt_live runs only in th-test, through scripts\\vm.ps1"
    );
}

/// A free port on localhost, found by binding and releasing it.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
#[ignore = "starts a real OpenVPN tunnel: th-test VM only"]
async fn the_real_openvpn_goes_from_hold_to_connected_and_exits_on_sigterm() {
    in_test_vm();
    let config = std::env::var_os("TH_OVPN_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| r"C:\th\configs\ovpn.ovpn".into());
    let openvpn =
        PathBuf::from(std::env::var("ProgramFiles").unwrap()).join(r"OpenVPN\bin\openvpn.exe");
    let port = free_port();
    let secret = Secret::new("live-test-management-password");
    let password_file = std::env::temp_dir().join("th-ovpn-mgmt-live.pw");
    std::fs::write(&password_file, format!("{}\n", secret.expose())).unwrap();

    let mut child = tokio::process::Command::new(&openvpn)
        .arg("--config")
        .arg(&config)
        .args(["--management", "127.0.0.1", &port.to_string()])
        .arg(&password_file)
        .args([
            "--management-hold",
            "--management-query-passwords",
            "--verb",
            "3",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("openvpn.exe starts");

    // OpenVPN opens the port a moment after it starts.
    let addr = format!("127.0.0.1:{port}").parse().unwrap();
    let mut management = None;
    for _ in 0..50 {
        if let Ok(m) = Management::connect(addr, &secret).await {
            management = Some(m);
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let mut management = management.expect("logged in to the management port");
    let _ = std::fs::remove_file(&password_file);

    management.start().await.expect("state on, hold release");
    let connected = timeout(Duration::from_secs(30), async {
        loop {
            match management.next_notification().await.unwrap() {
                Notification::State(change) if change.state == State::Connected => {
                    return change;
                }
                Notification::Fatal(message) => panic!("OpenVPN gave up: {message}"),
                _ => {}
            }
        }
    })
    .await
    .expect("connected within 30 s");
    assert_eq!(connected.detail, "SUCCESS");
    let ip = connected.local_ip.expect("a tunnel address");
    assert!(ip.to_string().starts_with("10.77.3."), "{ip}");

    management.sigterm().await.expect("SIGTERM accepted");
    timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("OpenVPN exits by itself within 15 s of SIGTERM")
        .unwrap();

    // Unlike a killed openvpn.exe, one that exits on SIGTERM removes its routes.
    let routes = std::process::Command::new("route")
        .args(["print", "-4"])
        .output()
        .unwrap();
    let routes = String::from_utf8_lossy(&routes.stdout);
    assert!(
        !routes.contains("10.77.3.1"),
        "a route through the closed tunnel is left:\n{routes}"
    );
}
