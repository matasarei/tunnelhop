//! The WireGuard driver — SPEC §4.2.
//!
//! Each tunnel is a `WireGuardTunnel$<name>` service that `wireguard.exe` installs from a config
//! in `tunnels\`. That copy has every peer's `Endpoint` rewritten to the address the orchestrator
//! resolved, so WireGuard never looks the name up again and lands on an address the kill-switch
//! does not permit. Only tunnels named `th-…` are ever touched: the others belong to the user.

use std::net::SocketAddr;

use th_core::driver::DriverError;

pub const SERVICE_PREFIX: &str = "WireGuardTunnel$";

/// The service WireGuard runs a tunnel as.
pub fn service_name(tunnel: &str) -> String {
    format!("{SERVICE_PREFIX}{tunnel}")
}

#[derive(Clone, Copy, PartialEq)]
enum Section {
    Other,
    Peer,
}

/// What a config line is, ignoring case, surrounding space and a `#` comment.
fn classify(line: &str) -> (Section, Option<String>) {
    let content = line.split('#').next().unwrap_or("").trim();
    if content.starts_with('[') {
        let section = if content.eq_ignore_ascii_case("[peer]") {
            Section::Peer
        } else {
            Section::Other
        };
        return (section, None);
    }
    let key = content
        .split_once('=')
        .map(|(key, _)| key.trim().to_ascii_lowercase());
    (Section::Other, key)
}

/// Rewrite each `[Peer]`'s `Endpoint` to `resolved[i]`, peers in file order. The sanitizer has
/// already required exactly one `Endpoint` per peer; anything else here is an error rather than
/// a guess. Every other line is kept as it was.
pub fn rewrite_endpoints(config: &str, resolved: &[SocketAddr]) -> Result<String, DriverError> {
    let mut out = String::with_capacity(config.len());
    let mut in_peer = false;
    let mut peers = 0usize;
    let mut rewritten = 0usize;
    for line in config.lines() {
        let (section, key) = classify(line);
        if line.trim_start().starts_with('[') {
            in_peer = section == Section::Peer;
            peers += usize::from(in_peer);
        }
        if in_peer && key.as_deref() == Some("endpoint") {
            let address = resolved.get(rewritten).ok_or_else(|| {
                DriverError::Failed(format!(
                    "the config has more peer endpoints than the {} resolved",
                    resolved.len()
                ))
            })?;
            out.push_str(&format!("Endpoint = {address}"));
            rewritten += 1;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if rewritten != peers || rewritten != resolved.len() {
        return Err(DriverError::Failed(format!(
            "the config has {peers} peers and {rewritten} endpoints, but {} were resolved",
            resolved.len()
        )));
    }
    Ok(out)
}

/// The endpoints written in an installed config: after [`rewrite_endpoints`] they are
/// addresses, which is how a restarted service learns what a leftover tunnel talks to.
pub fn installed_endpoints(config: &str) -> Vec<SocketAddr> {
    let mut in_peer = false;
    let mut endpoints = Vec::new();
    for line in config.lines() {
        let (section, key) = classify(line);
        if line.trim_start().starts_with('[') {
            in_peer = section == Section::Peer;
        }
        if in_peer && key.as_deref() == Some("endpoint") {
            let value = line.split('#').next().unwrap_or("");
            if let Some(address) = value
                .split_once('=')
                .and_then(|(_, v)| v.trim().parse().ok())
            {
                endpoints.push(address);
            }
        }
    }
    endpoints
}

#[cfg(windows)]
pub use driver::WireguardDriver;

#[cfg(windows)]
mod driver {
    use std::collections::HashMap;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;
    use th_core::driver::{
        DriverError, InterfaceId, ResolvedEndpoint, ResolvedProfile, TunnelEvent, TunnelHandle,
        TunnelStatus, VpnDriver,
    };
    use th_core::ipc::Availability;
    use th_core::manifest::Protocol;
    use th_core::sanitize::Transport;
    use th_core::tunnel_name::is_ours;
    use tokio::sync::broadcast;
    use tokio::task::AbortHandle;
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_sys::Win32::Foundation::ERROR_SERVICE_DOES_NOT_EXIST;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    use super::{SERVICE_PREFIX, installed_endpoints, rewrite_endpoints, service_name};
    use crate::runtimes::Runtime;
    use crate::store::write_atomic;
    use crate::win;

    /// How long the tunnel service may take to start, or to disappear.
    const SERVICE_TIMEOUT: Duration = Duration::from_secs(15);
    const POLL: Duration = Duration::from_millis(200);
    /// How often a connected tunnel's service is checked for having stopped on its own.
    const WATCH_INTERVAL: Duration = Duration::from_secs(2);

    pub struct WireguardDriver {
        exe: PathBuf,
        tunnels: PathBuf,
        events: broadcast::Sender<TunnelEvent>,
        /// One watcher per connected tunnel, by name. Cancelled before every uninstall, so a
        /// disconnect the engine asked for never reads as a lost tunnel.
        watchers: Mutex<HashMap<String, AbortHandle>>,
    }

    /// What a watcher does after one look at its tunnel's service.
    #[derive(Debug, PartialEq)]
    pub(super) enum Watch {
        Keep,
        Lost(String),
    }

    /// The service stopping, or vanishing, is the tunnel lost. Starting and continuing are
    /// transient; an error reading the state proves nothing either way.
    pub(super) fn watch(state: Result<Option<ServiceState>, String>) -> Watch {
        match state {
            Ok(Some(
                ServiceState::Running | ServiceState::StartPending | ServiceState::ContinuePending,
            )) => Watch::Keep,
            Ok(Some(state)) => Watch::Lost(format!("the tunnel service is {state:?}")),
            Ok(None) => Watch::Lost("the tunnel service is gone".into()),
            Err(e) => {
                log::warn!("cannot check a tunnel's service: {e}");
                Watch::Keep
            }
        }
    }

    impl Drop for WireguardDriver {
        fn drop(&mut self) {
            let watchers = self.watchers.get_mut().unwrap_or_else(|e| e.into_inner());
            for (_, watcher) in watchers.drain() {
                watcher.abort();
            }
        }
    }

    impl WireguardDriver {
        /// A driver for the discovered WireGuard runtime, writing configs into `tunnels`.
        pub fn new(runtime: &Runtime, tunnels: impl Into<PathBuf>) -> Result<Self, DriverError> {
            let exe = match (&runtime.availability, &runtime.path) {
                (Availability::Available { .. }, Some(path)) => path.clone(),
                (Availability::TooOld { found, required }, _) => {
                    return Err(DriverError::RuntimeTooOld {
                        runtime: "WireGuard".into(),
                        found: found.clone(),
                        required: required.clone(),
                    });
                }
                _ => return Err(DriverError::RuntimeMissing("WireGuard".into())),
            };
            Ok(WireguardDriver {
                exe,
                tunnels: tunnels.into(),
                events: broadcast::channel(16).0,
                watchers: Mutex::default(),
            })
        }

        /// Report the tunnel lost once its service stops without being asked to.
        fn watch(&self, tunnel: &str) {
            let (name, events) = (tunnel.to_string(), self.events.clone());
            let task = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(WATCH_INTERVAL).await;
                    let state = service_state(&name).map_err(|e| e.to_string());
                    if let Watch::Lost(reason) = watch(state) {
                        log::warn!("{name} was lost: {reason}");
                        let _ = events.send(TunnelEvent::Down {
                            tunnel: name,
                            reason,
                        });
                        return;
                    }
                }
            });
            let mut watchers = self.watchers.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(previous) = watchers.insert(tunnel.to_string(), task.abort_handle()) {
                previous.abort();
            }
        }

        fn unwatch(&self, tunnel: &str) {
            let mut watchers = self.watchers.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(watcher) = watchers.remove(tunnel) {
                watcher.abort();
            }
        }

        fn config_path(&self, tunnel: &str) -> PathBuf {
            self.tunnels.join(format!("{tunnel}.conf"))
        }

        async fn run(&self, arguments: &[&std::ffi::OsStr]) -> Result<(), DriverError> {
            let command = arguments
                .first()
                .map(|a| a.to_string_lossy().into_owned())
                .unwrap_or_default();
            // Bounded: a hung wireguard.exe would otherwise stall a rotation for good, with the
            // kill-switch engaged. Dropping the future on timeout kills the process.
            let output = tokio::time::timeout(
                SERVICE_TIMEOUT,
                tokio::process::Command::new(&self.exe)
                    .args(arguments)
                    .creation_flags(CREATE_NO_WINDOW)
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .map_err(|_| {
                DriverError::Timeout(format!(
                    "wireguard.exe {command} did not finish within {} s",
                    SERVICE_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|e| failed(format!("cannot run {}", self.exe.display()), e))?;
            if output.status.success() {
                return Ok(());
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(DriverError::Failed(format!(
                "wireguard.exe {command} exited with {}: {}",
                output.status,
                stderr.trim()
            )))
        }

        /// Wait until the tunnel's service reaches `wanted`; `None` means "no longer exists".
        async fn wait_for(
            &self,
            tunnel: &str,
            wanted: Option<ServiceState>,
        ) -> Result<(), DriverError> {
            let deadline = tokio::time::Instant::now() + SERVICE_TIMEOUT;
            loop {
                let state = service_state(tunnel)?;
                if state == wanted {
                    return Ok(());
                }
                if wanted.is_some() && state == Some(ServiceState::Stopped) {
                    return Err(DriverError::Failed(format!(
                        "the tunnel service for {tunnel} stopped instead of starting"
                    )));
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(DriverError::Timeout(format!(
                        "{} did not reach {wanted:?} within {} s (last {state:?})",
                        service_name(tunnel),
                        SERVICE_TIMEOUT.as_secs()
                    )));
                }
                tokio::time::sleep(POLL).await;
            }
        }

        /// The interface appears shortly after the service reports running.
        async fn interface(&self, tunnel: &str) -> Result<InterfaceId, DriverError> {
            let deadline = tokio::time::Instant::now() + SERVICE_TIMEOUT;
            loop {
                match win::interface_luid(tunnel) {
                    Ok(luid) => return Ok(InterfaceId(luid)),
                    Err(e) if tokio::time::Instant::now() >= deadline => {
                        return Err(failed(format!("no interface for {tunnel}"), e));
                    }
                    Err(_) => tokio::time::sleep(POLL).await,
                }
            }
        }

        async fn uninstall(&self, tunnel: &str) -> Result<(), DriverError> {
            // First, so the watcher cannot see the service go and call it lost.
            self.unwatch(tunnel);
            if service_state(tunnel)?.is_some() {
                self.run(&["/uninstalltunnelservice".as_ref(), tunnel.as_ref()])
                    .await?;
                self.wait_for(tunnel, None).await?;
            }
            match std::fs::remove_file(self.config_path(tunnel)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => {
                    log::warn!("could not remove the config of {tunnel}: {e}");
                }
                _ => {}
            }
            Ok(())
        }
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

    /// The state of the tunnel's service, `None` when there is no such service.
    fn service_state(tunnel: &str) -> Result<Option<ServiceState>, DriverError> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
            .map_err(|e| DriverError::Failed(format!("cannot open the SCM: {e}")))?;
        let service = match manager.open_service(service_name(tunnel), ServiceAccess::QUERY_STATUS)
        {
            Ok(service) => service,
            Err(windows_service::Error::Winapi(e))
                if e.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(DriverError::Failed(format!("cannot open {tunnel}: {e}"))),
        };
        let status = service
            .query_status()
            .map_err(|e| DriverError::Failed(format!("cannot query {tunnel}: {e}")))?;
        Ok(Some(status.current_state))
    }

    fn status_of(state: Option<ServiceState>) -> TunnelStatus {
        match state {
            Some(ServiceState::Running) => TunnelStatus::Up,
            Some(ServiceState::StartPending | ServiceState::ContinuePending) => {
                TunnelStatus::Reconnecting
            }
            _ => TunnelStatus::Down,
        }
    }

    fn endpoints_of(path: &Path) -> Vec<ResolvedEndpoint> {
        std::fs::read_to_string(path)
            .map(|text| {
                installed_endpoints(&text)
                    .into_iter()
                    .map(|addr| ResolvedEndpoint {
                        addr,
                        transport: Transport::Udp,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[async_trait]
    impl VpnDriver for WireguardDriver {
        async fn connect(&self, profile: &ResolvedProfile) -> Result<TunnelHandle, DriverError> {
            let tunnel = profile.tunnel_name.as_str();
            guard(tunnel)?;
            // A leftover with this name (a crash mid-rotation) would make the install fail.
            self.uninstall(tunnel).await?;

            let text = std::fs::read_to_string(&profile.config_path)
                .map_err(|e| failed(format!("cannot read {}", profile.config_path.display()), e))?;
            let addresses: Vec<_> = profile.endpoints.iter().map(|e| e.addr).collect();
            let config = rewrite_endpoints(&text, &addresses)?;
            let path = self.config_path(tunnel);
            write_atomic(&path, config.as_bytes())
                .map_err(|e| failed(format!("cannot write {}", path.display()), e))?;

            self.run(&["/installtunnelservice".as_ref(), path.as_os_str()])
                .await?;
            let up = async {
                self.wait_for(tunnel, Some(ServiceState::Running)).await?;
                self.interface(tunnel).await
            };
            let interface = match up.await {
                Ok(interface) => interface,
                Err(e) => {
                    // Leave nothing half-installed behind.
                    if let Err(cleanup) = self.uninstall(tunnel).await {
                        log::warn!("could not remove the failed tunnel {tunnel}: {cleanup}");
                    }
                    return Err(e);
                }
            };
            self.watch(tunnel);
            Ok(TunnelHandle {
                profile_id: profile.profile_id.clone(),
                protocol: Protocol::Wireguard,
                tunnel_name: tunnel.to_string(),
                interface,
                endpoints: profile.endpoints.clone(),
            })
        }

        async fn disconnect(&self, handle: &TunnelHandle) -> Result<(), DriverError> {
            guard(&handle.tunnel_name)?;
            // No event: events report what a tunnel did on its own. A `Down` for a disconnect
            // the engine asked for would read as a lost tunnel once a tunnel with the same name
            // is back up (a one-profile bundle, `reconnect_same`) and rotate it again.
            self.uninstall(&handle.tunnel_name).await
        }

        async fn status(&self, handle: &TunnelHandle) -> Result<TunnelStatus, DriverError> {
            guard(&handle.tunnel_name)?;
            Ok(status_of(service_state(&handle.tunnel_name)?))
        }

        fn events(&self) -> broadcast::Receiver<TunnelEvent> {
            self.events.subscribe()
        }

        /// Every `WireGuardTunnel$th-…` service, each watched from here on, since the engine may
        /// adopt it. The profile id is left empty: the orchestrator maps the tunnel name back
        /// through `tunnel_name(bundle, profile)`.
        async fn reconcile(&self) -> Result<Vec<TunnelHandle>, DriverError> {
            let services = win::services_with_prefix(&format!("{SERVICE_PREFIX}th-"))
                .map_err(|e| failed("cannot list services".into(), e))?;
            let mut handles = Vec::new();
            for service in services {
                let tunnel = &service[SERVICE_PREFIX.len()..];
                if !is_ours(tunnel) {
                    continue;
                }
                let interface = win::interface_luid(tunnel)
                    .map(InterfaceId)
                    .unwrap_or(InterfaceId(0));
                self.watch(tunnel);
                handles.push(TunnelHandle {
                    profile_id: String::new(),
                    protocol: Protocol::Wireguard,
                    tunnel_name: tunnel.to_string(),
                    interface,
                    endpoints: endpoints_of(&self.config_path(tunnel)),
                });
            }
            Ok(handles)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn service_states_map_to_tunnel_status() {
            assert_eq!(status_of(Some(ServiceState::Running)), TunnelStatus::Up);
            assert_eq!(
                status_of(Some(ServiceState::StartPending)),
                TunnelStatus::Reconnecting
            );
            assert_eq!(
                status_of(Some(ServiceState::StopPending)),
                TunnelStatus::Down
            );
            assert_eq!(status_of(Some(ServiceState::Stopped)), TunnelStatus::Down);
            assert_eq!(status_of(None), TunnelStatus::Down);
        }

        #[test]
        fn only_th_tunnels_are_managed() {
            assert!(guard("th-0123456789ab").is_ok());
            for name in ["wg0", "MyHomeVPN", "th-", "th-../x", "TH-0123"] {
                assert!(guard(name).is_err(), "{name}");
            }
        }

        #[test]
        fn a_tunnel_that_does_not_exist_has_no_service() {
            assert_eq!(service_state("th-000000000000").unwrap(), None);
        }

        #[test]
        fn a_watcher_calls_the_tunnel_lost_only_when_its_service_stops_or_vanishes() {
            for state in [
                ServiceState::Running,
                ServiceState::StartPending,
                ServiceState::ContinuePending,
            ] {
                assert_eq!(watch(Ok(Some(state))), Watch::Keep, "{state:?}");
            }
            for state in [
                ServiceState::Stopped,
                ServiceState::StopPending,
                ServiceState::Paused,
            ] {
                assert!(
                    matches!(watch(Ok(Some(state))), Watch::Lost(_)),
                    "{state:?}"
                );
            }
            assert!(matches!(watch(Ok(None)), Watch::Lost(_)));
            assert_eq!(watch(Err("access denied".into())), Watch::Keep);
        }

        #[tokio::test]
        async fn unwatching_a_tunnel_stops_its_watcher() {
            let runtime = Runtime {
                protocol: Protocol::Wireguard,
                path: Some(PathBuf::from(r"C:\nowhere\wireguard.exe")),
                availability: Availability::Available {
                    version: "1.1.1".into(),
                },
            };
            let driver = WireguardDriver::new(&runtime, std::env::temp_dir()).unwrap();
            let mut events = driver.events();
            // The tunnel does not exist, so an active watcher would report it lost.
            driver.watch("th-000000000000");
            driver.unwatch("th-000000000000");
            let heard = tokio::time::timeout(WATCH_INTERVAL * 2, events.recv()).await;
            assert!(heard.is_err(), "an unwatched tunnel produced {heard:?}");

            driver.watch("th-000000000000");
            let heard = tokio::time::timeout(WATCH_INTERVAL * 2, events.recv()).await;
            assert!(
                matches!(heard, Ok(Ok(TunnelEvent::Down { .. }))),
                "a watched tunnel that is gone was not reported: {heard:?}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "[Interface]\r\n\
        PrivateKey = aGVsbG8gd29ybGQgaGVsbG8gd29ybGQgaGVsbG8gd29ybGQ=\r\n\
        Address = 10.77.1.2/32\r\n\
        \r\n\
        [Peer] # first\r\n\
        PublicKey = Zm9vYmFyZm9vYmFyZm9vYmFyZm9vYmFyZm9vYmFyZm8=\r\n\
        AllowedIPs = 0.0.0.0/0, ::/0\r\n\
        endpoint = vpn.example.test:51820 # by name\r\n\
        \r\n\
        [peer]\r\n\
        PublicKey = YmFyZm9vYmFyZm9vYmFyZm9vYmFyZm9vYmFyZm9vYmE=\r\n\
        Endpoint=[2001:db8::1]:51821\r\n";

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn each_peer_endpoint_becomes_its_resolved_address() {
        let resolved = [addr("203.0.113.5:51820"), addr("[2001:db8::7]:51821")];
        let out = rewrite_endpoints(CONFIG, &resolved).unwrap();
        assert!(out.contains("Endpoint = 203.0.113.5:51820\n"), "{out}");
        assert!(out.contains("Endpoint = [2001:db8::7]:51821\n"), "{out}");
        assert!(!out.contains("vpn.example.test"));
        assert!(!out.contains('\r'));
        // Everything else survives untouched.
        assert!(out.contains("AllowedIPs = 0.0.0.0/0, ::/0\n"));
        assert!(out.contains("[Peer] # first\n"));
        assert_eq!(installed_endpoints(&out), resolved);
    }

    #[test]
    fn a_count_mismatch_is_an_error_not_a_guess() {
        assert!(rewrite_endpoints(CONFIG, &[addr("203.0.113.5:51820")]).is_err());
        let three = [
            addr("203.0.113.5:51820"),
            addr("203.0.113.6:51820"),
            addr("203.0.113.7:51820"),
        ];
        assert!(rewrite_endpoints(CONFIG, &three).is_err());
    }

    #[test]
    fn a_peer_without_an_endpoint_is_an_error() {
        let config = "[Interface]\nPrivateKey = x\n[Peer]\nPublicKey = y\n";
        assert!(rewrite_endpoints(config, &[addr("203.0.113.5:51820")]).is_err());
        assert!(rewrite_endpoints(config, &[]).is_err());
    }

    #[test]
    fn an_endpoint_key_outside_a_peer_is_left_alone() {
        let config = "[Interface]\n# Endpoint = not.a.peer:1\nPrivateKey = x\n\
                      [Peer]\nPublicKey = y\nEndpoint = a.test:1\n";
        let out = rewrite_endpoints(config, &[addr("198.51.100.1:1")]).unwrap();
        assert!(out.contains("# Endpoint = not.a.peer:1\n"));
        assert!(out.contains("Endpoint = 198.51.100.1:1\n"));
    }

    #[test]
    fn names_in_an_installed_config_are_not_endpoints() {
        assert!(
            installed_endpoints(CONFIG)
                .iter()
                .all(|a| a.port() == 51821)
        );
        assert_eq!(installed_endpoints(CONFIG).len(), 1);
    }

    #[test]
    fn the_service_name_carries_the_tunnel() {
        assert_eq!(
            service_name("th-0123456789ab"),
            "WireGuardTunnel$th-0123456789ab"
        );
    }
}
