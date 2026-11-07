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
    /// A recommendation, not a requirement (doesn't mark the tab as needing attention).
    #[serde(default)]
    pub optional: bool,
    /// Label of the button that fixes it (default "Grant access").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<&'static str>,
    /// Status shown when `granted` (default "Granted").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok_label: Option<&'static str>,
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
                optional: false,
                action: None,
                ok_label: None,
            },
            Permission {
                id: "input-monitoring",
                name: "Input Monitoring",
                granted: listen,
                help: "Needed to read this Mac's keyboard and mouse so they can control other machines. \
                       System Settings → Privacy & Security → Input Monitoring."
                    .into(),
                actionable: true,
                optional: false,
                action: None,
                ok_label: None,
            },
            Permission {
                id: "local-network",
                name: "Local Network",
                granted: true,
                help: "macOS asks once on first launch to allow finding devices on the local network. \
                       If you declined: System Settings → Privacy & Security → Local Network."
                    .into(),
                actionable: true,
                optional: false,
                action: None,
                ok_label: None,
            },
            Permission {
                id: "handoff",
                name: "Handoff (Universal Clipboard)",
                granted: !handoff_on(),
                help: "Recommended off. Handoff makes macOS share the clipboard between your Macs too: \
                       it downloads things copied on another Mac by itself (\"Pasting from …\"), \
                       even when you didn't paste. Shared Mouse Keyboard already shares the clipboard, \
                       and only copies files when you paste."
                    .into(),
                actionable: true,
                optional: true,
                action: Some("Turn off Handoff"),
                ok_label: Some("Off"),
            },
        ]
    }

    const HANDOFF_DOMAIN: &str = "com.apple.coreservices.useractivityd";
    const HANDOFF_KEYS: [&str; 2] = ["ActivityAdvertisingAllowed", "ActivityReceivingAllowed"];

    /// Handoff is on unless both directions are switched off (missing = on).
    fn handoff_on() -> bool {
        use core_foundation::base::{CFType, TCFType};
        use core_foundation_sys::preferences::*;
        let app = CFString::new(HANDOFF_DOMAIN);
        HANDOFF_KEYS.iter().any(|k| {
            let key = CFString::new(k);
            let v = unsafe {
                CFPreferencesCopyValue(
                    key.as_concrete_TypeRef(),
                    app.as_concrete_TypeRef(),
                    kCFPreferencesCurrentUser,
                    kCFPreferencesCurrentHost,
                )
            };
            if v.is_null() {
                return true;
            }
            let v = unsafe { CFType::wrap_under_create_rule(v) };
            v.downcast::<CFBoolean>().map_or_else(
                || v.downcast::<core_foundation::number::CFNumber>().and_then(|n| n.to_i64()).is_none_or(|n| n != 0),
                bool::from,
            )
        })
    }

    /// Switch Handoff off, as the switch in System Settings → General →
    /// AirDrop & Handoff does.
    fn turn_off_handoff() {
        use core_foundation_sys::preferences::*;
        let app = CFString::new(HANDOFF_DOMAIN);
        unsafe {
            for k in HANDOFF_KEYS {
                let key = CFString::new(k);
                CFPreferencesSetValue(
                    key.as_concrete_TypeRef(),
                    CFBoolean::false_value().as_CFTypeRef(),
                    app.as_concrete_TypeRef(),
                    kCFPreferencesCurrentUser,
                    kCFPreferencesCurrentHost,
                );
            }
            CFPreferencesSynchronize(app.as_concrete_TypeRef(), kCFPreferencesCurrentUser, kCFPreferencesCurrentHost);
        }
        // The Handoff service reads the setting when it starts; launchd
        // restarts it right away.
        let _ = std::process::Command::new("/usr/bin/killall").arg("useractivityd").status();
        tracing::info!("Handoff turned off");
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
            "handoff" => {
                turn_off_handoff();
                return;
            }
            _ => {}
        }
        open_settings(id);
    }

    pub fn open_settings(id: &str) {
        if id == "handoff" {
            let _ = std::process::Command::new("open")
                .arg("x-apple.systempreferences:com.apple.AirDrop-Handoff-Settings.extension")
                .spawn();
            return;
        }
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
                optional: false,
                action: None,
                ok_label: None,
            },
            Permission {
                id: "elevation",
                name: "Administrator (optional)",
                granted: elevated(),
                help: "Without administrator rights, remote input cannot reach windows running as administrator \
                       (e.g. Task Manager, installers). Run the app as administrator to control those too."
                    .into(),
                actionable: false,
                optional: false,
                action: None,
                ok_label: None,
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
                optional: false,
                action: None,
                ok_label: None,
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
                optional: false,
                action: None,
                ok_label: None,
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
                optional: false,
                action: None,
                ok_label: None,
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
    check().iter().all(|p| p.granted || !p.actionable || p.optional)
}

#[cfg(test)]
mod tests {
    #[test]
    fn check_lists_permissions() {
        let perms = super::check();
        for p in &perms {
            println!("{} granted={} optional={}", p.id, p.granted, p.optional);
        }
        #[cfg(target_os = "macos")]
        assert!(perms.iter().any(|p| p.id == "handoff" && p.optional));
    }
}
