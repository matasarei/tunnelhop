//! The rotation engine — SPEC §4.5.
//!
//! It owns the switch from one tunnel to the next and the rule that holds it together: the
//! kill-switch is engaged before the old tunnel goes down and stays engaged until a new tunnel
//! has proved itself with a public-IP probe. When every attempt fails the engine ends in
//! `Blocked`, still engaged — it fails closed.
//!
//! Timers live in the service. The engine is told the time (`now`) and answers whether a
//! rotation is due, so it can be tested without a clock.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rand::RngExt;
use rand::rngs::StdRng;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::driver::{
    InterfaceId, ResolvedEndpoint, ResolvedProfile, TunnelEvent, TunnelHandle, VpnDriver,
};
use crate::manifest::{Manifest, OnDisconnect, Protocol};

pub const MAX_ATTEMPTS: usize = 3;

#[async_trait]
pub trait KillSwitch: Send + Sync {
    /// Block all traffic except the given VPN endpoints (and the always-allowed basics).
    /// Replaces the permitted endpoint set when already engaged.
    async fn engage(&self, endpoints: &[ResolvedEndpoint]) -> Result<(), String>;
    async fn allow_interface(&self, interface: InterfaceId) -> Result<(), String>;
    async fn revoke_interface(&self, interface: InterfaceId) -> Result<(), String>;
    async fn disengage(&self) -> Result<(), String>;
}

#[async_trait]
pub trait Probe: Send + Sync {
    async fn public_ip(&self) -> Result<IpAddr, String>;
}

/// Turns a profile id into something a driver can connect: config path, tunnel name, and
/// endpoints resolved to addresses. Called while the old tunnel is still up.
#[async_trait]
pub trait Resolver: Send + Sync {
    async fn resolve(&self, profile_id: &str) -> Result<ResolvedProfile, String>;
}

/// The user's choices from the tray. Each one, when set, beats the manifest's default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overrides {
    pub auto_rotate: Option<bool>,
    pub interval_minutes: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Disconnected,
    Connected,
    /// The kill-switch is engaged and no tunnel is up.
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub state: State,
    pub profile_id: Option<String>,
    pub public_ip: Option<IpAddr>,
    pub killswitch_engaged: bool,
    /// After a timer or manual rotation that had to change the public IP: whether it did.
    /// `Some(false)` means no candidate gave a new address and the engine settled for one
    /// that kept it; the service tells the user. `None` when the rule did not apply.
    pub ip_changed: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// The interval elapsed.
    Timer,
    /// "Rotate Now".
    Manual,
    /// The current tunnel went down, or stayed reconnecting past the grace period.
    TunnelLost,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RotationError {
    #[error("no profile can be used: {0}")]
    NoCandidates(String),
    #[error("kill-switch failed: {0}")]
    KillSwitch(String),
    #[error("the previous tunnel could not be torn down: {0}")]
    Teardown(String),
    #[error("every attempt failed: {}", .0.join("; "))]
    Exhausted(Vec<String>),
}

pub struct Rotator {
    manifest: Manifest,
    overrides: Overrides,
    drivers: HashMap<Protocol, Arc<dyn VpnDriver>>,
    killswitch: Arc<dyn KillSwitch>,
    probe: Arc<dyn Probe>,
    resolver: Arc<dyn Resolver>,
    rng: StdRng,
    current: Option<TunnelHandle>,
    state: State,
    public_ip: Option<IpAddr>,
    isp_ip: Option<IpAddr>,
    engaged: bool,
    connected_at: Option<Instant>,
    reconnecting_since: Option<Instant>,
    missing_credentials: HashSet<String>,
    entry_hosts: HashMap<String, HashSet<String>>,
    ip_changed: Option<bool>,
}

impl Rotator {
    /// `drivers` holds only the protocols whose runtime is installed and new enough.
    pub fn new(
        manifest: Manifest,
        overrides: Overrides,
        drivers: HashMap<Protocol, Arc<dyn VpnDriver>>,
        killswitch: Arc<dyn KillSwitch>,
        probe: Arc<dyn Probe>,
        resolver: Arc<dyn Resolver>,
        rng: StdRng,
    ) -> Self {
        Rotator {
            manifest,
            overrides,
            drivers,
            killswitch,
            probe,
            resolver,
            rng,
            current: None,
            state: State::Disconnected,
            public_ip: None,
            isp_ip: None,
            engaged: false,
            connected_at: None,
            reconnecting_since: None,
            missing_credentials: HashSet::new(),
            entry_hosts: HashMap::new(),
            ip_changed: None,
        }
    }

    /// Start from what the service found at startup: a tunnel still up, filters still in place.
    pub fn adopt(&mut self, tunnel: Option<TunnelHandle>, killswitch_engaged: bool, now: Instant) {
        self.engaged = killswitch_engaged;
        self.state = match (&tunnel, killswitch_engaged) {
            (Some(_), _) => State::Connected,
            (None, true) => State::Blocked,
            (None, false) => State::Disconnected,
        };
        self.connected_at = tunnel.as_ref().map(|_| now);
        self.current = tunnel;
    }

    /// Give up the tunnel and the block without touching either, for another `Rotator` to
    /// [`adopt`](Self::adopt): the bundle was replaced by an edited one (SPEC §3.5), and
    /// nothing may flow outside the tunnel while the new one takes over.
    pub fn hand_over(self) -> (Option<TunnelHandle>, bool) {
        (self.current, self.engaged)
    }

    pub fn status(&self) -> Status {
        Status {
            state: self.state,
            profile_id: self.current.as_ref().map(|h| h.profile_id.clone()),
            public_ip: self.public_ip,
            killswitch_engaged: self.engaged,
            ip_changed: self.ip_changed,
        }
    }

    /// A runtime installed after the engine was built: its profiles become candidates.
    pub fn add_driver(&mut self, protocol: Protocol, driver: Arc<dyn VpnDriver>) {
        self.drivers.insert(protocol, driver);
    }

    /// Profiles that cannot connect until the user gives credentials. They are never picked
    /// for rotation, and connecting to one by name is refused. Replaces the previous set.
    pub fn set_missing_credentials(&mut self, profile_ids: HashSet<String>) {
        self.missing_credentials = profile_ids;
    }

    /// The server hosts each profile connects to, as written in its config (from import).
    /// Providers route several exits through one entry, so a rotation that must change the
    /// public IP tries profiles with a different entry first.
    pub fn set_entry_hosts(&mut self, hosts: HashMap<String, Vec<String>>) {
        self.entry_hosts = hosts
            .into_iter()
            .map(|(profile, hosts)| (profile, hosts.into_iter().collect()))
            .collect();
    }

    pub fn set_overrides(&mut self, overrides: Overrides) {
        self.overrides = overrides;
    }

    pub fn auto_rotate(&self) -> bool {
        self.overrides
            .auto_rotate
            .unwrap_or(self.manifest.rotation.enabled)
    }

    pub fn interval(&self) -> Duration {
        let minutes = self
            .overrides
            .interval_minutes
            .unwrap_or(self.manifest.rotation.interval_minutes);
        Duration::from_secs(u64::from(minutes) * 60)
    }

    /// Whether the interval timer should rotate now.
    pub fn due(&self, now: Instant) -> bool {
        self.state == State::Connected
            && self.auto_rotate()
            && self
                .connected_at
                .is_some_and(|at| now.duration_since(at) >= self.interval())
    }

    /// Feed a driver event. Returns a trigger when the event means the tunnel is lost.
    pub fn on_event(&mut self, event: &TunnelEvent, now: Instant) -> Option<Trigger> {
        let current = self.current.as_ref()?;
        if event.tunnel() != current.tunnel_name || self.state != State::Connected {
            return None;
        }
        match event {
            TunnelEvent::Up { .. } => {
                self.reconnecting_since = None;
                None
            }
            TunnelEvent::Reconnecting { .. } => {
                self.reconnecting_since.get_or_insert(now);
                self.check_grace(now)
            }
            TunnelEvent::Down { .. } => self.tunnel_lost(),
        }
    }

    /// Call periodically: a tunnel reconnecting for longer than the grace period counts as lost.
    pub fn check_grace(&mut self, now: Instant) -> Option<Trigger> {
        let since = self.reconnecting_since?;
        let grace = Duration::from_secs(u64::from(self.manifest.rotation.reconnect_grace_seconds));
        if self.state == State::Connected && now.duration_since(since) >= grace {
            self.tunnel_lost()
        } else {
            None
        }
    }

    fn tunnel_lost(&mut self) -> Option<Trigger> {
        self.reconnecting_since = None;
        if self.manifest.rotation.on_disconnect == OnDisconnect::StayDown {
            // The kill-switch stays engaged: losing the tunnel must not expose traffic.
            self.state = State::Blocked;
            self.public_ip = None;
            None
        } else {
            Some(Trigger::TunnelLost)
        }
    }

    /// Switch to a randomly chosen profile.
    pub async fn rotate(
        &mut self,
        trigger: Trigger,
        now: Instant,
    ) -> Result<Status, RotationError> {
        let candidates = self.candidates(trigger)?;
        // A rotation the user or the timer asked for exists to change the public IP. After a
        // lost tunnel the goal is to be connected again, and with one profile there is no
        // other IP to get.
        let avoid_ip = match trigger {
            Trigger::Timer | Trigger::Manual if self.usable_profiles().len() > 1 => self.public_ip,
            _ => None,
        };
        self.switch(candidates, avoid_ip, now).await
    }

    /// Switch to one specific profile (chosen in the dashboard).
    pub async fn connect_to(
        &mut self,
        profile_id: &str,
        now: Instant,
    ) -> Result<Status, RotationError> {
        if self.missing_credentials.contains(profile_id) {
            return Err(RotationError::NoCandidates(format!(
                "profile \"{profile_id}\" needs credentials"
            )));
        }
        if !self.usable_profiles().contains(&profile_id.to_string()) {
            return Err(RotationError::NoCandidates(format!(
                "profile \"{profile_id}\" does not exist or its runtime is unavailable"
            )));
        }
        self.switch(vec![profile_id.to_string()], None, now).await
    }

    /// Switch to `profile_id` if one attempt at it works, and to any other profile if not.
    /// For a bundle replaced by an edited one (SPEC §3.5): staying on the same server keeps
    /// the public IP, but it is not worth several attempts with traffic blocked meanwhile.
    pub async fn reconnect_or_rotate(
        &mut self,
        profile_id: &str,
        now: Instant,
    ) -> Result<Status, RotationError> {
        if self.usable_profiles().iter().any(|p| p == profile_id) {
            match self.attempt(profile_id, None).await {
                Ok((handle, ip)) => return Ok(self.settle(handle, ip, None, now).await),
                Err(AttemptError::Fatal(error)) => return Err(self.fatal(error)),
                Err(AttemptError::Retry(_) | AttemptError::SameIp(_)) => {}
            }
        }
        self.rotate(Trigger::Manual, now).await
    }

    /// Bring everything down and lift the kill-switch ("Disconnect & unblock", Exit).
    pub async fn disconnect(&mut self) -> Result<Status, RotationError> {
        if let Some(old) = self.current.take() {
            let _ = self.killswitch.revoke_interface(old.interface).await;
            if let Err(e) = self.driver(old.protocol)?.disconnect(&old).await {
                self.current = Some(old);
                return Err(RotationError::Teardown(e.to_string()));
            }
        }
        self.public_ip = None;
        self.ip_changed = None;
        self.connected_at = None;
        self.reconnecting_since = None;
        self.state = State::Blocked;
        self.killswitch
            .disengage()
            .await
            .map_err(RotationError::KillSwitch)?;
        self.engaged = false;
        self.state = State::Disconnected;
        Ok(self.status())
    }

    fn usable_profiles(&self) -> Vec<String> {
        let allowed = self.manifest.rotation_protocols();
        self.manifest
            .profiles
            .iter()
            .filter(|p| {
                allowed.contains(&p.protocol)
                    && self.drivers.contains_key(&p.protocol)
                    && !self.missing_credentials.contains(&p.id)
            })
            .map(|p| p.id.clone())
            .collect()
    }

    fn candidates(&self, trigger: Trigger) -> Result<Vec<String>, RotationError> {
        let usable = self.usable_profiles();
        let current = self.current.as_ref().map(|h| h.profile_id.clone());

        if trigger == Trigger::TunnelLost
            && self.manifest.rotation.on_disconnect == OnDisconnect::ReconnectSame
            && let Some(current) = &current
            && usable.contains(current)
        {
            return Ok(vec![current.clone()]);
        }
        let others: Vec<String> = usable
            .iter()
            .filter(|id| Some(*id) != current.as_ref())
            .cloned()
            .collect();
        match (others.is_empty(), usable.is_empty()) {
            (false, _) => Ok(others),
            // Only the current profile is left: reconnect it.
            (true, false) => Ok(usable),
            (true, true) => Err(RotationError::NoCandidates(
                "no profile uses a protocol whose runtime is available".into(),
            )),
        }
    }

    fn driver(&self, protocol: Protocol) -> Result<Arc<dyn VpnDriver>, RotationError> {
        self.drivers
            .get(&protocol)
            .cloned()
            .ok_or_else(|| RotationError::NoCandidates(format!("no driver for {protocol:?}")))
    }

    /// Try candidates until one connects. With `avoid_ip`, a tunnel whose probe still shows
    /// that address counts as a soft failure: it is torn down and the next candidate tried,
    /// and only when no candidate gives a new address does the engine reconnect the first
    /// soft failure rather than end `Blocked`.
    async fn switch(
        &mut self,
        candidates: Vec<String>,
        avoid_ip: Option<IpAddr>,
        now: Instant,
    ) -> Result<Status, RotationError> {
        if self.current.is_none() && !self.engaged {
            // Record the unprotected address while it is still visible, so a probe that still
            // sees it is a failure. Every time: the network may have changed since last time.
            self.isp_ip = self.probe.public_ip().await.ok();
        }
        let different_entry = self.profiles_with_different_entry(&candidates);

        let mut failures = Vec::new();
        let mut failed: HashSet<String> = HashSet::new();
        let mut fallback: Option<String> = None;
        for _ in 0..MAX_ATTEMPTS {
            let mut remaining: Vec<&String> =
                candidates.iter().filter(|c| !failed.contains(*c)).collect();
            if remaining.is_empty() {
                remaining = candidates.iter().collect();
            }
            let preferred: Vec<&String> = remaining
                .iter()
                .copied()
                .filter(|c| different_entry.contains(*c))
                .collect();
            if !preferred.is_empty() {
                remaining = preferred;
            }
            let pick = remaining[self.rng.random_range(0..remaining.len())].clone();

            match self.attempt(&pick, avoid_ip).await {
                Ok((handle, ip)) => {
                    return Ok(self.settle(handle, ip, avoid_ip.map(|_| true), now).await);
                }
                Err(AttemptError::Fatal(error)) => return Err(self.fatal(error)),
                Err(AttemptError::SameIp(ip)) => {
                    failures.push(format!("{pick}: public IP {ip} did not change"));
                    fallback.get_or_insert_with(|| pick.clone());
                    failed.insert(pick);
                }
                Err(AttemptError::Retry(reason)) => {
                    failures.push(format!("{pick}: {reason}"));
                    failed.insert(pick);
                }
            }
        }

        if let Some(pick) = fallback {
            // Every working candidate kept the address: staying connected beats blocking.
            match self.attempt(&pick, None).await {
                Ok((handle, ip)) => {
                    return Ok(self.settle(handle, ip, Some(false), now).await);
                }
                Err(AttemptError::Fatal(error)) => return Err(self.fatal(error)),
                // No address is avoided here, so SameIp cannot occur; if it ever did, it is one
                // more failed attempt, not a reason to take the service down.
                Err(AttemptError::Retry(reason)) => failures.push(format!("{pick}: {reason}")),
                Err(AttemptError::SameIp(ip)) => {
                    failures.push(format!("{pick}: public IP {ip} did not change"))
                }
            }
        }

        self.state = State::Blocked;
        self.public_ip = None;
        self.ip_changed = None;
        self.connected_at = None;
        Err(RotationError::Exhausted(failures))
    }

    async fn settle(
        &mut self,
        handle: TunnelHandle,
        ip: IpAddr,
        ip_changed: Option<bool>,
        now: Instant,
    ) -> Status {
        // Steady state: only the live tunnel's endpoints stay permitted.
        let _ = self.killswitch.engage(&handle.endpoints).await;
        self.current = Some(handle);
        self.public_ip = Some(ip);
        self.ip_changed = ip_changed;
        self.state = State::Connected;
        self.connected_at = Some(now);
        self.reconnecting_since = None;
        self.status()
    }

    fn fatal(&mut self, error: RotationError) -> RotationError {
        if self.current.is_none() && self.engaged {
            self.state = State::Blocked;
            self.public_ip = None;
            self.ip_changed = None;
        }
        error
    }

    /// Candidates that share no entry host with the current tunnel's profile. Empty when
    /// nothing is known, which leaves the choice purely random.
    fn profiles_with_different_entry(&self, candidates: &[String]) -> HashSet<String> {
        let Some(current_hosts) = self
            .current
            .as_ref()
            .and_then(|h| self.entry_hosts.get(&h.profile_id))
        else {
            return HashSet::new();
        };
        candidates
            .iter()
            .filter(|c| {
                self.entry_hosts
                    .get(*c)
                    .is_some_and(|hosts| hosts.is_disjoint(current_hosts))
            })
            .cloned()
            .collect()
    }

    /// One try at one profile. On a retryable failure nothing of the new tunnel is left up.
    async fn attempt(
        &mut self,
        profile_id: &str,
        avoid_ip: Option<IpAddr>,
    ) -> Result<(TunnelHandle, IpAddr), AttemptError> {
        let profile = self
            .resolver
            .resolve(profile_id)
            .await
            .map_err(|e| AttemptError::Retry(format!("could not resolve: {e}")))?;
        let driver = self.driver(profile.protocol).map_err(AttemptError::Fatal)?;

        let mut permitted = profile.endpoints.clone();
        if let Some(old) = &self.current {
            permitted.extend(old.endpoints.iter().copied());
        }
        self.killswitch
            .engage(&permitted)
            .await
            .map_err(|e| AttemptError::Fatal(RotationError::KillSwitch(e)))?;
        self.engaged = true;

        if let Some(old) = self.current.take() {
            let _ = self.killswitch.revoke_interface(old.interface).await;
            self.public_ip = None;
            if let Err(e) = self
                .driver(old.protocol)
                .map_err(AttemptError::Fatal)?
                .disconnect(&old)
                .await
            {
                // Keep the handle so a later disconnect can retry; its interface is no longer
                // permitted, so nothing flows meanwhile.
                self.current = Some(old);
                self.state = State::Blocked;
                self.ip_changed = None;
                return Err(AttemptError::Fatal(RotationError::Teardown(e.to_string())));
            }
        }

        let handle = driver
            .connect(&profile)
            .await
            .map_err(|e| AttemptError::Retry(format!("could not connect: {e}")))?;

        let verdict = match self.killswitch.allow_interface(handle.interface).await {
            Err(e) => Err(AttemptError::Retry(format!(
                "kill-switch refused the tunnel interface: {e}"
            ))),
            Ok(()) => match self.probe.public_ip().await {
                Err(e) => Err(AttemptError::Retry(format!("public IP probe failed: {e}"))),
                Ok(ip) if Some(ip) == self.isp_ip => Err(AttemptError::Retry(format!(
                    "public IP {ip} is still the ISP address"
                ))),
                Ok(ip) if Some(ip) == avoid_ip => Err(AttemptError::SameIp(ip)),
                Ok(ip) => Ok(ip),
            },
        };
        match verdict {
            Ok(ip) => Ok((handle, ip)),
            Err(error) => {
                let _ = self.killswitch.revoke_interface(handle.interface).await;
                let _ = driver.disconnect(&handle).await;
                Err(error)
            }
        }
    }
}

enum AttemptError {
    /// Try another candidate.
    Retry(String),
    /// The tunnel worked but the public IP did not change; worth keeping as a last resort.
    SameIp(IpAddr),
    /// Stop rotating: continuing could leave traffic unprotected or two tunnels up.
    Fatal(RotationError),
}
