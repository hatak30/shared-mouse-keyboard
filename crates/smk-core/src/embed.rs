//! Running the engine inside a mobile app: one process-wide engine plus a
//! string based command interface that is easy to expose over JNI or C.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use parking_lot::Mutex;
use uuid::Uuid;

use crate::config::Settings;
use crate::engine::{ChangeCallback, Command, EngineHandle, StartOptions};
use crate::input::{CapturedEvent, MouseButton};
use crate::layout::Layout;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static STARTING: AtomicBool = AtomicBool::new(false);
static ENGINE: Mutex<Option<EngineHandle>> = Mutex::new(None);

/// Start the engine once; later calls do nothing.
pub fn start(config_dir: PathBuf, download_dir: PathBuf, clipboard: bool, on_change: ChangeCallback) -> anyhow::Result<()> {
    // The engine reports its state from inside `start`, and callers read it
    // back from that callback, so the engine lock must not be held meanwhile.
    if ENGINE.lock().is_some() || STARTING.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let rt = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("smk-engine")
            .build()
            .expect("tokio runtime")
    });
    let opts = StartOptions { config_dir, download_root: Some(download_dir), clipboard, ..StartOptions::default() };
    let result = rt.block_on(crate::start(opts, on_change));
    STARTING.store(false, Ordering::SeqCst);
    *ENGINE.lock() = Some(result?);
    Ok(())
}

pub fn state_json() -> String {
    ENGINE.lock().as_ref().and_then(|e| serde_json::to_string(&e.state()).ok()).unwrap_or_default()
}

pub fn button_index(b: MouseButton) -> i32 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
        MouseButton::Back => 3,
        MouseButton::Forward => 4,
    }
}

pub fn button_from_index(i: i32) -> Option<MouseButton> {
    Some(match i {
        0 => MouseButton::Left,
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        3 => MouseButton::Back,
        4 => MouseButton::Forward,
        _ => return None,
    })
}

/// Touchpad / keyboard events from the app. `kind`: 0 motion (a=dx, b=dy),
/// 1 button (a=index, b=down), 2 scroll (a=dx, b=dy in 1/120 notches),
/// 3 key (a=evdev code, b=down).
pub fn input(kind: i32, a: f64, b: f64) {
    let ev = match kind {
        0 => CapturedEvent::Motion { x: f64::NAN, y: f64::NAN, dx: a, dy: b },
        1 => match button_from_index(a as i32) {
            Some(button) => CapturedEvent::Button { button, down: b != 0.0 },
            None => return,
        },
        2 => CapturedEvent::Scroll { dx: a as i32, dy: b as i32 },
        3 => CapturedEvent::Key { code: a as u16, down: b != 0.0 },
        _ => return,
    };
    if let Some(engine) = ENGINE.lock().as_ref() {
        engine.simulate_input(ev);
    }
}

/// The app finished (or failed) capturing the screen for a snapshot.
pub fn snapshot_ready(result: Result<Vec<u8>, String>) {
    if let Some(engine) = ENGINE.lock().as_ref() {
        engine.send(Command::SnapshotReady(result));
    }
}

/// The app finished recording the screen: offer the video to the paired machines.
pub fn recording_ready(path: std::path::PathBuf) {
    if let Some(engine) = ENGINE.lock().as_ref() {
        engine.send(Command::OfferFiles(vec![path]));
    }
}

/// Share text with the paired machines' clipboards.
pub fn share_text(text: String) {
    if let Some(engine) = ENGINE.lock().as_ref() {
        engine.send(Command::ShareText(text));
    }
}

pub fn parse_command(name: &str, a: &str, b: &str) -> Result<Command, String> {
    let id = || Uuid::parse_str(a).map_err(|e| e.to_string());
    Ok(match name {
        "set_layout" => {
            let layout: Layout = serde_json::from_str(a).map_err(|e| e.to_string())?;
            layout.validate()?;
            Command::SetLayout(layout)
        }
        "pair" => Command::Pair(id()?),
        "submit_code" => Command::SubmitCode(id()?, b.to_string()),
        "cancel_pair" => Command::CancelPair(id()?),
        "unpair" => Command::Unpair(id()?),
        "block" => Command::Block(id()?),
        "unblock" => Command::Unblock(id()?),
        "connect" => Command::Connect(a.to_string()),
        "remove_manual_peer" => Command::RemoveManualPeer(a.to_string()),
        "set_settings" => Command::SetSettings(serde_json::from_str::<Settings>(a).map_err(|e| e.to_string())?),
        "set_name" => Command::SetName(a.to_string()),
        "return_home" => Command::ReturnHome,
        "control_peer" => Command::ControlPeer(id()?),
        "start_transfer" => Command::StartTransfer(id()?),
        "save_transfer_to" => Command::SaveTransferTo(id()?, PathBuf::from(b)),
        "cancel_transfer" => Command::CancelTransfer(id()?),
        "dismiss_transfer" => Command::DismissTransfer(id()?),
        "refresh_permissions" => Command::RefreshPermissions,
        "send_snapshot" => Command::SendSnapshot(id()?),
        "request_snapshot" => Command::RequestSnapshot(Uuid::parse_str(a).ok()),
        other => return Err(format!("unknown command {other}")),
    })
}

/// Run a command by name. Returns an error message, or an empty string.
pub fn command(name: &str, a: &str, b: &str) -> String {
    match name {
        "request_permission" => {
            crate::permissions::request(a);
            return String::new();
        }
        "open_permission_settings" => {
            crate::permissions::open_settings(a);
            return String::new();
        }
        _ => {}
    }
    match (parse_command(name, a, b), ENGINE.lock().as_ref()) {
        (Ok(cmd), Some(engine)) => {
            engine.send(cmd);
            String::new()
        }
        (Ok(_), None) => "engine not running".to_string(),
        (Err(e), _) => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_parse() {
        let id = Uuid::new_v4().to_string();
        assert!(matches!(parse_command("pair", &id, ""), Ok(Command::Pair(_))));
        assert!(matches!(parse_command("submit_code", &id, "123456"), Ok(Command::SubmitCode(_, c)) if c == "123456"));
        assert!(parse_command("pair", "not-a-uuid", "").is_err());
        assert!(parse_command("nope", "", "").is_err());
    }

    #[test]
    fn button_indices_roundtrip() {
        for i in 0..5 {
            assert_eq!(button_index(button_from_index(i).unwrap()), i);
        }
        assert!(button_from_index(9).is_none());
    }
}
