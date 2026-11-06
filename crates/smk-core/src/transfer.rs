//! Streaming file transfer for copied files and folders.
//!
//! When files are copied, only a [`FileOffer`] (names and sizes) is sent to
//! peers. The machine that needs the files then opens a *separate* TLS
//! connection to the owner, sends `FetchFiles { id }`, and receives the file
//! contents as a sequence of `FileData` chunks that are written straight to
//! disk. Mouse and keyboard traffic never waits behind a file, memory use is
//! constant, and there is no size limit beyond free disk space.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

use crate::protocol::{read_frame, write_frame, Message};

/// Size of a `FileData` chunk.
pub const CHUNK: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileMeta {
    /// Path relative to the transfer root, `/`-separated. The first component
    /// is the name of the copied file or folder.
    pub rel_path: String,
    pub is_dir: bool,
    pub size: u64,
    /// Modification time, seconds since the Unix epoch.
    pub mtime: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileOffer {
    pub id: Uuid,
    pub entries: Vec<FileMeta>,
    pub total: u64,
}

impl FileOffer {
    pub fn file_count(&self) -> usize {
        self.entries.iter().filter(|e| !e.is_dir).count()
    }

    /// Names of the copied top-level items.
    pub fn top_level(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in &self.entries {
            let top = e.rel_path.split('/').next().unwrap_or_default().to_string();
            if !top.is_empty() && !out.contains(&top) {
                out.push(top);
            }
        }
        out
    }

    /// Short human description, e.g. `Photos` or `report.pdf + 2 more`.
    pub fn label(&self) -> String {
        let tops = self.top_level();
        match tops.len() {
            0 => "nothing".into(),
            1 => tops[0].clone(),
            n => format!("{} + {} more", tops[0], n - 1),
        }
    }
}

/// A local offer: the announced entries plus where to read each one from.
#[derive(Debug, Clone)]
pub struct LocalOffer {
    pub offer: FileOffer,
    pub sources: Vec<PathBuf>,
}

fn mtime_of(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// Walk the copied paths (recursively for folders). Symbolic links are
/// skipped. Blocking; run on a blocking thread for big trees.
pub fn scan(paths: &[PathBuf]) -> std::io::Result<LocalOffer> {
    let mut entries = Vec::new();
    let mut sources = Vec::new();
    for p in paths {
        let base = p.parent().unwrap_or(Path::new(""));
        walk(p, base, &mut entries, &mut sources)?;
    }
    let total = entries.iter().map(|e: &FileMeta| e.size).sum();
    Ok(LocalOffer {
        offer: FileOffer { id: Uuid::new_v4(), entries, total },
        sources,
    })
}

fn walk(p: &Path, base: &Path, entries: &mut Vec<FileMeta>, sources: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(p)?;
    let rel = p
        .strip_prefix(base)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    if meta.is_dir() {
        entries.push(FileMeta { rel_path: rel, is_dir: true, size: 0, mtime: mtime_of(&meta) });
        sources.push(p.to_path_buf());
        let mut children: Vec<PathBuf> = std::fs::read_dir(p)?.flatten().map(|e| e.path()).collect();
        children.sort();
        for c in children {
            // Unreadable entries inside a folder are skipped, not fatal.
            if let Err(e) = walk(&c, base, entries, sources) {
                tracing::warn!("skipping {}: {e}", c.display());
            }
        }
    } else if meta.is_file() {
        entries.push(FileMeta { rel_path: rel, is_dir: false, size: meta.len(), mtime: mtime_of(&meta) });
        sources.push(p.to_path_buf());
    }
    Ok(())
}

/// Join a peer-supplied relative path onto `dir`, refusing anything that
/// could escape it (`..`, absolute paths, drive letters, backslashes).
pub fn safe_join(dir: &Path, rel: &str) -> Option<PathBuf> {
    let mut out = dir.to_path_buf();
    let mut any = false;
    for c in rel.split('/') {
        if c.is_empty() || c == "." || c == ".." || c.contains('\\') || c.contains(':') || c.contains('\0') {
            if c.is_empty() || c == "." {
                continue;
            }
            return None;
        }
        out.push(c);
        any = true;
    }
    any.then_some(out)
}

/// Stream all files of `local` to `w`. `progress` receives the number of
/// bytes sent so far.
pub async fn send<W: AsyncWrite + Unpin>(
    w: &mut W,
    local: &LocalOffer,
    mut progress: impl FnMut(u64),
) -> anyhow::Result<()> {
    let mut sent = 0u64;
    let mut buf = vec![0u8; CHUNK];
    for (i, (meta, src)) in local.offer.entries.iter().zip(&local.sources).enumerate() {
        if meta.is_dir {
            continue;
        }
        let mut f = match tokio::fs::File::open(src).await {
            Ok(f) => f,
            Err(e) => {
                let msg = format!("cannot read {}: {e}", meta.rel_path);
                let _ = write_frame(w, &Message::FilesError(msg.clone())).await;
                anyhow::bail!(msg);
            }
        };
        loop {
            let n = f.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            write_frame(w, &Message::FileData { index: i as u32, data: buf[..n].to_vec() }).await?;
            sent += n as u64;
            progress(sent);
        }
    }
    write_frame(w, &Message::FilesDone).await?;
    w.flush().await?;
    Ok(())
}

/// Receive the files of `offer` into `dir`. Returns the paths of the
/// top-level items (what goes on the clipboard).
pub async fn receive<R: AsyncRead + Unpin>(
    r: &mut R,
    offer: &FileOffer,
    dir: &Path,
    mut progress: impl FnMut(u64),
) -> anyhow::Result<Vec<PathBuf>> {
    // Create the folder structure and empty files first, so empty files and
    // folders survive and every FileData only appends.
    let mut targets = Vec::with_capacity(offer.entries.len());
    for e in &offer.entries {
        let t = safe_join(dir, &e.rel_path)
            .ok_or_else(|| anyhow::anyhow!("refusing unsafe path {:?}", e.rel_path))?;
        if e.is_dir {
            tokio::fs::create_dir_all(&t).await?;
        } else {
            if let Some(parent) = t.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::File::create(&t).await?;
        }
        targets.push(t);
    }

    let mut received = 0u64;
    let mut current: Option<(u32, tokio::fs::File)> = None;
    loop {
        match read_frame(r).await? {
            Message::FileData { index, data } => {
                let i = index as usize;
                anyhow::ensure!(i < offer.entries.len() && !offer.entries[i].is_dir, "bad file index {index}");
                if current.as_ref().map(|(c, _)| *c) != Some(index) {
                    if let Some((_, mut f)) = current.take() {
                        f.flush().await?;
                    }
                    let f = tokio::fs::OpenOptions::new().append(true).open(&targets[i]).await?;
                    current = Some((index, f));
                }
                let (_, f) = current.as_mut().expect("file open");
                f.write_all(&data).await?;
                received += data.len() as u64;
                progress(received);
            }
            Message::FilesDone => break,
            Message::FilesError(e) => anyhow::bail!("the other machine reported: {e}"),
            other => anyhow::bail!("unexpected message during transfer: {other:?}"),
        }
    }
    if let Some((_, mut f)) = current.take() {
        f.flush().await?;
    }

    // Restore modification times (best effort).
    for (e, t) in offer.entries.iter().zip(&targets) {
        if let (false, Some(secs)) = (e.is_dir, e.mtime) {
            if let Ok(f) = std::fs::File::options().write(true).open(t) {
                let when = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
                let _ = f.set_times(std::fs::FileTimes::new().set_modified(when));
            }
        }
    }

    Ok(offer
        .top_level()
        .iter()
        .filter_map(|t| safe_join(dir, t))
        .collect())
}

/// Move received top-level items into `folder`, renaming on clashes
/// ("name 2.ext") so nothing existing is overwritten. Returns the new paths.
pub fn move_into(items: &[PathBuf], folder: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = item.file_name() else { continue };
        let dest = unique_path(folder, Path::new(name));
        std::fs::rename(item, &dest)?;
        out.push(dest);
    }
    Ok(out)
}

/// `folder/name`, or `folder/name 2.ext`, `name 3.ext`… if taken.
pub fn unique_path(folder: &Path, name: &Path) -> PathBuf {
    let first = folder.join(name);
    if std::fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let stem = name.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = name.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..)
        .map(|n| folder.join(format!("{stem} {n}{ext}")))
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .expect("some free name")
}

/// Default root for received files.
pub fn default_download_root() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("shared-mouse-keyboard")
        .join("clipboard")
}

/// Directory for an incoming transfer under `root`. Keeps the newest
/// `keep - 1` existing transfer folders and returns a fresh one.
pub fn new_transfer_dir(root: &Path, keep: usize) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(root)?;
    let mut existing: Vec<PathBuf> = std::fs::read_dir(root)?.flatten().map(|e| e.path()).collect();
    existing.sort();
    while existing.len() >= keep {
        let _ = std::fs::remove_dir_all(existing.remove(0));
    }
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    let dir = root.join(format!("{stamp}"));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_into_never_overwrites() {
        let root = std::env::temp_dir().join(format!("smk-move-{}", Uuid::new_v4()));
        let (src, dst) = (root.join("src"), root.join("dst"));
        std::fs::create_dir_all(src.join("Album")).unwrap();
        std::fs::create_dir_all(dst.join("Album")).unwrap();
        std::fs::write(src.join("a.txt"), "new").unwrap();
        std::fs::write(dst.join("a.txt"), "old").unwrap();
        let moved = move_into(&[src.join("a.txt"), src.join("Album")], &dst).unwrap();
        assert_eq!(moved, vec![dst.join("a 2.txt"), dst.join("Album 2")]);
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(dst.join("a 2.txt")).unwrap(), "new");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn safe_join_rejects_escapes() {
        let d = Path::new("/tmp/x");
        assert_eq!(safe_join(d, "a/b.txt"), Some(PathBuf::from("/tmp/x/a/b.txt")));
        assert_eq!(safe_join(d, "a/./b"), Some(PathBuf::from("/tmp/x/a/b")));
        assert_eq!(safe_join(d, "../etc/passwd"), None);
        assert_eq!(safe_join(d, "a/../../b"), None);
        assert_eq!(safe_join(d, "C:/Windows"), None);
        assert_eq!(safe_join(d, "a\\..\\b"), None);
        assert_eq!(safe_join(d, ""), None);
    }

    #[tokio::test]
    async fn folder_roundtrip_with_big_and_empty_files() {
        let tmp = std::env::temp_dir().join(format!("smk-xfer-{}", Uuid::new_v4()));
        let src = tmp.join("src/Folder");
        std::fs::create_dir_all(src.join("sub/empty-dir")).unwrap();
        // ~1.3 MB file spans several chunks.
        let big: Vec<u8> = (0..1_300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(src.join("big.bin"), &big).unwrap();
        std::fs::write(src.join("sub/note.txt"), b"hello").unwrap();
        std::fs::write(src.join("sub/empty.txt"), b"").unwrap();

        let local = scan(std::slice::from_ref(&src)).unwrap();
        assert_eq!(local.offer.total, big.len() as u64 + 5);
        assert_eq!(local.offer.file_count(), 3);
        assert_eq!(local.offer.label(), "Folder");

        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        let offer = local.offer.clone();
        let dst = tmp.join("dst");
        let sender = tokio::spawn(async move { send(&mut a, &local, |_| {}).await });
        let mut last = 0;
        let tops = receive(&mut b, &offer, &dst, |n| last = n).await.unwrap();
        sender.await.unwrap().unwrap();

        assert_eq!(last, offer.total);
        assert_eq!(tops, vec![dst.join("Folder")]);
        assert_eq!(std::fs::read(dst.join("Folder/big.bin")).unwrap(), big);
        assert_eq!(std::fs::read(dst.join("Folder/sub/note.txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(dst.join("Folder/sub/empty.txt")).unwrap(), b"");
        assert!(dst.join("Folder/sub/empty-dir").is_dir());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
