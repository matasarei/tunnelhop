//! TunnelHop's privileged service: the parts `main.rs` and the integration tests share.
//!
//! Windows only, apart from the pure logic, which also builds elsewhere so it can be tested.

pub mod credentials;
pub mod logging;
pub mod runtimes;
pub mod store;
#[cfg(windows)]
pub mod svc;
#[cfg(windows)]
pub mod win;
