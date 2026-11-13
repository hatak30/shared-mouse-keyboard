use std::path::PathBuf;

use tokio::sync::mpsc::UnboundedSender;

use super::{Clip, ReadOptions};
use crate::protocol::ClipboardData;

pub struct Backend;

impl Backend {
    pub fn new() -> anyhow::Result<Self> {
        anyhow::bail!("clipboard sharing is not available on this platform")
    }

    pub fn read(&self, _opts: &ReadOptions) -> anyhow::Result<Clip> {
        anyhow::bail!("unsupported")
    }

    pub fn write(&self, _data: ClipboardData) -> anyhow::Result<()> {
        anyhow::bail!("unsupported")
    }

    pub fn write_files(&self, _paths: Vec<PathBuf>) -> anyhow::Result<()> {
        anyhow::bail!("unsupported")
    }
}

pub fn watch(_changed: UnboundedSender<()>) {}
