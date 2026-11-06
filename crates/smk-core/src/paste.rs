//! Where a paste is going: the folder of the front file-manager window, or
//! just "some app" (identified so we can paste again once the download is
//! done, if that app is still in front).

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum PasteTarget {
    /// Pasting into this folder in the file manager: save straight there.
    Folder(PathBuf),
    /// Pasting into another app (`Some(id)` = which one, platform specific).
    App(Option<i64>),
}

/// Inspect the front app. May block briefly (talks to Finder); call from a
/// blocking thread.
pub fn target() -> PasteTarget {
    imp::target()
}

/// Identifier of the front app, comparable with [`PasteTarget::App`].
pub fn front_app() -> Option<i64> {
    imp::front_app()
}

#[cfg(target_os = "macos")]
mod imp {
    use super::PasteTarget;
    use objc2_app_kit::NSWorkspace;
    use std::path::PathBuf;
    use std::process::Command;

    fn front() -> Option<(i64, String)> {
        let ws = NSWorkspace::sharedWorkspace();
        let app = ws.frontmostApplication()?;
        let pid = app.processIdentifier() as i64;
        let bundle = app.bundleIdentifier().map(|s| s.to_string()).unwrap_or_default();
        Some((pid, bundle))
    }

    pub fn front_app() -> Option<i64> {
        front().map(|(pid, _)| pid)
    }

    pub fn target() -> PasteTarget {
        let Some((pid, bundle)) = front() else { return PasteTarget::App(None) };
        if bundle == "com.apple.finder" {
            // The folder a Finder paste would go to (front window, or the
            // Desktop). The first time, macOS asks to allow controlling Finder.
            let out = Command::new("/usr/bin/osascript")
                .args(["-e", "tell application \"Finder\" to get POSIX path of (insertion location as alias)"])
                .output();
            match out {
                Ok(o) if o.status.success() => {
                    let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    let p = PathBuf::from(p);
                    if p.is_dir() {
                        return PasteTarget::Folder(p);
                    }
                }
                Ok(o) => tracing::warn!("asking Finder for the paste folder failed: {}", String::from_utf8_lossy(&o.stderr).trim()),
                Err(e) => tracing::warn!("osascript: {e}"),
            }
        }
        PasteTarget::App(Some(pid))
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::PasteTarget;
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    pub fn front_app() -> Option<i64> {
        let h = unsafe { GetForegroundWindow() };
        (!h.is_invalid()).then_some(h.0 as i64)
    }

    pub fn target() -> PasteTarget {
        PasteTarget::App(front_app())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod imp {
    use super::PasteTarget;

    pub fn front_app() -> Option<i64> {
        None
    }

    pub fn target() -> PasteTarget {
        PasteTarget::App(None)
    }
}
