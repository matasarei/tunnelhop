//! The interface every VPN runtime driver implements — SPEC §4.1 — and the types it trades in.

use std::net::SocketAddr;
use std::path::PathBuf;

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::broadcast;

use crate::bundle::BundleId;
use crate::manifest::Protocol;
use crate::sanitize::Transport;

/// An endpoint after name resolution: what the kill-switch permits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResolvedEndpoint {
    pub addr: SocketAddr,
    pub transport: Transport,
}

/// The network interface a tunnel runs on (a Windows interface LUID).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InterfaceId(pub u64);

/// Everything a driver needs to bring a profile up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub bundle: BundleId,
    pub profile_id: String,
    pub protocol: Protocol,
    /// Absolute path of the installed, sanitized config.
    pub config_path: PathBuf,
    /// The generated WireGuard tunnel name; also used to label OpenVPN tunnels.
    pub tunnel_name: String,
    pub endpoints: Vec<ResolvedEndpoint>,
}

/// A tunnel that is up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelHandle {
    pub profile_id: String,
    pub protocol: Protocol,
    pub tunnel_name: String,
    pub interface: InterfaceId,
    pub endpoints: Vec<ResolvedEndpoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelStatus {
    Up,
    Reconnecting,
    Down,
}

/// Something a tunnel did on its own, identified by its tunnel name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelEvent {
    Up { tunnel: String },
    Reconnecting { tunnel: String },
    Down { tunnel: String, reason: String },
}

impl TunnelEvent {
    pub fn tunnel(&self) -> &str {
        match self {
            TunnelEvent::Up { tunnel }
            | TunnelEvent::Reconnecting { tunnel }
            | TunnelEvent::Down { tunnel, .. } => tunnel,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DriverError {
    #[error("{0} is not installed")]
    RuntimeMissing(String),
    #[error("{runtime} {found} is too old; {required} or newer is required")]
    RuntimeTooOld {
        runtime: String,
        found: String,
        required: String,
    },
    /// The server took the connection and then refused the username and password. Another
    /// try with the same ones cannot end differently.
    #[error("the server refused the username and password")]
    CredentialsRefused,
    #[error("timed out: {0}")]
    Timeout(String),
    #[error("{0}")]
    Failed(String),
}

#[async_trait]
pub trait VpnDriver: Send + Sync {
    /// Bring the profile up. Returns once the tunnel's interface exists and is up.
    async fn connect(&self, profile: &ResolvedProfile) -> Result<TunnelHandle, DriverError>;

    /// Tear the tunnel down. Returns only after teardown is confirmed; succeeds when the tunnel
    /// is already gone.
    async fn disconnect(&self, handle: &TunnelHandle) -> Result<(), DriverError>;

    async fn status(&self, handle: &TunnelHandle) -> Result<TunnelStatus, DriverError>;

    fn events(&self) -> broadcast::Receiver<TunnelEvent>;

    /// Tunnels left behind by a previous run of the service.
    async fn reconcile(&self) -> Result<Vec<TunnelHandle>, DriverError>;
}
