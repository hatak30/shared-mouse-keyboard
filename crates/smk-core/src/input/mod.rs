//! Platform input capture (global hooks) and injection (synthetic events).
//!
//! Each backend provides:
//! * `start_capture(tx, shared)` — install a global hook on a background
//!   thread, forward every physical event as a [`CapturedEvent`] and swallow
//!   events while `shared.grabbing` is set (so they only reach the remote),
//! * `set_grab(shared, bool)` — freeze/unfreeze the local cursor,
//! * `screen_bounds(fallback)` — the local desktop rectangle,
//! * `warp(x, y)` — move the local cursor,
//! * `Injector` — replays [`InputEvent`]s received from a remote machine.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use tokio::sync::mpsc::UnboundedSender;

pub use crate::protocol::{InputEvent, MouseButton, Rect};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

/// Marker attached to events we synthesise so our own hooks can ignore them.
pub const INJECTED_MAGIC: i64 = 0x534d_4b21; // "SMK!"

#[derive(Debug, Clone, PartialEq)]
pub enum CapturedEvent {
    /// `x`,`y`: local cursor position (if known), `dx`,`dy`: relative motion.
    Motion { x: f64, y: f64, dx: f64, dy: f64 },
    Button { button: MouseButton, down: bool },
    /// 1/120 notch units, positive y = up, positive x = right.
    Scroll { dx: i32, dy: i32 },
    /// evdev key code.
    Key { code: u16, down: bool },
    /// The capture backend stopped (e.g. permission revoked).
    Stopped(String),
    /// The paste shortcut was pressed locally while files copied on another
    /// machine are waiting; the keystroke itself was swallowed.
    Paste,
}

pub type CaptureSender = UnboundedSender<CapturedEvent>;

/// State shared between the engine and the capture hook thread.
#[derive(Default)]
pub struct CaptureShared {
    grabbing: AtomicBool,
    /// Keys pressed while input was delivered locally; their release must also
    /// be delivered locally even if we started grabbing in the meantime,
    /// otherwise the local machine would see a stuck key.
    local_pressed: Mutex<HashSet<u16>>,
    local_buttons: Mutex<HashSet<MouseButton>>,
    /// Set by the engine while remote files wait for a paste.
    paste_armed: AtomicBool,
    /// The V of an intercepted paste is held down (swallow its repeats/release).
    paste_swallowing: AtomicBool,
    paste_tx: Mutex<Option<UnboundedSender<CapturedEvent>>>,
}

impl CaptureShared {
    pub fn grabbing(&self) -> bool {
        self.grabbing.load(Ordering::SeqCst)
    }

    pub(crate) fn set_grabbing(&self, v: bool) {
        self.grabbing.store(v, Ordering::SeqCst);
    }

    /// Intercept the local paste shortcut while `armed` (see [`CapturedEvent::Paste`]).
    pub(crate) fn arm_paste(&self, armed: bool, tx: &UnboundedSender<CapturedEvent>) {
        if armed {
            self.paste_tx.lock().get_or_insert_with(|| tx.clone());
        }
        self.paste_armed.store(armed, Ordering::SeqCst);
    }

    /// Returns true if the key event must be swallowed (not delivered locally).
    pub fn filter_key(&self, code: u16, down: bool) -> bool {
        if code == crate::keymap::ev::V && !self.grabbing() {
            if self.paste_swallowing.load(Ordering::SeqCst) {
                if !down {
                    self.paste_swallowing.store(false, Ordering::SeqCst);
                }
                return true;
            }
            if down && self.paste_armed.load(Ordering::SeqCst) && crate::keymap::is_paste_chord(&self.local_pressed.lock()) {
                self.paste_swallowing.store(true, Ordering::SeqCst);
                if let Some(tx) = self.paste_tx.lock().as_ref() {
                    let _ = tx.send(CapturedEvent::Paste);
                }
                return true;
            }
        }
        let mut pressed = self.local_pressed.lock();
        if !self.grabbing() {
            if down {
                pressed.insert(code);
            } else {
                pressed.remove(&code);
            }
            false
        } else {
            !(!down && pressed.remove(&code))
        }
    }

    /// Same as [`filter_key`] for mouse buttons.
    pub fn filter_button(&self, button: MouseButton, down: bool) -> bool {
        let mut pressed = self.local_buttons.lock();
        if !self.grabbing() {
            if down {
                pressed.insert(button);
            } else {
                pressed.remove(&button);
            }
            false
        } else {
            !(!down && pressed.remove(&button))
        }
    }

    pub fn any_local_button(&self) -> bool {
        !self.local_buttons.lock().is_empty()
    }
}

/// Converts scroll units into whole "ticks" for platforms that need them,
/// keeping the remainder for later.
#[derive(Default)]
pub(crate) struct ScrollAccumulator {
    rem_x: i32,
    rem_y: i32,
}

impl ScrollAccumulator {
    #[allow(dead_code)]
    pub fn feed(&mut self, dx: i32, dy: i32, unit: i32) -> (i32, i32) {
        self.rem_x += dx;
        self.rem_y += dy;
        let tx = self.rem_x / unit;
        let ty = self.rem_y / unit;
        self.rem_x -= tx * unit;
        self.rem_y -= ty * unit;
        (tx, ty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::ev;

    #[test]
    fn paste_chord_is_swallowed_only_while_armed() {
        let shared = CaptureShared::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let m = if cfg!(target_os = "macos") { ev::LEFTMETA } else { ev::LEFTCTRL };

        // Not armed: a normal paste goes through.
        assert!(!shared.filter_key(m, true));
        assert!(!shared.filter_key(ev::V, true));
        assert!(!shared.filter_key(ev::V, false));
        assert!(rx.try_recv().is_err());

        // Armed: V (and its repeat and release) is swallowed, reported once.
        shared.arm_paste(true, &tx);
        assert!(shared.filter_key(ev::V, true));
        assert!(shared.filter_key(ev::V, true));
        assert!(shared.filter_key(ev::V, false));
        assert!(!shared.filter_key(m, false));
        assert_eq!(rx.try_recv().ok(), Some(CapturedEvent::Paste));
        assert!(rx.try_recv().is_err());

        // V alone, or with Shift as well, is left alone.
        assert!(!shared.filter_key(ev::V, true));
        assert!(!shared.filter_key(ev::V, false));
        assert!(!shared.filter_key(m, true));
        assert!(!shared.filter_key(ev::LEFTSHIFT, true));
        assert!(!shared.filter_key(ev::V, true));
        assert!(rx.try_recv().is_err());
    }
}
