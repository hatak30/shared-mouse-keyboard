//! OS permission checks shown in the control panel.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Permission {
    /// Stable identifier used by `open_settings` / `request`.
    pub id: &'static str,
    pub name: &'static str,
    pub granted: bool,
    /// What it is for and how to grant it.
    pub help: String,
    /// Whether the UI can offer a "Request" / "Open settings" button.
    pub actionable: bool,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Permission;
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: core_foundation::dictionary::CFDictionaryRef) -> bool;
    }
    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOHIDCheckAccess(request: u32) -> u32;
        fn IOHIDRequestAccess(request: u32) -> bool;
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightPostEventAccess() -> bool;
        fn CGRequestPostEventAccess() -> bool;
    }
    const LISTEN_EVENT: u32 = 1;
    const GRANTED: u32 = 0;

    pub fn check() -> Vec<Permission> {
        let ax = unsafe { AXIsProcessTrusted() };
        let listen = unsafe { IOHIDCheckAccess(LISTEN_EVENT) } == GRANTED;
        let post = unsafe { CGPreflightPostEventAccess() };
        vec![
            Permission {
                id: "accessibility",
                name: "Accessibility",
                granted: ax && post,
                help: "Needed to control this Mac from another machine and to capture the cursor. \
                       System Settings → Privacy & Security → Accessibility → enable Shared Mouse Keyboard. \
                       Restart the app after granting."
                    .into(),
                actionable: true,
            },
            Permission {
                id: "input-monitoring",
                name: "Input Monitoring",
                granted: listen,
                help: "Needed to read this Mac's keyboard and mouse so they can control other machines. \
                       System Settings → Privacy & Security → Input Monitoring."
                    .into(),
                actionable: true,
            },
            Permission {
                id: "local-network",
                name: "Local Network",
                granted: true,
                help: "macOS asks once on first launch to allow finding devices on the local network. \
                       If you declined: System Settings → Privacy & Security → Local Network."
                    .into(),
                actionable: true,
            },
        ]
    }

    pub fn request(id: &str) {
        match id {
            "accessibility" => unsafe {
                let key = CFString::new("AXTrustedCheckOptionPrompt");
                let dict = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), CFBoolean::true_value().as_CFType())]);
                AXIsProcessTrustedWithOptions(dict.as_concrete_TypeRef());
                CGRequestPostEventAccess();
            },
            "input-monitoring" => unsafe {
                IOHIDRequestAccess(LISTEN_EVENT);
            },
            _ => {}
        }
        open_settings(id);
    }

    pub fn open_settings(id: &str) {
        let anchor = match id {
            "accessibility" => "Privacy_Accessibility",
            "input-monitoring" => "Privacy_ListenEvent",
            "local-network" => "Privacy_LocalNetwork",
            _ => "Privacy",
        };
        let _ = std::process::Command::new("open")
            .arg(format!("x-apple.systempreferences:com.apple.preference.security?{anchor}"))
            .spawn();
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::Permission;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    fn elevated() -> bool {
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let mut elev = TOKEN_ELEVATION::default();
            let mut len = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some(&mut elev as *mut _ as *mut _),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(token);
            ok && elev.TokenIsElevated != 0
        }
    }

    pub fn check() -> Vec<Permission> {
        vec![
            Permission {
                id: "firewall",
                name: "Windows Firewall",
                granted: true,
                help: "On first start Windows asks whether to allow the app on private networks — choose Allow. \
                       To change later: Windows Security → Firewall → Allow an app through firewall."
                    .into(),
                actionable: true,
            },
            Permission {
                id: "elevation",
                name: "Administrator (optional)",
                granted: elevated(),
                help: "Without administrator rights, remote input cannot reach windows running as administrator \
                       (e.g. Task Manager, installers). Run the app as administrator to control those too."
                    .into(),
                actionable: false,
            },
        ]
    }

    pub fn request(id: &str) {
        open_settings(id);
    }

    pub fn open_settings(id: &str) {
        if id == "firewall" {
            let _ = std::process::Command::new("control")
                .args(["/name", "Microsoft.WindowsFirewall", "/page", "pageConfigureApps"])
                .spawn();
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::Permission;
    use std::fs::OpenOptions;

    fn can_read_input() -> bool {
        std::fs::read_dir("/dev/input")
            .map(|rd| {
                rd.flatten().any(|e| {
                    e.file_name().to_string_lossy().starts_with("event")
                        && OpenOptions::new().read(true).open(e.path()).is_ok()
                })
            })
            .unwrap_or(false)
    }

    fn can_write_uinput() -> bool {
        OpenOptions::new().write(true).open("/dev/uinput").is_ok()
    }

    pub fn check() -> Vec<Permission> {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let x11 = std::env::var_os("DISPLAY").is_some();
        let mut v = vec![
            Permission {
                id: "input",
                name: "Read input devices (/dev/input)",
                granted: can_read_input(),
                help: "Run: sudo usermod -aG input $USER — then log out and back in.".into(),
                actionable: false,
            },
            Permission {
                id: "uinput",
                name: "Create virtual devices (/dev/uinput)",
                granted: can_write_uinput(),
                help: "Install the udev rule: sudo cp packaging/linux/60-shared-mouse-keyboard.rules \
                       /etc/udev/rules.d/ && sudo udevadm control --reload && sudo udevadm trigger \
                       (also requires the 'input' group)."
                    .into(),
                actionable: false,
            },
        ];
        if wayland {
            v.push(Permission {
                id: "wayland",
                name: "Edge switching on Wayland",
                granted: x11,
                help: "Under pure Wayland the cursor position cannot be read, so moving to another screen \
                       by pushing against an edge only works over XWayland windows. Use the hotkeys \
                       (Ctrl+Alt+Shift+Arrow) to switch, or log in with an X11 session."
                    .into(),
                actionable: false,
            });
        }
        v
    }

    pub fn request(_id: &str) {}
    pub fn open_settings(_id: &str) {}
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod imp {
    use super::Permission;
    pub fn check() -> Vec<Permission> {
        Vec::new()
    }
    pub fn request(_id: &str) {}
    pub fn open_settings(_id: &str) {}
}

pub use imp::{check, open_settings, request};

pub fn all_granted() -> bool {
    check().iter().all(|p| p.granted || !p.actionable)
}
