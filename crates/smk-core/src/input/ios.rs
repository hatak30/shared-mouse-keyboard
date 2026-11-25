//! iOS backend. The system offers no way to capture or synthesise input, so
//! an iPhone can only act as a remote: the app feeds touchpad and keyboard
//! events into the engine (`EngineHandle::simulate_input`).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use super::{CaptureSender, CaptureShared, InputEvent, Rect};

static WIDTH: AtomicU32 = AtomicU32::new(0);
static HEIGHT: AtomicU32 = AtomicU32::new(0);

/// Called by the app with the screen size in points.
pub fn set_screen_size(w: u32, h: u32) {
    WIDTH.store(w, Ordering::Relaxed);
    HEIGHT.store(h, Ordering::Relaxed);
}

pub fn start_capture(_tx: CaptureSender, _shared: Arc<CaptureShared>) -> anyhow::Result<()> {
    Ok(())
}

pub fn set_grab(shared: &CaptureShared, grab: bool) {
    shared.set_grabbing(grab);
}

pub fn screen_bounds(fallback: (u32, u32)) -> Rect {
    let (w, h) = (WIDTH.load(Ordering::Relaxed), HEIGHT.load(Ordering::Relaxed));
    let (w, h) = if w > 0 && h > 0 { (w, h) } else { fallback };
    Rect { x: 0.0, y: 0.0, w: w as f64, h: h as f64 }
}

pub fn warp(_x: f64, _y: f64) {}

pub struct Injector;

impl Injector {
    pub fn new() -> anyhow::Result<Self> {
        anyhow::bail!("iOS does not allow other computers to control this device")
    }

    pub fn set_bounds(&mut self, _r: Rect) {}

    pub fn inject(&mut self, _event: InputEvent) {}
}
