//! Text-only clipboard through the host app. Android 10+ lets only the
//! foreground app read the clipboard, so changes are reported by the app
//! while it is visible; writing works at any time.

use std::path::PathBuf;

use tokio::sync::mpsc::UnboundedSender;

use super::{Clip, ReadOptions};
use crate::android_host;
use crate::protocol::ClipboardData;

pub struct Backend;

impl Backend {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self)
    }

    pub fn read(&self, _opts: &ReadOptions) -> anyhow::Result<Clip> {
        let text = android_host::host().and_then(|h| h.clipboard_text()).filter(|t| !t.is_empty());
        Ok(Clip::Data(ClipboardData { text, ..Default::default() }))
    }

    pub fn write(&self, data: ClipboardData) -> anyhow::Result<()> {
        if let (Some(text), Some(host)) = (data.text, android_host::host()) {
            host.set_clipboard_text(&text);
        }
        Ok(())
    }

    pub fn write_files(&self, _paths: Vec<PathBuf>) -> anyhow::Result<()> {
        anyhow::bail!("copied files cannot be placed on the Android clipboard; save them to Downloads instead")
    }
}

pub fn watch(changed: UnboundedSender<()>) {
    android_host::watch_clipboard(changed);
}
