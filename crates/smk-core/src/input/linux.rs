//! Linux backend: evdev for capture (works on X11 and Wayland), uinput for
//! injection. The cursor position needed for edge detection comes from X11
//! when available; on pure Wayland only hotkey switching works.
//!
//! Requires read access to `/dev/input/event*` and write access to
//! `/dev/uinput` (membership in the `input` group plus the udev rule shipped
//! in `packaging/linux/`).

use std::collections::HashSet;
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, Device, EventType, InputEvent as EvEvent, KeyCode,
    RelativeAxisCode, UinputAbsSetup,
};
use parking_lot::Mutex;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::rust_connection::RustConnection;

use super::{CaptureSender, CaptureShared, CapturedEvent, InputEvent, MouseButton, Rect};

const VIRTUAL_PREFIX: &str = "Shared Mouse Keyboard";
const EVIOCGRAB: libc::c_ulong = 0x4004_4590;

const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;
const BTN_TOUCH: u16 = 0x14a;
const BTN_TOOL_FINGER: u16 = 0x145;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;
const REL_WHEEL_HI_RES: u16 = 0x0b;
const REL_HWHEEL_HI_RES: u16 = 0x0c;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_RANGE: i32 = 65535;

/// File descriptors of every device we capture from, for EVIOCGRAB.
static DEVICE_FDS: Mutex<Vec<RawFd>> = Mutex::new(Vec::new());
/// Small virtual keyboard used to release keys that were pressed locally
/// before we grabbed the devices (a grabbed release never reaches the desktop).
static RELEASE_DEV: Mutex<Option<VirtualDevice>> = Mutex::new(None);

struct X11 {
    conn: RustConnection,
    root: u32,
    w: u16,
    h: u16,
}

fn x11() -> Option<X11> {
    std::env::var_os("DISPLAY")?;
    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let screen = conn.setup().roots.get(screen_num)?.clone();
    Some(X11 {
        conn,
        root: screen.root,
        w: screen.width_in_pixels,
        h: screen.height_in_pixels,
    })
}

fn query_pointer(x: &X11) -> Option<(f64, f64)> {
    let reply = x.conn.query_pointer(x.root).ok()?.reply().ok()?;
    Some((reply.root_x as f64, reply.root_y as f64))
}

fn button_from_code(code: u16) -> Option<MouseButton> {
    Some(match code {
        BTN_LEFT => MouseButton::Left,
        BTN_RIGHT => MouseButton::Right,
        BTN_MIDDLE => MouseButton::Middle,
        BTN_SIDE => MouseButton::Back,
        BTN_EXTRA => MouseButton::Forward,
        _ => return None,
    })
}

fn button_code(b: MouseButton) -> u16 {
    match b {
        MouseButton::Left => BTN_LEFT,
        MouseButton::Right => BTN_RIGHT,
        MouseButton::Middle => BTN_MIDDLE,
        MouseButton::Back => BTN_SIDE,
        MouseButton::Forward => BTN_EXTRA,
    }
}

fn is_input_device(dev: &Device) -> bool {
    if dev.name().unwrap_or("").starts_with(VIRTUAL_PREFIX) {
        return false;
    }
    let keyboard = dev
        .supported_keys()
        .is_some_and(|k| k.contains(KeyCode::KEY_A) && k.contains(KeyCode::KEY_ENTER));
    let mouse = dev
        .supported_relative_axes()
        .is_some_and(|r| r.contains(RelativeAxisCode::REL_X));
    let touchpad = dev
        .supported_absolute_axes()
        .is_some_and(|a| a.contains(AbsoluteAxisCode::ABS_X))
        && dev
            .supported_keys()
            .is_some_and(|k| k.contains(KeyCode::BTN_TOOL_FINGER));
    keyboard || mouse || touchpad
}

pub fn start_capture(tx: CaptureSender, shared: Arc<CaptureShared>) -> anyhow::Result<()> {
    let devices: Vec<(PathBuf, Device)> = evdev::enumerate().filter(|(_, d)| is_input_device(d)).collect();
    if devices.is_empty() {
        anyhow::bail!(
            "No readable input devices in /dev/input. Add your user to the 'input' group (see Permissions)."
        );
    }
    let opened = Arc::new(Mutex::new(HashSet::new()));
    for (path, dev) in devices {
        spawn_reader(path, dev, tx.clone(), shared.clone(), opened.clone());
    }
    // Pick up hot-plugged devices.
    std::thread::Builder::new().name("smk-hotplug".into()).spawn(move || loop {
        std::thread::sleep(Duration::from_secs(3));
        for (path, dev) in evdev::enumerate() {
            if !opened.lock().contains(&path) && is_input_device(&dev) {
                spawn_reader(path, dev, tx.clone(), shared.clone(), opened.clone());
            }
        }
    })?;
    Ok(())
}

fn spawn_reader(
    path: PathBuf,
    mut dev: Device,
    tx: CaptureSender,
    shared: Arc<CaptureShared>,
    opened: Arc<Mutex<HashSet<PathBuf>>>,
) {
    opened.lock().insert(path.clone());
    let fd = dev.as_raw_fd();
    DEVICE_FDS.lock().push(fd);
    if shared.grabbing() {
        grab_fd(fd, true);
    }
    let _ = std::thread::Builder::new().name("smk-evdev".into()).spawn(move || {
        let mut x11 = x11();
        let (tp_scale_x, tp_scale_y) = touchpad_scale(&dev);
        let mut rel = (0.0f64, 0.0f64);
        let mut wheel = (0i32, 0i32);
        let mut hires_seen = false;
        let mut touch_last: Option<(i32, i32)> = None;
        let mut touching = false;
        let mut abs_now: (Option<i32>, Option<i32>) = (None, None);
        loop {
            let events: Vec<EvEvent> = match dev.fetch_events() {
                Ok(it) => it.collect(),
                Err(_) => break,
            };
            for e in events {
                match e.event_type() {
                    EventType::RELATIVE => match e.code() {
                        REL_X => rel.0 += e.value() as f64,
                        REL_Y => rel.1 += e.value() as f64,
                        REL_WHEEL_HI_RES => {
                            hires_seen = true;
                            wheel.1 += e.value();
                        }
                        REL_HWHEEL_HI_RES => {
                            hires_seen = true;
                            wheel.0 += e.value();
                        }
                        REL_WHEEL if !hires_seen => wheel.1 += e.value() * 120,
                        REL_HWHEEL if !hires_seen => wheel.0 += e.value() * 120,
                        _ => {}
                    },
                    EventType::ABSOLUTE => match e.code() {
                        ABS_X => abs_now.0 = Some(e.value()),
                        ABS_Y => abs_now.1 = Some(e.value()),
                        _ => {}
                    },
                    EventType::KEY => {
                        let code = e.code();
                        let down = e.value() != 0;
                        if code == BTN_TOUCH || code == BTN_TOOL_FINGER {
                            if code == BTN_TOOL_FINGER {
                                touching = down;
                                if !down {
                                    touch_last = None;
                                }
                            }
                            continue;
                        }
                        if let Some(button) = button_from_code(code) {
                            if e.value() == 2 {
                                continue;
                            }
                            let swallow = shared.filter_button(button, down);
                            let _ = tx.send(CapturedEvent::Button { button, down });
                            if shared.grabbing() && !swallow {
                                release_locally(code);
                            }
                        } else if !(0x100..0x160).contains(&code) {
                            let swallow = shared.filter_key(code, down);
                            let _ = tx.send(CapturedEvent::Key { code, down });
                            if shared.grabbing() && !swallow {
                                release_locally(code);
                            }
                        }
                    }
                    EventType::SYNCHRONIZATION => {
                        if touching {
                            if let (Some(ax), Some(ay)) = abs_now {
                                if let Some((lx, ly)) = touch_last {
                                    rel.0 += (ax - lx) as f64 * tp_scale_x;
                                    rel.1 += (ay - ly) as f64 * tp_scale_y;
                                }
                                touch_last = Some((ax, ay));
                            }
                        }
                        if rel != (0.0, 0.0) {
                            let (x, y) = if shared.grabbing() {
                                (f64::NAN, f64::NAN)
                            } else {
                                x11.as_ref().and_then(query_pointer).unwrap_or((f64::NAN, f64::NAN))
                            };
                            let _ = tx.send(CapturedEvent::Motion { x, y, dx: rel.0, dy: rel.1 });
                            rel = (0.0, 0.0);
                        }
                        if wheel != (0, 0) {
                            let _ = tx.send(CapturedEvent::Scroll { dx: wheel.0, dy: wheel.1 });
                            wheel = (0, 0);
                        }
                    }
                    _ => {}
                }
            }
            if x11.is_none() && std::env::var_os("DISPLAY").is_some() {
                x11 = self::x11();
            }
        }
        // Device unplugged.
        DEVICE_FDS.lock().retain(|f| *f != fd);
        opened.lock().remove(&path);
    });
}

/// Pixels per touchpad unit, assuming ~4 px per mm when resolution is known.
fn touchpad_scale(dev: &Device) -> (f64, f64) {
    let Ok(state) = dev.get_abs_state() else { return (0.5, 0.5) };
    let rx = state[ABS_X as usize].resolution;
    let ry = state[ABS_Y as usize].resolution;
    let f = |r: i32| if r > 0 { 4.0 / r as f64 } else { 0.5 };
    (f(rx), f(ry))
}

fn grab_fd(fd: RawFd, grab: bool) {
    unsafe {
        libc::ioctl(fd, EVIOCGRAB as _, if grab { 1 as libc::c_int } else { 0 });
    }
}

fn release_locally(code: u16) {
    let mut guard = RELEASE_DEV.lock();
    if guard.is_none() {
        let mut keys = AttributeSet::<KeyCode>::new();
        for c in 1..0x2ffu16 {
            keys.insert(KeyCode(c));
        }
        let name = format!("{VIRTUAL_PREFIX} release helper");
        *guard = VirtualDevice::builder()
            .and_then(|b| b.name(&name).with_keys(&keys))
            .and_then(|b| b.build())
            .ok();
    }
    if let Some(dev) = guard.as_mut() {
        let _ = dev.emit(&[EvEvent::new(EventType::KEY.0, code, 0)]);
    }
}

pub fn set_grab(shared: &CaptureShared, grab: bool) {
    shared.set_grabbing(grab);
    for fd in DEVICE_FDS.lock().iter() {
        grab_fd(*fd, grab);
    }
}

pub fn screen_bounds(fallback: (u32, u32)) -> Rect {
    if let Some(x) = x11() {
        return Rect { x: 0.0, y: 0.0, w: x.w as f64, h: x.h as f64 };
    }
    Rect { x: 0.0, y: 0.0, w: fallback.0 as f64, h: fallback.1 as f64 }
}

pub fn warp(x: f64, y: f64) {
    if let Some(c) = x11() {
        let _ = c.conn.warp_pointer(x11rb::NONE, c.root, 0, 0, 0, 0, x as i16, y as i16);
        let _ = c.conn.flush();
    }
}

pub struct Injector {
    keyboard: VirtualDevice,
    pointer: VirtualDevice,
    bounds: Rect,
    pressed: HashSet<u16>,
}

impl Injector {
    pub fn new() -> anyhow::Result<Self> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for c in 1..0x100u16 {
            keys.insert(KeyCode(c));
        }
        keys.insert(KeyCode(crate::keymap::ev::FN));
        for c in 183..=194u16 {
            keys.insert(KeyCode(c));
        }
        let kb_name = format!("{VIRTUAL_PREFIX} virtual keyboard");
        let ptr_name = format!("{VIRTUAL_PREFIX} virtual pointer");
        let keyboard = VirtualDevice::builder()
            .map_err(perm_err)?
            .name(&kb_name)
            .with_keys(&keys)?
            .build()?;

        // Modelled after the QEMU USB tablet, which libinput treats as an
        // absolute pointer on both X11 and Wayland.
        let mut buttons = AttributeSet::<KeyCode>::new();
        for b in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA] {
            buttons.insert(KeyCode(b));
        }
        let mut rels = AttributeSet::<RelativeAxisCode>::new();
        for r in [REL_WHEEL, REL_HWHEEL, REL_WHEEL_HI_RES, REL_HWHEEL_HI_RES] {
            rels.insert(RelativeAxisCode(r));
        }
        let abs = AbsInfo::new(0, 0, ABS_RANGE, 0, 0, 0);
        let pointer = VirtualDevice::builder()?
            .name(&ptr_name)
            .with_keys(&buttons)?
            .with_relative_axes(&rels)?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_X, abs))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_Y, abs))?
            .build()?;
        Ok(Self {
            keyboard,
            pointer,
            bounds: screen_bounds((1920, 1080)),
            pressed: HashSet::new(),
        })
    }

    pub fn set_bounds(&mut self, r: Rect) {
        self.bounds = r;
    }

    pub fn inject(&mut self, event: InputEvent) {
        let syn = EvEvent::new(EventType::SYNCHRONIZATION.0, 0, 0);
        match event {
            InputEvent::MouseMove { x, y } => {
                let b = self.bounds;
                let nx = (((x - b.x) / b.w.max(1.0)) * ABS_RANGE as f64).round() as i32;
                let ny = (((y - b.y) / b.h.max(1.0)) * ABS_RANGE as f64).round() as i32;
                let _ = self.pointer.emit(&[
                    EvEvent::new(EventType::ABSOLUTE.0, ABS_X, nx.clamp(0, ABS_RANGE)),
                    EvEvent::new(EventType::ABSOLUTE.0, ABS_Y, ny.clamp(0, ABS_RANGE)),
                    syn,
                ]);
            }
            InputEvent::MouseButton { button, down } => {
                let _ = self.pointer.emit(&[
                    EvEvent::new(EventType::KEY.0, button_code(button), down as i32),
                    syn,
                ]);
            }
            InputEvent::Scroll { dx, dy } => {
                let mut evs = Vec::new();
                if dy != 0 {
                    evs.push(EvEvent::new(EventType::RELATIVE.0, REL_WHEEL_HI_RES, dy));
                    if dy.abs() >= 120 {
                        evs.push(EvEvent::new(EventType::RELATIVE.0, REL_WHEEL, dy / 120));
                    }
                }
                if dx != 0 {
                    evs.push(EvEvent::new(EventType::RELATIVE.0, REL_HWHEEL_HI_RES, dx));
                    if dx.abs() >= 120 {
                        evs.push(EvEvent::new(EventType::RELATIVE.0, REL_HWHEEL, dx / 120));
                    }
                }
                evs.push(syn);
                let _ = self.pointer.emit(&evs);
            }
            InputEvent::Key { code, down } => {
                let value = if down {
                    if self.pressed.insert(code) { 1 } else { 2 }
                } else {
                    self.pressed.remove(&code);
                    0
                };
                let _ = self
                    .keyboard
                    .emit(&[EvEvent::new(EventType::KEY.0, code, value), syn]);
            }
        }
    }
}

fn perm_err(e: std::io::Error) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot open /dev/uinput ({e}). Install the udev rule from packaging/linux and add your user to the 'input' group."
    )
}
