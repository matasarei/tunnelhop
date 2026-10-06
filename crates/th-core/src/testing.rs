//! Test doubles for the driver, kill-switch, probe and resolver. They record every call into a
//! shared [`CallLog`], so a test can assert the order in which the engine did things.

use std::collections::{HashSet, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::bundle::BundleId;
use crate::driver::{
    DriverError, InterfaceId, ResolvedEndpoint, ResolvedProfile, TunnelEvent, TunnelHandle,
    TunnelStatus, VpnDriver,
};
use crate::manifest::Manifest;
use crate::rotation::{KillSwitch, Probe, Resolver};
use crate::sanitize::Transport;
use crate::tunnel_name::tunnel_name;

#[derive(Debug, Clone, Default)]
pub struct CallLog(Arc<Mutex<Vec<String>>>);

impl CallLog {
    pub fn push(&self, entry: impl Into<String>) {
        self.0.lock().unwrap().push(entry.into());
    }

    /// Everything recorded so far, clearing the log.
    pub fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

pub struct MockDriver {
    log: CallLog,
    next_interface: AtomicU64,
    fail_connect: Mutex<HashSet<String>>,
    fail_disconnect: AtomicBool,
    events: broadcast::Sender<TunnelEvent>,
}

impl MockDriver {
    pub fn new(log: CallLog) -> Self {
        MockDriver {
            log,
            next_interface: AtomicU64::new(1),
            fail_connect: Mutex::new(HashSet::new()),
            fail_disconnect: AtomicBool::new(false),
            events: broadcast::channel(16).0,
        }
    }

    pub fn fail_connect(&self, profile_id: &str) {
        self.fail_connect
            .lock()
            .unwrap()
            .insert(profile_id.to_string());
    }

    pub fn fail_disconnect(&self, fail: bool) {
        self.fail_disconnect.store(fail, Ordering::SeqCst);
    }
}

#[async_trait]
impl VpnDriver for MockDriver {
    async fn connect(&self, profile: &ResolvedProfile) -> Result<TunnelHandle, DriverError> {
        self.log.push(format!("connect:{}", profile.profile_id));
        if self
            .fail_connect
            .lock()
            .unwrap()
            .contains(&profile.profile_id)
        {
            return Err(DriverError::Failed("mock connect failure".into()));
        }
        Ok(TunnelHandle {
            profile_id: profile.profile_id.clone(),
            protocol: profile.protocol,
            tunnel_name: profile.tunnel_name.clone(),
            interface: InterfaceId(self.next_interface.fetch_add(1, Ordering::SeqCst)),
            endpoints: profile.endpoints.clone(),
        })
    }

    async fn disconnect(&self, handle: &TunnelHandle) -> Result<(), DriverError> {
        self.log.push(format!("disconnect:{}", handle.profile_id));
        if self.fail_disconnect.load(Ordering::SeqCst) {
            return Err(DriverError::Timeout("mock disconnect failure".into()));
        }
        Ok(())
    }

    async fn status(&self, _handle: &TunnelHandle) -> Result<TunnelStatus, DriverError> {
        Ok(TunnelStatus::Up)
    }

    fn events(&self) -> broadcast::Receiver<TunnelEvent> {
        self.events.subscribe()
    }

    async fn reconcile(&self) -> Result<Vec<TunnelHandle>, DriverError> {
        Ok(Vec::new())
    }
}

#[derive(Default)]
pub struct FakeKillSwitch {
    log: CallLog,
    fail_engage: AtomicBool,
    fail_allow: AtomicBool,
}

impl FakeKillSwitch {
    pub fn new(log: CallLog) -> Self {
        FakeKillSwitch {
            log,
            ..FakeKillSwitch::default()
        }
    }

    pub fn fail_engage(&self, fail: bool) {
        self.fail_engage.store(fail, Ordering::SeqCst);
    }

    pub fn fail_allow(&self, fail: bool) {
        self.fail_allow.store(fail, Ordering::SeqCst);
    }
}

#[async_trait]
impl KillSwitch for FakeKillSwitch {
    /// Logged as `engage:<ports, sorted>`.
    async fn engage(&self, endpoints: &[ResolvedEndpoint]) -> Result<(), String> {
        let mut ports: Vec<u16> = endpoints.iter().map(|e| e.addr.port()).collect();
        ports.sort_unstable();
        ports.dedup();
        let ports: Vec<String> = ports.iter().map(u16::to_string).collect();
        self.log.push(format!("engage:{}", ports.join(",")));
        if self.fail_engage.load(Ordering::SeqCst) {
            return Err("mock engage failure".into());
        }
        Ok(())
    }

    async fn allow_interface(&self, interface: InterfaceId) -> Result<(), String> {
        self.log.push(format!("allow:{}", interface.0));
        if self.fail_allow.load(Ordering::SeqCst) {
            return Err("mock allow failure".into());
        }
        Ok(())
    }

    async fn revoke_interface(&self, interface: InterfaceId) -> Result<(), String> {
        self.log.push(format!("revoke:{}", interface.0));
        Ok(())
    }

    async fn disengage(&self) -> Result<(), String> {
        self.log.push("disengage");
        Ok(())
    }
}

/// Answers with scripted results. Once the script runs out it behaves like a VPN whose exit
/// changes on every connection: `first_exit`, then the next address up, and so on. The
/// sequence advances only on unscripted answers.
pub struct FakeProbe {
    log: CallLog,
    script: Mutex<VecDeque<Result<IpAddr, String>>>,
    first_exit: Ipv4Addr,
    exits_used: AtomicU64,
}

impl FakeProbe {
    pub fn new(log: CallLog, first_exit: Ipv4Addr) -> Self {
        FakeProbe {
            log,
            script: Mutex::new(VecDeque::new()),
            first_exit,
            exits_used: AtomicU64::new(0),
        }
    }

    pub fn then(&self, result: Result<IpAddr, String>) -> &Self {
        self.script.lock().unwrap().push_back(result);
        self
    }

    /// The address the n-th unscripted answer gives (0-based).
    pub fn exit(&self, n: u32) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(u32::from(self.first_exit) + n))
    }
}

#[async_trait]
impl Probe for FakeProbe {
    async fn public_ip(&self) -> Result<IpAddr, String> {
        self.log.push("probe");
        if let Some(scripted) = self.script.lock().unwrap().pop_front() {
            return scripted;
        }
        let n = self.exits_used.fetch_add(1, Ordering::SeqCst);
        Ok(self.exit(n as u32))
    }
}

/// Resolves the manifest's i-th profile to `192.0.2.<i+1>:<1000+i>`.
pub struct FakeResolver {
    log: CallLog,
    bundle: BundleId,
    manifest: Manifest,
    fail: Mutex<HashSet<String>>,
}

impl FakeResolver {
    pub fn new(log: CallLog, manifest: Manifest) -> Self {
        FakeResolver {
            log,
            bundle: BundleId::of(b"fake bundle"),
            manifest,
            fail: Mutex::new(HashSet::new()),
        }
    }

    pub fn fail(&self, profile_id: &str) {
        self.fail.lock().unwrap().insert(profile_id.to_string());
    }

    /// The port this resolver gives the profile's endpoint.
    pub fn port_of(&self, profile_id: &str) -> u16 {
        let index = self
            .manifest
            .profiles
            .iter()
            .position(|p| p.id == profile_id)
            .expect("profile exists");
        1000 + index as u16
    }
}

#[async_trait]
impl Resolver for FakeResolver {
    async fn resolve(&self, profile_id: &str) -> Result<ResolvedProfile, String> {
        self.log.push(format!("resolve:{profile_id}"));
        if self.fail.lock().unwrap().contains(profile_id) {
            return Err("mock resolve failure".into());
        }
        let (index, profile) = self
            .manifest
            .profiles
            .iter()
            .enumerate()
            .find(|(_, p)| p.id == profile_id)
            .ok_or_else(|| format!("unknown profile {profile_id}"))?;
        Ok(ResolvedProfile {
            bundle: self.bundle.clone(),
            profile_id: profile.id.clone(),
            protocol: profile.protocol,
            config_path: PathBuf::from(&profile.file),
            tunnel_name: tunnel_name(&self.bundle, &profile.id),
            endpoints: vec![ResolvedEndpoint {
                addr: SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::new(192, 0, 2, index as u8 + 1)),
                    1000 + index as u16,
                ),
                transport: Transport::Udp,
            }],
        })
    }
}
