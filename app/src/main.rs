// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod activity;
mod login_screen;

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use smk_core::config::Settings;
use smk_core::engine::{Command, EngineHandle, UiState};
use smk_core::layout::Layout;
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
fn block(app: State<App>, node_id: Uuid) {
    app.engine.send(Command::Block(node_id));
}

#[tauri::command]
fn unblock(app: State<App>, node_id: Uuid) {
    app.engine.send(Command::Unblock(node_id));
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
fn start_transfer(app: State<App>, id: Uuid) {
    app.engine.send(Command::StartTransfer(id));
}

/// Ask for a folder and download a pending copy straight into it.
#[tauri::command]
fn save_transfer_to(handle: AppHandle, app: State<App>, id: Uuid) {
    activity::save_to(&handle, app.engine.clone(), id);
}

#[tauri::command]
fn reveal_path(path: String) {
    activity::reveal(std::path::Path::new(&path));
}

#[tauri::command]
fn cancel_transfer(app: State<App>, id: Uuid) {
    app.engine.send(Command::CancelTransfer(id));
}

#[tauri::command]
fn dismiss_transfer(app: State<App>, id: Uuid) {
    app.engine.send(Command::DismissTransfer(id));
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

// ------------------------------------------------------------------ login screen

#[tauri::command]
fn get_login_screen() -> Option<bool> {
    #[cfg(target_os = "macos")]
    return Some(login_screen::is_installed());
    #[cfg(not(target_os = "macos"))]
    None
}

#[tauri::command]
async fn set_login_screen(enabled: bool) -> R<()> {
    #[cfg(target_os = "macos")]
    {
        let dir = smk_core::config::Config::default_dir();
        tauri::async_runtime::spawn_blocking(move || {
            if enabled { login_screen::install(&dir) } else { login_screen::uninstall() }
        })
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = enabled;
        Err("Login screen support is currently available on macOS only.".into())
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
        if !w.is_visible().unwrap_or(false) {
            move_to_pointer_screen(app, &w);
        }
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Center the window on the display the mouse pointer is on, so it doesn't
/// open on a screen you aren't looking at.
fn move_to_pointer_screen(app: &AppHandle, w: &tauri::WebviewWindow) {
    let Ok(p) = app.cursor_position() else { return };
    let Ok(monitors) = w.available_monitors() else { return };
    let Some(m) = monitors.iter().find(|m| {
        let (pos, size) = (m.position(), m.size());
        p.x >= pos.x as f64 && p.y >= pos.y as f64 && p.x < (pos.x + size.width as i32) as f64 && p.y < (pos.y + size.height as i32) as f64
    }) else {
        return;
    };
    let Ok(cur) = w.current_monitor() else { return };
    if cur.as_ref().is_some_and(|c| c.position() == m.position()) {
        return;
    }
    let Ok(size) = w.outer_size() else { return };
    let (pos, ms) = (m.position(), m.size());
    let x = pos.x + (ms.width as i32 - size.width as i32).max(0) / 2;
    let y = pos.y + (ms.height as i32 - size.height as i32).max(0) / 2;
    let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,smk_core=info".into()),
        )
        .init();

    #[cfg(target_os = "macos")]
    if std::env::args().any(|a| a == login_screen::ARG) {
        login_screen::run_agent();
        return;
    }

    let start_hidden = std::env::args().any(|a| a == "--hidden");

    tauri::Builder::default()
        // Must be registered first: a second launch (e.g. opening the app
        // while the login item is already running) just shows the window.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_window(app)))
        // Started at login with --hidden: run quietly in the tray.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
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

            // Tray icon: the app keeps running when the window is closed. Its
            // menu also shows file copies (see `activity`).
            let (menu, _) = activity::menu(app.handle(), &[])?;
            let tray_engine = engine.clone();
            let mut tray = TrayIconBuilder::with_id(activity::TRAY_ID)
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
                    id => {
                        activity::on_menu(app, &tray_engine, id, &tray_engine.state().transfers);
                    }
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
            activity::spawn(handle.clone(), engine.clone());

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
            block,
            unblock,
            connect,
            remove_manual_peer,
            set_settings,
            set_name,
            return_home,
            start_transfer,
            save_transfer_to,
            reveal_path,
            cancel_transfer,
            dismiss_transfer,
            request_permission,
            open_permission_settings,
            refresh_permissions,
            get_autostart,
            set_autostart,
            get_login_screen,
            set_login_screen,
            app_version,
            get_update,
            check_update,
            install_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running application");
}
