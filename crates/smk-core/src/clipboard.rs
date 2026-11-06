//! Clipboard synchronisation: text, HTML, RTF, images and files.
//!
//! A dedicated thread owns the platform clipboard (some platforms require the
//! owner to stay alive to serve paste requests). Files received from a peer
//! are written to a cache directory and placed on the clipboard as file
//! references, so pasting in a file manager copies them.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use clipboard_rs::common::RustImage;
use clipboard_rs::{
    Clipboard, ClipboardContent, ClipboardContext, ClipboardHandler, ClipboardWatcher,
    ClipboardWatcherContext, ContentFormat, RustImageData,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::protocol::{ClipboardData, ClipboardFile};

pub struct ReadOptions {
    pub files: bool,
    pub max_file_bytes: u64,
}

enum Cmd {
    Read(ReadOptions, mpsc::Sender<anyhow::Result<ClipboardData>>),
    Write(ClipboardData, mpsc::Sender<anyhow::Result<()>>),
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
    pub fn read(&self, opts: ReadOptions) -> anyhow::Result<ClipboardData> {
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
}

fn err(e: Box<dyn std::error::Error + Send + Sync>) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

fn read(ctx: &ClipboardContext, opts: &ReadOptions) -> anyhow::Result<ClipboardData> {
    let mut data = ClipboardData::default();
    if ctx.has(ContentFormat::Files) {
        if !opts.files {
            return Ok(data);
        }
        let paths: Vec<PathBuf> = ctx.get_files().map_err(err)?.iter().map(|p| uri_to_path(p)).collect();
        let total: u64 = paths.iter().map(|p| dir_size(p)).sum();
        if total > opts.max_file_bytes {
            anyhow::bail!(
                "copied files are {} MB, over the {} MB limit",
                total / 1_000_000,
                opts.max_file_bytes / 1_000_000
            );
        }
        for p in &paths {
            collect_files(p, p.parent().unwrap_or(Path::new("")), &mut data.files)?;
        }
        // File copies usually also carry the file name as text and an icon as
        // an image; only the files matter.
        return Ok(data);
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
    Ok(data)
}

fn write(ctx: &ClipboardContext, data: ClipboardData) -> anyhow::Result<()> {
    if !data.files.is_empty() {
        let dir = materialize_files(&data.files)?;
        let mut tops: Vec<String> = Vec::new();
        for f in &data.files {
            let top = f.rel_path.split('/').next().unwrap_or_default().to_string();
            if !top.is_empty() && !tops.contains(&top) {
                tops.push(top);
            }
        }
        let paths: Vec<String> = tops
            .iter()
            .map(|t| dir.join(t).to_string_lossy().into_owned())
            .collect();
        return ctx.set_files(paths).map_err(err);
    }
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

fn dir_size(p: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(p) else { return 0 };
    if meta.is_dir() {
        std::fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| dir_size(&e.path())).sum())
            .unwrap_or(0)
    } else {
        meta.len()
    }
}

fn collect_files(p: &Path, base: &Path, out: &mut Vec<ClipboardFile>) -> anyhow::Result<()> {
    let rel = p
        .strip_prefix(base)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let meta = std::fs::symlink_metadata(p)?;
    if meta.is_dir() {
        out.push(ClipboardFile { rel_path: rel, is_dir: true, data: Vec::new() });
        for entry in std::fs::read_dir(p)?.flatten() {
            collect_files(&entry.path(), base, out)?;
        }
    } else if meta.is_file() {
        out.push(ClipboardFile { rel_path: rel, is_dir: false, data: std::fs::read(p)? });
    }
    Ok(())
}

fn clipboard_cache() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("shared-mouse-keyboard")
        .join("clipboard")
}

fn materialize_files(files: &[ClipboardFile]) -> anyhow::Result<PathBuf> {
    let root = clipboard_cache();
    // Keep only the most recent few transfers.
    if let Ok(rd) = std::fs::read_dir(&root) {
        let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        dirs.sort();
        while dirs.len() >= 5 {
            let _ = std::fs::remove_dir_all(dirs.remove(0));
        }
    }
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    let dir = root.join(format!("{stamp}"));
    std::fs::create_dir_all(&dir)?;
    for f in files {
        // Never allow a peer to write outside the transfer directory.
        let safe: PathBuf = f
            .rel_path
            .split('/')
            .filter(|c| !c.is_empty() && *c != "." && *c != ".." && !c.contains('\\') && !c.contains(':'))
            .collect();
        if safe.as_os_str().is_empty() {
            continue;
        }
        let target = dir.join(safe);
        if f.is_dir {
            std::fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, &f.data)?;
        }
    }
    Ok(dir)
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
