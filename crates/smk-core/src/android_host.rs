//! Bridge between the engine and the Android app.
//!
//! Android offers no global input hooks and no clipboard daemon the engine
//! could talk to directly, so everything platform specific is delegated to
//! the host app (an AccessibilityService, ClipboardManager, ...) through this
//! trait. The module is compiled on every platform so the JNI crate can be
//! checked and tested on a desktop.

use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use tokio::sync::mpsc::UnboundedSender;

use crate::protocol::{InputEvent, Rect};

pub trait AndroidHost: Send + Sync {
    /// Replay an event received from another machine. Returns false when the
    /// app cannot do it right now (e.g. the accessibility service is off).
    fn inject(&self, event: InputEvent) -> bool;
    /// Current screen size in pixels.
    fn screen(&self) -> Rect;
    fn clipboard_text(&self) -> Option<String>;
    fn set_clipboard_text(&self, text: &str);
    /// Whether the permission with this id (see `permissions`) is granted.
    fn permission_granted(&self, id: &str) -> bool;
    fn open_settings(&self, id: &str);
    /// Start capturing the screen; the PNG arrives through `embed::snapshot_ready`.
    /// Returns false when the app cannot do it right now.
    fn capture_screen(&self) -> bool {
        false
    }
    /// Start screen recording, or stop it; the finished video is handed back
    /// through `embed::recording_ready`. Returns false when the app cannot.
    fn toggle_recording(&self) -> bool {
        false
    }
}

static HOST: OnceLock<Arc<dyn AndroidHost>> = OnceLock::new();
static CLIPBOARD_CHANGED: Mutex<Option<UnboundedSender<()>>> = Mutex::new(None);

/// Register the host. Only the first call has an effect.
pub fn set_host(host: Arc<dyn AndroidHost>) {
    let _ = HOST.set(host);
}

pub fn host() -> Option<&'static Arc<dyn AndroidHost>> {
    HOST.get()
}

/// Tell the engine the Android clipboard changed (called by the app).
pub fn notify_clipboard_changed() {
    if let Some(tx) = CLIPBOARD_CHANGED.lock().as_ref() {
        let _ = tx.send(());
    }
}

#[cfg(target_os = "android")]
pub(crate) fn watch_clipboard(tx: UnboundedSender<()>) {
    *CLIPBOARD_CHANGED.lock() = Some(tx);
}
