//! Persistent configuration: identity, trusted peers, layout and settings.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::layout::Layout;

pub const DEFAULT_PORT: u16 = 24801;
pub const DISCOVERY_PORT: u16 = 24800;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedPeer {
    pub node_id: Uuid,
    pub name: String,
    /// SHA-256 fingerprint of the peer's certificate (hex).
    pub fingerprint: String,
    /// Last address we successfully talked to (host:port).
    pub last_addr: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub clipboard_enabled: bool,
    pub clipboard_files_enabled: bool,
    /// Max total size of files sent through the clipboard, in MB.
    pub clipboard_max_file_mb: u64,
    /// When controlling a Windows/Linux machine from a Mac (or vice versa),
    /// swap Command and Control so shortcuts feel native.
    pub swap_cmd_ctrl: bool,
    /// Ctrl+Alt+Shift+Arrow switches machines, Ctrl+Alt+Shift+Enter returns home.
    pub hotkeys_enabled: bool,
    /// Pixels the cursor must push against an edge before switching (0 = immediate).
    pub edge_resistance_px: f64,
    /// Multiplier applied to relative mouse motion sent to remote machines.
    pub pointer_speed: f64,
    /// Screen size this machine reports when it cannot be detected
    /// (e.g. Linux under pure Wayland).
    pub fallback_screen: (u32, u32),
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            clipboard_enabled: true,
            clipboard_files_enabled: true,
            clipboard_max_file_mb: 100,
            swap_cmd_ctrl: true,
            hotkeys_enabled: true,
            edge_resistance_px: 0.0,
            pointer_speed: 1.0,
            fallback_screen: (1920, 1080),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub node_id: Uuid,
    pub name: String,
    pub port: u16,
    pub trusted: Vec<TrustedPeer>,
    pub layout: Layout,
    /// Addresses (host or host:port) to connect to in addition to discovery.
    pub manual_peers: Vec<String>,
    pub settings: Settings,
    /// Directory this config was loaded from (not serialized).
    #[serde(skip)]
    pub dir: PathBuf,
    /// Never write to disk (macOS login-screen helper runs as root and must
    /// not take ownership of the user's files).
    #[serde(skip)]
    pub read_only: bool,
}

impl Default for Config {
    fn default() -> Self {
        let name = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .map(|h| h.trim_end_matches(".local").to_string())
            .unwrap_or_else(|| "computer".into());
        Self {
            node_id: Uuid::new_v4(),
            name,
            port: DEFAULT_PORT,
            trusted: Vec::new(),
            layout: Layout::default(),
            manual_peers: Vec::new(),
            settings: Settings::default(),
            dir: Self::default_dir(),
            read_only: false,
        }
    }
}

impl Config {
    /// Default location; can be overridden with `SMK_CONFIG_DIR`.
    pub fn default_dir() -> PathBuf {
        if let Some(d) = std::env::var_os("SMK_CONFIG_DIR") {
            return PathBuf::from(d);
        }
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("shared-mouse-keyboard")
    }

    fn path(&self) -> PathBuf {
        self.dir.join("config.json")
    }

    /// Load the config. With `read_only`, it must already exist and is never
    /// written back.
    pub fn load_or_create(dir: &std::path::Path, read_only: bool) -> anyhow::Result<Self> {
        let path = dir.join("config.json");
        anyhow::ensure!(
            !read_only || path.exists(),
            "no configuration in {} (start the app normally once first)",
            dir.display()
        );
        let mut cfg: Config = if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            serde_json::from_str(&text)?
        } else {
            Config::default()
        };
        cfg.dir = dir.to_path_buf();
        cfg.read_only = read_only;
        let (id, name) = (cfg.node_id, cfg.name.clone());
        cfg.layout.ensure(id, &name);
        cfg.save()?;
        Ok(cfg)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        if self.read_only {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join("config.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(tmp, self.path())?;
        Ok(())
    }

    pub fn trusted_by_fingerprint(&self, fp: &str) -> Option<&TrustedPeer> {
        self.trusted.iter().find(|t| t.fingerprint == fp)
    }
}
