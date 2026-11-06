//! macOS login-screen support.
//!
//! A LaunchAgent in `/Library/LaunchAgents` with
//! `LimitLoadToSessionType = LoginWindow` is started by launchd while the
//! login window is shown (after boot or logout), before any user logs in. It
//! runs this same binary with `--login-agent`: a headless engine that uses the
//! user's existing identity, pairings and layout read-only, so paired machines
//! can control the login screen. It exits as soon as a user owns the console;
//! the normal app (started at login) then takes over.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::time::Duration;

pub const LABEL: &str = "io.github.hatak.shared-mouse-keyboard.loginwindow";
pub const ARG: &str = "--login-agent";

pub fn plist_path() -> PathBuf {
    PathBuf::from(format!("/Library/LaunchAgents/{LABEL}.plist"))
}

pub fn is_installed() -> bool {
    plist_path().exists()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn plist(exe: &Path, config_dir: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>LimitLoadToSessionType</key>
  <string>LoginWindow</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>{arg}</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>SMK_CONFIG_DIR</key>
    <string>{cfg}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ThrottleInterval</key>
  <integer>5</integer>
  <key>StandardOutPath</key>
  <string>/Library/Logs/shared-mouse-keyboard-loginwindow.log</string>
  <key>StandardErrorPath</key>
  <string>/Library/Logs/shared-mouse-keyboard-loginwindow.log</string>
</dict>
</plist>
"#,
        label = LABEL,
        exe = xml_escape(&exe.to_string_lossy()),
        arg = ARG,
        cfg = xml_escape(&config_dir.to_string_lossy()),
    )
}

/// Run a shell command as root via the standard macOS admin prompt.
fn run_as_admin(script: &str, prompt: &str) -> Result<(), String> {
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let apple = format!(
        "do shell script \"{}\" with administrator privileges with prompt \"{}\"",
        quote(script),
        quote(prompt)
    );
    let out = std::process::Command::new("osascript")
        .args(["-e", &apple])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("-128") {
            Err("cancelled".into())
        } else {
            Err(err.trim().to_string())
        }
    }
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn install(config_dir: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if !exe.starts_with("/Applications") {
        return Err(format!(
            "Move Shared Mouse Keyboard to the Applications folder first (currently running from {}).",
            exe.display()
        ));
    }
    let tmp = std::env::temp_dir().join(format!("{LABEL}.plist"));
    std::fs::write(&tmp, plist(&exe, config_dir)).map_err(|e| e.to_string())?;
    let dst = plist_path();
    let script = format!(
        "/bin/cp {src} {dst} && /usr/sbin/chown root:wheel {dst} && /bin/chmod 644 {dst}",
        src = sh_quote(&tmp.to_string_lossy()),
        dst = sh_quote(&dst.to_string_lossy()),
    );
    let res = run_as_admin(
        &script,
        "Shared Mouse Keyboard wants to install a helper so your other computers can control the login screen.",
    );
    let _ = std::fs::remove_file(&tmp);
    res
}

pub fn uninstall() -> Result<(), String> {
    let script = format!("/bin/rm -f {}", sh_quote(&plist_path().to_string_lossy()));
    run_as_admin(&script, "Shared Mouse Keyboard wants to remove its login screen helper.")
}

/// True when a real user (not root / loginwindow) owns the console.
fn user_logged_in() -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/dev/console").map(|m| m.uid() != 0).unwrap_or(false)
}

/// Entry point for `--login-agent`.
pub fn run_agent() {
    tracing::info!("login-screen helper starting");
    if user_logged_in() && std::env::var_os("SMK_AGENT_FORCE").is_none() {
        tracing::info!("a user is logged in; nothing to do");
        return;
    }
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        let opts = smk_core::StartOptions {
            clipboard: false,
            read_only: true,
            ..Default::default()
        };
        let _engine = match smk_core::start(opts, std::sync::Arc::new(|| {})).await {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("cannot start: {e:#}");
                // Non-zero exit: launchd retries (ThrottleInterval).
                std::process::exit(1);
            }
        };
        // Hand over to the normal app once someone logs in.
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if user_logged_in() && std::env::var_os("SMK_AGENT_FORCE").is_none() {
                tracing::info!("user logged in; login-screen helper exiting");
                std::process::exit(0);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_is_valid_and_escaped() {
        let xml = plist(
            Path::new("/Applications/Shared Mouse Keyboard.app/Contents/MacOS/shared-mouse-keyboard"),
            Path::new("/Users/a&b/Library/Application Support/shared-mouse-keyboard"),
        );
        assert!(xml.contains("a&amp;b"));
        let f = std::env::temp_dir().join("smk-plist-test.plist");
        std::fs::write(&f, &xml).unwrap();
        let ok = std::process::Command::new("plutil").arg("-lint").arg(&f).status().unwrap().success();
        let _ = std::fs::remove_file(&f);
        assert!(ok, "plutil rejected the plist");
    }
}
