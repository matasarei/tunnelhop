//! Platform-independent core of TunnelHop: bundle manifest, safe import, config sanitizers,
//! rotation engine and the IPC protocol between the UI and the service.

pub mod bundle;
pub mod driver;
pub mod edit;
pub mod ipc;
pub mod manifest;
pub mod rotation;
pub mod sanitize;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod tunnel_name;
