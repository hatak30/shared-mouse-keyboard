// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use smk_core::config::Settings;
use smk_core::engine::{Command, EngineHandle, UiState};
use smk_core::layout::Layout;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_updater::UpdaterExt;
use uuid::Uuid;

struct App {
    engine: EngineHandle,
}

type R<T> = Result<T, String>;

#[tauri::command]
fn get_state(app: State<App>) -> UiState {
    app.engine.state()
}

#[tauri::command]
fn set_layout(app: State<App>, layout: Layout) -> R<()> {
    layout.validate()?;
    app.engine.send(Command::SetLayout(layout));
    Ok(())
}

#[tauri::command]
fn pair(app: State<App>, node_id: Uuid) {
    app.engine.send(Command::Pair(node_id));
}

#[tauri::command]
fn submit_code(app: State<App>, node_id: Uuid, code: String) {
    app.engine.send(Command::SubmitCode(node_id, code));
}

#[tauri::command]
fn cancel_pair(app: State<App>, node_id: Uuid) {
    app.engine.send(Command::CancelPair(node_id));
}

#[tauri::command]
fn unpair(app: State<App>, node_id: Uuid) {
    app.engine.send(Command::Unpair(node_id));
}

#[tauri::command]
fn connect(app: State<App>, addr: String) {
    app.engine.send(Command::Connect(addr));
}

#[tauri::command]
fn remove_manual_peer(app: State<App>, addr: String) {
    app.engine.send(Command::RemoveManualPeer(addr));
}

#[tauri::command]
fn set_settings(app: State<App>, settings: Settings) {
    app.engine.send(Command::SetSettings(settings));
}

#[tauri::command]
fn set_name(app: State<App>, name: String) {
    app.engine.send(Command::SetName(name));
}

#[tauri::command]
fn return_home(app: State<App>) {
    app.engine.send(Command::ReturnHome);
}

#[tauri::command]
fn request_permission(app: State<App>, id: String) {
    smk_core::permissions::request(&id);
    app.engine.send(Command::RefreshPermissions);
}

#[tauri::command]
fn open_permission_settings(id: String) {
    smk_core::permissions::open_settings(&id);
}

#[tauri::command]
fn refresh_permissions(app: State<App>) {
    app.engine.send(Command::RefreshPermissions);
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> R<bool> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> R<()> {
    let al = app.autolaunch();
    if enabled { al.enable() } else { al.disable() }.map_err(|e| e.to_string())
}

/// Turn on start-at-login the first time the app runs; afterwards the
/// user's choice in Settings is respected.
fn init_autostart(app: &AppHandle) {
    let marker = smk_core::config::Config::default_dir().join("autostart-initialized");
    if marker.exists() {
        return;
    }
    match app.autolaunch().enable() {
        Ok(()) => {
            let _ = std::fs::create_dir_all(marker.parent().unwrap());
            let _ = std::fs::write(&marker, b"");
            tracing::info!("enabled start at login");
        }
        Err(e) => tracing::warn!("could not enable start at login: {e}"),
    }
}

// ------------------------------------------------------------------ updates

#[derive(Clone, Serialize)]
struct UpdateInfo {
    current: String,
    version: String,
    notes: Option<String>,
}

#[derive(Default)]
struct UpdateState {
    available: parking_lot::Mutex<Option<UpdateInfo>>,
    installing: std::sync::atomic::AtomicBool,
}

async fn check_for_update(app: &AppHandle) -> Result<Option<UpdateInfo>, String> {
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?;
    let info = update.map(|u| UpdateInfo {
        current: u.current_version.clone(),
        version: u.version.clone(),
        notes: u.body.clone(),
    });
    *app.state::<UpdateState>().available.lock() = info.clone();
    if let Some(i) = &info {
        tracing::info!("update available: {} -> {}", i.current, i.version);
        let _ = app.emit("update-available", i.clone());
    }
    Ok(info)
}

#[tauri::command]
fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

#[tauri::command]
fn get_update(state: State<UpdateState>) -> Option<UpdateInfo> {
    state.available.lock().clone()
}

#[tauri::command]
async fn check_update(app: AppHandle) -> R<Option<UpdateInfo>> {
    check_for_update(&app).await
}

#[tauri::command]
async fn install_update(app: AppHandle) -> R<()> {
    use std::sync::atomic::Ordering;
    let state = app.state::<UpdateState>();
    if state.installing.swap(true, Ordering::SeqCst) {
        return Err("an update is already being installed".into());
    }
    let result: R<()> = async {
        let update = app
            .updater()
            .map_err(|e| e.to_string())?
            .check()
            .await
            .map_err(|e| e.to_string())?
            .ok_or("already up to date")?;
        let progress_app = app.clone();
        let mut downloaded = 0u64;
        update
            .download_and_install(
                move |chunk, total| {
                    downloaded += chunk as u64;
                    let _ = progress_app.emit("update-progress", (downloaded, total));
                },
                || {},
            )
            .await
            .map_err(|e| e.to_string())
    }
    .await;
    if let Err(e) = result {
        state.installing.store(false, Ordering::SeqCst);
        return Err(e);
    }
    // Give the cursor back before the app goes away for a moment.
    app.state::<App>().engine.send(Command::ReturnHome);
    tokio::time::sleep(Duration::from_millis(300)).await;
    app.restart();
}

/// Check shortly after start and then every 6 hours. Installing is left to
/// the user ("Install & restart" in the control panel).
fn spawn_update_checker(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(20)).await;
        loop {
            if let Err(e) = check_for_update(&app).await {
                tracing::warn!("update check failed: {e}");
            }
            tokio::time::sleep(Duration::from_secs(6 * 60 * 60)).await;
        }
    });
}

fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,smk_core=info".into()),
        )
        .init();

    let start_hidden = std::env::args().any(|a| a == "--hidden");

    tauri::Builder::default()
        // Must be registered first: a second launch (e.g. opening the app
        // while the login item is already running) just shows the window.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_window(app)))
        // Started at login with --hidden: run quietly in the tray.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(UpdateState::default())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--hidden"])))
        .setup(move |app| {
            let handle = app.handle().clone();
            init_autostart(&handle);
            spawn_update_checker(handle.clone());
            let notify_handle = handle.clone();
            let on_change = Arc::new(move || {
                let _ = notify_handle.emit("state-changed", ());
            });
            let engine = tauri::async_runtime::block_on(smk_core::start(smk_core::StartOptions::default(), on_change))
                .map_err(|e| format!("failed to start engine: {e:#}"))?;
            app.manage(App { engine: engine.clone() });

            // Tray icon: the app keeps running when the window is closed.
            let open = MenuItem::with_id(app, "open", "Open control panel", true, None::<&str>)?;
            let home = MenuItem::with_id(app, "home", "Return cursor to this machine", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &home, &sep, &quit])?;
            let tray_engine = engine.clone();
            let mut tray = TrayIconBuilder::with_id("main")
                .tooltip("Shared Mouse & Keyboard")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| match event.id().as_ref() {
                    "open" => show_window(app),
                    "home" => tray_engine.send(Command::ReturnHome),
                    "quit" => {
                        tray_engine.send(Command::ReturnHome);
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_window(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            if start_hidden {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Keep sharing in the background; quit from the tray menu.
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            set_layout,
            pair,
            submit_code,
            cancel_pair,
            unpair,
            connect,
            remove_manual_peer,
            set_settings,
            set_name,
            return_home,
            request_permission,
            open_permission_settings,
            refresh_permissions,
            get_autostart,
            set_autostart,
            app_version,
            get_update,
            check_update,
            install_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running application");
}
