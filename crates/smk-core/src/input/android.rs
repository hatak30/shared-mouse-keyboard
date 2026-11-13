//! Android backend. Nothing is hooked globally: the app feeds touchpad and
//! keyboard events into the engine (`EngineHandle::simulate_input`) and
//! received events are replayed by the host app's AccessibilityService.

use std::sync::Arc;

use super::{CaptureSender, CaptureShared, InputEvent, Rect};
use crate::android_host;

pub fn start_capture(_tx: CaptureSender, _shared: Arc<CaptureShared>) -> anyhow::Result<()> {
    Ok(())
}

pub fn set_grab(shared: &CaptureShared, grab: bool) {
    shared.set_grabbing(grab);
}

pub fn screen_bounds(fallback: (u32, u32)) -> Rect {
    android_host::host()
        .map(|h| h.screen())
        .unwrap_or(Rect { x: 0.0, y: 0.0, w: fallback.0 as f64, h: fallback.1 as f64 })
}

pub fn warp(_x: f64, _y: f64) {}

pub struct Injector;

impl Injector {
    pub fn new() -> anyhow::Result<Self> {
        match android_host::host() {
            Some(h) if h.permission_granted("accessibility") => Ok(Self),
            Some(_) => anyhow::bail!("turn on the Shared Mouse & Keyboard accessibility service in Android settings"),
            None => anyhow::bail!("the Android host is not registered"),
        }
    }

    pub fn set_bounds(&mut self, _r: Rect) {}

    pub fn inject(&mut self, event: InputEvent) {
        if let Some(h) = android_host::host() {
            h.inject(event);
        }
    }
}
