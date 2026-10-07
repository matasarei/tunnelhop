//! The OpenVPN driver — SPEC §4.3.
//!
//! Each tunnel is an `openvpn.exe` the service starts itself, suspended, inside a Job Object
//! that kills it when the service goes, however the service goes. It runs a copy of the
//! profile's config in `tunnels\` with every `remote` pointing at the address the orchestrator
//! resolved, and is driven over its management interface (`ovpn_mgmt`): the driver releases the
//! hold, answers the credential prompt from the DPAPI store, and maps `>STATE:` to tunnel events.
//!
//! The management port is an ordinary local TCP port, so before the driver connects (and later
//! hands over credentials) it checks that the listener belongs to the `openvpn.exe` it started.

use std::ffi::OsString;
use std::path::Path;

use th_core::driver::DriverError;

/// What the listeners on the management port say about who answers there.
#[derive(Debug, PartialEq)]
pub enum PortOwner {
    /// Nobody listens yet: OpenVPN is still starting.
    Nobody,
    /// Our `openvpn.exe` and nothing else.
    Ours,
}

/// Judge the processes listening on the management port against the pid we started.
pub fn port_owner(listeners: &[u32], ours: u32) -> Result<PortOwner, DriverError> {
    match listeners.iter().find(|&&pid| pid != ours) {
        Some(other) => Err(DriverError::Failed(format!(
            "another process ({other}) listens on the management port; not sending credentials"
        ))),
        None if listeners.is_empty() => Ok(PortOwner::Nobody),
        None => Ok(PortOwner::Ours),
    }
}

/// The command line for one tunnel, after SPEC §4.3. `--log` keeps OpenVPN's own account of a
/// failure, which the service's log only summarises.
pub fn arguments(config: &Path, port: u16, password_file: &Path, log: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["--config".into(), config.into()];
    args.extend([
        "--management".into(),
        "127.0.0.1".into(),
        port.to_string().into(),
        password_file.into(),
    ]);
    for flag in [
        "--management-hold",
        "--management-query-passwords",
        "--auth-nocache",
    ] {
        args.push(flag.into());
    }
    args.extend(["--connect-retry-max".into(), "3".into()]);
    args.extend(["--log".into(), log.into()]);
    args
}

/// A fresh management password: 32 random bytes as hex.
pub fn management_password() -> String {
    let bytes: [u8; 32] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(windows)]
pub use driver::{CredentialLookup, OpenvpnDriver};

#[cfg(windows)]
mod driver {
    use std::collections::HashMap;
    use std::io;
    use std::net::{IpAddr, SocketAddr};
    use std::os::windows::io::BorrowedHandle;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use th_core::bundle::BundleId;
    use th_core::driver::{
        DriverError, InterfaceId, ResolvedProfile, TunnelEvent, TunnelHandle, TunnelStatus,
        VpnDriver,
    };
    use th_core::ipc::{Availability, Secret};
    use th_core::manifest::Protocol;
    use th_core::sanitize::Credentials;
    use th_core::sanitize::openvpn::{installed_remotes, rewrite_remotes};
    use th_core::tunnel_name::is_ours;
    use tokio::net::TcpStream;
    use tokio::process::Child;
    use tokio::sync::{broadcast, oneshot};
    use tokio::task::JoinHandle;
    use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
    use zeroize::Zeroizing;

    use super::{PortOwner, arguments, management_password, port_owner};
    use crate::drivers::ovpn_mgmt::{Management, ManagementError, Notification, State};
    use crate::runtimes::Runtime;
    use crate::store::{Layout, write_atomic};
    use crate::win::{self, Job};

    /// From starting `openvpn.exe` to CONNECTED with the interface found. `--connect-retry-max 3`
    /// makes OpenVPN give up on its own well within this.
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
    /// How long a SIGTERM may take before the job is terminated (SPEC §4.3).
    const SIGTERM_TIMEOUT: Duration = Duration::from_secs(10);
    /// How long the process may take to go once the job is terminated.
    const KILL_TIMEOUT: Duration = Duration::from_secs(5);
    const POLL: Duration = Duration::from_millis(100);
    /// The only realm the driver answers: the VPN username and password.
    const AUTH_REALM: &str = "Auth";

    /// Where the driver finds a profile's username and password: the DPAPI store in the service.
    pub type CredentialLookup =
        Arc<dyn Fn(&BundleId, &str) -> Result<Option<Credentials>, String> + Send + Sync>;

    pub struct OpenvpnDriver {
        exe: PathBuf,
        tunnels: PathBuf,
        secrets: PathBuf,
        logs: PathBuf,
        credentials: CredentialLookup,
        events: broadcast::Sender<TunnelEvent>,
        running: Mutex<HashMap<String, Running>>,
    }

    /// A tunnel this driver started and has not torn down.
    struct Running {
        job: Job,
        /// Tells the supervisor to hand back the session and the process for teardown.
        stop: oneshot::Sender<()>,
        supervisor: JoinHandle<(Option<Management<TcpStream>>, Child)>,
        status: Arc<Mutex<TunnelStatus>>,
        interface: u64,
        servers: Vec<IpAddr>,
    }

    /// What the supervisor needs to answer a credential prompt after a reconnect.
    #[derive(Clone)]
    struct Answerer {
        lookup: CredentialLookup,
        bundle: BundleId,
        profile: String,
    }

    impl Answerer {
        async fn answer(
            &self,
            management: &mut Management<TcpStream>,
            realm: &str,
        ) -> Result<(), DriverError> {
            if realm != AUTH_REALM {
                return Err(DriverError::Failed(format!(
                    "OpenVPN asks for '{realm}', which TunnelHop does not supply"
                )));
            }
            let credentials = (self.lookup)(&self.bundle, &self.profile)
                .map_err(DriverError::Failed)?
                .ok_or_else(|| {
                    DriverError::Failed(format!("profile {} needs credentials", self.profile))
                })?;
            let password = Secret::new(credentials.password.clone());
            management
                .send_credentials(realm, &credentials.username, &password)
                .await
                .map_err(management_failed)
        }
    }

    fn management_failed(error: ManagementError) -> DriverError {
        DriverError::Failed(format!("OpenVPN management: {error}"))
    }

    fn failed(context: String, error: io::Error) -> DriverError {
        DriverError::Failed(format!("{context}: {error}"))
    }

    fn guard(tunnel: &str) -> Result<(), DriverError> {
        if is_ours(tunnel) {
            Ok(())
        } else {
            Err(DriverError::Failed(format!(
                "refusing to manage {tunnel:?}: only th- tunnels belong to TunnelHop"
            )))
        }
    }

    fn remove(path: &std::path::Path) {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                log::warn!("could not remove {}: {e}", path.display());
            }
            _ => {}
        }
    }

    impl OpenvpnDriver {
        /// A driver for the discovered OpenVPN runtime, keeping its files in `layout`.
        pub fn new(
            runtime: &Runtime,
            layout: &Layout,
            credentials: CredentialLookup,
        ) -> Result<Self, DriverError> {
            let exe = match (&runtime.availability, &runtime.path) {
                (Availability::Available { .. }, Some(path)) => path.clone(),
                (Availability::TooOld { found, required }, _) => {
                    return Err(DriverError::RuntimeTooOld {
                        runtime: "OpenVPN".into(),
                        found: found.clone(),
                        required: required.clone(),
                    });
                }
                _ => return Err(DriverError::RuntimeMissing("OpenVPN".into())),
            };
            Ok(OpenvpnDriver {
                exe,
                tunnels: layout.tunnels(),
                secrets: layout.secrets(),
                logs: layout.logs(),
                credentials,
                events: broadcast::channel(16).0,
                running: Mutex::default(),
            })
        }

        fn config_path(&self, tunnel: &str) -> PathBuf {
            self.tunnels.join(format!("{tunnel}.ovpn"))
        }

        /// The interface a tunnel ran on, kept so routes can be cleared after a crash.
        fn interface_path(&self, tunnel: &str) -> PathBuf {
            self.tunnels.join(format!("{tunnel}.luid"))
        }

        fn password_path(&self, tunnel: &str) -> PathBuf {
            self.secrets.join(format!("{tunnel}.mgmt"))
        }

        fn take(&self, tunnel: &str) -> Option<Running> {
            self.running
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(tunnel)
        }

        fn remove_files(&self, tunnel: &str) {
            for path in [
                self.config_path(tunnel),
                self.interface_path(tunnel),
                self.password_path(tunnel),
            ] {
                remove(&path);
            }
        }

        /// Start `openvpn.exe` for `profile` and bring it to CONNECTED. On failure everything
        /// started is torn down again.
        async fn start(&self, profile: &ResolvedProfile) -> Result<TunnelHandle, DriverError> {
            let tunnel = profile.tunnel_name.as_str();
            let text = std::fs::read_to_string(&profile.config_path)
                .map_err(|e| failed(format!("cannot read {}", profile.config_path.display()), e))?;
            let config = rewrite_remotes(&text, &profile.endpoints).map_err(DriverError::Failed)?;
            let config_path = self.config_path(tunnel);
            write_atomic(&config_path, config.as_bytes())
                .map_err(|e| failed(format!("cannot write {}", config_path.display()), e))?;

            // From here on a failure before the session exists removes what was written: the
            // config copy holds the profile's private key, the other file a live password.
            let abandon = |error: DriverError| {
                self.remove_files(tunnel);
                error
            };
            let password = Secret::new(management_password());
            let password_path = self.password_path(tunnel);
            let line = Zeroizing::new(format!("{}\n", password.expose()));
            write_atomic(&password_path, line.as_bytes()).map_err(|e| {
                abandon(failed(
                    format!("cannot write {}", password_path.display()),
                    e,
                ))
            })?;
            // A port the OS just handed out; the ownership check below catches anyone who
            // takes it in between.
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .and_then(|l| l.local_addr())
                .map_err(|e| abandon(failed("cannot find a free port".into(), e)))?
                .port();

            let log = self.logs.join(format!("openvpn-{tunnel}.log"));
            let mut child = tokio::process::Command::new(&self.exe)
                .args(arguments(&config_path, port, &password_path, &log))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW)
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| abandon(failed(format!("cannot start {}", self.exe.display()), e)))?;
            // None only once the process has exited, which a suspended one cannot have; 0 would
            // name the idle process to the port check and to the resume.
            let pid = child.id().ok_or_else(|| {
                abandon(DriverError::Failed(
                    "openvpn.exe exited before it was started".into(),
                ))
            })?;
            // Into the job before it runs a single instruction, then let it go.
            let job = Job::kill_on_close()
                .and_then(|job| {
                    let handle = child
                        .raw_handle()
                        .ok_or_else(|| io::Error::other("openvpn.exe has already exited"))?;
                    // SAFETY: the handle belongs to `child`, which outlives this borrow.
                    job.assign(unsafe { BorrowedHandle::borrow_raw(handle) })?;
                    win::resume_process(pid)?;
                    Ok(job)
                })
                .map_err(|e| abandon(failed("cannot start openvpn.exe inside a job".into(), e)))?;

            let servers: Vec<IpAddr> = profile.endpoints.iter().map(|e| e.addr.ip()).collect();
            let answerer = Answerer {
                lookup: self.credentials.clone(),
                bundle: profile.bundle.clone(),
                profile: profile.profile_id.clone(),
            };
            let mut management = None;
            let connecting = connect(
                &mut child,
                pid,
                port,
                &password,
                &password_path,
                &answerer,
                &mut management,
            );
            let outcome = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
                .await
                .unwrap_or_else(|_| {
                    Err(DriverError::Timeout(format!(
                        "OpenVPN did not connect within {} s",
                        CONNECT_TIMEOUT.as_secs()
                    )))
                });
            remove(&password_path);
            let interface = match outcome {
                Ok(interface) => interface,
                Err(e) => {
                    let detail = format!("{e} (OpenVPN's log: {})", log.display());
                    // The interface is unknown, but the host routes to the servers may be there.
                    teardown(tunnel, management, child, &job, 0, &servers).await;
                    drop(job);
                    self.remove_files(tunnel);
                    return Err(match e {
                        DriverError::Timeout(_) => DriverError::Timeout(detail),
                        _ => DriverError::Failed(detail),
                    });
                }
            };
            if let Err(e) = write_atomic(
                &self.interface_path(tunnel),
                interface.to_string().as_bytes(),
            ) {
                log::warn!("could not record the interface of {tunnel}: {e}");
            }

            let management = management.expect("connect leaves the session on success");
            let status = Arc::new(Mutex::new(TunnelStatus::Up));
            let (stop, stopped) = oneshot::channel();
            let supervisor = tokio::spawn(supervise(
                tunnel.to_string(),
                management,
                child,
                answerer,
                status.clone(),
                self.events.clone(),
                stopped,
            ));
            let previous = self
                .running
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    tunnel.to_string(),
                    Running {
                        job,
                        stop,
                        supervisor,
                        status,
                        interface,
                        servers,
                    },
                );
            // connect() stops a tunnel of the same name first, and the engine never connects
            // twice at once, so this is a broken invariant rather than a path. Its supervisor
            // goes first, or the old process's exit would read as this tunnel being lost; its
            // job then ends the process. No SIGTERM: OpenVPN would remove the host route to the
            // server on its way out, which the new tunnel shares.
            if let Some(previous) = previous {
                log::error!("{tunnel} was already running; ending the older openvpn.exe");
                previous.supervisor.abort();
                drop(previous.job);
            }
            Ok(TunnelHandle {
                profile_id: profile.profile_id.clone(),
                protocol: Protocol::Openvpn,
                tunnel_name: tunnel.to_string(),
                interface: InterfaceId(interface),
                endpoints: profile.endpoints.clone(),
            })
        }

        /// Tear a running tunnel down; returns once its process is gone.
        async fn stop(&self, tunnel: &str) {
            let Some(running) = self.take(tunnel) else {
                self.remove_files(tunnel);
                return;
            };
            let _ = running.stop.send(());
            match running.supervisor.await {
                Ok((management, child)) => {
                    teardown(
                        tunnel,
                        management,
                        child,
                        &running.job,
                        running.interface,
                        &running.servers,
                    )
                    .await;
                }
                Err(e) => {
                    // The process went with the supervisor's panic: end it through the job.
                    log::error!("the supervisor of {tunnel} failed: {e}");
                    if let Err(e) = running.job.terminate() {
                        log::warn!("could not terminate the job of {tunnel}: {e}");
                    }
                    clear_routes(tunnel, running.interface, &running.servers);
                }
            }
            drop(running.job);
            self.remove_files(tunnel);
        }
    }

    /// Wait for our `openvpn.exe` to listen, log in, release the hold and answer prompts until
    /// CONNECTED. Returns the tunnel's interface; leaves the session in `session`, also on
    /// failure, so the teardown can ask OpenVPN to exit cleanly.
    async fn connect(
        child: &mut Child,
        pid: u32,
        port: u16,
        password: &Secret,
        password_path: &std::path::Path,
        answerer: &Answerer,
        session: &mut Option<Management<TcpStream>>,
    ) -> Result<u64, DriverError> {
        loop {
            let listeners =
                win::tcp_listeners(port).map_err(|e| failed("cannot list listeners".into(), e))?;
            if port_owner(&listeners, pid)? == PortOwner::Ours {
                break;
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|e| failed("openvpn.exe".into(), e))?
            {
                return Err(DriverError::Failed(format!(
                    "openvpn.exe exited with {status} before opening its management port"
                )));
            }
            tokio::time::sleep(POLL).await;
        }
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let management = session.insert(
            Management::connect(address, password)
                .await
                .map_err(management_failed)?,
        );
        // Logged in: OpenVPN has read the file, and nothing else needs it.
        remove(password_path);
        management.start().await.map_err(management_failed)?;

        let local_ip = loop {
            match management
                .next_notification()
                .await
                .map_err(management_failed)?
            {
                Notification::State(change) => match change.state {
                    State::Connected => {
                        break change.local_ip.ok_or_else(|| {
                            DriverError::Failed("OpenVPN connected without an address".into())
                        })?;
                    }
                    State::Exiting => {
                        return Err(DriverError::Failed(format!(
                            "OpenVPN exited while connecting ({})",
                            change.detail
                        )));
                    }
                    _ => {}
                },
                Notification::Hold => management
                    .command("hold release")
                    .await
                    .map(|_| ())
                    .map_err(management_failed)?,
                Notification::NeedCredentials { realm } => {
                    answerer.answer(management, &realm).await?
                }
                Notification::CredentialsRefused { .. } => {
                    return Err(DriverError::Failed(
                        "the server refused the username and password".into(),
                    ));
                }
                Notification::Fatal(message) => {
                    return Err(DriverError::Failed(format!("OpenVPN gave up: {message}")));
                }
                Notification::Other(_) => {}
            }
        };
        // CONNECTED comes while the address is still tentative (Windows checks it is not a
        // duplicate, about 2 s): nothing gets through until it is usable, so a probe made at
        // once fails with "host unreachable" (seen in th-test). "Up" means usable.
        loop {
            match win::interface_with_address(local_ip)
                .map_err(|e| failed("cannot list addresses".into(), e))?
            {
                Some((luid, true)) => return Ok(luid),
                _ => tokio::time::sleep(POLL).await,
            }
        }
    }

    /// What one turn of the supervisor saw.
    enum Step {
        Stop,
        Exited(io::Result<std::process::ExitStatus>),
        Notification(Result<Notification, ManagementError>),
    }

    /// Follow a connected tunnel until it is told to stop or goes down on its own; reports
    /// reconnects and losses, and answers the prompts a reconnect brings (`--auth-nocache` makes
    /// OpenVPN ask for the credentials again). Hands the session and the process back for the
    /// teardown.
    async fn supervise(
        tunnel: String,
        mut management: Management<TcpStream>,
        mut child: Child,
        answerer: Answerer,
        status: Arc<Mutex<TunnelStatus>>,
        events: broadcast::Sender<TunnelEvent>,
        mut stop: oneshot::Receiver<()>,
    ) -> (Option<Management<TcpStream>>, Child) {
        let set = |value: TunnelStatus| *status.lock().unwrap_or_else(|e| e.into_inner()) = value;
        let lost = |reason: String| {
            log::warn!("{tunnel} was lost: {reason}");
            set(TunnelStatus::Down);
            let _ = events.send(TunnelEvent::Down {
                tunnel: tunnel.clone(),
                reason,
            });
        };
        let mut reconnecting = false;
        loop {
            let step = tokio::select! {
                _ = &mut stop => Step::Stop,
                exited = child.wait() => Step::Exited(exited),
                notification = management.next_notification() => Step::Notification(notification),
            };
            let result: Result<(), String> = match step {
                Step::Stop => return (Some(management), child),
                Step::Exited(exited) => {
                    let how = exited.map_or_else(|e| e.to_string(), |s| s.to_string());
                    lost(format!("openvpn.exe exited ({how})"));
                    return (None, child);
                }
                Step::Notification(Err(e)) => Err(format!("the management connection failed: {e}")),
                Step::Notification(Ok(notification)) => match notification {
                    Notification::State(change) => match change.state {
                        State::Reconnecting => {
                            set(TunnelStatus::Reconnecting);
                            if !reconnecting {
                                reconnecting = true;
                                let _ = events.send(TunnelEvent::Reconnecting {
                                    tunnel: tunnel.clone(),
                                });
                            }
                            Ok(())
                        }
                        State::Connected => {
                            set(TunnelStatus::Up);
                            if reconnecting {
                                reconnecting = false;
                                let _ = events.send(TunnelEvent::Up {
                                    tunnel: tunnel.clone(),
                                });
                            }
                            Ok(())
                        }
                        State::Exiting => Err(format!("OpenVPN is exiting ({})", change.detail)),
                        State::Other(_) => Ok(()),
                    },
                    Notification::Hold => management
                        .command("hold release")
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string()),
                    Notification::NeedCredentials { realm } => answerer
                        .answer(&mut management, &realm)
                        .await
                        .map_err(|e| e.to_string()),
                    Notification::CredentialsRefused { .. } => {
                        Err("the server refused the username and password".into())
                    }
                    Notification::Fatal(message) => Err(format!("OpenVPN gave up: {message}")),
                    Notification::Other(_) => Ok(()),
                },
            };
            if let Err(reason) = result {
                lost(reason);
                return (Some(management), child);
            }
        }
    }

    /// Ask OpenVPN to exit, which removes its routes; terminate the job if it has not gone
    /// within the timeout, and then clear the routes it could not.
    async fn teardown(
        tunnel: &str,
        management: Option<Management<TcpStream>>,
        mut child: Child,
        job: &Job,
        interface: u64,
        servers: &[IpAddr],
    ) {
        // Without a session nobody can ask it to exit: only a process that has already gone
        // is spared the job's termination.
        let mut grace = Duration::ZERO;
        if let Some(mut management) = management {
            grace = SIGTERM_TIMEOUT;
            match tokio::time::timeout(SIGTERM_TIMEOUT, management.sigterm()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => log::warn!("{tunnel}: SIGTERM was not accepted: {e}"),
                Err(_) => log::warn!("{tunnel}: SIGTERM got no answer"),
            }
        }
        if tokio::time::timeout(grace, child.wait()).await.is_ok() {
            return;
        }
        if !grace.is_zero() {
            log::warn!(
                "{tunnel}: openvpn.exe did not exit within {} s of SIGTERM; terminating it",
                grace.as_secs()
            );
        }
        if let Err(e) = job.terminate() {
            log::warn!("could not terminate the job of {tunnel}: {e}");
        }
        if tokio::time::timeout(KILL_TIMEOUT, child.wait())
            .await
            .is_err()
        {
            log::error!("{tunnel}: openvpn.exe is still running after the job was terminated");
        }
        clear_routes(tunnel, interface, servers);
    }

    /// Remove what a killed OpenVPN leaves in the route table.
    fn clear_routes(tunnel: &str, interface: u64, servers: &[IpAddr]) {
        match win::delete_tunnel_routes(interface, servers) {
            Ok(0) => {}
            Ok(count) => log::info!("{tunnel}: removed {count} routes left behind"),
            Err(e) => log::warn!("{tunnel}: could not remove the routes left behind: {e}"),
        }
    }

    #[async_trait]
    impl VpnDriver for OpenvpnDriver {
        async fn connect(&self, profile: &ResolvedProfile) -> Result<TunnelHandle, DriverError> {
            guard(&profile.tunnel_name)?;
            // A tunnel of the same name (a reconnect to the same profile) goes first.
            self.stop(&profile.tunnel_name).await;
            self.start(profile).await
        }

        async fn disconnect(&self, handle: &TunnelHandle) -> Result<(), DriverError> {
            guard(&handle.tunnel_name)?;
            // No event: events report what a tunnel did on its own (see the WireGuard driver).
            self.stop(&handle.tunnel_name).await;
            Ok(())
        }

        async fn status(&self, handle: &TunnelHandle) -> Result<TunnelStatus, DriverError> {
            guard(&handle.tunnel_name)?;
            let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            Ok(running
                .get(&handle.tunnel_name)
                .map_or(TunnelStatus::Down, |r| {
                    *r.status.lock().unwrap_or_else(|e| e.into_inner())
                }))
        }

        fn events(&self) -> broadcast::Receiver<TunnelEvent> {
            self.events.subscribe()
        }

        /// Never adopts: an `openvpn.exe` dies with the job when the service does, so nothing
        /// of a previous run is still connected. What it may have left is cleared instead: the
        /// routes of a tunnel that was killed rather than stopped, and its files.
        async fn reconcile(&self) -> Result<Vec<TunnelHandle>, DriverError> {
            let entries = std::fs::read_dir(&self.tunnels)
                .map_err(|e| failed(format!("cannot list {}", self.tunnels.display()), e))?;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "ovpn") {
                    continue;
                }
                let Some(tunnel) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if !is_ours(tunnel) {
                    continue;
                }
                let servers: Vec<IpAddr> = std::fs::read_to_string(&path)
                    .map(|text| {
                        installed_remotes(&text)
                            .iter()
                            .map(|e| e.addr.ip())
                            .collect()
                    })
                    .unwrap_or_default();
                let interface = std::fs::read_to_string(self.interface_path(tunnel))
                    .ok()
                    .and_then(|text| text.trim().parse::<u64>().ok())
                    .unwrap_or(0);
                // A dead tunnel's adapter is down; one that is up has been taken by another
                // OpenVPN since (the user's own, perhaps), whose routes are not ours.
                let interface = match win::interface_is_up(interface) {
                    Ok(false) => interface,
                    _ => 0,
                };
                log::info!("clearing what the previous run's {tunnel} left");
                clear_routes(tunnel, interface, &servers);
                self.remove_files(tunnel);
            }
            Ok(Vec::new())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn only_th_tunnels_are_managed() {
            assert!(guard("th-0123456789ab").is_ok());
            for name in ["openvpn", "MyHomeVPN", "th-", "th-../x", "TH-0123"] {
                assert!(guard(name).is_err(), "{name}");
            }
        }

        #[tokio::test]
        async fn a_failed_start_leaves_no_config_copy_or_password_behind() {
            // An openvpn.exe that does not exist fails at spawn: nothing runs, so this is safe
            // anywhere.
            let root = tempfile::tempdir().unwrap();
            let layout = Layout::new(root.path());
            for dir in [layout.tunnels(), layout.secrets(), layout.logs()] {
                std::fs::create_dir_all(dir).unwrap();
            }
            let config = root.path().join("lab.ovpn");
            std::fs::write(&config, "client\nremote vpn.example.test 1194\n").unwrap();
            let runtime = Runtime {
                protocol: Protocol::Openvpn,
                path: Some(root.path().join("missing").join("openvpn.exe")),
                availability: Availability::Available {
                    version: "2.7.7".into(),
                },
            };
            let lookup: CredentialLookup = Arc::new(|_, _| Ok(None));
            let driver = OpenvpnDriver::new(&runtime, &layout, lookup).unwrap();
            let bundle = BundleId::parse("0123456789abcdef").unwrap();
            let profile = ResolvedProfile {
                bundle,
                profile_id: "lab".into(),
                protocol: Protocol::Openvpn,
                config_path: config,
                tunnel_name: "th-0123456789ab".into(),
                endpoints: vec![th_core::driver::ResolvedEndpoint {
                    addr: "192.0.2.1:1194".parse().unwrap(),
                    transport: th_core::sanitize::Transport::Udp,
                }],
            };

            let error = driver.connect(&profile).await.unwrap_err();
            assert!(error.to_string().contains("cannot start"), "{error}");
            for dir in [layout.tunnels(), layout.secrets()] {
                let left: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
                assert!(left.is_empty(), "{} still holds {left:?}", dir.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_port_is_ours_only_when_our_process_alone_listens() {
        assert_eq!(port_owner(&[], 42).unwrap(), PortOwner::Nobody);
        assert_eq!(port_owner(&[42], 42).unwrap(), PortOwner::Ours);
        assert_eq!(port_owner(&[42, 42], 42).unwrap(), PortOwner::Ours);
        for squatted in [&[7][..], &[42, 7], &[7, 42]] {
            let error = port_owner(squatted, 42).unwrap_err().to_string();
            assert!(error.contains("(7)"), "{error}");
        }
    }

    #[test]
    fn the_command_line_follows_the_spec() {
        let args = arguments(
            Path::new(r"C:\ProgramData\TunnelHop\tunnels\th-0123456789ab.ovpn"),
            50123,
            Path::new(r"C:\ProgramData\TunnelHop\secrets\th-0123456789ab.mgmt"),
            Path::new(r"C:\ProgramData\TunnelHop\logs\openvpn-th-0123456789ab.log"),
        );
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let joined = args.join(" ");
        assert!(joined.starts_with(
            r"--config C:\ProgramData\TunnelHop\tunnels\th-0123456789ab.ovpn --management 127.0.0.1 50123 C:\ProgramData\TunnelHop\secrets\th-0123456789ab.mgmt"
        ), "{joined}");
        for flag in [
            "--management-hold",
            "--management-query-passwords",
            "--auth-nocache",
        ] {
            assert!(args.iter().any(|a| a == flag), "{flag} missing: {joined}");
        }
        assert!(joined.contains("--connect-retry-max 3"), "{joined}");
    }

    #[test]
    fn management_passwords_are_fresh_hex() {
        let (a, b) = (management_password(), management_password());
        assert_eq!(a.len(), 64);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(a, b);
    }
}
