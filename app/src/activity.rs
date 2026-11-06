//! Makes file copies visible outside the control panel: progress next to the
//! menu bar / tray icon, Cancel and "Save to…" in its menu, and system
//! notifications when a download starts, finishes or fails.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use smk_core::engine::{Command, EngineHandle, TransferState, UiTransfer};
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::{AppHandle, Wry};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;
use uuid::Uuid;

pub const TRAY_ID: &str = "main";

/// Copies smaller than this finish too fast to be worth a notification.
const NOTIFY_MIN_BYTES: u64 = 50 * 1024 * 1024;

#[cfg(target_os = "macos")]
const FILE_MANAGER: &str = "Finder";
#[cfg(target_os = "windows")]
const FILE_MANAGER: &str = "Explorer";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const FILE_MANAGER: &str = "folder";

#[cfg(target_os = "macos")]
const PASTE_KEYS: &str = "⌘V";
#[cfg(not(target_os = "macos"))]
const PASTE_KEYS: &str = "Ctrl+V";

/// Progress lines in the tray menu, updated in place.
type StatusItems = Vec<(Uuid, MenuItem<Wry>)>;

/// The tray menu for the current transfers.
pub fn menu(app: &AppHandle, transfers: &[UiTransfer]) -> tauri::Result<(Menu<Wry>, StatusItems)> {
    let mut items: Vec<Box<dyn IsMenuItem<Wry>>> = Vec::new();
    let mut status = StatusItems::new();
    let mut add = |id: String, text: String, enabled: bool| -> tauri::Result<MenuItem<Wry>> {
        let item = MenuItem::with_id(app, id, text, enabled, None::<&str>)?;
        items.push(Box::new(item.clone()));
        Ok(item)
    };
    let mut any = false;
    for t in transfers.iter().filter(|t| t.state == TransferState::Running) {
        status.push((t.id, add(format!("status:{}", t.id), status_line(t), false)?));
        add(format!("cancel:{}", t.id), "Cancel copy".into(), true)?;
        any = true;
    }
    if let Some(t) = transfers.iter().find(|t| t.incoming && t.state == TransferState::Pending) {
        add(format!("status:{}", t.id), format!("Copied on {}: {} ({})", t.peer, t.label, human_bytes(t.total)), false)?;
        add(format!("hint:{}", t.id), format!("Paste with {PASTE_KEYS} to fetch it"), false)?;
        add(format!("save:{}", t.id), "Save copied files to…".into(), true)?;
        any = true;
    }
    if let Some(t) = transfers.iter().find(|t| t.incoming && t.state == TransferState::Done && !t.paths.is_empty()) {
        add(format!("reveal:{}", t.id), format!("Show {} in {FILE_MANAGER}", t.label), true)?;
        any = true;
    }
    let open = MenuItem::with_id(app, "open", "Open control panel", true, None::<&str>)?;
    let home = MenuItem::with_id(app, "home", "Return cursor to this machine", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    if any {
        items.push(Box::new(PredefinedMenuItem::separator(app)?));
    }
    items.push(Box::new(open));
    items.push(Box::new(home));
    items.push(Box::new(PredefinedMenuItem::separator(app)?));
    items.push(Box::new(quit));
    let refs: Vec<&dyn IsMenuItem<Wry>> = items.iter().map(|b| b.as_ref()).collect();
    Ok((Menu::with_items(app, &refs)?, status))
}

/// Handle a transfer item of the tray menu. Returns false if `id` isn't one.
pub fn on_menu(app: &AppHandle, engine: &EngineHandle, id: &str, transfers: &[UiTransfer]) -> bool {
    let Some((action, uuid)) = id.split_once(':') else { return false };
    let Ok(uuid) = Uuid::parse_str(uuid) else { return false };
    match action {
        "cancel" => engine.send(Command::CancelTransfer(uuid)),
        "save" => save_to(app, engine.clone(), uuid),
        "reveal" => {
            if let Some(p) = transfers.iter().find(|t| t.id == uuid).and_then(|t| t.paths.first()) {
                reveal(Path::new(p));
            }
        }
        _ => {}
    }
    true
}

/// Ask for a folder, then download the pending transfer straight into it.
pub fn save_to(app: &AppHandle, engine: EngineHandle, id: Uuid) {
    app.dialog().file().set_title("Save copied files to").pick_folder(move |folder| {
        if let Some(path) = folder.and_then(|f| f.into_path().ok()) {
            engine.send(Command::SaveTransferTo(id, path));
        }
    });
}

/// Show a file in the file manager.
pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let r = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
    if let Err(e) = r {
        tracing::warn!("cannot show {}: {e}", path.display());
    }
}

/// Watch transfers: keep the tray up to date and notify on changes.
pub fn spawn(app: AppHandle, engine: EngineHandle) {
    tauri::async_runtime::spawn(async move {
        let mut seen: HashMap<Uuid, TransferState> = HashMap::new();
        let mut menu_sig = String::new();
        let mut status = StatusItems::new();
        let mut title = String::new();
        loop {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let transfers = engine.state().transfers;
            notify_changes(&app, &transfers, &mut seen);

            let Some(tray) = app.tray_by_id(TRAY_ID) else { continue };
            // Rebuild the menu only when its items change (rebuilding closes
            // it if open); progress text is updated in place.
            let sig: String = transfers
                .iter()
                .filter(|t| t.state != TransferState::Done || t.incoming)
                .map(|t| format!("{}{:?}{};", t.id, t.state, t.paths.len()))
                .collect();
            if sig != menu_sig {
                menu_sig = sig;
                if let Ok((m, s)) = menu(&app, &transfers) {
                    let _ = tray.set_menu(Some(m));
                    status = s;
                }
            } else {
                for (id, item) in &status {
                    if let Some(t) = transfers.iter().find(|t| t.id == *id) {
                        let _ = item.set_text(status_line(t));
                    }
                }
            }

            let running: Vec<&UiTransfer> = transfers.iter().filter(|t| t.state == TransferState::Running).collect();
            let new_title = if running.is_empty() {
                String::new()
            } else {
                let (done, total) = running.iter().fold((0u64, 0u64), |(d, t), x| (d + x.done, t + x.total));
                format!("{}%", percent(done, total))
            };
            if new_title != title {
                title = new_title;
                #[cfg(not(target_os = "windows"))]
                let _ = tray.set_title(if title.is_empty() { None } else { Some(title.as_str()) });
                let tip = match running.first() {
                    Some(t) => format!("Shared Mouse & Keyboard — {}", status_line(t)),
                    None => "Shared Mouse & Keyboard".to_string(),
                };
                let _ = tray.set_tooltip(Some(tip));
            }
        }
    });
}

fn notify_changes(app: &AppHandle, transfers: &[UiTransfer], seen: &mut HashMap<Uuid, TransferState>) {
    for t in transfers.iter().filter(|t| t.incoming) {
        let before = seen.insert(t.id, t.state);
        if before == Some(t.state) {
            continue;
        }
        let big = t.total >= NOTIFY_MIN_BYTES;
        let (title, body) = match t.state {
            TransferState::Running if big => (
                format!("Copying {} from {}", t.label, t.peer),
                format!("{} — progress is shown in the menu bar.", human_bytes(t.total)),
            ),
            TransferState::Done if big || t.saved_to.is_some() => (
                format!("{} is ready", t.label),
                match &t.saved_to {
                    Some(f) => format!("Saved to {f}"),
                    None => "It's on the clipboard — paste it anywhere.".to_string(),
                },
            ),
            TransferState::Failed => (
                format!("Copying {} failed", t.label),
                t.error.clone().unwrap_or_default(),
            ),
            _ => continue,
        };
        // A state we only see for the first time after it already finished
        // (e.g. at startup) isn't news.
        if before.is_none() && t.state != TransferState::Running {
            continue;
        }
        if let Err(e) = app.notification().builder().title(title).body(body).show() {
            tracing::debug!("notification failed: {e}");
        }
    }
    seen.retain(|id, _| transfers.iter().any(|t| t.id == *id));
}

fn status_line(t: &UiTransfer) -> String {
    let what = if t.incoming {
        format!("Receiving {} from {}", t.label, t.peer)
    } else {
        format!("Sending {} to {}", t.label, t.peer)
    };
    format!("{what} — {}% of {}", percent(t.done, t.total), human_bytes(t.total))
}

fn percent(done: u64, total: u64) -> u64 {
    done.saturating_mul(100).checked_div(total).map_or(100, |p| p.min(100))
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}
