//! Platform-independent core of TunnelHop: bundle manifest, safe import, config sanitizers,
//! rotation engine and the IPC protocol between the UI and the service.

pub mod bundle;
pub mod manifest;
pub mod sanitize;
pub mod tunnel_name;
