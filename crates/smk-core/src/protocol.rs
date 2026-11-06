//! Wire protocol spoken between peers over the TLS connection.
//!
//! Frames are `u32` big-endian length + bincode-encoded [`Message`].

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

use crate::layout::Layout;

/// Bumped whenever the message format changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 2;
/// Hard cap for a single frame (clipboard with files can be large).
pub const MAX_FRAME: usize = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Os {
    MacOs,
    Windows,
    Linux,
    Other,
}

impl Os {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "linux") {
            Os::Linux
        } else {
            Os::Other
        }
    }
}

/// Rectangle in the platform's global desktop coordinate space
/// (points on macOS, physical pixels on Windows / X11).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    pub fn clamp(&self, x: f64, y: f64) -> (f64, f64) {
        (
            x.clamp(self.x, self.right() - 1.0),
            y.clamp(self.y, self.bottom() - 1.0),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// Input event as sent over the wire. Keys use Linux evdev key codes as the
/// canonical, platform-neutral representation (see [`crate::keymap`]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    /// Absolute position inside the receiver's screen rect.
    MouseMove { x: f64, y: f64 },
    MouseButton { button: MouseButton, down: bool },
    /// Scroll in 1/120 notch units (positive y = up, positive x = right).
    Scroll { dx: i32, dy: i32 },
    Key { code: u16, down: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub node_id: Uuid,
    pub name: String,
    pub os: Os,
    pub app_version: String,
    /// TCP port this node listens on.
    pub port: u16,
    pub screen: Rect,
    pub layout: Layout,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClipboardData {
    pub text: Option<String>,
    pub html: Option<String>,
    pub rtf: Option<String>,
    pub image_png: Option<Vec<u8>>,
}

impl ClipboardData {
    pub fn is_empty(&self) -> bool {
        self.text.is_none()
            && self.html.is_none()
            && self.rtf.is_none()
            && self.image_png.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    Hello(Hello),
    /// Initiator asks the receiver to display a pairing code.
    PairRequest,
    /// Initiator sends the code the user typed in.
    PairCode(String),
    PairResult { ok: bool, reason: String },
    /// Sender takes control of the receiver; cursor appears at (x, y).
    Enter { x: f64, y: f64 },
    /// Sender stops controlling the receiver.
    Leave,
    Input(InputEvent),
    /// Receiver's screen geometry changed.
    Screen(Rect),
    Layout(Layout),
    Clipboard(ClipboardData),
    Ping,
    Pong,
    /// Files were copied on the sender; contents are fetched separately.
    ClipboardFiles(crate::transfer::FileOffer),
    /// First frame on a dedicated transfer connection.
    FetchFiles { id: Uuid },
    /// A chunk of file `index` of the offer (transfer connection only).
    FileData { index: u32, data: Vec<u8> },
    FilesDone,
    FilesError(String),
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, msg: &Message) -> anyhow::Result<()> {
    let bytes = bincode::serialize(msg)?;
    anyhow::ensure!(bytes.len() <= MAX_FRAME, "frame too large");
    w.write_u32(bytes.len() as u32).await?;
    w.write_all(&bytes).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> anyhow::Result<Message> {
    let len = r.read_u32().await? as usize;
    anyhow::ensure!(len <= MAX_FRAME, "frame too large: {len}");
    // Grow the buffer as data actually arrives instead of trusting the
    // length prefix, so an unauthenticated peer cannot make us allocate
    // hundreds of MB by sending a 4-byte header.
    let mut buf = Vec::with_capacity(len.min(64 * 1024));
    let n = (&mut *r).take(len as u64).read_to_end(&mut buf).await?;
    anyhow::ensure!(n == len, "connection closed mid-frame");
    Ok(bincode::deserialize(&buf)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_roundtrip_and_truncated_header() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, &Message::Ping).await.unwrap();
        assert!(matches!(read_frame(&mut b).await.unwrap(), Message::Ping));

        // A header promising 400 MB followed by EOF must fail cleanly.
        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_u32(400 * 1024 * 1024).await.unwrap();
        drop(a);
        assert!(read_frame(&mut b).await.is_err());
    }
}
