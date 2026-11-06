//! Windows backend: low-level hooks (WH_MOUSE_LL / WH_KEYBOARD_LL) for
//! capture, SendInput for injection. No special permission is needed, but
//! input cannot reach elevated windows unless the app runs as administrator.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP,
    MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY, VK_PAUSE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetCursorPos, GetMessageW, GetSystemMetrics, SetCursorPos,
    SetWindowsHookExW, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_UP, MSG, MSLLHOOKSTRUCT,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN,
    WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SYSKEYDOWN, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

use super::{CaptureSender, CaptureShared, CapturedEvent, InputEvent, MouseButton, Rect, INJECTED_MAGIC};
use crate::keymap::{ev, ev_to_win, win_to_ev};

struct HookState {
    tx: CaptureSender,
    shared: Arc<CaptureShared>,
}

static HOOK: OnceLock<HookState> = OnceLock::new();
/// Cursor anchor while grabbing; deltas are measured relative to it.
static ANCHOR_X: AtomicI32 = AtomicI32::new(0);
static ANCHOR_Y: AtomicI32 = AtomicI32::new(0);

fn dpi_aware() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn start_capture(tx: CaptureSender, shared: Arc<CaptureShared>) -> anyhow::Result<()> {
    dpi_aware();
    if HOOK.set(HookState { tx: tx.clone(), shared }).is_err() {
        anyhow::bail!("capture already started");
    }
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("smk-capture".into())
        .spawn(move || unsafe {
            let hmod = GetModuleHandleW(None).ok().map(|m| HINSTANCE(m.0));
            let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hmod, 0);
            let kbd = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), hmod, 0);
            if let Err(e) = mouse.as_ref().and(kbd.as_ref()) {
                let _ = ready_tx.send(Err(format!("SetWindowsHookEx failed: {e}")));
                return;
            }
            let _ = ready_tx.send(Ok(()));
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
            let _ = tx.send(CapturedEvent::Stopped("hook message loop exited".into()));
        })?;
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(anyhow::anyhow!(e)),
        Err(_) => Err(anyhow::anyhow!("input hooks did not start")),
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let Some(state) = HOOK.get() else {
        return CallNextHookEx(None, code, wparam, lparam);
    };
    let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
    if info.dwExtraInfo == INJECTED_MAGIC as usize {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let grabbing = state.shared.grabbing();
    let msg = wparam.0 as u32;
    let swallow = match msg {
        WM_MOUSEMOVE => {
            let (dx, dy) = if grabbing {
                let ax = ANCHOR_X.load(Ordering::Relaxed);
                let ay = ANCHOR_Y.load(Ordering::Relaxed);
                ((info.pt.x - ax) as f64, (info.pt.y - ay) as f64)
            } else {
                let mut cur = POINT::default();
                let _ = GetCursorPos(&mut cur);
                ((info.pt.x - cur.x) as f64, (info.pt.y - cur.y) as f64)
            };
            let _ = state.tx.send(CapturedEvent::Motion {
                x: info.pt.x as f64,
                y: info.pt.y as f64,
                dx,
                dy,
            });
            grabbing
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN
        | WM_MBUTTONUP | WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let down = matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN);
            let button = match msg {
                WM_LBUTTONDOWN | WM_LBUTTONUP => MouseButton::Left,
                WM_RBUTTONDOWN | WM_RBUTTONUP => MouseButton::Right,
                WM_MBUTTONDOWN | WM_MBUTTONUP => MouseButton::Middle,
                _ => {
                    if (info.mouseData >> 16) & 0xffff == 1 {
                        MouseButton::Back
                    } else {
                        MouseButton::Forward
                    }
                }
            };
            let swallow = state.shared.filter_button(button, down);
            let _ = state.tx.send(CapturedEvent::Button { button, down });
            swallow
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = ((info.mouseData >> 16) & 0xffff) as u16 as i16 as i32;
            let ev = if msg == WM_MOUSEWHEEL {
                CapturedEvent::Scroll { dx: 0, dy: delta }
            } else {
                CapturedEvent::Scroll { dx: delta, dy: 0 }
            };
            let _ = state.tx.send(ev);
            grabbing
        }
        _ => grabbing,
    };
    if swallow {
        LRESULT(1)
    } else {
        CallNextHookEx(None, code, wparam, lparam)
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let Some(state) = HOOK.get() else {
        return CallNextHookEx(None, code, wparam, lparam);
    };
    let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
    if info.dwExtraInfo == INJECTED_MAGIC as usize {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let msg = wparam.0 as u32;
    let down = matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN) && (info.flags.0 & LLKHF_UP.0) == 0;
    let extended = info.flags.0 & LLKHF_EXTENDED.0 != 0;
    let evcode = if info.vkCode == VK_PAUSE.0 as u32 {
        Some(ev::PAUSE)
    } else {
        win_to_ev(info.scanCode as u16, extended)
    };
    let swallow = match evcode {
        Some(c) => {
            let s = state.shared.filter_key(c, down);
            let _ = state.tx.send(CapturedEvent::Key { code: c, down });
            s
        }
        None => state.shared.grabbing(),
    };
    if swallow {
        LRESULT(1)
    } else {
        CallNextHookEx(None, code, wparam, lparam)
    }
}

pub fn set_grab(shared: &CaptureShared, grab: bool) {
    if grab {
        // Park the cursor in the middle of the desktop so motion in every
        // direction produces deltas, then swallow further movement.
        let b = screen_bounds((0, 0));
        let (cx, cy) = b.center();
        ANCHOR_X.store(cx as i32, Ordering::Relaxed);
        ANCHOR_Y.store(cy as i32, Ordering::Relaxed);
        unsafe {
            let _ = SetCursorPos(cx as i32, cy as i32);
        }
    }
    shared.set_grabbing(grab);
}

pub fn screen_bounds(_fallback: (u32, u32)) -> Rect {
    dpi_aware();
    unsafe {
        Rect {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN) as f64,
            y: GetSystemMetrics(SM_YVIRTUALSCREEN) as f64,
            w: GetSystemMetrics(SM_CXVIRTUALSCREEN) as f64,
            h: GetSystemMetrics(SM_CYVIRTUALSCREEN) as f64,
        }
    }
}

pub fn warp(x: f64, y: f64) {
    unsafe {
        let _ = SetCursorPos(x as i32, y as i32);
    }
}

pub struct Injector {
    scroll: super::ScrollAccumulator,
}

impl Injector {
    pub fn new() -> anyhow::Result<Self> {
        dpi_aware();
        Ok(Self { scroll: Default::default() })
    }

    fn mouse(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data as u32,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: INJECTED_MAGIC as usize,
                },
            },
        };
        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }
    }

    fn key(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) {
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: INJECTED_MAGIC as usize,
                },
            },
        };
        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }
    }

    pub fn set_bounds(&mut self, _r: Rect) {}

    pub fn inject(&mut self, event: InputEvent) {
        match event {
            InputEvent::MouseMove { x, y } => {
                let b = screen_bounds((0, 0));
                let nx = ((x - b.x) * 65535.0 / (b.w - 1.0).max(1.0)).round() as i32;
                let ny = ((y - b.y) * 65535.0 / (b.h - 1.0).max(1.0)).round() as i32;
                Self::mouse(nx, ny, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK);
            }
            InputEvent::MouseButton { button, down } => {
                let (flags, data) = match (button, down) {
                    (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                    (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
                    (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                    (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
                    (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                    (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                    (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, 1),
                    (MouseButton::Back, false) => (MOUSEEVENTF_XUP, 1),
                    (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, 2),
                    (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, 2),
                };
                Self::mouse(0, 0, data, flags);
            }
            InputEvent::Scroll { dx, dy } => {
                // Windows accepts partial notches; pass small deltas through in
                // batches of 1/8 notch to avoid flooding.
                let (tx, ty) = self.scroll.feed(dx, dy, 15);
                if ty != 0 {
                    Self::mouse(0, 0, ty * 15, MOUSEEVENTF_WHEEL);
                }
                if tx != 0 {
                    Self::mouse(0, 0, tx * 15, MOUSEEVENTF_HWHEEL);
                }
            }
            InputEvent::Key { code, down } => {
                let up = if down { KEYBD_EVENT_FLAGS(0) } else { KEYEVENTF_KEYUP };
                if code == ev::PAUSE {
                    Self::key(VK_PAUSE.0, 0, up);
                    return;
                }
                let Some((scan, ext)) = ev_to_win(code) else { return };
                let mut flags = KEYEVENTF_SCANCODE | up;
                if ext {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                Self::key(0, scan, flags);
            }
        }
    }
}
