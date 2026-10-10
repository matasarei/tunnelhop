use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::SeedableRng;
use rand::rngs::StdRng;
use serde_json::json;
use th_core::driver::{TunnelEvent, VpnDriver};
use th_core::manifest::{self, Manifest, Protocol};
use th_core::rotation::{MAX_ATTEMPTS, Overrides, RotationError, Rotator, State, Trigger};
use th_core::testing::{CallLog, FakeKillSwitch, FakeProbe, FakeResolver, MockDriver};

const ISP: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1));
const FIRST_EXIT: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 50);
const VPN: IpAddr = IpAddr::V4(FIRST_EXIT);

fn manifest(profiles: &[&str], on_disconnect: &str) -> Manifest {
    let profiles: Vec<_> = profiles
        .iter()
        .map(|id| {
            let (protocol, file) = if id.starts_with("ovpn") {
                ("openvpn", format!("openvpn/{id}.ovpn"))
            } else {
                ("wireguard", format!("wireguard/{id}.conf"))
            };
            json!({ "id": id, "name": id, "protocol": protocol, "file": file })
        })
        .collect();
    let value = json!({
        "schema_version": "1",
        "bundle_name": "Test",
        "rotation": { "on_disconnect": on_disconnect },
        "profiles": profiles,
    });
    manifest::parse(&serde_json::to_vec(&value).unwrap()).unwrap()
}

struct Rig {
    rotator: Rotator,
    log: CallLog,
    driver: Arc<MockDriver>,
    killswitch: Arc<FakeKillSwitch>,
    probe: Arc<FakeProbe>,
    resolver: Arc<FakeResolver>,
    now: Instant,
}

impl Rig {
    fn new(m: Manifest) -> Self {
        Self::with_overrides(m, Overrides::default())
    }

    fn with_overrides(m: Manifest, overrides: Overrides) -> Self {
        let log = CallLog::default();
        let driver = Arc::new(MockDriver::new(log.clone()));
        let killswitch = Arc::new(FakeKillSwitch::new(log.clone()));
        let probe = Arc::new(FakeProbe::new(log.clone(), FIRST_EXIT));
        let resolver = Arc::new(FakeResolver::new(log.clone(), m.clone()));
        let mut drivers: HashMap<Protocol, Arc<dyn VpnDriver>> = HashMap::new();
        drivers.insert(Protocol::Wireguard, driver.clone());
        drivers.insert(Protocol::Openvpn, driver.clone());
        probe.then(Ok(ISP));
        let rotator = Rotator::new(
            m,
            overrides,
            drivers,
            killswitch.clone(),
            probe.clone(),
            resolver.clone(),
            StdRng::seed_from_u64(7),
        );
        Rig {
            rotator,
            log,
            driver,
            killswitch,
            probe,
            resolver,
            now: Instant::now(),
        }
    }

    async fn rotate(&mut self) -> Result<th_core::rotation::Status, RotationError> {
        self.rotator.rotate(Trigger::Manual, self.now).await
    }

    fn current(&self) -> String {
        self.rotator.status().profile_id.expect("connected")
    }

    fn tunnel_of(&self, profile: &str) -> String {
        th_core::tunnel_name::tunnel_name(&th_core::bundle::BundleId::of(b"fake bundle"), profile)
    }
}

#[tokio::test]
async fn first_connect_records_isp_address_then_follows_the_safe_order() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b", "ovpn-c"], "reconnect_random"));

    let status = rig.rotate().await.unwrap();

    let id = rig.current();
    let port = rig.resolver.port_of(&id);
    assert_eq!(
        rig.log.take(),
        vec![
            "probe".to_string(),
            format!("resolve:{id}"),
            format!("engage:{port}"),
            format!("connect:{id}"),
            "allow:1".to_string(),
            "probe".to_string(),
            format!("engage:{port}"),
        ]
    );
    assert_eq!(status.state, State::Connected);
    assert_eq!(status.public_ip, Some(VPN));
    assert!(status.killswitch_engaged);
}

#[tokio::test]
async fn rotation_engages_before_teardown_and_allows_after_connect() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b", "ovpn-c"], "reconnect_random"));
    rig.rotate().await.unwrap();
    let old = rig.current();
    rig.log.take();

    rig.rotate().await.unwrap();

    let new = rig.current();
    assert_ne!(old, new);
    let (old_port, new_port) = (rig.resolver.port_of(&old), rig.resolver.port_of(&new));
    let mut both = [old_port, new_port];
    both.sort_unstable();
    assert_eq!(
        rig.log.take(),
        vec![
            format!("resolve:{new}"),
            format!("engage:{},{}", both[0], both[1]),
            "revoke:1".to_string(),
            format!("disconnect:{old}"),
            format!("connect:{new}"),
            "allow:2".to_string(),
            "probe".to_string(),
            format!("engage:{new_port}"),
        ]
    );
}

#[tokio::test]
async fn rotation_never_picks_the_current_profile_when_others_exist() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b", "ovpn-c"], "reconnect_random"));
    rig.rotate().await.unwrap();
    for _ in 0..30 {
        let before = rig.current();
        rig.rotate().await.unwrap();
        assert_ne!(before, rig.current());
    }
}

#[tokio::test]
async fn a_single_profile_bundle_reconnects_that_profile() {
    let mut rig = Rig::new(manifest(&["wg-only"], "reconnect_random"));
    rig.rotate().await.unwrap();
    let status = rig.rotate().await.unwrap();
    assert_eq!(status.profile_id.as_deref(), Some("wg-only"));
    assert_eq!(status.state, State::Connected);
}

#[tokio::test]
async fn when_every_attempt_fails_the_engine_ends_blocked_and_engaged() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b", "ovpn-c"], "reconnect_random"));
    for id in ["wg-a", "wg-b", "ovpn-c"] {
        rig.driver.fail_connect(id);
    }

    let error = rig.rotate().await.unwrap_err();

    let RotationError::Exhausted(failures) = error else {
        panic!("{error:?}")
    };
    assert_eq!(failures.len(), 3);
    let status = rig.rotator.status();
    assert_eq!(status.state, State::Blocked);
    assert!(status.killswitch_engaged);
    assert!(
        !rig.log.take().contains(&"disengage".to_string()),
        "must fail closed"
    );
}

#[tokio::test]
async fn each_failed_attempt_tries_a_different_profile() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b", "ovpn-c"], "reconnect_random"));
    for id in ["wg-a", "wg-b", "ovpn-c"] {
        rig.driver.fail_connect(id);
    }
    rig.rotate().await.unwrap_err();
    let mut connects: Vec<String> = rig
        .log
        .take()
        .into_iter()
        .filter(|e| e.starts_with("connect:"))
        .collect();
    connects.sort();
    assert_eq!(connects, ["connect:ovpn-c", "connect:wg-a", "connect:wg-b"]);
}

#[tokio::test]
async fn a_probe_that_still_sees_the_isp_address_is_a_failure() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b"], "reconnect_random"));
    rig.probe.then(Ok(ISP)); // first attempt: traffic is not going through the tunnel

    let status = rig.rotate().await.unwrap();

    let log = rig.log.take();
    assert_eq!(log.iter().filter(|e| e.starts_with("connect:")).count(), 2);
    assert!(log.contains(&"revoke:1".to_string()), "{log:?}");
    assert_eq!(status.public_ip, Some(VPN));
}

#[tokio::test]
async fn a_failed_probe_or_refused_interface_tears_the_new_tunnel_down() {
    let mut rig = Rig::new(manifest(&["wg-a"], "reconnect_random"));
    rig.killswitch.fail_allow(true);
    rig.rotate().await.unwrap_err();
    let log = rig.log.take();
    assert_eq!(
        log.iter().filter(|e| *e == "disconnect:wg-a").count(),
        3,
        "{log:?}"
    );
}

#[tokio::test]
async fn a_kill_switch_failure_aborts_before_touching_the_old_tunnel() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b"], "reconnect_random"));
    rig.rotate().await.unwrap();
    let old = rig.current();
    rig.log.take();
    rig.killswitch.fail_engage(true);

    let error = rig.rotate().await.unwrap_err();

    assert!(matches!(error, RotationError::KillSwitch(_)));
    assert!(!rig.log.take().iter().any(|e| e.starts_with("disconnect:")));
    assert_eq!(rig.rotator.status().state, State::Connected);
    assert_eq!(rig.current(), old);
}

#[tokio::test]
async fn a_failed_teardown_stops_and_blocks_instead_of_running_two_tunnels() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b"], "reconnect_random"));
    rig.rotate().await.unwrap();
    rig.driver.fail_disconnect(true);
    rig.log.take();

    let error = rig.rotate().await.unwrap_err();

    assert!(matches!(error, RotationError::Teardown(_)));
    assert!(!rig.log.take().iter().any(|e| e.starts_with("connect:")));
    let status = rig.rotator.status();
    assert_eq!(status.state, State::Blocked);
    assert!(status.killswitch_engaged);
}

#[tokio::test]
async fn a_resolve_failure_moves_on_to_another_profile() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b"], "reconnect_random"));
    rig.resolver.fail("wg-a");
    let status = rig.rotate().await.unwrap();
    assert_eq!(status.profile_id.as_deref(), Some("wg-b"));
}

#[tokio::test]
async fn no_usable_profile_is_an_error() {
    let m = manifest(&["wg-a"], "reconnect_random");
    let log = CallLog::default();
    let mut rotator = Rotator::new(
        m.clone(),
        Overrides::default(),
        HashMap::new(),
        Arc::new(FakeKillSwitch::new(log.clone())),
        Arc::new(FakeProbe::new(log.clone(), FIRST_EXIT)),
        Arc::new(FakeResolver::new(log.clone(), m)),
        StdRng::seed_from_u64(1),
    );
    let error = rotator
        .rotate(Trigger::Manual, Instant::now())
        .await
        .unwrap_err();
    assert!(matches!(error, RotationError::NoCandidates(_)));
    assert!(log.take().is_empty(), "nothing may be touched");
}

#[tokio::test]
async fn connect_to_picks_the_named_profile() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b", "ovpn-c"], "reconnect_random"));
    let status = rig.rotator.connect_to("ovpn-c", rig.now).await.unwrap();
    assert_eq!(status.profile_id.as_deref(), Some("ovpn-c"));
    let error = rig.rotator.connect_to("nope", rig.now).await.unwrap_err();
    assert!(matches!(error, RotationError::NoCandidates(_)));
}

#[tokio::test]
async fn tunnel_down_triggers_a_rotation_unless_stay_down() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b"], "reconnect_random"));
    rig.rotate().await.unwrap();
    let tunnel = rig.tunnel_of(&rig.current());
    let down = TunnelEvent::Down {
        tunnel,
        reason: "gone".into(),
    };
    assert_eq!(
        rig.rotator.on_event(&down, rig.now),
        Some(Trigger::TunnelLost)
    );

    let mut rig = Rig::new(manifest(&["wg-a", "wg-b"], "stay_down"));
    rig.rotate().await.unwrap();
    let tunnel = rig.tunnel_of(&rig.current());
    let down = TunnelEvent::Down {
        tunnel,
        reason: "gone".into(),
    };
    assert_eq!(rig.rotator.on_event(&down, rig.now), None);
    let status = rig.rotator.status();
    assert_eq!(status.state, State::Blocked);
    assert!(status.killswitch_engaged, "stay_down still fails closed");
}

#[tokio::test]
async fn reconnect_same_reconnects_the_lost_profile() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b", "ovpn-c"], "reconnect_same"));
    rig.rotate().await.unwrap();
    let lost = rig.current();
    let status = rig
        .rotator
        .rotate(Trigger::TunnelLost, rig.now)
        .await
        .unwrap();
    assert_eq!(status.profile_id, Some(lost));
}

#[tokio::test]
async fn reconnecting_past_the_grace_period_counts_as_lost() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b"], "reconnect_random"));
    rig.rotate().await.unwrap();
    let tunnel = rig.tunnel_of(&rig.current());
    let t0 = rig.now;
    let reconnecting = TunnelEvent::Reconnecting {
        tunnel: tunnel.clone(),
    };

    assert_eq!(rig.rotator.on_event(&reconnecting, t0), None);
    assert_eq!(rig.rotator.check_grace(t0 + Duration::from_secs(19)), None);
    // A repeated reconnecting event does not restart the clock.
    assert_eq!(
        rig.rotator
            .on_event(&reconnecting, t0 + Duration::from_secs(10)),
        None
    );
    assert_eq!(
        rig.rotator.check_grace(t0 + Duration::from_secs(20)),
        Some(Trigger::TunnelLost)
    );

    // Coming back up resets it.
    rig.rotator.on_event(&reconnecting, t0);
    rig.rotator
        .on_event(&TunnelEvent::Up { tunnel }, t0 + Duration::from_secs(5));
    assert_eq!(rig.rotator.check_grace(t0 + Duration::from_secs(60)), None);
}

#[tokio::test]
async fn events_for_other_tunnels_are_ignored() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b"], "reconnect_random"));
    rig.rotate().await.unwrap();
    let other = TunnelEvent::Down {
        tunnel: "wg0".into(),
        reason: "user tunnel".into(),
    };
    assert_eq!(rig.rotator.on_event(&other, rig.now), None);
    assert_eq!(rig.rotator.status().state, State::Connected);
}

#[tokio::test]
async fn the_timer_follows_the_interval_and_user_overrides_win() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b"], "reconnect_random"));
    let t0 = rig.now;
    assert!(!rig.rotator.due(t0), "nothing to rotate while disconnected");
    rig.rotate().await.unwrap();
    // The manifest sets no interval: the default, 60 minutes (SPEC §3.2), applies.
    assert!(!rig.rotator.due(t0 + Duration::from_secs(59 * 60)));
    assert!(rig.rotator.due(t0 + Duration::from_secs(60 * 60)));

    rig.rotator.set_overrides(Overrides {
        auto_rotate: None,
        interval_minutes: Some(5),
    });
    assert!(rig.rotator.due(t0 + Duration::from_secs(5 * 60)));

    rig.rotator.set_overrides(Overrides {
        auto_rotate: Some(false),
        interval_minutes: Some(5),
    });
    assert!(!rig.rotator.due(t0 + Duration::from_secs(3600)));

    let m = manifest(&["wg-a"], "reconnect_random");
    let mut disabled = m.clone();
    disabled.rotation.enabled = false;
    let rig = Rig::with_overrides(
        disabled,
        Overrides {
            auto_rotate: Some(true),
            interval_minutes: None,
        },
    );
    assert!(
        rig.rotator.auto_rotate(),
        "the user's switch beats the manifest's"
    );
}

#[tokio::test]
async fn the_isp_address_is_recorded_again_after_a_disconnect() {
    let new_isp = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9));
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b"], "reconnect_random"));
    rig.rotate().await.unwrap();
    rig.rotator.disconnect().await.unwrap();
    rig.log.take();

    // The laptop moved to another network: the unprotected address is now `new_isp`, and the
    // first attempt's probe still sees it.
    rig.probe.then(Ok(new_isp)).then(Ok(new_isp));
    let status = rig.rotate().await.unwrap();

    let log = rig.log.take();
    assert_eq!(log[0], "probe", "{log:?}");
    assert_eq!(
        log.iter().filter(|e| e.starts_with("connect:")).count(),
        2,
        "{log:?}"
    );
    assert_eq!(
        status.public_ip,
        Some(rig.probe.exit(1)),
        "the second connection's exit"
    );
}

#[tokio::test]
async fn disconnect_tears_down_and_lifts_the_kill_switch() {
    let mut rig = Rig::new(manifest(&["wg-a"], "reconnect_random"));
    rig.rotate().await.unwrap();
    rig.log.take();

    let status = rig.rotator.disconnect().await.unwrap();

    assert_eq!(rig.log.take(), ["revoke:1", "disconnect:wg-a", "disengage"]);
    assert_eq!(status.state, State::Disconnected);
    assert!(!status.killswitch_engaged);
    assert_eq!(status.profile_id, None);
}

#[tokio::test]
async fn adopt_restores_state_found_at_startup() {
    let mut rig = Rig::new(manifest(&["wg-a"], "reconnect_random"));
    rig.rotator.adopt(None, true, rig.now);
    assert_eq!(rig.rotator.status().state, State::Blocked);
    rig.rotator.adopt(None, false, rig.now);
    assert_eq!(rig.rotator.status().state, State::Disconnected);
}

#[tokio::test]
async fn profiles_missing_credentials_are_never_picked() {
    let mut rig = Rig::new(manifest(&["wg-a", "ovpn-b", "ovpn-c"], "reconnect_random"));
    rig.rotator
        .set_missing_credentials(["ovpn-b".to_string(), "ovpn-c".to_string()].into());

    for _ in 0..10 {
        let status = rig.rotate().await.unwrap();
        assert_eq!(status.profile_id.as_deref(), Some("wg-a"));
    }
    assert!(!rig.log.take().iter().any(|e| e.contains("ovpn-")));

    let error = rig.rotator.connect_to("ovpn-b", rig.now).await.unwrap_err();
    assert_eq!(
        error,
        RotationError::NoCandidates("profile \"ovpn-b\" needs credentials".into())
    );

    // Once the credentials are set, the profile is usable again.
    rig.rotator.set_missing_credentials(Default::default());
    let status = rig.rotator.connect_to("ovpn-b", rig.now).await.unwrap();
    assert_eq!(status.profile_id.as_deref(), Some("ovpn-b"));
}

#[tokio::test]
async fn a_bundle_where_every_profile_lacks_credentials_has_no_candidates() {
    let mut rig = Rig::new(manifest(&["ovpn-a"], "reconnect_random"));
    rig.rotator
        .set_missing_credentials(["ovpn-a".to_string()].into());
    let error = rig.rotate().await.unwrap_err();
    assert!(matches!(error, RotationError::NoCandidates(_)), "{error:?}");
    assert!(rig.log.take().is_empty(), "nothing may be touched");
}

// ---- A rotation must change the public IP ---------------------------------------------------

fn connects(log: &[String]) -> Vec<String> {
    log.iter()
        .filter(|e| e.starts_with("connect:"))
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_rotation_that_keeps_the_ip_tries_another_candidate() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b", "ovpn-c"], "reconnect_random"));
    let first = rig.rotate().await.unwrap();
    assert_eq!(
        first.ip_changed, None,
        "nothing to compare on the first connect"
    );
    let previous = first.public_ip.unwrap();
    rig.log.take();

    rig.probe.then(Ok(previous)); // the first candidate exits through the same address
    let status = rig.rotate().await.unwrap();

    let log = rig.log.take();
    assert_eq!(connects(&log).len(), 2, "{log:?}");
    assert!(
        log.contains(&"revoke:2".to_string()),
        "the same-IP tunnel is torn down: {log:?}"
    );
    assert_eq!(status.ip_changed, Some(true));
    assert_ne!(status.public_ip, Some(previous));
}

#[tokio::test]
async fn when_no_candidate_changes_the_ip_it_stays_connected_and_says_so() {
    let mut rig = Rig::new(manifest(
        &["wg-a", "wg-b", "ovpn-c", "wg-d"],
        "reconnect_random",
    ));
    let previous = rig.rotate().await.unwrap().public_ip.unwrap();
    rig.log.take();
    for _ in 0..=MAX_ATTEMPTS {
        rig.probe.then(Ok(previous));
    }

    let status = rig.rotate().await.unwrap();

    let log = rig.log.take();
    let attempts = connects(&log);
    assert_eq!(
        attempts.len(),
        MAX_ATTEMPTS + 1,
        "three tries, then the fallback: {log:?}"
    );
    assert_eq!(
        attempts.last(),
        attempts.first(),
        "the fallback is the first soft failure"
    );
    assert_eq!(status.state, State::Connected, "never Blocked for this");
    assert_eq!(status.ip_changed, Some(false));
    assert_eq!(status.public_ip, Some(previous));
}

#[tokio::test]
async fn a_fallback_that_fails_ends_blocked_like_any_exhausted_rotation() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b", "ovpn-c"], "reconnect_random"));
    let previous = rig.rotate().await.unwrap().public_ip.unwrap();
    for _ in 0..MAX_ATTEMPTS {
        rig.probe.then(Ok(previous));
    }
    rig.probe.then(Err("network down".into()));

    let error = rig.rotate().await.unwrap_err();

    let RotationError::Exhausted(failures) = error else {
        panic!("{error:?}")
    };
    assert_eq!(failures.len(), MAX_ATTEMPTS + 1, "{failures:?}");
    assert!(failures[0].contains("did not change"), "{failures:?}");
    let status = rig.rotator.status();
    assert_eq!(status.state, State::Blocked);
    assert!(status.killswitch_engaged);
    assert_eq!(status.ip_changed, None);
}

#[tokio::test]
async fn reconnecting_a_lost_tunnel_accepts_the_same_ip() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b"], "reconnect_same"));
    let previous = rig.rotate().await.unwrap().public_ip.unwrap();
    rig.log.take();
    rig.probe.then(Ok(previous));

    let status = rig
        .rotator
        .rotate(Trigger::TunnelLost, rig.now)
        .await
        .unwrap();

    assert_eq!(connects(&rig.log.take()).len(), 1);
    assert_eq!(status.public_ip, Some(previous));
    assert_eq!(status.ip_changed, None);
}

#[tokio::test]
async fn a_chosen_profile_or_a_single_profile_bundle_accepts_the_same_ip() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b"], "reconnect_random"));
    let previous = rig.rotate().await.unwrap().public_ip.unwrap();
    rig.probe.then(Ok(previous));
    let status = rig.rotator.connect_to("wg-a", rig.now).await.unwrap();
    assert_eq!(
        (status.public_ip, status.ip_changed),
        (Some(previous), None)
    );

    let mut rig = Rig::new(manifest(&["wg-only"], "reconnect_random"));
    let previous = rig.rotate().await.unwrap().public_ip.unwrap();
    rig.log.take();
    rig.probe.then(Ok(previous));
    let status = rig.rotate().await.unwrap();
    assert_eq!(connects(&rig.log.take()).len(), 1);
    assert_eq!(
        (status.public_ip, status.ip_changed),
        (Some(previous), None)
    );
}

#[tokio::test]
async fn profiles_behind_a_different_entry_are_tried_first() {
    let mut rig = Rig::new(manifest(
        &["wg-a", "wg-b", "ovpn-c", "wg-d"],
        "reconnect_random",
    ));
    rig.rotator.set_entry_hosts(HashMap::from([
        ("wg-a".to_string(), vec!["149.88.110.33".to_string()]),
        ("wg-b".to_string(), vec!["130.195.241.2".to_string()]),
        // Same entry as wg-a, like Proton's UA#21 (WireGuard) and UA#39 (OpenVPN).
        ("ovpn-c".to_string(), vec!["149.88.110.33".to_string()]),
        ("wg-d".to_string(), vec!["149.88.110.33".to_string()]),
    ]));
    rig.rotator.connect_to("wg-a", rig.now).await.unwrap();
    rig.log.take();

    let status = rig.rotate().await.unwrap();

    // wg-b is the only profile behind another entry, so it is the one tried.
    assert_eq!(status.profile_id.as_deref(), Some("wg-b"));
    assert_eq!(connects(&rig.log.take()), ["connect:wg-b"]);
}

#[tokio::test]
async fn a_failed_teardown_does_not_keep_the_previous_ip_verdict() {
    let mut rig = Rig::new(manifest(&["wg-a", "wg-b", "ovpn-c"], "reconnect_random"));
    rig.rotate().await.unwrap();
    assert_eq!(rig.rotate().await.unwrap().ip_changed, Some(true));
    rig.driver.fail_disconnect(true);

    rig.rotate().await.unwrap_err();

    let status = rig.rotator.status();
    assert_eq!(status.state, State::Blocked);
    assert_eq!(
        status.ip_changed, None,
        "Blocked must not report the last rotation's result"
    );
}

// ---- A server that refuses the sign-in ------------------------------------------------------

#[tokio::test]
async fn a_server_that_refuses_the_sign_in_is_tried_once_and_then_left_alone() {
    let mut rig = Rig::new(manifest(&["wg-a", "ovpn-b"], "reconnect_random"));
    rig.driver.refuse_credentials("ovpn-b", true);

    // Asked for by name: one try, not MAX_ATTEMPTS of the same refusal.
    let error = rig.rotator.connect_to("ovpn-b", rig.now).await.unwrap_err();
    let RotationError::Exhausted(failures) = &error else {
        panic!("{error:?}");
    };
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("refused the username and password"),
        "{failures:?}"
    );
    assert_eq!(connects(&rig.log.take()), ["connect:ovpn-b"]);
    assert!(rig.rotator.refused_credentials().contains("ovpn-b"));
    assert_eq!(rig.rotator.status().state, State::Blocked);

    // Asking again does not reach the server at all.
    let error = rig.rotator.connect_to("ovpn-b", rig.now).await.unwrap_err();
    assert!(
        matches!(&error, RotationError::NoCandidates(why) if why.contains("refused the saved ones")),
        "{error:?}"
    );
    assert!(connects(&rig.log.take()).is_empty());

    // Rotation never picks it either.
    for _ in 0..10 {
        let status = rig.rotate().await.unwrap();
        assert_eq!(status.profile_id.as_deref(), Some("wg-a"));
    }
    assert!(!rig.log.take().iter().any(|e| e.contains("ovpn-b")));

    // New credentials make it a candidate again.
    rig.driver.refuse_credentials("ovpn-b", false);
    rig.rotator.credentials_changed(&["ovpn-b".to_string()]);
    assert!(rig.rotator.refused_credentials().is_empty());
    let status = rig.rotator.connect_to("ovpn-b", rig.now).await.unwrap();
    assert_eq!(status.profile_id.as_deref(), Some("ovpn-b"));
}

#[tokio::test]
async fn a_rotation_moves_past_a_server_that_refuses_the_sign_in() {
    let mut rig = Rig::new(manifest(&["ovpn-a", "ovpn-b"], "reconnect_random"));
    rig.driver.refuse_credentials("ovpn-a", true);
    rig.driver.refuse_credentials("ovpn-b", true);

    // Both refuse: each is tried once and the rotation ends, with nothing tried twice.
    let error = rig.rotate().await.unwrap_err();
    assert!(matches!(error, RotationError::Exhausted(_)), "{error:?}");
    let mut tried = connects(&rig.log.take());
    tried.sort();
    assert_eq!(tried, ["connect:ovpn-a", "connect:ovpn-b"]);

    // Credentials for the whole bundle clear both.
    rig.driver.refuse_credentials("ovpn-a", false);
    rig.driver.refuse_credentials("ovpn-b", false);
    rig.rotator.credentials_changed(&[]);
    assert!(rig.rotate().await.is_ok());
}
