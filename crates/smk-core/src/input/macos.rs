//! macOS backend: CGEventTap for capture, CGEventPost for injection.
//! Requires the Accessibility and Input Monitoring permissions.

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use core_foundation::base::TCFType;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventType, CGMouseButton, CallbackResult, EventField,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

use super::{CaptureSender, CaptureShared, CapturedEvent, InputEvent, MouseButton, Rect, INJECTED_MAGIC};
use crate::keymap::{ev, ev_to_mac, mac_to_ev};

extern "C" {
    fn CGEventTapEnable(tap: *mut c_void, enable: bool);
}

static TAP_PORT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

// Device-dependent modifier bits (IOLLEvent.h) to tell left from right.
const NX_DEVICELCTLKEYMASK: u64 = 0x0000_0001;
const NX_DEVICELSHIFTKEYMASK: u64 = 0x0000_0002;
const NX_DEVICERSHIFTKEYMASK: u64 = 0x0000_0004;
const NX_DEVICELCMDKEYMASK: u64 = 0x0000_0008;
const NX_DEVICERCMDKEYMASK: u64 = 0x0000_0010;
const NX_DEVICELALTKEYMASK: u64 = 0x0000_0020;
const NX_DEVICERALTKEYMASK: u64 = 0x0000_0040;
const NX_DEVICERCTLKEYMASK: u64 = 0x0000_2000;

fn modifier_down(mac_code: u16, flags: u64) -> Option<bool> {
    let bit = match mac_code {
        0x38 => NX_DEVICELSHIFTKEYMASK,
        0x3C => NX_DEVICERSHIFTKEYMASK,
        0x3B => NX_DEVICELCTLKEYMASK,
        0x3E => NX_DEVICERCTLKEYMASK,
        0x3A => NX_DEVICELALTKEYMASK,
        0x3D => NX_DEVICERALTKEYMASK,
        0x37 => NX_DEVICELCMDKEYMASK,
        0x36 => NX_DEVICERCMDKEYMASK,
        0x3F => CGEventFlags::CGEventFlagSecondaryFn.bits(),
        _ => return None,
    };
    Some(flags & bit != 0)
}

fn button_from_number(n: i64) -> MouseButton {
    match n {
        0 => MouseButton::Left,
        1 => MouseButton::Right,
        3 => MouseButton::Back,
        4 => MouseButton::Forward,
        _ => MouseButton::Middle,
    }
}

pub fn start_capture(tx: CaptureSender, shared: Arc<CaptureShared>) -> anyhow::Result<()> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("smk-capture".into())
        .spawn(move || {
            use CGEventType::*;
            let events = vec![
                MouseMoved, LeftMouseDragged, RightMouseDragged, OtherMouseDragged,
                LeftMouseDown, LeftMouseUp, RightMouseDown, RightMouseUp,
                OtherMouseDown, OtherMouseUp, ScrollWheel, KeyDown, KeyUp, FlagsChanged,
            ];
            let cb_tx = tx.clone();
            let tap = CGEventTap::new(
                CGEventTapLocation::Session,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                events,
                move |_proxy, ty, event| handle_event(&cb_tx, &shared, ty, event),
            );
            let tap = match tap {
                Ok(t) => t,
                Err(()) => {
                    let _ = ready_tx.send(Err(
                        "Could not create event tap. Grant Accessibility and Input Monitoring permissions."
                            .into(),
                    ));
                    return;
                }
            };
            TAP_PORT.store(tap.mach_port().as_concrete_TypeRef() as *mut c_void, Ordering::SeqCst);
            let source = tap
                .mach_port()
                .create_runloop_source(0)
                .expect("runloop source");
            CFRunLoop::get_current().add_source(&source, unsafe { kCFRunLoopCommonModes });
            tap.enable();
            let _ = ready_tx.send(Ok(()));
            CFRunLoop::run_current();
            let _ = tx.send(CapturedEvent::Stopped("event tap run loop exited".into()));
        })?;
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(anyhow::anyhow!(e)),
        Err(_) => Err(anyhow::anyhow!("event tap did not start")),
    }
}

fn handle_event(
    tx: &CaptureSender,
    shared: &CaptureShared,
    ty: CGEventType,
    event: &CGEvent,
) -> CallbackResult {
    use CGEventType::*;
    if matches!(ty, TapDisabledByTimeout | TapDisabledByUserInput) {
        let port = TAP_PORT.load(Ordering::SeqCst);
        if !port.is_null() {
            unsafe { CGEventTapEnable(port, true) };
        }
        return CallbackResult::Keep;
    }
    if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == INJECTED_MAGIC {
        return CallbackResult::Keep;
    }
    let grabbing = shared.grabbing();
    let verdict = |swallow: bool| if swallow { CallbackResult::Drop } else { CallbackResult::Keep };
    match ty {
        MouseMoved | LeftMouseDragged | RightMouseDragged | OtherMouseDragged => {
            let p = event.location();
            let dx = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X) as f64;
            let dy = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y) as f64;
            let _ = tx.send(CapturedEvent::Motion { x: p.x, y: p.y, dx, dy });
            verdict(grabbing)
        }
        LeftMouseDown | LeftMouseUp | RightMouseDown | RightMouseUp | OtherMouseDown
        | OtherMouseUp => {
            let down = matches!(ty, LeftMouseDown | RightMouseDown | OtherMouseDown);
            let button = button_from_number(
                event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER),
            );
            let swallow = shared.filter_button(button, down);
            let _ = tx.send(CapturedEvent::Button { button, down });
            verdict(swallow)
        }
        ScrollWheel => {
            let continuous =
                event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS) != 0;
            let (dy, dx) = if continuous {
                (
                    event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1) * 12,
                    event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2) * 12,
                )
            } else {
                (
                    event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1) * 120,
                    event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2) * 120,
                )
            };
            // macOS axis 2 is positive when scrolling left.
            let _ = tx.send(CapturedEvent::Scroll { dx: -dx as i32, dy: dy as i32 });
            verdict(grabbing)
        }
        KeyDown | KeyUp => {
            let mac = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
            let down = matches!(ty, KeyDown);
            match mac_to_ev(mac) {
                Some(code) => {
                    let swallow = shared.filter_key(code, down);
                    let _ = tx.send(CapturedEvent::Key { code, down });
                    verdict(swallow)
                }
                None => verdict(grabbing),
            }
        }
        FlagsChanged => {
            let mac = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
            let flags = event.get_flags().bits();
            let Some(code) = mac_to_ev(mac) else {
                return verdict(grabbing);
            };
            if mac == 0x39 {
                // Caps lock toggles: report a full press.
                let swallow = shared.filter_key(code, true);
                shared.filter_key(code, false);
                let _ = tx.send(CapturedEvent::Key { code, down: true });
                let _ = tx.send(CapturedEvent::Key { code, down: false });
                return verdict(swallow);
            }
            let Some(down) = modifier_down(mac, flags) else {
                return verdict(grabbing);
            };
            let swallow = shared.filter_key(code, down);
            let _ = tx.send(CapturedEvent::Key { code, down });
            verdict(swallow)
        }
        _ => CallbackResult::Keep,
    }
}

pub fn set_grab(shared: &CaptureShared, grab: bool) {
    shared.set_grabbing(grab);
    // Freezing the cursor keeps delta events flowing while it stays put.
    let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(!grab);
    let main = CGDisplay::main();
    if grab {
        let _ = main.hide_cursor();
    } else {
        let _ = main.show_cursor();
    }
}

pub fn screen_bounds(_fallback: (u32, u32)) -> Rect {
    let ids = CGDisplay::active_displays().unwrap_or_default();
    let mut rect: Option<(f64, f64, f64, f64)> = None;
    for id in ids {
        let b = CGDisplay::new(id).bounds();
        let (x0, y0, x1, y1) = (b.origin.x, b.origin.y, b.origin.x + b.size.width, b.origin.y + b.size.height);
        rect = Some(match rect {
            None => (x0, y0, x1, y1),
            Some((a, b, c, d)) => (a.min(x0), b.min(y0), c.max(x1), d.max(y1)),
        });
    }
    let (x0, y0, x1, y1) = rect.unwrap_or_else(|| {
        let b = CGDisplay::main().bounds();
        (b.origin.x, b.origin.y, b.size.width, b.size.height)
    });
    Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
}

pub fn warp(x: f64, y: f64) {
    let _ = CGDisplay::warp_mouse_cursor_position(CGPoint::new(x, y));
    // Warping suppresses events for 250ms by default; undo that.
    unsafe { CGSetLocalEventsSuppressionInterval(0.0) };
}

extern "C" {
    fn CGSetLocalEventsSuppressionInterval(seconds: f64) -> i32;
}

pub struct Injector {
    pos: (f64, f64),
    buttons: [bool; 5],
    mods: u64,
    last_click: Option<(Instant, MouseButton, (f64, f64), i64)>,
    scroll: super::ScrollAccumulator,
}

impl Injector {
    pub fn new() -> anyhow::Result<Self> {
        let b = screen_bounds((0, 0));
        Ok(Self {
            pos: b.center(),
            buttons: [false; 5],
            mods: 0,
            last_click: None,
            scroll: Default::default(),
        })
    }

    fn source() -> Option<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()
    }

    fn post(&self, ev: CGEvent) {
        ev.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, INJECTED_MAGIC);
        ev.post(CGEventTapLocation::HID);
    }

    fn button_index(b: MouseButton) -> usize {
        match b {
            MouseButton::Left => 0,
            MouseButton::Right => 1,
            MouseButton::Middle => 2,
            MouseButton::Back => 3,
            MouseButton::Forward => 4,
        }
    }

    fn cg_flags(&self) -> CGEventFlags {
        CGEventFlags::from_bits_truncate(self.mods)
    }

    fn update_mods(&mut self, code: u16, down: bool) {
        let (generic, device) = match code {
            ev::LEFTSHIFT => (CGEventFlags::CGEventFlagShift.bits(), NX_DEVICELSHIFTKEYMASK),
            ev::RIGHTSHIFT => (CGEventFlags::CGEventFlagShift.bits(), NX_DEVICERSHIFTKEYMASK),
            ev::LEFTCTRL => (CGEventFlags::CGEventFlagControl.bits(), NX_DEVICELCTLKEYMASK),
            ev::RIGHTCTRL => (CGEventFlags::CGEventFlagControl.bits(), NX_DEVICERCTLKEYMASK),
            ev::LEFTALT => (CGEventFlags::CGEventFlagAlternate.bits(), NX_DEVICELALTKEYMASK),
            ev::RIGHTALT => (CGEventFlags::CGEventFlagAlternate.bits(), NX_DEVICERALTKEYMASK),
            ev::LEFTMETA => (CGEventFlags::CGEventFlagCommand.bits(), NX_DEVICELCMDKEYMASK),
            ev::RIGHTMETA => (CGEventFlags::CGEventFlagCommand.bits(), NX_DEVICERCMDKEYMASK),
            ev::FN => (CGEventFlags::CGEventFlagSecondaryFn.bits(), 0),
            ev::CAPSLOCK => {
                if down {
                    self.mods ^= CGEventFlags::CGEventFlagAlphaShift.bits();
                }
                return;
            }
            _ => return,
        };
        if down {
            self.mods |= generic | device;
        } else {
            self.mods &= !device;
            // Clear the generic bit only if the other side is not held.
            let pair = match code {
                ev::LEFTSHIFT | ev::RIGHTSHIFT => NX_DEVICELSHIFTKEYMASK | NX_DEVICERSHIFTKEYMASK,
                ev::LEFTCTRL | ev::RIGHTCTRL => NX_DEVICELCTLKEYMASK | NX_DEVICERCTLKEYMASK,
                ev::LEFTALT | ev::RIGHTALT => NX_DEVICELALTKEYMASK | NX_DEVICERALTKEYMASK,
                ev::LEFTMETA | ev::RIGHTMETA => NX_DEVICELCMDKEYMASK | NX_DEVICERCMDKEYMASK,
                _ => 0,
            };
            if self.mods & pair == 0 {
                self.mods &= !generic;
            }
        }
    }

    pub fn set_bounds(&mut self, _r: Rect) {}

    pub fn inject(&mut self, event: InputEvent) {
        let Some(src) = Self::source() else { return };
        match event {
            InputEvent::MouseMove { x, y } => {
                self.pos = (x, y);
                let (ty, btn) = if self.buttons[0] {
                    (CGEventType::LeftMouseDragged, CGMouseButton::Left)
                } else if self.buttons[1] {
                    (CGEventType::RightMouseDragged, CGMouseButton::Right)
                } else if self.buttons[2..].iter().any(|b| *b) {
                    (CGEventType::OtherMouseDragged, CGMouseButton::Center)
                } else {
                    (CGEventType::MouseMoved, CGMouseButton::Left)
                };
                if let Ok(ev) = CGEvent::new_mouse_event(src, ty, CGPoint::new(x, y), btn) {
                    ev.set_flags(self.cg_flags());
                    self.post(ev);
                }
            }
            InputEvent::MouseButton { button, down } => {
                let idx = Self::button_index(button);
                self.buttons[idx] = down;
                let (ty, btn) = match (button, down) {
                    (MouseButton::Left, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
                    (MouseButton::Left, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
                    (MouseButton::Right, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
                    (MouseButton::Right, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
                    (_, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
                    (_, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
                };
                // Multi-click detection so double clicks work.
                let mut clicks = 1;
                if down {
                    if let Some((t, b, p, n)) = self.last_click {
                        let near = (p.0 - self.pos.0).abs() < 5.0 && (p.1 - self.pos.1).abs() < 5.0;
                        if b == button && near && t.elapsed() < Duration::from_millis(500) {
                            clicks = n + 1;
                        }
                    }
                    self.last_click = Some((Instant::now(), button, self.pos, clicks));
                } else if let Some((_, b, _, n)) = self.last_click {
                    if b == button {
                        clicks = n;
                    }
                }
                let point = CGPoint::new(self.pos.0, self.pos.1);
                if let Ok(ev) = CGEvent::new_mouse_event(src, ty, point, btn) {
                    ev.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, clicks);
                    ev.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, match button {
                        MouseButton::Left => 0,
                        MouseButton::Right => 1,
                        MouseButton::Middle => 2,
                        MouseButton::Back => 3,
                        MouseButton::Forward => 4,
                    });
                    ev.set_flags(self.cg_flags());
                    self.post(ev);
                }
            }
            InputEvent::Scroll { dx, dy } => {
                // 1 notch (120 units) = 10 pixels.
                let (px, py) = self.scroll.feed(dx, dy, 12);
                if px == 0 && py == 0 {
                    return;
                }
                if let Ok(ev) = CGEvent::new_scroll_event(src, ScrollEventUnit::PIXEL, 2, py, -px, 0) {
                    ev.set_flags(self.cg_flags());
                    self.post(ev);
                }
            }
            InputEvent::Key { code, down } => {
                let Some(mac) = ev_to_mac(code) else { return };
                self.update_mods(code, down);
                if let Ok(ev) = CGEvent::new_keyboard_event(src, mac, down) {
                    let is_mod = matches!(mac, 0x36..=0x3F);
                    if is_mod {
                        ev.set_type(CGEventType::FlagsChanged);
                    }
                    ev.set_flags(self.cg_flags());
                    self.post(ev);
                }
            }
        }
    }
}
