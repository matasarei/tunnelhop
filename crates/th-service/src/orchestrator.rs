//! The orchestrator: the rotation engine wired to the real world — PLAN.md "Wiring the engine".
//!
//! It holds the active bundle and the user's settings, builds a [`Rotator`] for the bundle with
//! the installed drivers, the kill-switch and the probe, answers the pipe's requests, runs the
//! interval timer and reacts to tunnels that drop. Every change it makes is published to the
//! pipe's clients as an `Event::Status`, and the ones the user must see as an `Event::Alert`.
//!
//! It is generic over the parts that touch Windows (the vault, the drivers, the kill-switch and
//! the probe arrive as traits), so its logic is tested anywhere with th-core's test doubles.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rand::SeedableRng;
use rand::rngs::StdRng;
use th_core::bundle::{self, BundleId, ImportedBundle, MAX_BUNDLES};
use th_core::driver::{ResolvedEndpoint, ResolvedProfile, TunnelEvent, TunnelHandle, VpnDriver};
use th_core::ipc::{self, BundleInfo, Event, Request, Response, RuntimeInfo, StatusReport};
use th_core::manifest::Protocol;
use th_core::rotation::{KillSwitch, Overrides, Probe, Resolver, Rotator, State, Status, Trigger};
use th_core::sanitize::Endpoint;
use th_core::tunnel_name::tunnel_name;
use tokio::sync::{Mutex, broadcast, mpsc};

use crate::credentials::{CredentialStore, Vault, bundle_info};
use crate::store::{Layout, Settings};

/// The default interval when neither the user nor a bundle set one.
const DEFAULT_INTERVAL_MINUTES: u32 = 60;
/// How often the timer and the reconnect grace are checked.
const TICK: Duration = Duration::from_secs(1);

/// Everything the orchestrator is built from.
pub struct Parts<V> {
    pub layout: Layout,
    pub settings: Settings,
    pub credentials: CredentialStore<V>,
    pub runtimes: Vec<RuntimeInfo>,
    /// Only the protocols whose runtime is installed and new enough.
    pub drivers: HashMap<Protocol, Arc<dyn VpnDriver>>,
    pub killswitch: Arc<dyn KillSwitch>,
    /// Whether our block was found in place at start (a crash left it).
    pub killswitch_engaged: bool,
    pub probe: Arc<dyn Probe>,
    /// Seeds rotation's random choices; `None` seeds from the OS.
    pub seed: Option<u64>,
}

pub struct Orchestrator<V> {
    layout: Layout,
    credentials: CredentialStore<V>,
    runtimes: Vec<RuntimeInfo>,
    drivers: HashMap<Protocol, Arc<dyn VpnDriver>>,
    killswitch: Arc<dyn KillSwitch>,
    probe: Arc<dyn Probe>,
    seed: Option<u64>,
    events: broadcast::Sender<Event>,
    inner: Mutex<Inner>,
    /// The latest report, readable while a rotation holds `inner`.
    report: StdMutex<StatusReport>,
}

struct Inner {
    settings: Settings,
    active: Option<Active>,
    /// Our block is in place with no bundle active (a crash left it): only Disconnect lifts it.
    orphan_block: bool,
}

struct Active {
    bundle: ImportedBundle,
    rotator: Rotator,
}

impl<V: Vault + Send + Sync + 'static> Orchestrator<V> {
    /// Build from `parts`, reopen the active bundle, and take over what a previous run left:
    /// a tunnel still up is adopted, any other `th-` tunnel is removed, and a block in place
    /// stays (the state is then `Blocked`).
    pub async fn start(parts: Parts<V>) -> Arc<Self> {
        let orchestrator = Arc::new(Orchestrator {
            layout: parts.layout,
            credentials: parts.credentials,
            runtimes: parts.runtimes,
            drivers: parts.drivers,
            killswitch: parts.killswitch,
            probe: parts.probe,
            seed: parts.seed,
            events: broadcast::channel(64).0,
            inner: Mutex::new(Inner {
                settings: parts.settings,
                active: None,
                orphan_block: false,
            }),
            report: StdMutex::new(empty_report(Vec::new())),
        });
        {
            let mut inner = orchestrator.inner.lock().await;
            let bundle = inner
                .settings
                .active_bundle
                .as_deref()
                .and_then(BundleId::parse)
                .and_then(
                    |id| match bundle::open(&orchestrator.layout.bundles(), &id) {
                        Ok(bundle) => Some(bundle),
                        Err(errors) => {
                            log::warn!("the active bundle {id} cannot be opened: {errors:?}");
                            None
                        }
                    },
                );
            if bundle.is_none() {
                inner.settings.active_bundle = None;
            }
            let leftover = orchestrator.reconcile(bundle.as_ref()).await;
            match bundle {
                Some(bundle) => {
                    let mut rotator = orchestrator.rotator(&bundle, &inner.settings);
                    rotator.adopt(leftover, parts.killswitch_engaged, Instant::now());
                    inner.active = Some(Active { bundle, rotator });
                }
                None => inner.orphan_block = parts.killswitch_engaged,
            }
            orchestrator.publish(&inner);
        }
        orchestrator
    }

    /// The sender the pipe hands to its clients.
    pub fn events(&self) -> broadcast::Sender<Event> {
        self.events.clone()
    }

    /// The latest status, without waiting for a rotation in progress.
    pub fn report(&self) -> StatusReport {
        self.report
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Run the interval timer, the reconnect grace and the drivers' events until the returned
    /// task is aborted.
    pub fn spawn_watch(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let (forward, mut events) = mpsc::channel::<TunnelEvent>(64);
        for driver in self.drivers.values() {
            let mut source = driver.events();
            let forward = forward.clone();
            tokio::spawn(async move {
                loop {
                    match source.recv().await {
                        Ok(event) => {
                            if forward.send(event).await.is_err() {
                                return;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
            });
        }
        let this = self.clone();
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(TICK);
            loop {
                tokio::select! {
                    _ = ticks.tick() => this.tick(Instant::now()).await,
                    Some(event) = events.recv() => this.on_tunnel_event(&event, Instant::now()).await,
                }
            }
        })
    }

    /// Rotate when the interval is due, or when a tunnel has been reconnecting past its grace.
    pub async fn tick(&self, now: Instant) {
        let mut inner = self.inner.lock().await;
        let Some(active) = inner.active.as_mut() else {
            return;
        };
        let trigger = if active.rotator.due(now) {
            Some(Trigger::Timer)
        } else {
            active.rotator.check_grace(now)
        };
        if let Some(trigger) = trigger {
            self.rotate(&mut inner, trigger, now).await;
        }
    }

    pub async fn on_tunnel_event(&self, event: &TunnelEvent, now: Instant) {
        let mut inner = self.inner.lock().await;
        let Some(active) = inner.active.as_mut() else {
            return;
        };
        let state_before = active.rotator.status().state;
        match active.rotator.on_event(event, now) {
            Some(trigger) => {
                log::warn!("{} was lost; rotating", event.tunnel());
                self.rotate(&mut inner, trigger, now).await;
            }
            // `stay_down`: the engine blocked instead of rotating.
            None if state_before != State::Blocked
                && active.rotator.status().state == State::Blocked =>
            {
                self.alert("The tunnel went down and traffic is blocked.");
                self.publish(&inner);
            }
            None => {}
        }
    }

    /// What the service does before it stops: bring the tunnel down and lift the block.
    pub async fn shutdown(&self) {
        let mut inner = self.inner.lock().await;
        if let Err(e) = self.disconnect(&mut inner).await {
            log::error!("could not disconnect while stopping: {e}");
        }
    }

    /// Answer one validated request from the pipe.
    pub async fn handle(&self, request: Request) -> Response {
        // From the cached report, before the lock: a rotation holds the lock for as long as a
        // tunnel takes to connect, and the UI must not freeze meanwhile.
        match request {
            Request::GetStatus => return self.status(),
            Request::ListBundles => return self.list(),
            _ => {}
        }
        let mut inner = self.inner.lock().await;
        let now = Instant::now();
        match request {
            Request::GetStatus => self.status(),
            Request::ImportBundle { zip_base64 } => self.import(&inner, &zip_base64).await,
            Request::SelectBundle { bundle_id } => self.select(&mut inner, &bundle_id).await,
            Request::ListBundles => self.list(),
            Request::DeleteBundle { bundle_id } => self.delete(&mut inner, &bundle_id).await,
            Request::Connect { profile_id } => {
                if inner.active.is_none() {
                    return error("import and select a bundle first");
                }
                if let Some(id) = &profile_id {
                    inner.settings.selected_profile = Some(id.clone());
                    self.save(&inner.settings);
                }
                let active = inner.active.as_mut().expect("checked above");
                let result = match &profile_id {
                    Some(id) => active.rotator.connect_to(id, now).await,
                    None => active.rotator.rotate(Trigger::Manual, now).await,
                };
                self.finish(&inner, result)
            }
            Request::RotateNow => {
                if inner.active.is_none() {
                    return error("import and select a bundle first");
                }
                self.rotate(&mut inner, Trigger::Manual, now).await
            }
            Request::SetAutoRotate { enabled } => {
                inner.settings.auto_rotate = Some(enabled);
                self.apply_overrides(&mut inner)
            }
            Request::SetInterval { minutes } => {
                inner.settings.interval_minutes = Some(minutes);
                self.apply_overrides(&mut inner)
            }
            Request::Disconnect | Request::Shutdown { disconnect: true } => {
                match self.disconnect(&mut inner).await {
                    Ok(()) => {
                        self.publish(&inner);
                        self.status()
                    }
                    Err(e) => {
                        self.publish(&inner);
                        error(e)
                    }
                }
            }
            Request::Shutdown { disconnect: false } => Response::Ok,
            Request::SetCredentials {
                bundle_id,
                profile_ids,
                username,
                password,
            } => {
                let Some(id) = BundleId::parse(&bundle_id) else {
                    return error("bundle id must be 16 lowercase hex characters");
                };
                if let Err(e) = self
                    .credentials
                    .apply(&id, &profile_ids, &username, &password)
                {
                    return error(e);
                }
                if let Some(active) = inner.active.as_mut()
                    && active.bundle.id == id
                {
                    let missing = self.missing_credentials(&active.bundle);
                    active.rotator.set_missing_credentials(missing);
                }
                self.publish(&inner);
                self.status()
            }
            // Answered by the pipe itself.
            Request::Hello { .. } | Request::Ping => error("not a request for the service"),
        }
    }

    fn status(&self) -> Response {
        Response::Status {
            report: self.report(),
        }
    }

    async fn import(&self, inner: &Inner, zip_base64: &str) -> Response {
        let bytes = match ipc::decode_zip(zip_base64) {
            Ok(bytes) => bytes,
            Err(e) => return error(e),
        };
        let root = self.layout.bundles();
        // The same zip again is not another bundle, so it is never what the cap refuses.
        match bundle::installed(&root) {
            Ok(installed)
                if installed.len() >= MAX_BUNDLES && !installed.contains(&BundleId::of(&bytes)) =>
            {
                return error(format!(
                    "{MAX_BUNDLES} bundles are installed, the most TunnelHop keeps: delete a \
                     bundle first"
                ));
            }
            Ok(_) => {}
            Err(e) => return error(format!("the installed bundles cannot be read: {e}")),
        }
        let imported =
            match tokio::task::spawn_blocking(move || bundle::import(&bytes, &root)).await {
                Ok(Ok(imported)) => imported,
                Ok(Err(errors)) => {
                    return Response::ImportFailed {
                        errors: errors.iter().map(ToString::to_string).collect(),
                    };
                }
                Err(e) => return error(format!("the import failed: {e}")),
            };
        if let Err(e) = self.credentials.store_imported(&imported) {
            return error(format!("the bundle's credentials could not be stored: {e}"));
        }
        log::info!(
            "imported bundle {} ({})",
            imported.id,
            imported.manifest.bundle_name
        );
        let missing = self.missing_credentials(&imported);
        self.publish(inner);
        Response::Imported {
            bundle: bundle_info(
                &imported.id,
                &imported.manifest,
                &imported.needs_credentials,
                |p| self.credentials.has(&imported.id, p),
            ),
            already_present: imported.already_present,
            needs_credentials: imported
                .needs_credentials
                .iter()
                .filter(|p| missing.contains(*p))
                .cloned()
                .collect(),
        }
    }

    async fn select(&self, inner: &mut Inner, bundle_id: &str) -> Response {
        let Some(id) = BundleId::parse(bundle_id) else {
            return error("bundle id must be 16 lowercase hex characters");
        };
        if inner.active.as_ref().is_some_and(|a| a.bundle.id == id) {
            return self.status();
        }
        let bundle = match bundle::open(&self.layout.bundles(), &id) {
            Ok(bundle) => bundle,
            Err(_) => return error(format!("bundle {id} is not installed; import it first")),
        };
        // The old bundle's tunnel and block go before the new bundle takes over.
        if let Err(e) = self.disconnect(inner).await {
            self.publish(inner);
            return error(format!("could not leave the current bundle: {e}"));
        }
        inner.settings.active_bundle = Some(id.as_str().to_string());
        inner.settings.selected_profile = None;
        self.save(&inner.settings);
        let rotator = self.rotator(&bundle, &inner.settings);
        log::info!("selected bundle {id} ({})", bundle.manifest.bundle_name);
        inner.active = Some(Active { bundle, rotator });
        self.publish(inner);
        self.status()
    }

    /// Every installed bundle, ordered by name, and the active one's id. The active id comes
    /// from the cached report, so this never waits for a rotation in progress.
    fn list(&self) -> Response {
        let root = self.layout.bundles();
        let ids = match bundle::installed(&root) {
            Ok(ids) => ids,
            Err(e) => return error(format!("the installed bundles cannot be read: {e}")),
        };
        let mut bundles: Vec<BundleInfo> = ids
            .iter()
            .map(|id| match bundle::open(&root, id) {
                Ok(bundle) => bundle_info(id, &bundle.manifest, &bundle.needs_credentials, |p| {
                    self.credentials.has(id, p)
                }),
                // Still listed, so that it can be deleted.
                Err(errors) => {
                    log::warn!("installed bundle {id} cannot be opened: {errors:?}");
                    BundleInfo {
                        bundle_id: id.as_str().to_string(),
                        bundle_name: "Damaged bundle".into(),
                        profiles: Vec::new(),
                    }
                }
            })
            .collect();
        bundles.sort_by_cached_key(|b| (b.bundle_name.to_lowercase(), b.bundle_id.clone()));
        Response::Bundles {
            bundles,
            active: self.report().bundle.map(|b| b.bundle_id),
        }
    }

    /// Remove a bundle: its stored credentials first, then its files. The active bundle is
    /// disconnected before anything is removed, and no bundle is active afterwards.
    async fn delete(&self, inner: &mut Inner, bundle_id: &str) -> Response {
        let Some(id) = BundleId::parse(bundle_id) else {
            return error("bundle id must be 16 lowercase hex characters");
        };
        let root = self.layout.bundles();
        match bundle::installed(&root) {
            Ok(installed) if installed.contains(&id) => {}
            Ok(_) => return error(format!("bundle {id} is not installed")),
            Err(e) => return error(format!("the installed bundles cannot be read: {e}")),
        }
        if inner.active.as_ref().is_some_and(|a| a.bundle.id == id) {
            if let Err(e) = self.disconnect(inner).await {
                self.publish(inner);
                return error(format!(
                    "could not disconnect, so the bundle was not deleted: {e}"
                ));
            }
            inner.active = None;
            inner.settings.active_bundle = None;
            inner.settings.selected_profile = None;
            self.save(&inner.settings);
            self.publish(inner);
        }
        if let Err(e) = self.credentials.remove_bundle(&id) {
            return error(format!(
                "the bundle's credentials could not be removed, so it was not deleted: {e}"
            ));
        }
        if let Err(e) = bundle::remove(&root, &id) {
            return error(format!("the bundle's files could not be removed: {e}"));
        }
        log::info!("deleted bundle {id}");
        self.list()
    }

    async fn rotate(&self, inner: &mut Inner, trigger: Trigger, now: Instant) -> Response {
        let active = inner.active.as_mut().expect("callers check for a bundle");
        let result = active.rotator.rotate(trigger, now).await;
        self.finish(inner, result)
    }

    /// Publish what an engine operation ended in, and tell the user what they must know.
    fn finish(
        &self,
        inner: &Inner,
        result: Result<Status, th_core::rotation::RotationError>,
    ) -> Response {
        let outcome = match result {
            Ok(status) => {
                if status.ip_changed == Some(false) {
                    self.alert(
                        "Rotated, but the public IP did not change: no other profile gave a new one.",
                    );
                }
                log::info!(
                    "connected to {} ({})",
                    status.profile_id.as_deref().unwrap_or("-"),
                    status
                        .public_ip
                        .map_or_else(|| "no IP".into(), |ip| ip.to_string())
                );
                Ok(())
            }
            Err(e) => {
                log::error!("rotation failed: {e}");
                let blocked = inner
                    .active
                    .as_ref()
                    .is_some_and(|a| a.rotator.status().state == State::Blocked);
                if blocked {
                    self.alert(&format!("Every attempt failed; traffic is blocked. ({e})"));
                }
                Err(e.to_string())
            }
        };
        self.publish(inner);
        match outcome {
            Ok(()) => self.status(),
            Err(e) => error(e),
        }
    }

    async fn disconnect(&self, inner: &mut Inner) -> Result<(), String> {
        if let Some(active) = inner.active.as_mut() {
            active
                .rotator
                .disconnect()
                .await
                .map_err(|e| e.to_string())?;
        } else if inner.orphan_block {
            self.killswitch.disengage().await?;
        }
        inner.orphan_block = false;
        Ok(())
    }

    fn apply_overrides(&self, inner: &mut Inner) -> Response {
        self.save(&inner.settings);
        let overrides = overrides(&inner.settings);
        if let Some(active) = inner.active.as_mut() {
            active.rotator.set_overrides(overrides);
        }
        self.publish(inner);
        self.status()
    }

    fn save(&self, settings: &Settings) {
        if let Err(e) = settings.save(&self.layout.settings_file()) {
            log::warn!("could not save settings.json: {e}");
        }
    }

    fn rotator(&self, bundle: &ImportedBundle, settings: &Settings) -> Rotator {
        let rng = match self.seed {
            Some(seed) => StdRng::seed_from_u64(seed),
            None => StdRng::from_rng(&mut rand::rng()),
        };
        let mut rotator = Rotator::new(
            bundle.manifest.clone(),
            overrides(settings),
            self.drivers.clone(),
            self.killswitch.clone(),
            self.probe.clone(),
            Arc::new(BundleResolver::new(bundle)),
            rng,
        );
        rotator.set_missing_credentials(self.missing_credentials(bundle));
        rotator.set_entry_hosts(entry_hosts(&bundle.endpoints));
        rotator
    }

    /// Profiles whose runtime will ask for credentials that the store cannot answer.
    fn missing_credentials(&self, bundle: &ImportedBundle) -> HashSet<String> {
        bundle
            .needs_credentials
            .iter()
            .filter(|p| !self.credentials.has(&bundle.id, p))
            .cloned()
            .collect()
    }

    /// Take over the tunnels a previous run left: the active bundle's one is returned to be
    /// adopted; any other of ours is removed.
    async fn reconcile(&self, bundle: Option<&ImportedBundle>) -> Option<TunnelHandle> {
        let names: HashMap<String, String> = bundle
            .map(|b| {
                b.manifest
                    .profiles
                    .iter()
                    .map(|p| (tunnel_name(&b.id, &p.id), p.id.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let mut adopted = None;
        for driver in self.drivers.values() {
            let leftovers = match driver.reconcile().await {
                Ok(leftovers) => leftovers,
                Err(e) => {
                    log::warn!("could not look for leftover tunnels: {e}");
                    continue;
                }
            };
            for mut handle in leftovers {
                match names.get(&handle.tunnel_name) {
                    Some(profile) if adopted.is_none() => {
                        log::info!("adopting {} ({profile})", handle.tunnel_name);
                        handle.profile_id = profile.clone();
                        adopted = Some(handle);
                    }
                    _ => {
                        log::info!("removing leftover tunnel {}", handle.tunnel_name);
                        if let Err(e) = driver.disconnect(&handle).await {
                            log::warn!("could not remove {}: {e}", handle.tunnel_name);
                        }
                    }
                }
            }
        }
        adopted
    }

    /// Refresh the report and send it to every client.
    fn publish(&self, inner: &Inner) {
        let report = match &inner.active {
            Some(active) => {
                let status = active.rotator.status();
                StatusReport {
                    state: status.state,
                    profile_id: status.profile_id,
                    public_ip: status.public_ip,
                    killswitch_engaged: status.killswitch_engaged,
                    bundle: Some(bundle_info(
                        &active.bundle.id,
                        &active.bundle.manifest,
                        &active.bundle.needs_credentials,
                        |p| self.credentials.has(&active.bundle.id, p),
                    )),
                    auto_rotate: active.rotator.auto_rotate(),
                    interval_minutes: (active.rotator.interval().as_secs() / 60) as u32,
                    runtimes: self.runtimes.clone(),
                }
            }
            None => StatusReport {
                state: if inner.orphan_block {
                    State::Blocked
                } else {
                    State::Disconnected
                },
                killswitch_engaged: inner.orphan_block,
                auto_rotate: inner.settings.auto_rotate.unwrap_or(true),
                interval_minutes: inner
                    .settings
                    .interval_minutes
                    .unwrap_or(DEFAULT_INTERVAL_MINUTES),
                ..empty_report(self.runtimes.clone())
            },
        };
        *self.report.lock().unwrap_or_else(|e| e.into_inner()) = report.clone();
        let _ = self.events.send(Event::Status { report });
    }

    fn alert(&self, message: &str) {
        log::warn!("alert: {message}");
        let _ = self.events.send(Event::Alert {
            message: message.to_string(),
        });
    }
}

fn error(message: impl ToString) -> Response {
    Response::Error {
        message: message.to_string(),
    }
}

fn empty_report(runtimes: Vec<RuntimeInfo>) -> StatusReport {
    StatusReport {
        state: State::Disconnected,
        profile_id: None,
        public_ip: None,
        killswitch_engaged: false,
        bundle: None,
        auto_rotate: true,
        interval_minutes: DEFAULT_INTERVAL_MINUTES,
        runtimes,
    }
}

fn overrides(settings: &Settings) -> Overrides {
    Overrides {
        auto_rotate: settings.auto_rotate,
        interval_minutes: settings.interval_minutes,
    }
}

/// Each profile's server hosts as written, which the engine uses to prefer a different entry.
fn entry_hosts(endpoints: &BTreeMap<String, Vec<Endpoint>>) -> HashMap<String, Vec<String>> {
    endpoints
        .iter()
        .map(|(profile, list)| {
            (
                profile.clone(),
                list.iter().map(|e| e.host.clone()).collect(),
            )
        })
        .collect()
}

/// Turns a profile of the active bundle into what a driver connects: its installed config,
/// its tunnel name, and each endpoint resolved to one address (IPv4 first), in config order.
struct BundleResolver {
    id: BundleId,
    dir: PathBuf,
    profiles: HashMap<String, (Protocol, String)>,
    endpoints: BTreeMap<String, Vec<Endpoint>>,
}

impl BundleResolver {
    fn new(bundle: &ImportedBundle) -> Self {
        BundleResolver {
            id: bundle.id.clone(),
            dir: bundle.dir.clone(),
            profiles: bundle
                .manifest
                .profiles
                .iter()
                .map(|p| (p.id.clone(), (p.protocol, p.file.clone())))
                .collect(),
            endpoints: bundle.endpoints.clone(),
        }
    }
}

#[async_trait]
impl Resolver for BundleResolver {
    async fn resolve(&self, profile_id: &str) -> Result<ResolvedProfile, String> {
        let (protocol, file) = self
            .profiles
            .get(profile_id)
            .ok_or_else(|| format!("no profile \"{profile_id}\" in the bundle"))?;
        let mut endpoints = Vec::new();
        for endpoint in self.endpoints.get(profile_id).into_iter().flatten() {
            let target = (endpoint.host.as_str(), endpoint.port);
            let addresses: Vec<_> = tokio::net::lookup_host(target)
                .await
                .map_err(|e| format!("cannot resolve {}: {e}", endpoint.host))?
                .collect();
            let addr = addresses
                .iter()
                .find(|a| a.is_ipv4())
                .or(addresses.first())
                .copied()
                .ok_or_else(|| format!("{} has no address", endpoint.host))?;
            endpoints.push(ResolvedEndpoint {
                addr,
                transport: endpoint.transport,
            });
        }
        Ok(ResolvedProfile {
            bundle: self.id.clone(),
            profile_id: profile_id.to_string(),
            protocol: *protocol,
            config_path: self.dir.join(file),
            tunnel_name: tunnel_name(&self.id, profile_id),
            endpoints,
        })
    }
}

#[cfg(windows)]
impl<V: Vault + Send + Sync + 'static> crate::pipe::Handler for Orchestrator<V> {
    async fn handle(&self, request: Request) -> Response {
        Orchestrator::handle(self, request).await
    }
}
