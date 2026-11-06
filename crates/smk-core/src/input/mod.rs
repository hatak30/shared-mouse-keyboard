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
}

impl CaptureShared {
    pub fn grabbing(&self) -> bool {
        self.grabbing.load(Ordering::SeqCst)
    }

    pub(crate) fn set_grabbing(&self, v: bool) {
        self.grabbing.store(v, Ordering::SeqCst);
    }

    /// Returns true if the key event must be swallowed (not delivered locally).
    pub fn filter_key(&self, code: u16, down: bool) -> bool {
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
