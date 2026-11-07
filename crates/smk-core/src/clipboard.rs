//! Clipboard access: text, HTML, RTF, images, and file references.
//!
//! A dedicated thread owns the platform clipboard (some platforms require the
//! owner to stay alive to serve paste requests). Copied *files* are only
//! reported as paths here; their contents travel via [`crate::transfer`] and
//! are placed back on the receiver's clipboard as file references.

use std::path::PathBuf;
use std::sync::mpsc;

use clipboard_rs::common::RustImage;
use clipboard_rs::{
    Clipboard, ClipboardContent, ClipboardContext, ClipboardHandler, ClipboardWatcher,
    ClipboardWatcherContext, ContentFormat, RustImageData,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::protocol::ClipboardData;

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
                let ctx = match ClipboardContext::new() {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("clipboard unavailable: {e}");
                        return;
                    }
                };
                for cmd in rx {
                    match cmd {
                        Cmd::Read(opts, reply) => {
                            let _ = reply.send(read(&ctx, &opts));
                        }
                        Cmd::Write(data, reply) => {
                            let _ = reply.send(write(&ctx, data));
                        }
                        Cmd::WriteFiles(paths, reply) => {
                            let files = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                            let _ = reply.send(ctx.set_files(files).map_err(err));
                        }
                    }
                }
            })?;

        std::thread::Builder::new()
            .name("smk-clipboard-watch".into())
            .spawn(move || {
                struct Handler(UnboundedSender<()>);
                impl ClipboardHandler for Handler {
                    fn on_clipboard_change(&mut self) {
                        let _ = self.0.send(());
                    }
                }
                match ClipboardWatcherContext::new() {
                    Ok(mut w) => {
                        w.add_handler(Handler(changed));
                        w.start_watch();
                    }
                    Err(e) => tracing::error!("clipboard watcher unavailable: {e}"),
                }
            })?;
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

fn err(e: Box<dyn std::error::Error + Send + Sync>) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

fn read(ctx: &ClipboardContext, opts: &ReadOptions) -> anyhow::Result<Clip> {
    let mut data = ClipboardData::default();
    if from_universal_clipboard() {
        // Copied on another Apple device: macOS already shares it, and
        // reading it here would make macOS download it right away ("Pasting
        // from …"), even though nobody pasted.
        return Ok(Clip::Data(data));
    }
    if ctx.has(ContentFormat::Files) {
        if !opts.files {
            return Ok(Clip::Data(data));
        }
        // File copies usually also carry the name as text and an icon as an
        // image; only the files matter.
        let paths = ctx.get_files().map_err(err)?.iter().map(|p| uri_to_path(p)).collect();
        return Ok(Clip::Files(paths));
    }
    if ctx.has(ContentFormat::Text) {
        data.text = ctx.get_text().ok().filter(|t| !t.is_empty());
    }
    if ctx.has(ContentFormat::Html) {
        data.html = ctx.get_html().ok().filter(|t| !t.is_empty());
    }
    if ctx.has(ContentFormat::Rtf) {
        data.rtf = ctx.get_rich_text().ok().filter(|t| !t.is_empty());
    }
    if ctx.has(ContentFormat::Image) {
        if let Ok(img) = ctx.get_image() {
            if let Ok(png) = img.to_png() {
                data.image_png = Some(png.get_bytes().to_vec());
            }
        }
    }
    Ok(Clip::Data(data))
}

fn write(ctx: &ClipboardContext, data: ClipboardData) -> anyhow::Result<()> {
    let mut contents = Vec::new();
    if let Some(t) = data.text {
        contents.push(ClipboardContent::Text(t));
    }
    if let Some(h) = data.html {
        contents.push(ClipboardContent::Html(h));
    }
    if let Some(r) = data.rtf {
        contents.push(ClipboardContent::Rtf(r));
    }
    if let Some(png) = data.image_png {
        if let Ok(img) = RustImageData::from_bytes(&png) {
            contents.push(ClipboardContent::Image(img));
        }
    }
    if contents.is_empty() {
        return Ok(());
    }
    ctx.set(contents).map_err(err)
}

/// Clipboard file lists come as plain paths (Windows) or `file://` URIs.
/// True if the clipboard holds an item from macOS Universal Clipboard
/// (Handoff). Only its type list is looked at, which doesn't fetch it.
#[cfg(target_os = "macos")]
fn from_universal_clipboard() -> bool {
    use objc2_app_kit::NSPasteboard;
    let pb = NSPasteboard::generalPasteboard();
    let Some(types) = pb.types() else { return false };
    types.iter().any(|t| t.to_string() == "com.apple.is-remote-clipboard")
}

#[cfg(not(target_os = "macos"))]
fn from_universal_clipboard() -> bool {
    false
}

fn uri_to_path(s: &str) -> PathBuf {
    let Some(rest) = s.strip_prefix("file://") else {
        return PathBuf::from(s);
    };
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_decoding() {
        assert_eq!(uri_to_path("file:///tmp/a%20b.txt"), PathBuf::from("/tmp/a b.txt"));
        assert_eq!(uri_to_path("C:\\x\\y.txt"), PathBuf::from("C:\\x\\y.txt"));
    }
}
