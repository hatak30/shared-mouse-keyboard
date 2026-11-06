# Shared Mouse & Keyboard

Use one mouse and keyboard to control several computers on your local network, and share the clipboard (text, rich text, images and **files**) between them. Works across **macOS, Windows and Linux** in any mix.

- **Seamless cursor movement.** Push the cursor against a screen edge and it continues on the neighbouring machine.
- **Visual layout editor.** Drag machines on a grid to match where your screens physically stand (left, right, above, below).
- **Clipboard sharing.** Copy on one machine and paste on another. Copied files are transferred too, up to a configurable size limit.
- **Secure by default.** All traffic is encrypted with TLS. Each machine is paired once with a 6‑digit code.
- **Automatic discovery.** Machines on the same LAN find each other. You can also add one manually by IP or host name.
- **Standalone app.** One self-contained app per OS. It keeps running in the system tray when its window is closed.

## How it works

Every machine runs the same app. Whichever machine you're physically using captures its mouse and keyboard with OS-level hooks. When the cursor crosses an edge that has a neighbour in the layout, the app keeps the local cursor in place and streams input to the neighbour, which replays it as synthetic input. Moving back across the edge returns control. Any machine can be the controlling one.

```
┌──────────────┐  TLS (port 24801)  ┌──────────────┐
│   macOS      │ ◄────────────────► │   Windows    │
│  (keyboard,  │   input events,    │              │
│   mouse)     │   clipboard,       │              │
└──────────────┘   layout sync      └──────────────┘
        ▲      UDP broadcast discovery (port 24800)
        └──────────────────────────────► Linux …
```

| Component | macOS | Windows | Linux |
|---|---|---|---|
| Capture | `CGEventTap` | `WH_MOUSE_LL` / `WH_KEYBOARD_LL` hooks | `evdev` (+ `EVIOCGRAB`) |
| Injection | `CGEventPost` | `SendInput` | `uinput` virtual devices |
| Cursor position | CoreGraphics | Win32 | X11 (see Wayland notes) |
| Clipboard | NSPasteboard | Win32 clipboard | X11 / Wayland (`wl-clipboard`) |

Keys are sent in a neutral format (Linux evdev key codes), so a key pressed on one OS arrives as the same key on another. When a Mac controls a Windows or Linux machine (or the reverse), **⌘ Command and Ctrl are swapped**, so ⌘C becomes Ctrl+C. You can turn this off in Settings.

## Getting started

1. Install and start the app on every computer. See [Building](#building).
2. Grant the permissions shown on the **Permissions** tab (details below).
3. Open the **Machines** tab. Your other computers appear within a few seconds. Click **Pair** on one of them.
4. The other computer shows a 6‑digit code. Type it into the first one. You only need to do this once per pair of machines.
5. Open the **Layout** tab, drag the machines to match your desk, and click **Save layout**. The layout is shared with every paired machine.
6. Move the cursor off the shared edge to switch machines.

### Hotkeys

| Shortcut | Action |
|---|---|
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>←</kbd>/<kbd>→</kbd>/<kbd>↑</kbd>/<kbd>↓</kbd> | Jump to the neighbouring machine in that direction |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>Enter</kbd> | Bring the cursor back to the machine you're using |

The tray menu also has **Return cursor to this machine**.

## Permissions

The app checks these at runtime and explains each one on the Permissions tab.

### macOS
- **Accessibility.** Required to capture the cursor and to be controlled by other machines. Go to System Settings → Privacy & Security → Accessibility.
- **Input Monitoring.** Required to read the keyboard and mouse. Go to System Settings → Privacy & Security → Input Monitoring.
- **Local Network.** macOS asks on first launch (macOS 15+). If you declined, go to System Settings → Privacy & Security → Local Network.
- The firewall may also ask whether to accept incoming connections. Click **Allow**.

> macOS ties these permissions to the app's code signature. Build with `scripts/build-macos.sh` (`--install` also copies the app to /Applications and launches it). It signs with the first **Developer ID Application** certificate in your keychain, which keeps permissions across updates. Ad-hoc signed builds (no certificate) lose them on every rebuild. Set `APPLE_ID`, `APPLE_PASSWORD` (app-specific password) and `APPLE_TEAM_ID` to notarize as well, so other Macs open the app without a Gatekeeper warning.

### Windows
- **Firewall.** Windows asks on first start. Allow access on **private** networks.
- **Administrator (optional).** Windows blocks synthetic input to elevated windows (Task Manager, installers, UAC prompts) unless the app itself runs as administrator. The secure desktop (UAC prompt, lock screen) can never be controlled remotely.

### Linux
- **Read input devices.** Run `sudo usermod -aG input $USER`, then log out and back in.
- **Create virtual devices** (`/dev/uinput`). Install the udev rule. The `.deb` package does this automatically.
  ```sh
  sudo cp packaging/linux/60-shared-mouse-keyboard.rules /etc/udev/rules.d/
  echo uinput | sudo tee /etc/modules-load.d/shared-mouse-keyboard.conf
  sudo modprobe uinput && sudo udevadm control --reload && sudo udevadm trigger
  ```
- **Wayland.** Input capture and injection work on both X11 and Wayland, because they go through the kernel. Under a pure Wayland session, however, apps can't read the global cursor position, so switching by pushing against an edge isn't available there. Use the hotkeys, or log in with an X11 session. The clipboard works on both.

## Network

| Port | Protocol | Purpose |
|---|---|---|
| 24800 | UDP | Discovery beacons (broadcast) |
| 24801 | TCP | Encrypted connection between machines |

Machines must be able to reach each other on these ports. If broadcast is blocked (common on guest or corporate Wi‑Fi), add the other machine by address on the Machines tab.

## Security model

- Each installation generates a private key and a self-signed certificate on first start. They're stored in the config directory, and the key file is readable only by the user.
- Every connection is TLS 1.2/1.3 with mutual authentication, so both sides prove they hold their private key.
- Pairing derives a 6‑digit code from **both** certificate fingerprints. The code is shown on one machine and typed into the other. If someone intercepts the connection, the fingerprints differ, the code doesn't match, and pairing fails. After 3 wrong attempts, pairing is cancelled.
- Unpaired machines can only exchange pairing messages. Input, clipboard and layout updates from them are ignored.
- Files received through the clipboard are written to a cache directory, and paths that try to escape it are rejected.

## Configuration

The app stores its settings and identity in:

| OS | Path |
|---|---|
| macOS | `~/Library/Application Support/shared-mouse-keyboard/` |
| Windows | `%APPDATA%\shared-mouse-keyboard\` |
| Linux | `~/.config/shared-mouse-keyboard/` |

Set `SMK_CONFIG_DIR` to use a different directory, for example to run two instances for testing. Set `RUST_LOG=debug` for verbose logs. Start with `--hidden` to go straight to the tray, which is useful for autostart.

## Building

Prerequisites: [Rust](https://rustup.rs) (stable) and the [Tauri system dependencies](https://tauri.app/start/prerequisites/) for your OS. Node.js is **not** needed, because the UI is plain HTML/CSS/JS.

```sh
cargo install tauri-cli --version "^2" --locked

# run in development
cargo tauri dev            # from app/  (or: cargo run -p shared-mouse-keyboard)

# build installers for the current OS
cd app && cargo tauri build
#   macOS  → target/release/bundle/macos/*.app, dmg/*.dmg
#   Windows→ target/release/bundle/msi/*.msi, nsis/*.exe
#   Linux  → target/release/bundle/deb/*.deb, appimage/*.AppImage, rpm/*.rpm
```

Linux build dependencies (Debian/Ubuntu):
```sh
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev build-essential
```

GitHub Actions runs lint checks and tests on all three platforms for every push (`.github/workflows/ci.yml`).

### Releases and automatic updates

Installers for every platform are published on the [Releases page](https://github.com/hatak30/shared-mouse-keyboard/releases). The app checks GitHub for a newer release at startup and every 6 hours. When one exists, it shows **Version X is available → Install & restart**. You can also check by hand under **Settings → Updates**.

- Every update is verified against the public key built into the app (`plugins.updater.pubkey` in `app/tauri.conf.json`). Updates signed with a different key are rejected.
- macOS releases are signed with the same Developer ID, so Accessibility and Input Monitoring permissions survive updates.
- Windows updates run the installer in passive mode. On Linux, only the **AppImage** updates itself. `.deb`/`.rpm` installs need the new package installed manually.

To publish a version (maintainers):

```sh
scripts/release.sh 0.2.0     # bumps versions, commits, tags v0.2.0, pushes
```

`.github/workflows/release.yml` then builds and signs on macOS (universal), Windows and Linux. It publishes the release together with `latest.json`, which installed apps poll. The secrets it needs are listed at the top of the workflow. The update-signing key lives in `~/.tauri/shared-mouse-keyboard-updater.key` (with its `.password` file); back it up, because without it you can't ship updates to existing installs.

### Tests

```sh
cargo test --workspace
```

The integration test (`crates/smk-core/tests/two_peers.rs`) starts two engines that talk over real TLS on localhost. It covers pairing (including a wrong code and a rejected pairing), layout sync, edge switching with proportional entry position, input forwarding, and releasing held keys on return.

## Project layout

```
crates/smk-core/       core engine (no UI)
  src/engine.rs        connection management, switching logic, clipboard & layout sync
  src/net.rs           TLS transport, framing
  src/tls.rs           identity, certificate fingerprints, pairing codes
  src/discovery.rs     UDP broadcast discovery
  src/protocol.rs      wire messages
  src/layout.rs        grid layout & neighbour lookup
  src/keymap.rs        macOS / Windows ↔ evdev key code tables
  src/clipboard.rs     clipboard read/write incl. files
  src/permissions.rs   per-OS permission checks
  src/input/           capture & injection backends (macos.rs, windows.rs, linux.rs)
app/                   Tauri desktop app (control panel + tray)
  ui/                  control panel (HTML/CSS/JS)
packaging/linux/       udev rule and module config for uinput
```

## Known limitations / roadmap

- Pure Wayland: there is no edge switching (hotkeys only), because Wayland doesn't expose the cursor position.
- Dragging files or windows *across* machines isn't supported. Copy and paste instead.
- The local cursor isn't hidden while you control another machine on Windows/Linux. It stays parked in the centre of the screen.
- Linux touchpads: basic one-finger movement and physical clicks are forwarded. Tap-to-click and gestures are not.
- Planned: autostart option, per-edge offsets for screens of different sizes, multi-monitor dead-zone handling, and a clipboard-on-demand mode for very large files.

## License

MIT
