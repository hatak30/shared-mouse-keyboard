//! Clipboard access: text, HTML, RTF, images, and file references.
//!
//! A dedicated thread owns the platform clipboard (some platforms require the
//! owner to stay alive to serve paste requests). Copied *files* are only
//! reported as paths here; their contents travel via [`crate::transfer`] and
//! are placed back on the receiver's clipboard as file references.

use std::path::PathBuf;
use std::sync::mpsc;

use tokio::sync::mpsc::UnboundedSender;

use crate::protocol::ClipboardData;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
use android as backend;
#[cfg(target_os = "ios")]
mod unsupported;
#[cfg(target_os = "ios")]
use unsupported as backend;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod desktop;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use desktop as backend;

pub struct ReadOptions {
    pub files: bool,
}

/// What is on the local clipboard.
#[derive(Debug)]
pub enum Clip {
    Data(ClipboardData),
    Files(Vec<PathBuf>),
}

enum Cmd {
    Read(ReadOptions, mpsc::Sender<anyhow::Result<Clip>>),
    Write(ClipboardData, mpsc::Sender<anyhow::Result<()>>),
    WriteFiles(Vec<PathBuf>, mpsc::Sender<anyhow::Result<()>>),
}

#[derive(Clone)]
pub struct ClipboardHandle {
    tx: mpsc::Sender<Cmd>,
}

impl ClipboardHandle {
    /// Start the clipboard owner thread and a change watcher that sends `()`
    /// on `changed` whenever the local clipboard changes.
    pub fn start(changed: UnboundedSender<()>) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel::<Cmd>();
        std::thread::Builder::new()
            .name("smk-clipboard".into())
            .spawn(move || {
                let ctx = match backend::Backend::new() {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("clipboard unavailable: {e}");
                        return;
                    }
                };
                for cmd in rx {
                    match cmd {
                        Cmd::Read(opts, reply) => {
                            let _ = reply.send(ctx.read(&opts));
                        }
                        Cmd::Write(data, reply) => {
                            let _ = reply.send(ctx.write(data));
                        }
                        Cmd::WriteFiles(paths, reply) => {
                            let _ = reply.send(ctx.write_files(paths));
                        }
                    }
                }
            })?;

        std::thread::Builder::new()
            .name("smk-clipboard-watch".into())
            .spawn(move || backend::watch(changed))?;
        Ok(Self { tx })
    }

    /// Read the clipboard (blocking; call from `spawn_blocking`).
    pub fn read(&self, opts: ReadOptions) -> anyhow::Result<Clip> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(Cmd::Read(opts, tx))?;
        rx.recv()?
    }

    /// Replace the clipboard (blocking; call from `spawn_blocking`).
    pub fn write(&self, data: ClipboardData) -> anyhow::Result<()> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(Cmd::Write(data, tx))?;
        rx.recv()?
    }

    /// Put file references on the clipboard (blocking).
    pub fn write_files(&self, paths: Vec<PathBuf>) -> anyhow::Result<()> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(Cmd::WriteFiles(paths, tx))?;
        rx.recv()?
    }
}
