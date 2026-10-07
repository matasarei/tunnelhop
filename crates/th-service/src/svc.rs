//! The Windows service: registration with the service control manager, and the control handler
//! (STOP and PRESHUTDOWN) — SPEC §6.

use std::error::Error;
use std::ffi::OsString;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
    ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

use crate::credentials::{CredentialStore, Dpapi};
use crate::store::{self, Layout, Settings};
use crate::{logging, runtimes};

pub const SERVICE_NAME: &str = "TunnelHop";
const DISPLAY_NAME: &str = "TunnelHop";
const DESCRIPTION: &str = "Rotates WireGuard and OpenVPN tunnels and keeps traffic blocked while \
                           it switches.";
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

define_windows_service!(ffi_service_main, service_main);

/// Hand the process to the service control manager. Only works when the SCM started us.
pub fn run() -> windows_service::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(e) = run_service() {
        log::error!("service failed: {}", describe(e.as_ref()));
    }
}

/// An error and its causes, outermost first: `windows-service` keeps the OS error in `source`.
pub fn describe(error: &dyn Error) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(inner) = cause {
        text.push_str(": ");
        text.push_str(&inner.to_string());
        cause = inner.source();
    }
    text
}

fn run_service() -> Result<(), Box<dyn Error>> {
    let (stop_tx, stop_rx) = mpsc::channel();
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Preshutdown => {
            let _ = stop_tx.send(control);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = service_control_handler::register(SERVICE_NAME, handler)?;
    let report = |state, accept, exit_code, wait_hint| {
        status.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: state,
            controls_accepted: accept,
            exit_code,
            checkpoint: 0,
            wait_hint,
            process_id: None,
        })
    };
    report(
        ServiceState::StartPending,
        ServiceControlAccept::empty(),
        ServiceExitCode::Win32(0),
        Duration::from_secs(10),
    )?;

    if let Err(e) = start() {
        // The SCM records the code; the log, if it could be opened, has the reason.
        log::error!("could not start: {}", describe(e.as_ref()));
        report(
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            ServiceExitCode::ServiceSpecific(1),
            Duration::ZERO,
        )?;
        return Err(e);
    }

    report(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::PRESHUTDOWN,
        ServiceExitCode::Win32(0),
        Duration::ZERO,
    )?;
    log::info!("running");

    let control = stop_rx.recv().unwrap_or(ServiceControl::Stop);
    report(
        ServiceState::StopPending,
        ServiceControlAccept::empty(),
        ServiceExitCode::Win32(0),
        Duration::from_secs(20),
    )?;
    // From step 13 on: disconnect the tunnel and disengage the kill-switch here.
    log::info!("stopping ({control:?})");
    report(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        ServiceExitCode::Win32(0),
        Duration::ZERO,
    )?;
    Ok(())
}

/// Prepare the store, open the log, and look at what is installed.
fn start() -> Result<(), Box<dyn Error>> {
    let layout = Layout::system()?;
    store::prepare(&layout)?;
    logging::to_file(&layout.logs())?;
    log::info!(
        "starting th-service {} in {}",
        env!("CARGO_PKG_VERSION"),
        layout.root().display()
    );

    match Settings::load(&layout.settings_file()) {
        Ok(settings) => log::info!("settings: {settings:?}"),
        Err(e) => log::warn!("settings.json is unreadable, using defaults: {e}"),
    }
    let _credentials = CredentialStore::new(layout.credentials(), Dpapi);
    for runtime in runtimes::discover() {
        log::info!(
            "{:?} runtime: {:?} at {}",
            runtime.protocol,
            runtime.availability,
            runtime
                .path
                .as_deref()
                .map_or_else(|| "-".into(), |p| p.display().to_string())
        );
    }
    Ok(())
}

/// Register the service to start automatically as LocalSystem, restart it if it crashes, and
/// start it now.
pub fn install() -> Result<(), Box<dyn Error>> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;
    let info = ServiceInfo {
        name: SERVICE_NAME.into(),
        display_name: DISPLAY_NAME.into(),
        service_type: SERVICE_TYPE,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: std::env::current_exe()?,
        launch_arguments: vec!["run".into()],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let service = manager.create_service(
        &info,
        ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::QUERY_STATUS,
    )?;
    service.set_description(DESCRIPTION)?;
    let restart = ServiceAction {
        action_type: ServiceActionType::Restart,
        delay: Duration::from_secs(5),
    };
    service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 60 * 60)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![restart.clone(), restart.clone(), restart]),
    })?;
    service.start::<&str>(&[])?;
    log::info!("installed and started {SERVICE_NAME}");
    Ok(())
}

/// Stop and delete the service, then remove `%ProgramData%\TunnelHop`.
pub fn uninstall() -> Result<(), Box<dyn Error>> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::STOP | ServiceAccess::QUERY_STATUS | ServiceAccess::DELETE,
    )?;
    if service.query_status()?.current_state != ServiceState::Stopped {
        service.stop()?;
        let started = Instant::now();
        while service.query_status()?.current_state != ServiceState::Stopped {
            if started.elapsed() > STOP_TIMEOUT {
                return Err(format!("{SERVICE_NAME} did not stop within 30 s").into());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    service.delete()?;
    drop(service);
    log::info!("removed {SERVICE_NAME}");

    let layout = Layout::system()?;
    match std::fs::remove_dir_all(layout.root()) {
        Ok(()) => log::info!("removed {}", layout.root().display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("could not remove {}: {e}", layout.root().display()).into()),
    }
    Ok(())
}
