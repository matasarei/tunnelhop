//! The orchestrator with th-core's test doubles: a mock WireGuard driver, a fake kill-switch and
//! a fake probe. Nothing here touches the machine's network.

use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use th_core::driver::{TunnelEvent, VpnDriver};
use th_core::ipc::{
    self, Availability, Event, ProfileFile, Request, Response, RuntimeInfo, Secret, StatusReport,
};
use th_core::manifest::Protocol;
use th_core::rotation::State;
use th_core::testing::{CallLog, FakeKillSwitch, FakeProbe, MockDriver};
use th_core::tunnel_name::tunnel_name;
use th_service::credentials::{CredentialStore, Vault};
use th_service::orchestrator::{Discover, Orchestrator, Parts};
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
    let log = CallLog::default();
    let driver = Arc::new(MockDriver::new(log.clone()));
    lab_with(dir, killswitch_engaged, log, driver).await
}

async fn lab_with(
    dir: tempfile::TempDir,
    killswitch_engaged: bool,
    log: CallLog,
    driver: Arc<dyn VpnDriver>,
) -> Lab {
    let drivers: HashMap<Protocol, Arc<dyn VpnDriver>> =
        HashMap::from([(Protocol::Wireguard, driver)]);
    lab_from(dir, killswitch_engaged, log, Vec::new(), drivers, None).await
}

/// A lab with exactly these runtimes and drivers, and this way of looking for runtimes again.
async fn lab_from(
    dir: tempfile::TempDir,
    killswitch_engaged: bool,
    log: CallLog,
    runtimes: Vec<RuntimeInfo>,
    drivers: HashMap<Protocol, Arc<dyn VpnDriver>>,
    discover: Option<Discover>,
) -> Lab {
    let layout = Layout::new(dir.path());
    std::fs::create_dir_all(layout.bundles()).unwrap();
    std::fs::create_dir_all(layout.credentials()).unwrap();
    let orchestrator = Orchestrator::start(Parts {
        layout: layout.clone(),
        settings: Settings::load(&layout.settings_file()).unwrap(),
        credentials: CredentialStore::new(layout.credentials(), XorVault),
        runtimes,
        drivers,
        discover,
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

/// A driver whose connect waits until the test opens the gate: a rotation held in progress.
struct GatedDriver {
    inner: MockDriver,
    gate: tokio::sync::Semaphore,
}

#[async_trait::async_trait]
impl VpnDriver for GatedDriver {
    async fn connect(
        &self,
        profile: &th_core::driver::ResolvedProfile,
    ) -> Result<th_core::driver::TunnelHandle, th_core::driver::DriverError> {
        let _pass = self.gate.acquire().await.unwrap();
        self.inner.connect(profile).await
    }

    async fn disconnect(
        &self,
        handle: &th_core::driver::TunnelHandle,
    ) -> Result<(), th_core::driver::DriverError> {
        self.inner.disconnect(handle).await
    }

    async fn status(
        &self,
        handle: &th_core::driver::TunnelHandle,
    ) -> Result<th_core::driver::TunnelStatus, th_core::driver::DriverError> {
        self.inner.status(handle).await
    }

    fn events(&self) -> tokio::sync::broadcast::Receiver<TunnelEvent> {
        self.inner.events()
    }

    async fn reconcile(
        &self,
    ) -> Result<Vec<th_core::driver::TunnelHandle>, th_core::driver::DriverError> {
        self.inner.reconcile().await
    }
}

#[tokio::test]
async fn status_answers_while_a_rotation_is_in_progress() {
    let log = CallLog::default();
    let driver = Arc::new(GatedDriver {
        inner: MockDriver::new(log.clone()),
        gate: tokio::sync::Semaphore::new(0),
    });
    let lab = lab_with(tempfile::tempdir().unwrap(), false, log, driver.clone()).await;
    imported_and_selected(&lab).await;

    let orchestrator = lab.orchestrator.clone();
    let connecting = tokio::spawn(async move {
        orchestrator
            .handle(Request::Connect {
                profile_id: Some("wg-a".into()),
            })
            .await
    });
    // Let the connect reach the driver and wait there.
    while !lab.log.take().iter().any(|e| e.starts_with("engage")) {
        tokio::task::yield_now().await;
    }

    let status = tokio::time::timeout(
        Duration::from_secs(1),
        lab.orchestrator.handle(Request::GetStatus),
    )
    .await
    .expect("GetStatus answered while the connect was held");
    assert!(matches!(status, Response::Status { .. }));

    driver.gate.add_permits(1);
    let connected = report(connecting.await.unwrap());
    assert_eq!(connected.state, State::Connected);
}

#[tokio::test]
async fn selecting_another_bundle_first_leaves_the_current_one() {
    let lab = lab().await;
    imported_and_selected(&lab).await;
    report(
        lab.orchestrator
            .handle(Request::Connect {
                profile_id: Some("wg-a".into()),
            })
            .await,
    );
    let other = MANIFEST.replace(r#""bundle_name": "Lab""#, r#""bundle_name": "Other lab""#);
    let Response::Imported { bundle: second, .. } = lab
        .orchestrator
        .handle(Request::ImportBundle {
            zip_base64: ipc::encode_zip(&bundle_zip(&other)),
        })
        .await
    else {
        panic!("the second import failed");
    };
    lab.log.take();

    let status = report(
        lab.orchestrator
            .handle(Request::SelectBundle {
                bundle_id: second.bundle_id.clone(),
            })
            .await,
    );
    let log = lab.log.take();
    assert!(log.contains(&"disconnect:wg-a".to_string()), "{log:?}");
    assert!(log.contains(&"disengage".to_string()), "{log:?}");
    assert_eq!(status.state, State::Disconnected);
    assert!(!status.killswitch_engaged);
    assert_eq!(status.bundle.unwrap().bundle_name, "Other lab");
    let saved = Settings::load(&lab.layout.settings_file()).unwrap();
    assert_eq!(
        saved.active_bundle.as_deref(),
        Some(second.bundle_id.as_str())
    );
    assert_eq!(
        saved.selected_profile, None,
        "a profile of the old bundle is forgotten"
    );
}

/// Import the lab bundle under another name, which makes it another bundle; returns its id.
async fn imported_as(lab: &Lab, name: &str) -> String {
    let manifest = MANIFEST.replace(
        r#""bundle_name": "Lab""#,
        &format!(r#""bundle_name": "{name}""#),
    );
    let response = lab
        .orchestrator
        .handle(Request::ImportBundle {
            zip_base64: ipc::encode_zip(&bundle_zip(&manifest)),
        })
        .await;
    match response {
        Response::Imported { bundle, .. } => bundle.bundle_id,
        other => panic!("importing {name:?} failed: {other:?}"),
    }
}

/// A `Bundles` answer as (name, id) pairs in the order given, and the active id.
fn listed(response: Response) -> (Vec<(String, String)>, Option<String>) {
    match response {
        Response::Bundles { bundles, active } => (
            bundles
                .into_iter()
                .map(|b| (b.bundle_name, b.bundle_id))
                .collect(),
            active,
        ),
        other => panic!("expected the bundles, got {other:?}"),
    }
}

async fn delete(lab: &Lab, bundle_id: &str) -> Response {
    lab.orchestrator
        .handle(Request::DeleteBundle {
            bundle_id: bundle_id.to_string(),
        })
        .await
}

#[tokio::test]
async fn installed_bundles_are_listed_by_name_with_the_active_one_marked() {
    let lab = lab().await;
    assert_eq!(
        listed(lab.orchestrator.handle(Request::ListBundles).await),
        (Vec::new(), None)
    );

    let zeta = imported_as(&lab, "Zeta").await;
    let alpha = imported_as(&lab, "alpha").await;
    report(
        lab.orchestrator
            .handle(Request::SelectBundle {
                bundle_id: zeta.clone(),
            })
            .await,
    );

    let response = lab.orchestrator.handle(Request::ListBundles).await;
    let Response::Bundles { bundles, .. } = &response else {
        panic!("{response:?}");
    };
    let flags: Vec<_> = bundles[0]
        .profiles
        .iter()
        .map(|p| (p.id.as_str(), p.has_credentials))
        .collect();
    assert_eq!(flags, [("wg-a", true), ("wg-b", true), ("ovpn", false)]);
    assert_eq!(
        listed(response),
        (
            vec![("alpha".into(), alpha), ("Zeta".into(), zeta.clone())],
            Some(zeta)
        )
    );
}

#[tokio::test]
async fn deleting_another_bundle_leaves_the_tunnel_alone() {
    let lab = lab().await;
    let active = imported_and_selected(&lab).await;
    report(
        lab.orchestrator
            .handle(Request::Connect {
                profile_id: Some("wg-a".into()),
            })
            .await,
    );
    let other = imported_as(&lab, "Other lab").await;
    report(
        lab.orchestrator
            .handle(Request::SetCredentials {
                bundle_id: other.clone(),
                profile_ids: Vec::new(),
                username: "user".into(),
                password: Secret::new("secret"),
            })
            .await,
    );
    assert!(lab.layout.credentials().join(&other).is_dir());
    lab.log.take();

    let (bundles, still_active) = listed(delete(&lab, &other).await);
    assert_eq!(bundles, [("Lab".to_string(), active.clone())]);
    assert_eq!(still_active, Some(active));
    assert!(lab.log.take().is_empty(), "the tunnel was not touched");
    assert!(!lab.layout.bundles().join(&other).exists());
    assert!(
        !lab.layout.credentials().join(&other).exists(),
        "its credentials go with it"
    );
    let status = report(lab.orchestrator.handle(Request::GetStatus).await);
    assert_eq!(status.state, State::Connected);

    let again = delete(&lab, &other).await;
    assert!(is_error(&again, "not installed"), "{again:?}");
}

#[tokio::test]
async fn deleting_the_active_bundle_disconnects_first_and_leaves_none_active() {
    let lab = lab().await;
    let active = imported_and_selected(&lab).await;
    report(
        lab.orchestrator
            .handle(Request::Connect {
                profile_id: Some("wg-a".into()),
            })
            .await,
    );
    lab.log.take();

    assert_eq!(listed(delete(&lab, &active).await), (Vec::new(), None));
    let log = lab.log.take();
    assert!(log.contains(&"disconnect:wg-a".to_string()), "{log:?}");
    assert!(log.contains(&"disengage".to_string()), "{log:?}");
    let status = report(lab.orchestrator.handle(Request::GetStatus).await);
    assert_eq!(status.state, State::Disconnected);
    assert!(!status.killswitch_engaged);
    assert!(status.bundle.is_none());
    let saved = Settings::load(&lab.layout.settings_file()).unwrap();
    assert_eq!(saved.active_bundle, None);
    assert_eq!(saved.selected_profile, None);
    let response = lab.orchestrator.handle(Request::RotateNow).await;
    assert!(is_error(&response, "select a bundle"), "{response:?}");

    // A restart finds nothing to reopen.
    let lab = lab_in(lab._dir, false).await;
    let status = report(lab.orchestrator.handle(Request::GetStatus).await);
    assert!(status.bundle.is_none());
}

#[tokio::test]
async fn a_bundle_whose_tunnel_cannot_be_brought_down_is_not_deleted() {
    let log = CallLog::default();
    let driver = Arc::new(MockDriver::new(log.clone()));
    let lab = lab_with(tempfile::tempdir().unwrap(), false, log, driver.clone()).await;
    let active = imported_and_selected(&lab).await;
    report(
        lab.orchestrator
            .handle(Request::Connect {
                profile_id: Some("wg-a".into()),
            })
            .await,
    );
    driver.fail_disconnect(true);

    let response = delete(&lab, &active).await;
    assert!(is_error(&response, "was not deleted"), "{response:?}");
    assert!(lab.layout.bundles().join(&active).is_dir());
    let (bundles, still_active) = listed(lab.orchestrator.handle(Request::ListBundles).await);
    assert_eq!(bundles, [("Lab".to_string(), active.clone())]);
    assert_eq!(still_active, Some(active));
}

#[tokio::test]
async fn a_21st_bundle_is_refused_but_an_installed_one_imports_again() {
    let lab = lab().await;
    let mut ids = Vec::new();
    for n in 0..th_core::bundle::MAX_BUNDLES {
        ids.push(imported_as(&lab, &format!("Lab {n}")).await);
    }
    let one_more = MANIFEST.replace(r#""bundle_name": "Lab""#, r#""bundle_name": "One more""#);
    let import = || async {
        lab.orchestrator
            .handle(Request::ImportBundle {
                zip_base64: ipc::encode_zip(&bundle_zip(&one_more)),
            })
            .await
    };

    let response = import().await;
    assert!(is_error(&response, "delete a bundle first"), "{response:?}");
    assert_eq!(
        std::fs::read_dir(lab.layout.bundles()).unwrap().count(),
        th_core::bundle::MAX_BUNDLES,
        "nothing of the refused bundle was written"
    );
    assert_eq!(
        imported_as(&lab, "Lab 0").await,
        ids[0],
        "already installed"
    );

    listed(delete(&lab, &ids[7]).await);
    let response = import().await;
    assert!(
        matches!(response, Response::Imported { .. }),
        "{response:?}"
    );
}

#[tokio::test]
async fn a_damaged_bundle_is_listed_so_that_it_can_be_deleted() {
    let lab = lab().await;
    let id = imported_as(&lab, "Lab").await;
    std::fs::remove_file(lab.layout.bundles().join(&id).join("manifest.json")).unwrap();

    let (bundles, _) = listed(lab.orchestrator.handle(Request::ListBundles).await);
    assert_eq!(bundles, [("Damaged bundle".to_string(), id.clone())]);
    assert_eq!(listed(delete(&lab, &id).await), (Vec::new(), None));
}

/// Add one config file to a bundle.
async fn add(lab: &Lab, bundle_id: &str, file_name: &str, content: &str) -> Response {
    lab.orchestrator
        .handle(Request::AddProfiles {
            bundle_id: bundle_id.to_string(),
            files: vec![ProfileFile::new("", file_name, content.as_bytes())],
        })
        .await
}

async fn remove(lab: &Lab, bundle_id: &str, profile_ids: &[&str]) -> Response {
    lab.orchestrator
        .handle(Request::RemoveProfiles {
            bundle_id: bundle_id.to_string(),
            profile_ids: profile_ids.iter().map(|id| id.to_string()).collect(),
        })
        .await
}

/// An `Imported` answer as the new bundle's id, its profile ids, and who still needs sign-in.
fn edited(response: Response) -> (String, Vec<String>, Vec<String>) {
    match response {
        Response::Imported {
            bundle,
            needs_credentials,
            ..
        } => (
            bundle.bundle_id,
            bundle.profiles.into_iter().map(|p| p.id).collect(),
            needs_credentials,
        ),
        other => panic!("expected an edited bundle, got {other:?}"),
    }
}

async fn connect(lab: &Lab, profile: &str) -> StatusReport {
    report(
        lab.orchestrator
            .handle(Request::Connect {
                profile_id: Some(profile.into()),
            })
            .await,
    )
}

#[tokio::test]
async fn an_edit_replaces_the_bundle_and_carries_its_credentials_over() {
    let lab = lab().await;
    let old = imported_as(&lab, "Lab").await;
    report(
        lab.orchestrator
            .handle(Request::SetCredentials {
                bundle_id: old.clone(),
                profile_ids: Vec::new(),
                username: "user".into(),
                password: Secret::new("secret"),
            })
            .await,
    );

    let (new, profiles, needs) =
        edited(add(&lab, &old, "NL 1.conf", &wireguard("198.51.100.20:51820")).await);

    assert_ne!(new, old);
    assert_eq!(profiles, ["wg-a", "wg-b", "ovpn", "nl-1"]);
    assert!(needs.is_empty(), "the saved sign-in came along: {needs:?}");
    let (bundles, active) = listed(lab.orchestrator.handle(Request::ListBundles).await);
    assert_eq!(bundles, [("Lab".to_string(), new.clone())]);
    assert_eq!(active, None, "it was not the active bundle");
    assert!(!lab.layout.bundles().join(&old).exists());
    assert!(!lab.layout.credentials().join(&old).exists());
    assert!(lab.layout.credentials().join(&new).is_dir());

    let (newer, profiles, _) = edited(remove(&lab, &new, &["wg-b", "ovpn"]).await);
    assert_eq!(profiles, ["wg-a", "nl-1"]);
    let (bundles, _) = listed(lab.orchestrator.handle(Request::ListBundles).await);
    assert_eq!(bundles, [("Lab".to_string(), newer)]);
}

#[tokio::test]
async fn an_added_profile_that_asks_for_sign_in_is_reported() {
    let lab = lab().await;
    let old = imported_as(&lab, "Lab").await;
    let (_, profiles, needs) = edited(add(&lab, &old, "se.ovpn", OPENVPN).await);
    assert_eq!(profiles, ["wg-a", "wg-b", "ovpn", "se"]);
    assert_eq!(needs, ["ovpn", "se"]);
}

#[tokio::test]
async fn an_edit_that_is_refused_changes_nothing() {
    let lab = lab().await;
    let id = imported_and_selected(&lab).await;
    let hostile =
        wireguard("198.51.100.20:51820").replace("[Interface]\n", "[Interface]\nPostUp = x\n");

    let response = add(&lab, &id, "evil.conf", &hostile).await;
    let Response::ImportFailed { errors } = &response else {
        panic!("{response:?}");
    };
    assert!(
        errors
            .iter()
            .any(|e| e.starts_with("wireguard/evil.conf:2") && e.contains("PostUp")),
        "{errors:?}"
    );
    let response = remove(&lab, &id, &["wg-a", "wg-b", "ovpn"]).await;
    assert!(
        matches!(&response, Response::ImportFailed { errors } if errors[0].contains("at least one")),
        "{response:?}"
    );
    let response = remove(&lab, &id, &["nope"]).await;
    assert!(
        matches!(&response, Response::ImportFailed { .. }),
        "{response:?}"
    );
    let response = remove(&lab, "0123456789abcdef", &["wg-a"]).await;
    assert!(is_error(&response, "not installed"), "{response:?}");

    let (bundles, active) = listed(lab.orchestrator.handle(Request::ListBundles).await);
    assert_eq!(bundles, [("Lab".to_string(), id.clone())]);
    assert_eq!(active, Some(id));
    assert!(lab.log.take().is_empty(), "nothing was touched");
}

#[tokio::test]
async fn editing_the_bundle_in_use_keeps_the_block_and_the_server() {
    let lab = lab().await;
    let old = imported_and_selected(&lab).await;
    connect(&lab, "wg-a").await;
    lab.log.take();

    let (new, _, _) = edited(add(&lab, &old, "nl.conf", &wireguard("198.51.100.20:51820")).await);

    let log = lab.log.take();
    assert!(!log.contains(&"disengage".to_string()), "{log:?}");
    let position = |entry: &str| log.iter().position(|e| e == entry);
    let block = log.iter().position(|e| e.starts_with("engage:"));
    assert!(block.is_some(), "{log:?}");
    assert!(block < position("disconnect:wg-a"), "{log:?}");
    assert!(
        position("disconnect:wg-a") < position("connect:wg-a"),
        "{log:?}"
    );
    let status = report(lab.orchestrator.handle(Request::GetStatus).await);
    assert_eq!(status.state, State::Connected);
    assert_eq!(status.profile_id.as_deref(), Some("wg-a"));
    assert!(status.killswitch_engaged);
    assert_eq!(status.bundle.unwrap().bundle_id, new);
    let saved = Settings::load(&lab.layout.settings_file()).unwrap();
    assert_eq!(saved.active_bundle.as_deref(), Some(new.as_str()));
    assert_eq!(saved.selected_profile.as_deref(), Some("wg-a"));
}

#[tokio::test]
async fn removing_the_server_in_use_moves_to_another_with_the_block_held() {
    let lab = lab().await;
    let old = imported_and_selected(&lab).await;
    connect(&lab, "wg-a").await;
    lab.log.take();

    let (new, profiles, _) = edited(remove(&lab, &old, &["wg-a"]).await);

    assert_eq!(profiles, ["wg-b", "ovpn"]);
    let log = lab.log.take();
    assert!(!log.contains(&"disengage".to_string()), "{log:?}");
    assert!(log.contains(&"disconnect:wg-a".to_string()), "{log:?}");
    assert!(log.contains(&"connect:wg-b".to_string()), "{log:?}");
    let status = report(lab.orchestrator.handle(Request::GetStatus).await);
    assert_eq!(status.state, State::Connected);
    assert_eq!(status.profile_id.as_deref(), Some("wg-b"));
    assert!(status.killswitch_engaged);
    assert_eq!(status.bundle.unwrap().bundle_id, new);
    let saved = Settings::load(&lab.layout.settings_file()).unwrap();
    assert_eq!(saved.selected_profile, None, "it named the removed server");
}

#[tokio::test]
async fn an_edit_tries_the_same_server_once_and_then_rotates() {
    let log = CallLog::default();
    let driver = Arc::new(MockDriver::new(log.clone()));
    let lab = lab_with(tempfile::tempdir().unwrap(), false, log, driver.clone()).await;
    let old = imported_and_selected(&lab).await;
    connect(&lab, "wg-a").await;
    driver.fail_connect("wg-a");
    lab.log.take();

    edited(add(&lab, &old, "nl.conf", &wireguard("198.51.100.20:51820")).await);

    let log = lab.log.take();
    assert!(!log.contains(&"disengage".to_string()), "{log:?}");
    let tries = log.iter().filter(|e| *e == "connect:wg-a").count();
    assert!(tries >= 1, "{log:?}");
    let first_other = log
        .iter()
        .position(|e| e == "connect:wg-b" || e == "connect:nl")
        .expect("it moved on");
    let second_try = log
        .iter()
        .enumerate()
        .filter(|(_, e)| *e == "connect:wg-a")
        .nth(1)
        .map(|(i, _)| i);
    assert!(
        second_try.is_none_or(|i| i > first_other),
        "another server was tried before wg-a a second time: {log:?}"
    );
    let status = report(lab.orchestrator.handle(Request::GetStatus).await);
    assert_eq!(status.state, State::Connected);
    assert_ne!(status.profile_id.as_deref(), Some("wg-a"));
    assert!(status.killswitch_engaged);
}

#[tokio::test]
async fn an_edit_that_cannot_reconnect_ends_blocked_not_open() {
    let log = CallLog::default();
    let driver = Arc::new(MockDriver::new(log.clone()));
    let lab = lab_with(tempfile::tempdir().unwrap(), false, log, driver.clone()).await;
    let old = imported_and_selected(&lab).await;
    connect(&lab, "wg-a").await;
    driver.fail_connect("wg-a");
    driver.fail_connect("wg-b");
    driver.fail_connect("nl");
    lab.log.take();

    let (new, _, _) = edited(add(&lab, &old, "nl.conf", &wireguard("198.51.100.20:51820")).await);

    let log = lab.log.take();
    assert!(!log.contains(&"disengage".to_string()), "{log:?}");
    let status = report(lab.orchestrator.handle(Request::GetStatus).await);
    assert_eq!(status.state, State::Blocked);
    assert!(status.killswitch_engaged);
    assert_eq!(
        status.bundle.unwrap().bundle_id,
        new,
        "the edit itself stands"
    );
}

#[tokio::test]
async fn editing_without_a_tunnel_starts_none() {
    let lab = lab().await;
    let old = imported_and_selected(&lab).await;
    lab.log.take();

    let (new, _, _) = edited(add(&lab, &old, "nl.conf", &wireguard("198.51.100.20:51820")).await);

    assert!(lab.log.take().is_empty(), "no tunnel, no block");
    let status = report(lab.orchestrator.handle(Request::GetStatus).await);
    assert_eq!(status.state, State::Disconnected);
    assert_eq!(status.bundle.unwrap().bundle_id, new);

    // And a bundle that is not the one in use leaves the tunnel of the one that is.
    connect(&lab, "wg-a").await;
    let other = imported_as(&lab, "Other lab").await;
    lab.log.take();
    edited(remove(&lab, &other, &["wg-b"]).await);
    assert!(lab.log.take().is_empty(), "the tunnel was not touched");
    let status = report(lab.orchestrator.handle(Request::GetStatus).await);
    assert_eq!(status.state, State::Connected);
    assert_eq!(status.bundle.unwrap().bundle_id, new);
}

async fn create(lab: &Lab, name: &str, files: &[(&str, &str)]) -> Response {
    lab.orchestrator
        .handle(Request::CreateBundle {
            name: name.into(),
            files: files
                .iter()
                .map(|(file_name, content)| ProfileFile::new("", file_name, content.as_bytes()))
                .collect(),
        })
        .await
}

/// A zip of bare config files, as a user makes one by zipping what a provider gave them.
fn config_zip(files: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in files {
        writer
            .start_file(*name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[tokio::test]
async fn loose_config_files_become_the_bundle_in_use() {
    let lab = lab().await;
    let ua = wireguard("198.51.100.30:51820");

    let response = create(
        &lab,
        "Proton UA",
        &[("wg-UA-83.conf", &ua), ("c.ovpn", OPENVPN)],
    )
    .await;

    let (id, profiles, needs) = edited(response);
    assert_eq!(profiles, ["wg-ua-83", "c"]);
    assert_eq!(needs, ["c"], "the OpenVPN file asks for sign-in");
    let status = report(lab.orchestrator.handle(Request::GetStatus).await);
    let bundle = status.bundle.expect("the new bundle is the one in use");
    assert_eq!(
        (bundle.bundle_id.as_str(), bundle.bundle_name.as_str()),
        (id.as_str(), "Proton UA")
    );
    assert_eq!(status.state, State::Disconnected, "made, not connected");
    assert!(lab.log.take().is_empty(), "no tunnel, no block");

    // It is a bundle like any other from here on.
    assert_eq!(connect(&lab, "wg-ua-83").await.state, State::Connected);

    // The same files and name again are the same bundle.
    let again = create(
        &lab,
        "Proton UA",
        &[("wg-UA-83.conf", &ua), ("c.ovpn", OPENVPN)],
    )
    .await;
    assert!(
        matches!(&again, Response::Imported { bundle, already_present: true, .. } if bundle.bundle_id == id),
        "{again:?}"
    );
}

#[tokio::test]
async fn a_new_bundle_does_not_take_over_from_the_one_in_use() {
    let lab = lab().await;
    let active = imported_and_selected(&lab).await;
    connect(&lab, "wg-a").await;
    lab.log.take();

    let (id, _, _) =
        edited(create(&lab, "", &[("x.conf", &wireguard("198.51.100.31:51820"))]).await);

    assert!(lab.log.take().is_empty(), "the tunnel was not touched");
    let status = report(lab.orchestrator.handle(Request::GetStatus).await);
    assert_eq!(status.state, State::Connected);
    assert_eq!(status.bundle.unwrap().bundle_id, active);
    let (bundles, in_use) = listed(lab.orchestrator.handle(Request::ListBundles).await);
    assert!(
        bundles.contains(&("My servers".to_string(), id)),
        "{bundles:?}"
    );
    assert_eq!(in_use, Some(active));
}

#[tokio::test]
async fn files_that_cannot_become_a_bundle_leave_nothing_behind() {
    let lab = lab().await;
    let hostile = wireguard("198.51.100.32:51820").replace("[Peer]", "PostUp = calc.exe\n[Peer]");

    let response = create(
        &lab,
        "Bad",
        &[
            ("ok.conf", &wireguard("198.51.100.33:51820")),
            ("evil.conf", &hostile),
        ],
    )
    .await;
    let Response::ImportFailed { errors } = &response else {
        panic!("expected a refusal, got {response:?}");
    };
    assert!(
        errors
            .iter()
            .any(|e| e.contains("evil.conf") && e.contains("PostUp")),
        "{errors:?}"
    );

    // Refused before a zip is built: something that is not a config at all.
    let response = lab
        .orchestrator
        .handle(Request::CreateBundle {
            name: "Notes".into(),
            files: vec![ProfileFile::new("", "notes.txt", b"hello")],
        })
        .await;
    assert!(
        matches!(response, Response::ImportFailed { .. }),
        "{response:?}"
    );

    assert_eq!(std::fs::read_dir(lab.layout.bundles()).unwrap().count(), 0);
    let status = report(lab.orchestrator.handle(Request::GetStatus).await);
    assert!(status.bundle.is_none());
}

#[tokio::test]
async fn a_new_bundle_counts_against_the_cap() {
    let lab = lab().await;
    for n in 0..th_core::bundle::MAX_BUNDLES {
        imported_as(&lab, &format!("Lab {n}")).await;
    }
    let response = create(
        &lab,
        "One more",
        &[("x.conf", &wireguard("198.51.100.34:51820"))],
    )
    .await;
    assert!(is_error(&response, "delete a bundle first"), "{response:?}");
    assert_eq!(
        std::fs::read_dir(lab.layout.bundles()).unwrap().count(),
        th_core::bundle::MAX_BUNDLES
    );
}

#[tokio::test]
async fn a_zip_of_bare_configs_imports_as_a_bundle() {
    let lab = lab().await;
    let a = wireguard("198.51.100.35:51820");
    let b = wireguard("198.51.100.36:51820");
    let import = |zip: Vec<u8>| {
        let zip_base64 = ipc::encode_zip(&zip);
        let orchestrator = &lab.orchestrator;
        async move {
            orchestrator
                .handle(Request::ImportBundle { zip_base64 })
                .await
        }
    };

    let response = import(config_zip(&[("wg-UA-83.conf", &a), ("wg-UA-21.conf", &b)])).await;
    let Response::Imported {
        bundle,
        already_present,
        ..
    } = response
    else {
        panic!("expected an import, got {response:?}");
    };
    assert_eq!(bundle.bundle_name, "My servers");
    let mut ids: Vec<_> = bundle.profiles.iter().map(|p| p.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, ["wg-ua-21", "wg-ua-83"]);
    assert!(!already_present);
    // An import never switches by itself; the window asks for that.
    assert!(
        report(lab.orchestrator.handle(Request::GetStatus).await)
            .bundle
            .is_none()
    );

    // Configs with something else beside them are refused, naming it.
    let response = import(config_zip(&[("a.conf", &a), ("readme.txt", "hello")])).await;
    let Response::ImportFailed { errors } = &response else {
        panic!("expected a refusal, got {response:?}");
    };
    assert!(errors[0].starts_with("readme.txt"), "{errors:?}");
    assert_eq!(std::fs::read_dir(lab.layout.bundles()).unwrap().count(), 1);
}

#[tokio::test]
async fn a_new_bundle_does_not_lift_a_block_left_by_a_crash() {
    let lab = lab_in(tempfile::tempdir().unwrap(), true).await;

    let response = create(&lab, "", &[("x.conf", &wireguard("198.51.100.37:51820"))]).await;

    let (id, _, _) = edited(response);
    assert!(lab.log.take().is_empty(), "the block was not touched");
    let status = report(lab.orchestrator.handle(Request::GetStatus).await);
    assert_eq!(status.state, State::Blocked);
    assert!(status.killswitch_engaged);
    assert!(status.bundle.is_none(), "installed, not put in use");
    let (bundles, in_use) = listed(lab.orchestrator.handle(Request::ListBundles).await);
    assert_eq!(bundles, [("My servers".to_string(), id)]);
    assert_eq!(in_use, None);
}

#[tokio::test]
async fn a_runtime_installed_after_the_start_is_noticed_and_used() {
    let log = CallLog::default();
    let missing = RuntimeInfo {
        protocol: Protocol::Wireguard,
        availability: Availability::Missing,
    };
    let available = RuntimeInfo {
        protocol: Protocol::Wireguard,
        availability: Availability::Available {
            version: "1.1.1".into(),
        },
    };
    // WireGuard is not there at first; `installed` is flipped when the user installs it.
    let installed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let looks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let discover: Discover = {
        let (installed, looks, log) = (installed.clone(), looks.clone(), log.clone());
        let (missing, available) = (missing.clone(), available.clone());
        Arc::new(move |protocol| {
            assert_eq!(
                protocol,
                Protocol::Wireguard,
                "only what is missing is looked for"
            );
            looks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if installed.load(std::sync::atomic::Ordering::SeqCst) {
                let driver: Arc<dyn VpnDriver> = Arc::new(MockDriver::new(log.clone()));
                (available.clone(), Some(driver))
            } else {
                (missing.clone(), None)
            }
        })
    };
    let lab = lab_from(
        tempfile::tempdir().unwrap(),
        false,
        log,
        vec![missing.clone()],
        HashMap::new(),
        Some(discover),
    )
    .await;
    imported_and_selected(&lab).await;
    let start = Instant::now();

    // Not installed: reported as missing, and nothing can connect.
    lab.orchestrator.tick(start).await;
    let status = report(lab.orchestrator.handle(Request::GetStatus).await);
    assert_eq!(status.runtimes, [missing]);
    let response = lab
        .orchestrator
        .handle(Request::Connect { profile_id: None })
        .await;
    assert!(matches!(response, Response::Error { .. }), "{response:?}");

    // Installed now. The next look is a few seconds away, not on every tick.
    installed.store(true, std::sync::atomic::Ordering::SeqCst);
    lab.orchestrator.tick(start + Duration::from_secs(1)).await;
    assert_eq!(looks.load(std::sync::atomic::Ordering::SeqCst), 1);
    let mut events = lab.orchestrator.events().subscribe();
    lab.orchestrator.tick(start + Duration::from_secs(11)).await;

    let status = report(lab.orchestrator.handle(Request::GetStatus).await);
    assert_eq!(status.runtimes, std::slice::from_ref(&available));
    assert!(
        matches!(events.try_recv(), Ok(Event::Status { report }) if report.runtimes == std::slice::from_ref(&available)),
        "the window is told"
    );
    assert_eq!(connect(&lab, "wg-a").await.state, State::Connected);

    // Found: no more looking.
    lab.orchestrator.tick(start + Duration::from_secs(60)).await;
    assert_eq!(looks.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_tunnel_lost_on_a_runtime_found_later_is_replaced() {
    let log = CallLog::default();
    let missing = RuntimeInfo {
        protocol: Protocol::Wireguard,
        availability: Availability::Missing,
    };
    let available = RuntimeInfo {
        protocol: Protocol::Wireguard,
        availability: Availability::Available {
            version: "1.1.1".into(),
        },
    };
    // The driver the look hands over; kept here to make it report a lost tunnel.
    let late = Arc::new(MockDriver::new(log.clone()));
    let discover: Discover = {
        let (late, available) = (late.clone(), available.clone());
        Arc::new(move |_| (available.clone(), Some(late.clone() as Arc<dyn VpnDriver>)))
    };
    let lab = lab_from(
        tempfile::tempdir().unwrap(),
        false,
        log,
        vec![missing],
        HashMap::new(),
        Some(discover),
    )
    .await;
    let bundle_id = imported_and_selected(&lab).await;

    // The watch runs as in the service: its first tick finds the runtime.
    let watch = lab.orchestrator.spawn_watch();
    let until = |what: &'static str, done: Box<dyn Fn() -> bool>| async move {
        for _ in 0..300 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting until {what}");
    };
    let orchestrator = lab.orchestrator.clone();
    until(
        "the runtime is found",
        Box::new(move || orchestrator.report().runtimes == std::slice::from_ref(&available)),
    )
    .await;
    assert_eq!(connect(&lab, "wg-a").await.state, State::Connected);
    lab.log.take();

    let id = th_core::bundle::BundleId::parse(&bundle_id).unwrap();
    late.emit(TunnelEvent::Down {
        tunnel: tunnel_name(&id, "wg-a"),
        reason: "test".into(),
    });

    let seen = lab.log.clone();
    let reconnects = Arc::new(std::sync::Mutex::new(Vec::new()));
    let kept = reconnects.clone();
    until(
        "the lost tunnel is replaced",
        Box::new(move || {
            let mut kept = kept.lock().unwrap();
            kept.extend(seen.take());
            kept.iter().any(|e: &String| e.starts_with("connect:"))
        }),
    )
    .await;
    assert_eq!(lab.orchestrator.report().state, State::Connected);
    watch.abort();
}

#[tokio::test]
async fn a_refused_sign_in_is_reported_as_missing_until_it_is_given_again() {
    let log = CallLog::default();
    let driver = Arc::new(MockDriver::new(log.clone()));
    let drivers: HashMap<Protocol, Arc<dyn VpnDriver>> = HashMap::from([
        (Protocol::Wireguard, driver.clone() as Arc<dyn VpnDriver>),
        (Protocol::Openvpn, driver.clone() as Arc<dyn VpnDriver>),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let lab = lab_from(dir, false, log, Vec::new(), drivers, None).await;
    let bundle_id = imported_and_selected(&lab).await;
    let sign_in = |password: &str| Request::SetCredentials {
        bundle_id: bundle_id.clone(),
        profile_ids: Vec::new(),
        username: "user".into(),
        password: Secret::new(password),
    };
    let connect = || Request::Connect {
        profile_id: Some("ovpn".into()),
    };
    let has_sign_in = |status: StatusReport| status.bundle.unwrap().profiles[2].has_credentials;

    assert!(has_sign_in(report(
        lab.orchestrator.handle(sign_in("wrong")).await
    )));
    driver.refuse_credentials("ovpn", true);
    let response = lab.orchestrator.handle(connect()).await;
    assert!(
        is_error(&response, "refused the username and password"),
        "{response:?}"
    );
    // The saved sign-in is still on disk, but the window is told the server needs one.
    assert!(!has_sign_in(lab.orchestrator.report()));

    driver.refuse_credentials("ovpn", false);
    assert!(has_sign_in(report(
        lab.orchestrator.handle(sign_in("right")).await
    )));
    let status = report(lab.orchestrator.handle(connect()).await);
    assert_eq!(status.state, State::Connected);
    assert_eq!(status.profile_id.as_deref(), Some("ovpn"));
}
