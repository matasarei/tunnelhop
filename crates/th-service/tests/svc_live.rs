//! The service's own install and uninstall, against the real service manager. Never on the
//! host PC: only inside the `th-test` VM, from a checkpoint where TunnelHop is not installed
//! (DEVELOPMENT.md §5):
//!
//! ```powershell
//! scripts\vm.ps1 copy
//! scripts\vm.ps1 test svc_live
//! ```
//!
//! It drives the release `th-service.exe` that `copy` puts in `C:\th`: `install` registers the
//! running executable, which in a test would be the test binary.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use windows_service::service::{ServiceAccess, ServiceState};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

const SERVICE: &str = "TunnelHop";

fn in_test_vm() {
    assert_eq!(
        std::env::var("TH_TEST_VM").as_deref(),
        Ok("1"),
        "svc_live runs only in th-test, through scripts\\vm.ps1"
    );
}

fn service_exe() -> PathBuf {
    std::env::var_os("TH_SERVICE_EXE")
        .map_or_else(|| PathBuf::from(r"C:\th\th-service.exe"), PathBuf::from)
}

fn run(args: &[&str]) {
    let output = Command::new(service_exe()).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "th-service {args:?} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The service's state and process id, or `None` when it is not registered. Asked of the
/// service manager itself: `sc.exe` words its answer in the display language.
fn query() -> Option<(ServiceState, u32)> {
    const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
    let manager =
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).unwrap();
    let service = match manager.open_service(SERVICE, ServiceAccess::QUERY_STATUS) {
        Ok(service) => service,
        Err(windows_service::Error::Winapi(e))
            if e.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST) =>
        {
            return None;
        }
        Err(e) => panic!("cannot open {SERVICE}: {e}"),
    };
    let status = service.query_status().unwrap();
    Some((status.current_state, status.process_id.unwrap_or(0)))
}

/// Wait until the service runs, in another process than `not`; its process id.
fn running(not: Option<u32>, within: Duration) -> u32 {
    let deadline = Instant::now() + within;
    loop {
        if let Some((state, pid)) = query()
            && state == ServiceState::Running
            && pid != 0
            && Some(pid) != not
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "{SERVICE} was not running within {within:?}: {:?}",
            query()
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var("ProgramData").unwrap()).join("TunnelHop")
}

#[test]
#[ignore = "installs a Windows service: th-test VM only"]
fn the_service_installs_restarts_after_a_crash_and_uninstalls_without_a_trace() {
    in_test_vm();
    assert_eq!(
        query(),
        None,
        "{SERVICE} is already installed: restore a checkpoint without it"
    );

    run(&["install"]);
    let first = running(None, Duration::from_secs(30));
    assert!(data_dir().is_dir(), "the service made its data directory");

    // A crash: the service manager starts it again after its 5 s delay.
    let killed = Command::new("taskkill")
        .args(["/F", "/PID", &first.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    running(Some(first), Duration::from_secs(30));

    // Installing over an installed service updates it and keeps it running.
    run(&["install"]);
    running(None, Duration::from_secs(30));

    run(&["uninstall"]);
    assert_eq!(query(), None, "the service is gone");
    assert!(!data_dir().exists(), "and so is its data");
    // A second uninstall finds nothing to do and says so without failing.
    run(&["uninstall"]);
}
