//! The Windows service: registration with the service control manager, and the control handler
//! (STOP and PRESHUTDOWN) — SPEC §6.

use std::error::Error;
use std::ffi::OsString;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use th_core::ipc::{Request, Response, RuntimeInfo, StatusReport};
use th_core::rotation::State;
use tokio::sync::{broadcast, oneshot};
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
    ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

use crate::logging;
use crate::pipe::{Handler, Listener, PIPE_NAME};
use crate::runtimes::{self, Runtime};
use crate::store::{self, Layout, Settings};

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

    let running = match start() {
        Ok(running) => running,
        Err(e) => {
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
    };

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
    running.stop();
    report(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        ServiceExitCode::Win32(0),
        Duration::ZERO,
    )?;
    Ok(())
}

/// What runs while the service is up.
struct Running {
    runtime: tokio::runtime::Runtime,
    stop_pipe: oneshot::Sender<()>,
    pipe: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Running {
    fn stop(self) {
        let _ = self.stop_pipe.send(());
        match self.runtime.block_on(self.pipe) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => log::error!("the pipe server had failed: {e}"),
            Err(e) => log::error!("the pipe server panicked: {e}"),
        }
        self.runtime.shutdown_timeout(Duration::from_secs(5));
    }
}

/// Answers what the service can answer before the orchestrator exists (step 14).
struct ShellHandler {
    settings: Settings,
    runtimes: Vec<RuntimeInfo>,
}

impl Handler for ShellHandler {
    async fn handle(&self, request: Request) -> Response {
        match request {
            Request::GetStatus => Response::Status {
                report: StatusReport {
                    state: State::Disconnected,
                    profile_id: None,
                    public_ip: None,
                    killswitch_engaged: false,
                    bundle: None,
                    auto_rotate: self.settings.auto_rotate.unwrap_or(true),
                    interval_minutes: self.settings.interval_minutes.unwrap_or(15),
                    runtimes: self.runtimes.clone(),
                },
            },
            _ => Response::Error {
                message: "this version of the service cannot do that yet".into(),
            },
        }
    }
}

/// Prepare the store, open the log, look at what is installed, and open the pipe.
fn start() -> Result<Running, Box<dyn Error>> {
    let layout = Layout::system()?;
    store::prepare(&layout)?;
    logging::to_file(&layout.logs())?;
    log::info!(
        "starting th-service {} in {}",
        env!("CARGO_PKG_VERSION"),
        layout.root().display()
    );

    let settings = Settings::load(&layout.settings_file()).unwrap_or_else(|e| {
        log::warn!("settings.json is unreadable, using defaults: {e}");
        Settings::default()
    });
    log::info!("settings: {settings:?}");
    let runtimes = runtimes::discover();
    for runtime in &runtimes {
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

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    // Binding fails if something already holds the pipe name; the service must not start then.
    let listener = runtime.block_on(async { Listener::bind(PIPE_NAME) })?;
    let handler = Arc::new(ShellHandler {
        settings,
        runtimes: runtimes.iter().map(Runtime::info).collect(),
    });
    let (events, _) = broadcast::channel(64);
    let (stop_pipe, stopped) = oneshot::channel();
    let pipe = runtime.spawn(listener.serve(handler, events, async {
        let _ = stopped.await;
    }));
    log::info!("listening on {PIPE_NAME}");
    Ok(Running {
        runtime,
        stop_pipe,
        pipe,
    })
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
