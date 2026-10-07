//! The kill-switch on a real machine. It blocks that machine's traffic, so it runs only inside
//! the `th-test` VM, after a checkpoint, through `scripts\vm.ps1` (which sets `TH_TEST_VM`):
//!
//! ```powershell
//! scripts\vm.ps1 checkpoint before-killswitch
//! scripts\vm.ps1 test killswitch_live -Env @{ TH_WG_CONFIG = 'C:\th\configs\wg-a.conf' }
//! scripts\vm.ps1 test killswitch_live -Features dev-safety -Env @{ TH_DEV_SAFETY_SECONDS = '3' }
//! ```
//!
//! PowerShell Direct reaches the VM through Hyper-V, not its network, so a stuck block can
//! always be lifted: `scripts\vm.ps1 run 'C:\th\th-service.exe disengage'`, or a restore.
#![cfg(windows)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use th_core::driver::{InterfaceId, ResolvedEndpoint, ResolvedProfile, VpnDriver};
use th_core::manifest::Protocol;
use th_core::rotation::KillSwitch;
use th_core::sanitize::Transport;
use th_service::drivers::wireguard::WireguardDriver;
use th_service::killswitch::WfpKillSwitch;
use th_service::runtimes;

const PROBE: &str = "198.18.77.2:8080";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Refuse to run anywhere but the test VM: on the PC this would cut off its network.
fn in_test_vm() {
    assert_eq!(
        std::env::var("TH_TEST_VM").as_deref(),
        Ok("1"),
        "killswitch_live runs only in th-test, through scripts\\vm.ps1"
    );
}

/// Disengages when dropped, so a failing assertion never leaves the VM blocked.
struct Lift;

impl Drop for Lift {
    fn drop(&mut self) {
        if let Err(e) = WfpKillSwitch::disengage_now() {
            eprintln!("could not disengage: {e}");
        }
    }
}

fn start() -> Lift {
    in_test_vm();
    WfpKillSwitch::disengage_now().expect("start disengaged");
    Lift
}

fn addr(text: &str) -> SocketAddr {
    text.to_socket_addrs().unwrap().next().unwrap()
}

fn reachable(target: SocketAddr) -> bool {
    TcpStream::connect_timeout(&target, CONNECT_TIMEOUT).is_ok()
}

/// The probe's answer, or `None` if it cannot be reached.
fn probe() -> Option<String> {
    let mut stream = TcpStream::connect_timeout(&addr(PROBE), CONNECT_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(CONNECT_TIMEOUT)).ok()?;
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: probe\r\n\r\n")
        .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
}

#[test]
#[ignore = "blocks the machine's traffic: th-test VM only"]
fn engage_permits_only_the_endpoints_and_disengage_restores_everything() {
    let _lift = start();
    // Resolved while DNS still works.
    let internet = addr("example.com:443");
    let dns = addr("8.8.8.8:53");
    assert!(
        reachable(internet),
        "the VM reaches the internet before engaging"
    );
    let no_tunnel = probe().expect("the probe answers before engaging");

    let endpoint = ResolvedEndpoint {
        addr: addr(PROBE),
        transport: Transport::Tcp,
    };
    WfpKillSwitch::engage_now(&[endpoint]).expect("engage");
    assert!(WfpKillSwitch::is_engaged().unwrap());
    assert_eq!(
        probe(),
        Some(no_tunnel),
        "a permitted endpoint stays reachable"
    );
    assert!(!reachable(internet), "the internet is blocked");
    assert!(!reachable(dns), "DNS is blocked");
    assert!(!reachable(addr("192.168.77.1:8080")), "the LAN is blocked");

    // Engaging again replaces the endpoint set: the probe is no longer permitted.
    WfpKillSwitch::engage_now(&[]).expect("engage with no endpoints");
    assert_eq!(probe(), None, "a dropped endpoint is blocked");

    WfpKillSwitch::disengage_now().expect("disengage");
    assert!(!WfpKillSwitch::is_engaged().unwrap());
    assert_eq!(
        WfpKillSwitch::filter_count().unwrap(),
        0,
        "nothing of ours is left"
    );
    assert!(reachable(internet), "the internet is back");
}

#[tokio::test]
#[ignore = "blocks the machine's traffic and needs the test servers: th-test VM only"]
async fn a_tunnel_interface_carries_traffic_only_while_allowed() {
    let _lift = start();
    let config = std::env::var_os("TH_WG_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| r"C:\th\configs\wg-a.conf".into());
    let text = std::fs::read_to_string(&config).unwrap();
    let sanitized = th_core::sanitize::wireguard::sanitize("wg-a.conf", &text).unwrap();
    let endpoints: Vec<ResolvedEndpoint> = sanitized
        .endpoints
        .iter()
        .map(|e| ResolvedEndpoint {
            addr: addr(&format!("{}:{}", e.host, e.port)),
            transport: e.transport,
        })
        .collect();
    let bundle = th_core::bundle::BundleId::parse("0123456789abcdef").unwrap();
    let profile = ResolvedProfile {
        tunnel_name: th_core::tunnel_name::tunnel_name(&bundle, "live-ks"),
        bundle,
        profile_id: "live-ks".into(),
        protocol: Protocol::Wireguard,
        config_path: config,
        endpoints: endpoints.clone(),
    };
    let runtime = runtimes::discover()
        .into_iter()
        .find(|r| r.protocol == Protocol::Wireguard)
        .unwrap();
    let driver = WireguardDriver::new(&runtime, r"C:\th\tunnels").unwrap();
    let killswitch = WfpKillSwitch::new();

    // The order a rotation uses: engage with the endpoint, connect, then allow the interface.
    killswitch.engage(&endpoints).await.expect("engage");
    let handle = driver
        .connect(&profile)
        .await
        .expect("WireGuard connects through the block");
    assert_eq!(
        probe(),
        None,
        "the tunnel is blocked until its interface is allowed"
    );

    killswitch
        .allow_interface(handle.interface)
        .await
        .expect("allow");
    let through = probe().expect("the probe answers through the allowed tunnel");
    assert_eq!(through, "172.31.77.11", "the answer came through wg-a");

    killswitch
        .revoke_interface(handle.interface)
        .await
        .expect("revoke");
    assert_eq!(probe(), None, "a revoked interface is blocked again");

    // Revoking one interface leaves others; an unknown one is harmless.
    killswitch
        .revoke_interface(InterfaceId(0xdead))
        .await
        .expect("revoke unknown");

    driver.disconnect(&handle).await.expect("disconnect");
    killswitch.disengage().await.expect("disengage");
    assert!(
        probe().is_some(),
        "the probe answers again without a tunnel"
    );
}

/// Run by `filters_outlive_a_crashed_process` in a child process, never on its own.
#[test]
#[ignore = "helper for filters_outlive_a_crashed_process"]
fn helper_engage_then_abort() {
    if std::env::var("TH_KS_HELPER").as_deref() != Ok("1") {
        return;
    }
    in_test_vm();
    WfpKillSwitch::engage_now(&[]).expect("engage");
    std::process::abort();
}

#[test]
#[ignore = "blocks the machine's traffic: th-test VM only"]
fn filters_outlive_a_crashed_process() {
    let _lift = start();
    let internet = addr("example.com:443");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "helper_engage_then_abort",
            "--exact",
            "--ignored",
            "--test-threads=1",
        ])
        .env("TH_KS_HELPER", "1")
        .env("TH_TEST_VM", "1")
        .status()
        .unwrap();
    assert!(!status.success(), "the helper aborted");
    assert!(
        WfpKillSwitch::is_engaged().unwrap(),
        "the block survived the crash"
    );
    assert!(!reachable(internet), "and still blocks");
}

#[cfg(feature = "dev-safety")]
#[tokio::test]
#[ignore = "blocks the machine's traffic: th-test VM only, dev-safety build"]
async fn the_dev_safety_timer_lifts_an_unconfirmed_engage() {
    let _lift = start();
    let seconds: u64 = std::env::var("TH_DEV_SAFETY_SECONDS")
        .expect("set TH_DEV_SAFETY_SECONDS, such as 3")
        .parse()
        .unwrap();
    let wait = Duration::from_secs(seconds + 3);
    let killswitch = WfpKillSwitch::new();

    killswitch.engage(&[]).await.unwrap();
    tokio::time::sleep(wait).await;
    assert!(
        !WfpKillSwitch::is_engaged().unwrap(),
        "an unconfirmed engage was lifted"
    );

    killswitch.engage(&[]).await.unwrap();
    killswitch.confirm();
    tokio::time::sleep(wait).await;
    assert!(
        WfpKillSwitch::is_engaged().unwrap(),
        "a confirmed engage stays"
    );
}
