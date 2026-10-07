//! The WireGuard driver against a real server. Never on the host PC: only inside the `th-test`
//! VM, elevated, with the test servers up (DEVELOPMENT.md §4–5):
//!
//! ```powershell
//! scripts\vm.ps1 test wg_live -Env @{ TH_WG_CONFIG = 'C:\th\configs\wg-a.conf' }
//! ```
#![cfg(windows)]

use std::net::ToSocketAddrs;
use std::path::PathBuf;

use th_core::bundle::BundleId;
use th_core::driver::{ResolvedEndpoint, ResolvedProfile, TunnelEvent, TunnelStatus, VpnDriver};
use th_core::manifest::Protocol;
use th_core::sanitize::wireguard;
use th_core::tunnel_name::tunnel_name;
use th_service::drivers::wireguard::{WireguardDriver, service_name};
use th_service::{runtimes, win};
use tokio::time::{Duration, timeout};

fn env_path(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name).map_or_else(|| PathBuf::from(default), PathBuf::from)
}

/// The client config from `test-infra/out/`, resolved the way the orchestrator will.
fn profile() -> ResolvedProfile {
    let config = env_path("TH_WG_CONFIG", r"C:\th\configs\wg-a.conf");
    let text = std::fs::read_to_string(&config)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", config.display()));
    let sanitized = wireguard::sanitize("wg-a.conf", &text).expect("a valid test config");
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
        tunnel_name: tunnel_name(&bundle, "live-wg"),
        bundle,
        profile_id: "live-wg".into(),
        protocol: Protocol::Wireguard,
        config_path: config,
        endpoints,
    }
}

fn driver() -> WireguardDriver {
    let runtime = runtimes::discover()
        .into_iter()
        .find(|r| r.protocol == Protocol::Wireguard)
        .unwrap();
    // A directory only administrators can write, as the service's own `tunnels\` is.
    let tunnels = env_path("TH_TUNNELS_DIR", r"C:\th\tunnels");
    std::fs::create_dir_all(&tunnels).unwrap();
    WireguardDriver::new(&runtime, tunnels).expect("WireGuard is installed in the VM")
}

fn tunnel_services() -> Vec<String> {
    win::services_with_prefix("WireGuardTunnel$th-").unwrap()
}

#[tokio::test]
#[ignore = "needs WireGuard, an elevated VM and the test servers; never run on the host"]
async fn connect_status_reconcile_and_disconnect() {
    let driver = driver();
    let profile = profile();
    let service = service_name(&profile.tunnel_name);

    let handle = driver.connect(&profile).await.expect("connect");
    assert_ne!(handle.interface.0, 0, "the tunnel has an interface");
    assert_eq!(driver.status(&handle).await.unwrap(), TunnelStatus::Up);
    assert!(tunnel_services().contains(&service));

    let found = driver.reconcile().await.unwrap();
    let leftover = found
        .iter()
        .find(|h| h.tunnel_name == profile.tunnel_name)
        .expect("reconcile finds the running tunnel");
    assert_eq!(leftover.endpoints, profile.endpoints, "endpoints read back");
    assert_eq!(leftover.interface, handle.interface);

    // Connecting again over a leftover replaces it rather than failing.
    let handle = driver
        .connect(&profile)
        .await
        .expect("reconnect over a leftover");

    // A disconnect the engine asked for is not an event.
    let mut events = driver.events();
    driver.disconnect(&handle).await.expect("disconnect");
    assert_eq!(driver.status(&handle).await.unwrap(), TunnelStatus::Down);
    assert!(!tunnel_services().contains(&service));
    let heard = timeout(Duration::from_secs(5), events.recv()).await;
    assert!(heard.is_err(), "a planned disconnect produced {heard:?}");
    driver
        .disconnect(&handle)
        .await
        .expect("disconnect is idempotent");

    // A tunnel stopped behind the driver's back is reported lost.
    let handle = driver.connect(&profile).await.expect("connect again");
    let stopped = std::process::Command::new("sc.exe")
        .args(["stop", &service])
        .status()
        .unwrap();
    assert!(stopped.success(), "sc stop {service}");
    let heard = timeout(Duration::from_secs(15), events.recv()).await;
    assert!(
        matches!(&heard, Ok(Ok(TunnelEvent::Down { tunnel, .. })) if *tunnel == profile.tunnel_name),
        "a stopped tunnel was not reported: {heard:?}"
    );
    driver.disconnect(&handle).await.expect("clean up");
    assert!(!tunnel_services().contains(&service));
}
