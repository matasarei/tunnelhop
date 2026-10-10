//! The Windows service: registration with the service control manager, and the control handler
//! (STOP and PRESHUTDOWN) — SPEC §6.

use std::collections::HashMap;
use std::error::Error;
use std::ffi::OsString;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use th_core::driver::VpnDriver;
use th_core::manifest::Protocol;
use tokio::sync::oneshot;
use windows_service::service::{
    Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
    ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

use crate::credentials::{CredentialStore, Dpapi};
use crate::drivers::openvpn::{CredentialLookup, OpenvpnDriver};
use crate::drivers::wireguard::WireguardDriver;
use crate::killswitch::WfpKillSwitch;
use crate::logging;
use crate::orchestrator::{Discover, Orchestrator, Parts};
use crate::pipe::{Listener, PIPE_NAME};
use crate::probe::HttpProbe;
use crate::runtimes::{self, Runtime};
use crate::store::{self, Layout, Settings};

pub const SERVICE_NAME: &str = "TunnelHop";
const DISPLAY_NAME: &str = "TunnelHop";
const DESCRIPTION: &str = "Rotates WireGuard and OpenVPN tunnels and keeps traffic blocked while \
                           it switches.";
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;
const STOP_TIMEOUT: Duration = Duration::from_secs(30);
/// How long disconnecting may take while the service stops; the SCM waits about as long.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(20);

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

/// Why the service stops.
#[derive(Debug)]
enum Stop {
    /// The SCM asked (STOP or PRESHUTDOWN).
    Control(ServiceControl),
    /// Something the service cannot run without broke; exit with an error so the SCM's
    /// restart-on-failure actions apply.
    Failed(String),
}

fn run_service() -> Result<(), Box<dyn Error>> {
    let (stop_tx, stop_rx) = mpsc::channel();
    let failures = stop_tx.clone();
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Preshutdown => {
            let _ = stop_tx.send(Stop::Control(control));
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

    let running = match start(failures) {
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

    let reason = stop_rx
        .recv()
        .unwrap_or(Stop::Control(ServiceControl::Stop));
    report(
        ServiceState::StopPending,
        ServiceControlAccept::empty(),
        ServiceExitCode::Win32(0),
        Duration::from_secs(20),
    )?;
    // From step 13 on: disconnect the tunnel and disengage the kill-switch here.
    let exit_code = match &reason {
        Stop::Control(control) => {
            log::info!("stopping ({control:?})");
            ServiceExitCode::Win32(0)
        }
        Stop::Failed(why) => {
            log::error!("stopping after a failure: {why}");
            ServiceExitCode::ServiceSpecific(2)
        }
    };
    running.stop();
    report(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        exit_code,
        Duration::ZERO,
    )?;
    Ok(())
}

/// What runs while the service is up.
struct Running {
    runtime: tokio::runtime::Runtime,
    orchestrator: Arc<Orchestrator<Dpapi>>,
    watch: tokio::task::JoinHandle<()>,
    stop_pipe: oneshot::Sender<()>,
    pipe: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Running {
    /// SPEC §6: bring the tunnel down and lift the block, then stop serving.
    fn stop(self) {
        self.watch.abort();
        // The timeout is built inside the runtime: a timer created outside it panics ("no
        // reactor running"), which aborted every stop before anything was disconnected.
        let orchestrator = &self.orchestrator;
        let shutdown = self.runtime.block_on(async {
            tokio::time::timeout(SHUTDOWN_TIMEOUT, orchestrator.shutdown()).await
        });
        if shutdown.is_err() {
            log::error!("disconnecting took too long; stopping anyway");
        }
        let _ = self.stop_pipe.send(());
        match self.runtime.block_on(self.pipe) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => log::error!("the pipe server had failed: {e}"),
            Err(e) => log::error!("the pipe server panicked: {e}"),
        }
        self.runtime.shutdown_timeout(Duration::from_secs(5));
    }
}

/// Prepare the store, open the log, look at what is installed, and open the pipe. A pipe that
/// fails later is reported on `failures`: a service nobody can reach must not look healthy.
fn start(failures: mpsc::Sender<Stop>) -> Result<Running, Box<dyn Error>> {
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

    let mut drivers: HashMap<Protocol, Arc<dyn VpnDriver>> = HashMap::new();
    for runtime_found in &runtimes {
        if runtime_found.is_available() {
            drivers.insert(runtime_found.protocol, driver_for(runtime_found, &layout)?);
        }
    }
    // A runtime installed or updated while the service runs is picked up without a restart.
    let discover: Discover = {
        let layout = layout.clone();
        Arc::new(move |protocol| {
            let found = runtimes::discover_one(protocol);
            let driver = if found.is_available() {
                driver_for(&found, &layout)
                    .map_err(|e| {
                        log::error!("{protocol:?} was found but its driver could not start: {e}")
                    })
                    .ok()
            } else {
                None
            };
            (found.info(), driver)
        })
    };
    let killswitch_engaged = WfpKillSwitch::is_engaged().unwrap_or_else(|e| {
        log::error!("cannot tell whether the kill-switch is engaged: {e}");
        false
    });
    if killswitch_engaged {
        log::warn!("the kill-switch was left engaged; traffic stays blocked");
    }
    let probe = HttpProbe::new(settings.probe_urls.clone())?;
    let parts = Parts {
        credentials: CredentialStore::new(layout.credentials(), Dpapi),
        layout,
        settings,
        runtimes: runtimes.iter().map(Runtime::info).collect(),
        drivers,
        discover: Some(discover),
        killswitch: Arc::new(WfpKillSwitch::new()),
        killswitch_engaged,
        probe: Arc::new(probe),
        seed: None,
    };
    let orchestrator = runtime.block_on(Orchestrator::start(parts));
    let watch = {
        let _context = runtime.enter();
        orchestrator.spawn_watch()
    };

    let (stop_pipe, stopped) = oneshot::channel();
    let server =
        runtime.spawn(
            listener.serve(orchestrator.clone(), orchestrator.events(), async {
                let _ = stopped.await;
            }),
        );
    let pipe = runtime.spawn(async move {
        let result = match server.await {
            Ok(result) => result,
            Err(e) => Err(std::io::Error::other(format!("panicked: {e}"))),
        };
        if let Err(e) = &result {
            let _ = failures.send(Stop::Failed(format!("the pipe server failed: {e}")));
        }
        result
    });
    log::info!("listening on {PIPE_NAME}");
    Ok(Running {
        runtime,
        orchestrator,
        watch,
        stop_pipe,
        pipe,
    })
}

/// Whether opening the service failed because there is none, as opposed to, say, access denied.
fn not_registered(error: &windows_service::Error) -> bool {
    const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
    matches!(error, windows_service::Error::Winapi(e)
        if e.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST))
}

/// The driver for a runtime that was found and is new enough.
fn driver_for(found: &Runtime, layout: &Layout) -> Result<Arc<dyn VpnDriver>, Box<dyn Error>> {
    Ok(match found.protocol {
        Protocol::Wireguard => Arc::new(WireguardDriver::new(found, layout.tunnels())?),
        Protocol::Openvpn => {
            // The driver answers OpenVPN's credential prompts from the same store.
            let store = CredentialStore::new(layout.credentials(), Dpapi);
            let lookup: CredentialLookup = Arc::new(move |bundle, profile| {
                store.get(bundle, profile).map_err(|e| e.to_string())
            });
            Arc::new(OpenvpnDriver::new(found, layout, lookup)?)
        }
    })
}

/// Stop the service and wait until it has: its stop handler disconnects and lifts the block.
fn stop_and_wait(service: &Service) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    let mut asked = false;
    loop {
        match service.query_status()?.current_state {
            ServiceState::Stopped => return Ok(()),
            // A service that is still starting, or already stopping, refuses a stop: wait
            // until it has settled.
            ServiceState::StartPending | ServiceState::StopPending => {}
            _ if !asked => {
                service.stop()?;
                asked = true;
            }
            _ => {}
        }
        if started.elapsed() > STOP_TIMEOUT {
            return Err(format!("{SERVICE_NAME} did not stop within 30 s").into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Wait until a service that was just started is running.
fn wait_until_running(service: &Service) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    loop {
        match service.query_status()?.current_state {
            ServiceState::Running => return Ok(()),
            ServiceState::Stopped => {
                return Err(format!("{SERVICE_NAME} stopped while starting").into());
            }
            _ if started.elapsed() > STOP_TIMEOUT => {
                return Err(format!("{SERVICE_NAME} did not start within 30 s").into());
            }
            _ => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

/// Register the service to start automatically as LocalSystem, restart it if it crashes, and
/// start it now. Over an existing registration (an upgrade, or a repeated install) the service
/// is stopped, pointed at this executable and started again; its data is kept.
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
    let access = ServiceAccess::CHANGE_CONFIG
        | ServiceAccess::START
        | ServiceAccess::STOP
        | ServiceAccess::QUERY_STATUS;
    let service = match manager.open_service(SERVICE_NAME, access) {
        Ok(existing) => {
            stop_and_wait(&existing)?;
            existing.change_config(&info)?;
            log::info!("{SERVICE_NAME} was already registered; updating it");
            existing
        }
        Err(e) if not_registered(&e) => manager.create_service(&info, access)?,
        Err(e) => return Err(e.into()),
    };
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
    // Also restart after the service stops itself with an error (Stop::Failed), not only after
    // a crash.
    service.set_failure_actions_on_non_crash_failures(true)?;
    service.start::<&str>(&[])?;
    log::info!("installed and started {SERVICE_NAME}");
    Ok(())
}

/// Stop and delete the service, lift any block, then remove `%ProgramData%\TunnelHop`
/// (SPEC §6). Each part is attempted whatever the state of the others, so an uninstall after a
/// crash, or a second uninstall, still leaves the machine clean.
pub fn uninstall() -> Result<(), Box<dyn Error>> {
    let service = remove_service();
    if let Err(e) = &service {
        log::error!("{SERVICE_NAME} could not be removed: {e}");
    }
    // Whatever the service managed: no filter of ours outlives an uninstall.
    let block = WfpKillSwitch::disengage_now();
    if let Err(e) = &block {
        log::error!("the block could not be lifted: {e}");
    }
    let data = remove_data();
    service?;
    block?;
    data
}

fn remove_service() -> Result<(), Box<dyn Error>> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let access = ServiceAccess::START
        | ServiceAccess::STOP
        | ServiceAccess::QUERY_STATUS
        | ServiceAccess::DELETE;
    match manager.open_service(SERVICE_NAME, access) {
        Ok(service) => {
            // A service that is not running (it crashed, or was stopped by hand) may have left
            // a tunnel and the block behind. Starting it takes both over, and stopping it then
            // brings them down the way every stop does.
            if service.query_status()?.current_state == ServiceState::Stopped {
                match service
                    .start::<&str>(&[])
                    .map_err(Into::into)
                    .and_then(|()| wait_until_running(&service))
                {
                    Ok(()) => {}
                    Err(e) => log::warn!("{SERVICE_NAME} could not be started to clean up: {e}"),
                }
            }
            stop_and_wait(&service)?;
            service.delete()?;
            log::info!("removed {SERVICE_NAME}");
        }
        Err(e) if not_registered(&e) => log::info!("{SERVICE_NAME} is not registered"),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

fn remove_data() -> Result<(), Box<dyn Error>> {
    let layout = Layout::system()?;
    match std::fs::remove_dir_all(layout.root()) {
        Ok(()) => log::info!("removed {}", layout.root().display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("could not remove {}: {e}", layout.root().display()).into()),
    }
    Ok(())
}
