//! TunnelHop's app: the tray and the dashboard, unelevated, talking to th-service over its pipe.
//! It holds no state of its own beyond the last status report: every change goes to the
//! service as a request, and what the app shows comes back as the service's next report.

pub mod actions;
pub mod bundles;
pub mod import;
pub mod lang;
pub mod pipe_client;
pub mod servers;
pub mod tray;
pub mod window;

#[cfg(windows)]
pub use app::{run, show_dashboard};

#[cfg(windows)]
mod app {
    use std::sync::Mutex;
    use std::time::Duration;

    use tauri::{AppHandle, Emitter, Manager, WindowEvent};
    use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
    use tauri_plugin_dialog::DialogExt;
    use tauri_plugin_notification::NotificationExt;
    use th_core::ipc::{Event, ProfileFile, Request, Response, Secret, StatusReport};

    use crate::actions::{Freshness, WindowAction, tray_request, window_request};
    use crate::bundles::BundleList;
    use crate::import::{ImportOutcome, exported, outcome, read_bundle, write_export};
    use crate::lang::text;
    use crate::pipe_client::{Options, ServiceClient, Update};
    use crate::servers::ChosenFile;
    use crate::tray::{Action, Tray};

    /// Passed by the autostart entry: start in the tray, without the window.
    const HIDDEN_ARG: &str = "--hidden";
    /// How long Exit waits for the service to disconnect and lift the block.
    const EXIT_TIMEOUT: Duration = Duration::from_secs(30);
    const RETRY: Duration = Duration::from_secs(2);
    /// Written once autostart has been switched on, so a user who later turns it off in
    /// Windows' settings is not overruled on the next start.
    const AUTOSTART_MARKER: &str = "autostart-offered";

    struct AppState {
        client: ServiceClient,
        report: Mutex<Option<StatusReport>>,
        tray: Mutex<Option<Tray>>,
        /// A `GetStatus` reply that arrives after a status event is older than what is shown,
        /// and is dropped.
        freshness: Freshness,
    }

    /// Size the dashboard for the screen it opens on, before it is first shown: as tall as its
    /// view without servers needs, or as tall as fits (`window::fitted_height`), and centred.
    /// Only at startup: a size the user picks later stays theirs.
    fn fit_to_screen(app: &AppHandle) {
        let Some(window) = app.get_webview_window("main") else {
            return;
        };
        let monitor = window
            .current_monitor()
            .ok()
            .flatten()
            .or_else(|| window.primary_monitor().ok().flatten());
        let Some(monitor) = monitor else {
            return;
        };
        let scale = monitor.scale_factor();
        let work_area = f64::from(monitor.work_area().size.height) / scale;
        let width = window
            .inner_size()
            .map(|size| size.to_logical::<f64>(scale).width)
            .unwrap_or(600.0);
        let height = crate::window::fitted_height(work_area);
        if let Err(e) = window.set_size(tauri::LogicalSize::new(width, height)) {
            log::warn!("could not size the window for the screen: {e}");
        }
        let _ = window.center();
    }

    /// The dashboard window, shown and brought to the front.
    pub fn show_dashboard(app: &AppHandle) {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }

    /// The last status report, for the dashboard as it opens; `null` while the service is
    /// unreachable.
    #[tauri::command]
    fn status(state: tauri::State<'_, AppState>) -> Option<StatusReport> {
        state
            .report
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn notify(app: &AppHandle, message: &str) {
        if let Err(e) = app
            .notification()
            .builder()
            .title("TunnelHop")
            .body(message)
            .show()
        {
            log::warn!("could not show a notification: {e}");
        }
    }

    /// Show `report` everywhere: the tray, the window, and the copy `status` returns.
    fn apply(app: &AppHandle, report: Option<StatusReport>) {
        let state = app.state::<AppState>();
        if let Some(tray) = state
            .tray
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            && let Err(e) = tray.show(report.as_ref())
        {
            log::warn!("could not update the tray: {e}");
        }
        let _ = app.emit("status", &report);
        *state.report.lock().unwrap_or_else(|e| e.into_inner()) = report;
    }

    fn on_update(app: &AppHandle, update: Update) {
        match update {
            Update::Connected => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app.state::<AppState>();
                    let asked = state.freshness.asking();
                    let client = state.client.clone();
                    if let Ok(Response::Status { report }) =
                        client.request(Request::GetStatus).await
                        && state.freshness.is_current(asked)
                    {
                        apply(&app, Some(report));
                    }
                });
            }
            Update::Disconnected(reason) => {
                log::info!("service unreachable: {reason}");
                apply(app, None);
            }
            Update::Event(Event::Status { report }) => {
                app.state::<AppState>().freshness.pushed();
                apply(app, Some(report));
            }
            Update::Event(Event::Alert { message }) => {
                notify(app, &message);
                let _ = app.emit("alert", &message);
            }
        }
    }

    /// Disconnect and lift the block, then quit (SPEC §5's Exit). Quits anyway when the service
    /// cannot be reached or does not answer in time.
    async fn exit(app: AppHandle) {
        let client = app.state::<AppState>().client.clone();
        let shutdown = Request::Shutdown { disconnect: true };
        match tokio::time::timeout(EXIT_TIMEOUT, client.request(shutdown)).await {
            Ok(Ok(Response::Error { message })) => {
                log::warn!("the service could not disconnect: {message}")
            }
            Ok(Err(e)) => log::info!("exiting without the service: {e}"),
            Err(_) => log::warn!("the service did not answer Exit in time"),
            Ok(Ok(_)) => {}
        }
        app.exit(0);
    }

    /// Send one request; a refusal comes back as its message. On a refusal the tray and the
    /// window are put back as the last report had them: a switch or a check item flips itself
    /// on click, before the service has said anything.
    async fn send(app: &AppHandle, request: Request) -> Result<(), String> {
        let client = app.state::<AppState>().client.clone();
        let result = match client.request(request).await {
            Ok(Response::Error { message }) => Err(message),
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        };
        if result.is_err() {
            let report = app
                .state::<AppState>()
                .report
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            apply(app, report);
        }
        result
    }

    /// Send what a tray click asks for. The tray changes only with the service's next report;
    /// a refusal is shown as a notification.
    fn on_action(app: &AppHandle, action: Action) {
        match action {
            Action::Dashboard => return show_dashboard(app),
            Action::Exit => {
                tauri::async_runtime::spawn(exit(app.clone()));
                return;
            }
            _ => {}
        }
        let report = app
            .state::<AppState>()
            .report
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(request) = tray_request(action, report.as_ref()) else {
            return;
        };
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(message) = send(&app, request).await {
                notify(&app, &message);
            }
        });
    }

    /// The window's controls. A refusal comes back to the page, which shows it in place.
    #[tauri::command]
    async fn act(app: AppHandle, action: WindowAction) -> Result<(), String> {
        match window_request(action) {
            Some(request) => send(&app, request).await,
            None => {
                exit(app).await;
                Ok(())
            }
        }
    }

    /// Import a bundle: the file dropped on the window, or one chosen in the native picker when
    /// no path is given. The file is read here, as the signed-in user, and its bytes go to the
    /// service, with the `password` it was protected with when the service asked for one.
    #[tauri::command]
    async fn import_bundle(
        app: AppHandle,
        path: Option<String>,
        password: Option<String>,
    ) -> Result<ImportOutcome, String> {
        let path = match path {
            Some(path) => std::path::PathBuf::from(path),
            None => {
                let mut dialog = app.dialog().file();
                // Owned by the window, so the picker cannot end up behind it.
                if let Some(window) = app.get_webview_window("main") {
                    dialog = dialog.set_parent(&window);
                }
                let picked = dialog
                    .set_title(text("picker.bundle.title", &[]))
                    .add_filter(text("picker.bundle.filter", &[]), &["zip"])
                    .blocking_pick_file();
                match picked.and_then(|file| file.into_path().ok()) {
                    Some(path) => path,
                    None => return Ok(ImportOutcome::Cancelled),
                }
            }
        };
        let shown = path.to_string_lossy().into_owned();
        let zip_base64 = tauri::async_runtime::spawn_blocking(move || read_bundle(&path))
            .await
            .map_err(|e| text("file.read_failed", &[("reason", &e.to_string())]))??;
        let client = app.state::<AppState>().client.clone();
        let response = client
            .request(Request::ImportBundle {
                zip_base64,
                password: password.map(Secret::new),
            })
            .await?;
        match response {
            // The page asks for the password and imports the same file again with it.
            Response::PasswordNeeded { wrong } => Ok(ImportOutcome::Locked { path: shown, wrong }),
            other => outcome(other),
        }
    }

    /// Export a bundle to a file the user chooses: `true` once it is written, `false` when the
    /// save dialog was closed. The service builds the zip; it is written here, as the signed-in
    /// user, since the service never writes where a user chose.
    #[tauri::command]
    async fn export_bundle(
        app: AppHandle,
        bundle_id: String,
        include_sign_ins: bool,
        password: Option<String>,
    ) -> Result<bool, String> {
        let client = app.state::<AppState>().client.clone();
        let (file_name, zip) = exported(
            client
                .request(Request::ExportBundle {
                    bundle_id,
                    include_sign_ins,
                    password: password.map(Secret::new),
                })
                .await?,
        )?;
        let mut dialog = app.dialog().file();
        if let Some(window) = app.get_webview_window("main") {
            dialog = dialog.set_parent(&window);
        }
        let chosen = dialog
            .set_title(text("picker.export.title", &[]))
            .add_filter(text("picker.bundle.filter", &[]), &["zip"])
            .set_file_name(&file_name)
            .blocking_save_file();
        let Some(path) = chosen.and_then(|file| file.into_path().ok()) else {
            return Ok(false);
        };
        let name = path.display().to_string();
        tauri::async_runtime::spawn_blocking(move || write_export(&path, &zip))
            .await
            .map_err(|e| {
                text(
                    "file.cannot_write",
                    &[("name", &name), ("reason", &e.to_string())],
                )
            })??;
        Ok(true)
    }

    /// Make an installed bundle the active one. The service disconnects the old one first.
    #[tauri::command]
    async fn select_bundle(app: AppHandle, bundle_id: String) -> Result<(), String> {
        send(&app, Request::SelectBundle { bundle_id }).await
    }

    /// Every installed bundle, and which one is active.
    #[tauri::command]
    async fn list_bundles(app: AppHandle) -> Result<BundleList, String> {
        let client = app.state::<AppState>().client.clone();
        crate::bundles::list(client.request(Request::ListBundles).await?)
    }

    /// Delete an installed bundle and its stored credentials; the list that is left comes
    /// back. The service disconnects first when the bundle is the active one.
    #[tauri::command]
    async fn delete_bundle(app: AppHandle, bundle_id: String) -> Result<BundleList, String> {
        let client = app.state::<AppState>().client.clone();
        crate::bundles::list(client.request(Request::DeleteBundle { bundle_id }).await?)
    }

    /// The config files to add as servers, each with the name it would show under: the files
    /// dropped on the window, or ones chosen in the native picker when no paths are given.
    /// Nothing is read yet. An empty list means the picker was closed.
    #[tauri::command]
    async fn choose_servers(
        app: AppHandle,
        paths: Option<Vec<String>>,
    ) -> Result<Vec<ChosenFile>, String> {
        let paths: Vec<std::path::PathBuf> = match paths {
            Some(paths) => paths.into_iter().map(Into::into).collect(),
            None => {
                let mut dialog = app.dialog().file();
                if let Some(window) = app.get_webview_window("main") {
                    dialog = dialog.set_parent(&window);
                }
                dialog
                    .set_title(text("picker.servers.title", &[]))
                    .add_filter(text("picker.servers.filter", &[]), &["conf", "ovpn"])
                    .blocking_pick_files()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|file| file.into_path().ok())
                    .collect()
            }
        };
        paths
            .iter()
            .map(|path| crate::servers::describe(path))
            .collect()
    }

    /// Add the chosen files to a bundle as servers. They are read here, as the signed-in
    /// user; the bundle is replaced by a new one, as an import would make it.
    #[tauri::command]
    async fn add_servers(
        app: AppHandle,
        bundle_id: String,
        files: Vec<ChosenFile>,
    ) -> Result<ImportOutcome, String> {
        let files = read_servers(files).await?;
        let client = app.state::<AppState>().client.clone();
        outcome(
            client
                .request(Request::AddProfiles { bundle_id, files })
                .await?,
        )
    }

    /// The chosen files as the service takes them, read off the UI thread.
    async fn read_servers(files: Vec<ChosenFile>) -> Result<Vec<ProfileFile>, String> {
        tauri::async_runtime::spawn_blocking(move || {
            files
                .iter()
                .map(crate::servers::read)
                .collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(|e| text("file.read_failed", &[("reason", &e.to_string())]))?
    }

    /// Make a new bundle of the chosen config files (SPEC §3.5), read here as the signed-in
    /// user. The service makes it the bundle in use when there is none.
    #[tauri::command]
    async fn create_bundle(
        app: AppHandle,
        name: String,
        files: Vec<ChosenFile>,
    ) -> Result<ImportOutcome, String> {
        let files = read_servers(files).await?;
        let client = app.state::<AppState>().client.clone();
        outcome(
            client
                .request(Request::CreateBundle { name, files })
                .await?,
        )
    }

    /// Remove servers from a bundle; it is replaced the same way.
    #[tauri::command]
    async fn remove_servers(
        app: AppHandle,
        bundle_id: String,
        profile_ids: Vec<String>,
    ) -> Result<ImportOutcome, String> {
        let client = app.state::<AppState>().client.clone();
        let request = Request::RemoveProfiles {
            bundle_id,
            profile_ids,
        };
        outcome(client.request(request).await?)
    }

    /// Store a username and password for a bundle's OpenVPN profiles: the listed ones, or every
    /// one as the bundle's default when the list is empty. The password is a `Secret` from the
    /// moment it is deserialized: never logged, wiped when dropped.
    #[tauri::command]
    async fn set_credentials(
        app: AppHandle,
        bundle_id: String,
        profile_ids: Vec<String>,
        username: String,
        password: Secret,
    ) -> Result<(), String> {
        let request = Request::SetCredentials {
            bundle_id,
            profile_ids,
            username,
            password,
        };
        send(&app, request).await
    }

    /// The language the app speaks, by the name of its file under `locales`: the window shows
    /// its own text in the same one.
    #[tauri::command]
    fn language() -> &'static str {
        crate::lang::current()
    }

    /// The intervals the tray offers, with their labels, so the window offers the same.
    #[tauri::command]
    fn intervals() -> Vec<(u32, String)> {
        crate::actions::intervals()
    }

    /// Switch autostart on the first time the app runs, and never again.
    fn offer_autostart(app: &AppHandle) {
        let Ok(dir) = app.path().app_config_dir() else {
            return;
        };
        let marker = dir.join(AUTOSTART_MARKER);
        if marker.exists() {
            return;
        }
        match app.autolaunch().enable() {
            Ok(()) => {
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(&marker, b"");
            }
            Err(e) => log::warn!("could not enable autostart: {e}"),
        }
    }

    pub fn run() {
        tauri::Builder::default()
            // The app has no console: what it logs (an unreachable service, a tray it could not
            // update, autostart it could not enable) goes to %LOCALAPPDATA%\app.tunnelhop\logs.
            .plugin(
                tauri_plugin_log::Builder::new()
                    .clear_targets()
                    .target(tauri_plugin_log::Target::new(
                        tauri_plugin_log::TargetKind::LogDir {
                            file_name: Some("tunnelhop".into()),
                        },
                    ))
                    .level(log::LevelFilter::Info)
                    .max_file_size(1024 * 1024)
                    .build(),
            )
            // A second launch (the Start menu while the tray icon runs) shows the first one.
            .plugin(tauri_plugin_single_instance::init(|app, _, _| {
                show_dashboard(app)
            }))
            .plugin(tauri_plugin_autostart::init(
                MacosLauncher::LaunchAgent,
                Some(vec![HIDDEN_ARG]),
            ))
            .plugin(tauri_plugin_notification::init())
            .plugin(tauri_plugin_dialog::init())
            .invoke_handler(tauri::generate_handler![
                status,
                act,
                intervals,
                language,
                import_bundle,
                export_bundle,
                select_bundle,
                list_bundles,
                delete_bundle,
                choose_servers,
                add_servers,
                create_bundle,
                remove_servers,
                set_credentials
            ])
            .setup(|app| {
                let handle = app.handle().clone();
                let options = Options {
                    pipe: th_service::pipe::PIPE_NAME.into(),
                    verify_owner: true,
                    retry: RETRY,
                };
                let (client, connection) =
                    ServiceClient::new(options, move |update| on_update(&handle, update));
                app.manage(AppState {
                    client,
                    report: Mutex::new(None),
                    freshness: Freshness::default(),
                    tray: Mutex::new(None),
                });
                let tray = Tray::build(app.handle(), on_action)?;
                *app.state::<AppState>()
                    .tray
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(tray);
                tauri::async_runtime::spawn(connection);

                offer_autostart(app.handle());
                fit_to_screen(app.handle());
                if !std::env::args().any(|a| a == HIDDEN_ARG) {
                    show_dashboard(app.handle());
                }
                Ok(())
            })
            .on_window_event(|window, event| {
                // Closing hides to the tray (SPEC §5); Exit in the tray quits.
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            })
            .run(tauri::generate_context!())
            .expect("the TunnelHop app failed to start");
    }
}
