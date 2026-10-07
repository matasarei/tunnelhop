//! The orchestrator with th-core's test doubles: a mock WireGuard driver, a fake kill-switch and
//! a fake probe. Nothing here touches the machine's network.

use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use th_core::driver::{TunnelEvent, VpnDriver};
use th_core::ipc::{self, Event, Request, Response, Secret, StatusReport};
use th_core::manifest::Protocol;
use th_core::rotation::State;
use th_core::testing::{CallLog, FakeKillSwitch, FakeProbe, MockDriver};
use th_core::tunnel_name::tunnel_name;
use th_service::credentials::{CredentialStore, Vault};
use th_service::orchestrator::{Orchestrator, Parts};
use th_service::store::{Layout, Settings};
use zeroize::Zeroizing;
use zip::write::SimpleFileOptions;

struct XorVault;

impl Vault for XorVault {
    fn seal(&self, plain: &[u8]) -> std::io::Result<Vec<u8>> {
        Ok(plain.iter().map(|b| b ^ 0x5A).collect())
    }

    fn open(&self, sealed: &[u8]) -> std::io::Result<Zeroizing<Vec<u8>>> {
        Ok(Zeroizing::new(sealed.iter().map(|b| b ^ 0x5A).collect()))
    }
}

const MANIFEST: &str = r#"{
  "schema_version": "1",
  "bundle_name": "Lab",
  "rotation": { "interval_minutes": 15 },
  "profiles": [
    { "id": "wg-a", "name": "WG A", "protocol": "wireguard", "file": "wireguard/a.conf" },
    { "id": "wg-b", "name": "WG B", "protocol": "wireguard", "file": "wireguard/b.conf" },
    { "id": "ovpn", "name": "OpenVPN", "protocol": "openvpn", "file": "openvpn/c.ovpn" }
  ]
}"#;

fn wireguard(endpoint: &str) -> String {
    format!(
        "[Interface]\nPrivateKey = cGxhY2Vob2xkZXJwbGFjZWhvbGRlcnBsYWNlaG9sZGU=\n\
         Address = 10.0.0.2/32\n\n[Peer]\n\
         PublicKey = cGxhY2Vob2xkZXJwbGFjZWhvbGRlcnBsYWNlaG9sZGU=\n\
         AllowedIPs = 0.0.0.0/0\nEndpoint = {endpoint}\n"
    )
}

/// An OpenVPN profile that asks for credentials the bundle does not carry, as Proton's do.
const OPENVPN: &str = "client\ndev tun\nproto udp\nremote 198.51.100.8 1194\nauth-user-pass\n\
                       <ca>\n-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n</ca>\n";

fn bundle_zip(manifest: &str) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    for (name, content) in [
        ("manifest.json", manifest.to_string()),
        ("wireguard/a.conf", wireguard("198.51.100.7:51820")),
        ("wireguard/b.conf", wireguard("198.51.100.9:51821")),
        ("openvpn/c.ovpn", OPENVPN.to_string()),
    ] {
        writer.start_file(name, options).unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

struct Lab {
    orchestrator: Arc<Orchestrator<XorVault>>,
    log: CallLog,
    layout: Layout,
    _dir: tempfile::TempDir,
}

async fn lab_in(dir: tempfile::TempDir, killswitch_engaged: bool) -> Lab {
    let layout = Layout::new(dir.path());
    std::fs::create_dir_all(layout.bundles()).unwrap();
    std::fs::create_dir_all(layout.credentials()).unwrap();
    let log = CallLog::default();
    let driver = Arc::new(MockDriver::new(log.clone()));
    let drivers: HashMap<Protocol, Arc<dyn VpnDriver>> =
        HashMap::from([(Protocol::Wireguard, driver as Arc<dyn VpnDriver>)]);
    let orchestrator = Orchestrator::start(Parts {
        layout: layout.clone(),
        settings: Settings::load(&layout.settings_file()).unwrap(),
        credentials: CredentialStore::new(layout.credentials(), XorVault),
        runtimes: Vec::new(),
        drivers,
        killswitch: Arc::new(FakeKillSwitch::new(log.clone())),
        killswitch_engaged,
        probe: Arc::new(FakeProbe::new(log.clone(), Ipv4Addr::new(203, 0, 113, 1))),
        seed: Some(7),
    })
    .await;
    Lab {
        orchestrator,
        log,
        layout,
        _dir: dir,
    }
}

async fn lab() -> Lab {
    lab_in(tempfile::tempdir().unwrap(), false).await
}

fn report(response: Response) -> StatusReport {
    match response {
        Response::Status { report } => report,
        other => panic!("expected a status, got {other:?}"),
    }
}

fn is_error(response: &Response, containing: &str) -> bool {
    matches!(response, Response::Error { message } if message.contains(containing))
}

/// Import the lab bundle and select it; returns its id.
async fn imported_and_selected(lab: &Lab) -> String {
    let response = lab
        .orchestrator
        .handle(Request::ImportBundle {
            zip_base64: ipc::encode_zip(&bundle_zip(MANIFEST)),
        })
        .await;
    let Response::Imported { bundle, .. } = response else {
        panic!("import failed: {response:?}");
    };
    let status = report(
        lab.orchestrator
            .handle(Request::SelectBundle {
                bundle_id: bundle.bundle_id.clone(),
            })
            .await,
    );
    assert_eq!(status.bundle.as_ref().unwrap().bundle_id, bundle.bundle_id);
    bundle.bundle_id
}

#[tokio::test]
async fn nothing_connects_before_a_bundle_is_selected() {
    let lab = lab().await;
    let status = report(lab.orchestrator.handle(Request::GetStatus).await);
    assert_eq!(status.state, State::Disconnected);
    assert!(status.bundle.is_none());
    for request in [Request::RotateNow, Request::Connect { profile_id: None }] {
        let response = lab.orchestrator.handle(request).await;
        assert!(is_error(&response, "select a bundle"), "{response:?}");
    }
    assert!(lab.log.take().is_empty(), "nothing was touched");
}

#[tokio::test]
async fn an_import_reports_the_profiles_still_waiting_for_credentials() {
    let lab = lab().await;
    let response = lab
        .orchestrator
        .handle(Request::ImportBundle {
            zip_base64: ipc::encode_zip(&bundle_zip(MANIFEST)),
        })
        .await;
    let Response::Imported {
        bundle,
        already_present,
        needs_credentials,
    } = response
    else {
        panic!("{response:?}");
    };
    assert!(!already_present);
    assert_eq!(needs_credentials, ["ovpn"]);
    let flags: Vec<_> = bundle
        .profiles
        .iter()
        .map(|p| (p.id.as_str(), p.has_credentials))
        .collect();
    assert_eq!(flags, [("wg-a", true), ("wg-b", true), ("ovpn", false)]);
}

#[tokio::test]
async fn a_hostile_bundle_is_refused_with_its_errors() {
    let lab = lab().await;
    let hostile = MANIFEST.replace(r#""file": "wireguard/a.conf""#, r#""file": "../a.conf""#);
    let response = lab
        .orchestrator
        .handle(Request::ImportBundle {
            zip_base64: ipc::encode_zip(&bundle_zip(&hostile)),
        })
        .await;
    let Response::ImportFailed { errors } = response else {
        panic!("{response:?}");
    };
    assert!(!errors.is_empty());
    assert!(
        std::fs::read_dir(lab.layout.bundles())
            .unwrap()
            .next()
            .is_none()
    );
}

#[tokio::test]
async fn connect_rotate_and_disconnect_go_through_the_engine() {
    let lab = lab().await;
    imported_and_selected(&lab).await;
    lab.log.take();

    let connected = report(
        lab.orchestrator
            .handle(Request::Connect {
                profile_id: Some("wg-a".into()),
            })
            .await,
    );
    assert_eq!(connected.state, State::Connected);
    assert_eq!(connected.profile_id.as_deref(), Some("wg-a"));
    assert!(connected.killswitch_engaged);
    let log = lab.log.take();
    assert!(log.contains(&"connect:wg-a".to_string()), "{log:?}");
    assert!(log.iter().any(|e| e.starts_with("engage")), "{log:?}");

    let rotated = report(lab.orchestrator.handle(Request::RotateNow).await);
    assert_eq!(rotated.state, State::Connected);
    assert_eq!(
        rotated.profile_id.as_deref(),
        Some("wg-b"),
        "the only other usable profile"
    );
    assert_ne!(rotated.public_ip, connected.public_ip);

    let disconnected = report(lab.orchestrator.handle(Request::Disconnect).await);
    assert_eq!(disconnected.state, State::Disconnected);
    assert!(!disconnected.killswitch_engaged);
    let log = lab.log.take();
    assert!(log.contains(&"disconnect:wg-b".to_string()), "{log:?}");
    assert!(log.contains(&"disengage".to_string()), "{log:?}");
}

#[tokio::test]
async fn a_profile_waiting_for_credentials_is_refused_until_they_are_set() {
    let lab = lab().await;
    let bundle_id = imported_and_selected(&lab).await;
    let response = lab
        .orchestrator
        .handle(Request::Connect {
            profile_id: Some("ovpn".into()),
        })
        .await;
    assert!(is_error(&response, "needs credentials"), "{response:?}");

    let status = report(
        lab.orchestrator
            .handle(Request::SetCredentials {
                bundle_id,
                profile_ids: Vec::new(),
                username: "user".into(),
                password: Secret::new("p\"w\\"),
            })
            .await,
    );
    let ovpn = &status.bundle.unwrap().profiles[2];
    assert_eq!((ovpn.id.as_str(), ovpn.has_credentials), ("ovpn", true));
    // Now refused for another reason only: no OpenVPN driver yet.
    let response = lab
        .orchestrator
        .handle(Request::Connect {
            profile_id: Some("ovpn".into()),
        })
        .await;
    assert!(
        is_error(&response, "runtime is unavailable"),
        "{response:?}"
    );
}

#[tokio::test]
async fn the_users_choices_and_the_bundle_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let first = lab_in(dir, false).await;
    let bundle_id = imported_and_selected(&first).await;
    let status = report(
        first
            .orchestrator
            .handle(Request::SetInterval { minutes: 60 })
            .await,
    );
    assert_eq!(status.interval_minutes, 60);
    report(
        first
            .orchestrator
            .handle(Request::SetAutoRotate { enabled: false })
            .await,
    );
    let Lab { _dir: dir, .. } = first;
    assert_eq!(dir.path(), path);

    let second = lab_in(dir, false).await;
    let status = second.orchestrator.report();
    assert_eq!(status.bundle.unwrap().bundle_id, bundle_id);
    assert_eq!(status.interval_minutes, 60, "beats the manifest's 15");
    assert!(!status.auto_rotate);
}

#[tokio::test]
async fn a_block_left_by_a_crash_is_reported_and_lifted_by_disconnect() {
    let lab = lab_in(tempfile::tempdir().unwrap(), true).await;
    let status = lab.orchestrator.report();
    assert_eq!(status.state, State::Blocked);
    assert!(status.killswitch_engaged);

    let status = report(lab.orchestrator.handle(Request::Disconnect).await);
    assert_eq!(status.state, State::Disconnected);
    assert!(!status.killswitch_engaged);
    assert_eq!(lab.log.take(), ["disengage"]);
}

#[tokio::test]
async fn the_timer_rotates_once_the_interval_has_passed() {
    let lab = lab().await;
    imported_and_selected(&lab).await;
    report(
        lab.orchestrator
            .handle(Request::SetInterval { minutes: 1 })
            .await,
    );
    report(
        lab.orchestrator
            .handle(Request::Connect {
                profile_id: Some("wg-a".into()),
            })
            .await,
    );
    lab.log.take();

    lab.orchestrator.tick(Instant::now()).await;
    assert!(lab.log.take().is_empty(), "not due yet");

    lab.orchestrator
        .tick(Instant::now() + Duration::from_secs(61))
        .await;
    assert!(lab.log.take().contains(&"connect:wg-b".to_string()));
    assert_eq!(
        lab.orchestrator.report().profile_id.as_deref(),
        Some("wg-b")
    );
}

#[tokio::test]
async fn a_lost_tunnel_is_replaced() {
    let lab = lab().await;
    let bundle_id = imported_and_selected(&lab).await;
    report(
        lab.orchestrator
            .handle(Request::Connect {
                profile_id: Some("wg-a".into()),
            })
            .await,
    );
    lab.log.take();
    let id = th_core::bundle::BundleId::parse(&bundle_id).unwrap();
    lab.orchestrator
        .on_tunnel_event(
            &TunnelEvent::Down {
                tunnel: tunnel_name(&id, "wg-a"),
                reason: "test".into(),
            },
            Instant::now(),
        )
        .await;
    let log = lab.log.take();
    assert!(log.iter().any(|e| e.starts_with("connect:")), "{log:?}");
    assert_eq!(lab.orchestrator.report().state, State::Connected);
}

#[tokio::test]
async fn every_change_reaches_the_pipes_clients() {
    let lab = lab().await;
    let mut events = lab.orchestrator.events().subscribe();
    imported_and_selected(&lab).await;
    report(
        lab.orchestrator
            .handle(Request::Connect {
                profile_id: Some("wg-a".into()),
            })
            .await,
    );
    let mut last = None;
    while let Ok(event) = events.try_recv() {
        last = Some(event);
    }
    match last {
        Some(Event::Status { report }) => {
            assert_eq!(report.state, State::Connected);
            assert_eq!(report.profile_id.as_deref(), Some("wg-a"));
        }
        other => panic!("expected a status event, got {other:?}"),
    }
}
