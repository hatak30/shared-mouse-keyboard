//! The engine ties everything together: it owns the connections, decides
//! which machine receives input, forwards captured events, replays received
//! events, synchronises the layout and the clipboard, and publishes a
//! snapshot of its state for the control panel.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio_rustls::TlsConnector;
use uuid::Uuid;

use crate::clipboard::{Clip, ClipboardHandle, ReadOptions};
use crate::config::{BlockedPeer, Config, Settings, TrustedPeer};
use crate::discovery::{self, Discovered};
use crate::input::{self, CaptureShared, CapturedEvent, Injector};
use crate::keymap::{self, ev};
use crate::layout::{Direction, Layout};
use crate::net::{self, ConnId, NetEvent};
use crate::paste::{self, PasteTarget};
use crate::permissions::{self, Permission};
use crate::protocol::{ClipboardData, Hello, InputEvent, Message, MouseButton, Os, Rect, PROTOCOL_VERSION};
use crate::tls::{pairing_code, Identity};
use crate::transfer::{self, FileOffer, LocalOffer};

const MAX_PAIR_ATTEMPTS: u32 = 3;
/// PairResult reason when a machine doesn't accept pairing requests.
const DECLINED: &str = "declined";

// ---------------------------------------------------------------- UI state

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiPairing {
    /// We asked the peer to pair; the user must type the code shown there.
    EnterCode { error: Option<String> },
    /// The peer asked to pair; show this code to the user.
    ShowCode { code: String },
    /// We asked, but the peer doesn't accept pairing requests.
    Declined,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiPeer {
    pub node_id: Uuid,
    pub name: String,
    pub os: Option<Os>,
    pub addr: Option<String>,
    pub connected: bool,
    pub trusted: bool,
    pub fingerprint: String,
    pub screen: Option<Rect>,
    pub pairing: Option<UiPairing>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiBlocked {
    pub node_id: Uuid,
    pub name: String,
    pub online: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiMe {
    pub node_id: Uuid,
    pub name: String,
    pub os: Os,
    pub port: u16,
    pub fingerprint: String,
    pub screen: Rect,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiState {
    pub me: UiMe,
    pub peers: Vec<UiPeer>,
    pub layout: Layout,
    pub settings: Settings,
    pub manual_peers: Vec<String>,
    /// Machines whose pairing requests are ignored (not listed in `peers`).
    pub blocked: Vec<UiBlocked>,
    /// Name of the machine currently receiving our input, if not local.
    pub controlling: Option<String>,
    /// Name of the machine currently sending us input.
    pub controlled_by: Option<String>,
    pub permissions: Vec<Permission>,
    pub capture_error: Option<String>,
    pub inject_error: Option<String>,
    pub log: Vec<LogEntry>,
    /// File transfers (copied files/folders), newest first.
    pub transfers: Vec<UiTransfer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    /// Offered by another machine; downloads when the user pastes.
    Pending,
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiTransfer {
    pub id: Uuid,
    /// true = receiving files on this machine, false = sending.
    pub incoming: bool,
    pub peer: String,
    pub label: String,
    pub files: usize,
    pub total: u64,
    pub done: u64,
    pub state: TransferState,
    pub error: Option<String>,
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
    /// Receiving straight into this folder (paste in Finder / "Save to…").
    pub saved_to: Option<String>,
    /// Received files and folders, once done.
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    /// Milliseconds since the Unix epoch.
    pub ts: u64,
    pub text: String,
}

// ---------------------------------------------------------------- commands

#[derive(Debug)]
pub enum Command {
    SetLayout(Layout),
    Pair(Uuid),
    SubmitCode(Uuid, String),
    CancelPair(Uuid),
    Unpair(Uuid),
    /// Reject this machine's pairing requests from now on (unpairs it too).
    Block(Uuid),
    Unblock(Uuid),
    Connect(String),
    RemoveManualPeer(String),
    SetSettings(Settings),
    SetName(String),
    ReturnHome,
    RefreshPermissions,
    /// Download a pending incoming transfer now and put it on the clipboard.
    StartTransfer(Uuid),
    /// Download a pending incoming transfer straight into this folder.
    SaveTransferTo(Uuid, std::path::PathBuf),
    /// Internal: where an intercepted paste is going.
    #[doc(hidden)]
    PasteResolved(Uuid, PasteTarget),
    /// Internal: paste again into this app (files are on the clipboard now).
    #[doc(hidden)]
    Repaste(i64),
    /// Internal: delete received copies that are no longer needed; the
    /// paths are the files on the clipboard now (kept).
    #[doc(hidden)]
    PruneCache(Vec<std::path::PathBuf>),
    /// Offer these files to paired machines as if they had been copied.
    OfferFiles(Vec<std::path::PathBuf>),
    CancelTransfer(Uuid),
    DismissTransfer(Uuid),
}

/// Options for [`start`]. `Default` gives the normal desktop behaviour.
#[derive(Clone)]
pub struct StartOptions {
    pub config_dir: std::path::PathBuf,
    /// Override the listening port stored in the config.
    pub port: Option<u16>,
    /// Use the real OS input hooks / injection. Disabled in tests.
    pub os_input: bool,
    pub clipboard: bool,
    pub discovery: bool,
    /// Fixed screen geometry (tests).
    pub screen: Option<Rect>,
    /// Receives events that would otherwise be injected (tests).
    pub inject_sink: Option<UnboundedSender<InputEvent>>,
    /// Use the existing config/identity without ever writing them
    /// (macOS login-screen helper).
    pub read_only: bool,
    /// Where received files are stored (default: the user cache folder).
    pub download_root: Option<std::path::PathBuf>,
}

impl Default for StartOptions {
    fn default() -> Self {
        Self {
            config_dir: Config::default_dir(),
            port: None,
            os_input: true,
            clipboard: true,
            discovery: true,
            screen: None,
            inject_sink: None,
            read_only: false,
            download_root: None,
        }
    }
}

#[derive(Clone)]
pub struct EngineHandle {
    cmd: UnboundedSender<Command>,
    state: Arc<RwLock<UiState>>,
    capture: UnboundedSender<CapturedEvent>,
}

impl EngineHandle {
    pub fn send(&self, c: Command) {
        let _ = self.cmd.send(c);
    }
    pub fn state(&self) -> UiState {
        self.state.read().clone()
    }
    /// Feed a synthetic captured event (as if it came from the local hooks).
    pub fn simulate_input(&self, ev: CapturedEvent) {
        let _ = self.capture.send(ev);
    }
}

pub type ChangeCallback = Arc<dyn Fn() + Send + Sync>;

// ---------------------------------------------------------------- internals

#[derive(Debug, Clone)]
enum PairState {
    None,
    /// We initiated; waiting for the user to type the peer's code.
    Initiated { attempts: u32, error: Option<String> },
    /// We sent a code and wait for the peer's verdict.
    Submitted,
    /// Peer initiated; we display the code.
    Showing { attempts: u32 },
    /// We asked, but the peer doesn't accept pairing requests.
    Declined,
}

struct Peer {
    tx: UnboundedSender<Message>,
    abort: tokio::task::AbortHandle,
    fingerprint: String,
    addr: SocketAddr,
    outbound: bool,
    hello: Option<Hello>,
    screen: Option<Rect>,
    trusted: bool,
    pairing: PairState,
}

impl Peer {
    fn node_id(&self) -> Option<Uuid> {
        self.hello.as_ref().map(|h| h.node_id)
    }
    fn send(&self, m: Message) {
        let _ = self.tx.send(m);
    }
}

enum Control {
    Local,
    Remote {
        node: Uuid,
        conn: ConnId,
        rect: Rect,
        pos: (f64, f64),
        os: Os,
    },
}

struct Xfer {
    ui: UiTransfer,
    abort: Option<tokio::task::AbortHandle>,
    /// Receiving side: the transfer directory (deleted on cancel/failure).
    dir: Option<std::path::PathBuf>,
    /// Receiving side: the offering machine and its offer.
    from: Option<(Uuid, FileOffer)>,
    /// Paste again into this app once the files are on the clipboard.
    repaste: Option<i64>,
}

enum XferEvent {
    Progress { id: Uuid, done: u64 },
    /// `Ok(Some(paths))` for a finished download, `Ok(None)` for an upload.
    Finished { id: Uuid, result: Result<Option<Vec<std::path::PathBuf>>, String> },
}

struct Engine {
    opts: StartOptions,
    cfg: Config,
    identity: Arc<Identity>,
    connector: TlsConnector,
    net_tx: UnboundedSender<NetEvent>,
    peers: HashMap<ConnId, Peer>,
    discovered: HashMap<Uuid, Discovered>,
    dialing: HashSet<String>,
    control: Control,
    controlled_by: Option<ConnId>,
    shared: Arc<CaptureShared>,
    screen: Rect,
    edge_push: f64,
    /// Modifier keys physically held (for hotkeys).
    mods: HashSet<u16>,
    /// Keys / buttons we pressed on the remote, to release on switch.
    remote_keys: HashSet<u16>,
    remote_buttons: HashSet<MouseButton>,
    /// Keys / buttons we injected locally on behalf of a peer.
    injected_keys: HashSet<u16>,
    injected_buttons: HashSet<MouseButton>,
    injector: Option<Injector>,
    inject_error: Option<String>,
    capture_error: Option<String>,
    capture_started: bool,
    capture_tx: UnboundedSender<CapturedEvent>,
    clipboard: Option<ClipboardHandle>,
    clip_tx: UnboundedSender<ClipboardData>,
    offer_tx: UnboundedSender<Result<LocalOffer, String>>,
    xfer_tx: UnboundedSender<XferEvent>,
    /// Files copied on this machine that peers may fetch.
    local_offer: Option<Arc<LocalOffer>>,
    cmd_tx: UnboundedSender<Command>,
    /// Waiting to learn where an intercepted paste goes (this transfer).
    paste_resolving: Option<Uuid>,
    /// The V of a paste chord from the controlling machine was swallowed.
    remote_paste_v: bool,
    transfers: Vec<Xfer>,
    clip_ignore_until: Instant,
    clip_last_hash: Option<[u8; 32]>,
    clip_pending: Option<Instant>,
    log: VecDeque<LogEntry>,
    state: Arc<RwLock<UiState>>,
    on_change: ChangeCallback,
    dirty: bool,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn is_mac(os: Os) -> bool {
    os == Os::MacOs
}

/// Start the engine on the current Tokio runtime.
pub async fn start(opts: StartOptions, on_change: ChangeCallback) -> anyhow::Result<EngineHandle> {
    let mut cfg = Config::load_or_create(&opts.config_dir, opts.read_only)?;
    if let Some(p) = opts.port {
        cfg.port = p;
    }
    let identity = Arc::new(Identity::load_or_create(&opts.config_dir, opts.read_only)?);
    let (net_tx, net_rx) = unbounded_channel();
    let (cmd_tx, cmd_rx) = unbounded_channel();
    let (capture_tx, capture_rx) = unbounded_channel();
    let (disc_tx, disc_rx) = unbounded_channel();
    let (clip_change_tx, clip_change_rx) = unbounded_channel();
    let (clip_tx, clip_rx) = unbounded_channel();
    let (offer_tx, offer_rx) = unbounded_channel();
    let (xfer_tx, xfer_rx) = unbounded_channel();

    net::listen(cfg.port, net::acceptor(identity.server_config()?), net_tx.clone()).await?;
    let connector = net::connector(identity.client_config()?);

    if opts.discovery {
        tokio::spawn(discovery::run(
            discovery::beacon(cfg.node_id, &cfg.name, cfg.port, &identity.fingerprint),
            disc_tx,
        ));
    } else {
        drop(disc_tx);
    }

    let clipboard = if opts.clipboard {
        match ClipboardHandle::start(clip_change_tx) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::error!("clipboard disabled: {e}");
                None
            }
        }
    } else {
        None
    };

    let screen = opts.screen.unwrap_or_else(|| input::screen_bounds(cfg.settings.fallback_screen));
    let state = Arc::new(RwLock::new(UiState {
        me: UiMe {
            node_id: cfg.node_id,
            name: cfg.name.clone(),
            os: Os::current(),
            port: cfg.port,
            fingerprint: identity.fingerprint.clone(),
            screen,
        },
        peers: vec![],
        layout: cfg.layout.clone(),
        settings: cfg.settings.clone(),
        manual_peers: cfg.manual_peers.clone(),
        blocked: Vec::new(),
        controlling: None,
        controlled_by: None,
        permissions: permissions::check(),
        capture_error: None,
        inject_error: None,
        log: vec![],
        transfers: vec![],
    }));

    let mut engine = Engine {
        opts: opts.clone(),
        cfg,
        identity,
        connector,
        net_tx,
        peers: HashMap::new(),
        discovered: HashMap::new(),
        dialing: HashSet::new(),
        control: Control::Local,
        controlled_by: None,
        shared: Arc::new(CaptureShared::default()),
        screen,
        edge_push: 0.0,
        mods: HashSet::new(),
        remote_keys: HashSet::new(),
        remote_buttons: HashSet::new(),
        injected_keys: HashSet::new(),
        injected_buttons: HashSet::new(),
        injector: None,
        inject_error: None,
        capture_error: None,
        capture_started: false,
        capture_tx: capture_tx.clone(),
        clipboard,
        clip_tx,
        offer_tx,
        xfer_tx,
        local_offer: None,
        cmd_tx: cmd_tx.clone(),
        paste_resolving: None,
        remote_paste_v: false,
        transfers: Vec::new(),
        clip_ignore_until: Instant::now(),
        clip_last_hash: None,
        clip_pending: None,
        log: VecDeque::new(),
        state: state.clone(),
        on_change,
        dirty: true,
    };
    engine.try_start_capture();
    engine.log(format!("Started as \"{}\" on port {}", engine.cfg.name, engine.cfg.port));
    engine.publish();

    tokio::spawn(engine.run(net_rx, cmd_rx, capture_rx, disc_rx, clip_change_rx, clip_rx, offer_rx, xfer_rx));
    Ok(EngineHandle { cmd: cmd_tx, state, capture: capture_tx })
}

impl Engine {
    #[allow(clippy::too_many_arguments)] // one receiver per event source
    async fn run(
        mut self,
        mut net_rx: UnboundedReceiver<NetEvent>,
        mut cmd_rx: UnboundedReceiver<Command>,
        mut capture_rx: UnboundedReceiver<CapturedEvent>,
        mut disc_rx: UnboundedReceiver<Discovered>,
        mut clip_change_rx: UnboundedReceiver<()>,
        mut clip_rx: UnboundedReceiver<ClipboardData>,
        mut offer_rx: UnboundedReceiver<Result<LocalOffer, String>>,
        mut xfer_rx: UnboundedReceiver<XferEvent>,
    ) {
        // Copies received in an earlier session are only kept while they
        // are still on the clipboard.
        self.check_cache();
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        let mut fast = tokio::time::interval(Duration::from_millis(150));
        loop {
            tokio::select! {
                biased;
                Some(ev) = capture_rx.recv() => {
                    self.on_captured(ev);
                    // Drain bursts without re-polling everything else.
                    while let Ok(ev) = capture_rx.try_recv() {
                        self.on_captured(ev);
                    }
                }
                Some(ev) = net_rx.recv() => self.on_net(ev),
                Some(cmd) = cmd_rx.recv() => self.on_command(cmd),
                Some(d) = disc_rx.recv() => self.on_discovered(d),
                Some(()) = clip_change_rx.recv() => {
                    if Instant::now() >= self.clip_ignore_until {
                        // Debounce: apps often write several formats in a row.
                        self.clip_pending = Some(Instant::now() + Duration::from_millis(250));
                    }
                }
                Some(data) = clip_rx.recv() => self.broadcast_clipboard(data),
                Some(offer) = offer_rx.recv() => self.on_local_offer(offer),
                Some(ev) = xfer_rx.recv() => self.on_xfer(ev),
                _ = fast.tick() => {
                    if let Some(t) = self.clip_pending {
                        if Instant::now() >= t {
                            self.clip_pending = None;
                            self.read_clipboard();
                        }
                    }
                    if self.dirty {
                        self.publish();
                    }
                }
                _ = tick.tick() => self.periodic(),
            }
        }
    }

    // ------------------------------------------------------------ helpers

    fn log(&mut self, s: impl Into<String>) {
        let s = s.into();
        tracing::info!("{s}");
        self.log.push_back(LogEntry { ts: now_ms(), text: s });
        while self.log.len() > 80 {
            self.log.pop_front();
        }
        self.dirty = true;
    }

    fn save(&mut self) {
        if let Err(e) = self.cfg.save() {
            self.log(format!("Failed to save config: {e}"));
        }
        self.dirty = true;
    }

    fn peer_by_node(&self, node: Uuid) -> Option<(ConnId, &Peer)> {
        self.peers
            .iter()
            .find(|(_, p)| p.node_id() == Some(node))
            .map(|(c, p)| (*c, p))
    }

    fn node_name(&self, node: Uuid) -> String {
        self.peer_by_node(node)
            .and_then(|(_, p)| p.hello.as_ref().map(|h| h.name.clone()))
            .or_else(|| self.cfg.trusted.iter().find(|t| t.node_id == node).map(|t| t.name.clone()))
            .unwrap_or_else(|| node.to_string())
    }

    fn try_start_capture(&mut self) {
        if self.capture_started || !self.opts.os_input {
            return;
        }
        match input::start_capture(self.capture_tx.clone(), self.shared.clone()) {
            Ok(()) => {
                self.capture_started = true;
                if self.capture_error.take().is_some() {
                    self.log("Input capture started");
                }
            }
            Err(e) => {
                let msg = e.to_string();
                if self.capture_error.as_deref() != Some(&msg) {
                    self.log(format!("Input capture unavailable: {msg}"));
                }
                self.capture_error = Some(msg);
            }
        }
        self.dirty = true;
    }

    fn inject(&mut self, ev: InputEvent) {
        if let Some(sink) = &self.opts.inject_sink {
            let _ = sink.send(ev);
            return;
        }
        if let Some(inj) = self.injector() {
            inj.inject(ev);
        }
    }

    fn set_grab(&mut self, grab: bool) {
        if self.opts.os_input {
            input::set_grab(&self.shared, grab);
        }
    }

    fn warp(&self, x: f64, y: f64) {
        if self.opts.os_input {
            input::warp(x, y);
        }
    }

    fn injector(&mut self) -> Option<&mut Injector> {
        if self.injector.is_none() {
            match Injector::new() {
                Ok(mut i) => {
                    i.set_bounds(self.screen);
                    self.injector = Some(i);
                    self.inject_error = None;
                }
                Err(e) => {
                    let msg = e.to_string();
                    if self.inject_error.as_deref() != Some(&msg) {
                        self.log(format!("Cannot inject input: {msg}"));
                    }
                    self.inject_error = Some(msg);
                    self.dirty = true;
                    return None;
                }
            }
        }
        self.injector.as_mut()
    }

    fn hello(&self) -> Message {
        Message::Hello(Hello {
            protocol: PROTOCOL_VERSION,
            node_id: self.cfg.node_id,
            name: self.cfg.name.clone(),
            os: Os::current(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            port: self.cfg.port,
            screen: self.screen,
            layout: self.cfg.layout.clone(),
        })
    }

    fn dial(&mut self, addr: String) {
        if !self.dialing.insert(addr.clone()) {
            return;
        }
        let connector = self.connector.clone();
        let tx = self.net_tx.clone();
        tokio::spawn(net::dial(addr, connector, tx));
    }

    fn broadcast_trusted(&self, m: &Message) {
        for p in self.peers.values().filter(|p| p.trusted) {
            p.send(m.clone());
        }
    }

    // ------------------------------------------------------------ network

    fn on_net(&mut self, ev: NetEvent) {
        match ev {
            NetEvent::Connected { conn, fingerprint, addr, outbound, tx, abort } => {
                let trusted = self.cfg.trusted_by_fingerprint(&fingerprint).is_some();
                let peer = Peer {
                    tx,
                    abort,
                    fingerprint,
                    addr,
                    outbound,
                    hello: None,
                    screen: None,
                    trusted,
                    pairing: PairState::None,
                };
                peer.send(self.hello());
                self.peers.insert(conn, peer);
            }
            NetEvent::Closed { conn, reason } => {
                if let Some(p) = self.peers.remove(&conn) {
                    if let Some(h) = &p.hello {
                        let name = h.name.clone();
                        self.log(format!("Disconnected from {name}: {reason}"));
                    }
                }
                if let Control::Remote { conn: c, .. } = self.control {
                    if c == conn {
                        let center = self.screen.center();
                        self.return_local(center);
                    }
                }
                if self.controlled_by == Some(conn) {
                    self.end_controlled();
                }
                self.dirty = true;
            }
            NetEvent::DialEnded { addr, error } => {
                // Keeping the address in `dialing` while its connection lives
                // prevents duplicate dials; it is released only here.
                self.dialing.remove(&addr);
                if let Some(e) = error {
                    tracing::debug!("dial {addr} failed: {e}");
                }
            }
            NetEvent::Message { conn, msg } => self.on_message(conn, msg),
            NetEvent::FetchRequest { fingerprint, id, stream } => self.on_fetch_request(fingerprint, id, stream),
        }
    }

    fn on_message(&mut self, conn: ConnId, msg: Message) {
        let Some(peer) = self.peers.get(&conn) else { return };
        let trusted = peer.trusted;
        match msg {
            Message::Hello(h) => self.on_hello(conn, h),
            Message::Ping => peer.send(Message::Pong),
            Message::Pong => {}
            Message::PairRequest => self.on_pair_request(conn),
            Message::PairCode(code) => self.on_pair_code(conn, code),
            Message::PairResult { ok, reason } => self.on_pair_result(conn, ok, reason),
            // Everything below requires a paired machine.
            _ if !trusted => {
                tracing::warn!("ignoring message from unpaired peer {}", peer.addr);
            }
            Message::Screen(r) => {
                if let Some(p) = self.peers.get_mut(&conn) {
                    p.screen = Some(r);
                }
                if let Control::Remote { conn: c, rect, .. } = &mut self.control {
                    if *c == conn {
                        *rect = r;
                    }
                }
                self.dirty = true;
            }
            Message::Layout(l) => self.adopt_layout(l),
            Message::Enter { x, y } => {
                // If we were controlling someone, the user is now here instead.
                if !matches!(self.control, Control::Local) {
                    let c = self.screen.center();
                    self.return_local(c);
                }
                self.controlled_by = Some(conn);
                let name = self.peers.get(&conn).and_then(|p| p.hello.as_ref()).map(|h| h.name.clone());
                if let Some(n) = name {
                    tracing::debug!("{n} took control");
                }
                self.inject(InputEvent::MouseMove { x, y });
                self.dirty = true;
            }
            Message::Leave => {
                if self.controlled_by == Some(conn) {
                    self.end_controlled();
                }
            }
            Message::Input(ev) => self.inject_remote(ev),
            Message::Clipboard(data) => {
                // Newer clipboard content replaces any pending file download.
                self.cancel_incoming("replaced by a newer copy");
                self.write_clipboard(data);
            }
            Message::ClipboardFiles(offer) => self.on_remote_offer(conn, offer),
            // Transfer frames only belong on transfer connections.
            Message::FetchFiles { .. } | Message::FileData { .. } | Message::FilesDone | Message::FilesError(_) => {}
        }
    }

    fn on_hello(&mut self, conn: ConnId, h: Hello) {
        if h.protocol != PROTOCOL_VERSION {
            self.log(format!(
                "{} uses an incompatible protocol version ({} vs {PROTOCOL_VERSION}); update both apps",
                h.name, h.protocol
            ));
            if let Some(p) = self.peers.remove(&conn) {
                p.abort.abort();
            }
            return;
        }
        if h.node_id == self.cfg.node_id {
            // Connected to ourselves (e.g. manual peer pointing here).
            if let Some(p) = self.peers.remove(&conn) {
                p.abort.abort();
            }
            return;
        }
        // De-duplicate: keep the connection dialled by the smaller node id.
        let me = self.cfg.node_id;
        let dup = self
            .peers
            .iter()
            .find(|(c, p)| **c != conn && p.node_id() == Some(h.node_id))
            .map(|(c, p)| (*c, p.outbound));
        if let Some((other, other_outbound)) = dup {
            let preferred_outbound = me < h.node_id;
            let this_outbound = self.peers[&conn].outbound;
            let drop_conn = if this_outbound == preferred_outbound && other_outbound != preferred_outbound {
                other
            } else {
                conn
            };
            if let Some(p) = self.peers.remove(&drop_conn) {
                p.abort.abort();
            }
            if drop_conn == conn {
                return;
            }
        }

        let peer = self.peers.get_mut(&conn).expect("peer exists");
        // A trusted fingerprint must belong to the node id we paired with.
        if let Some(t) = self.cfg.trusted.iter_mut().find(|t| t.fingerprint == peer.fingerprint) {
            if t.node_id != h.node_id {
                peer.trusted = false;
            } else {
                t.name = h.name.clone();
                t.last_addr = Some(format!("{}:{}", peer.addr.ip(), h.port));
            }
        }
        peer.screen = Some(h.screen);
        let trusted = peer.trusted;
        let name = h.name.clone();
        let layout = h.layout.clone();
        // A repeated Hello (e.g. after a rename) only updates details.
        let first = peer.hello.replace(h).is_none();
        if trusted {
            if first {
                self.log(format!("Connected to {name}"));
            }
            self.adopt_layout(layout);
            self.save();
        } else if first {
            self.log(format!("Found {name} (not paired)"));
        }
        self.dirty = true;
    }

    fn on_discovered(&mut self, d: Discovered) {
        let id = d.beacon.node_id;
        let known = self.peer_by_node(id).is_some();
        self.discovered.insert(id, d.clone());
        // Only the smaller id dials to avoid duplicate connections.
        if !known && self.cfg.node_id < id {
            self.dial(d.addr.to_string());
        }
    }

    fn periodic(&mut self) {
        // Reconnect to trusted peers and manual addresses.
        let connected: HashSet<Uuid> = self.peers.values().filter_map(|p| p.node_id()).collect();
        let mut targets = Vec::new();
        for t in &self.cfg.trusted {
            if connected.contains(&t.node_id) {
                continue;
            }
            if let Some(d) = self.discovered.get(&t.node_id) {
                targets.push(d.addr.to_string());
            } else if let Some(a) = &t.last_addr {
                targets.push(a.clone());
            }
        }
        for m in &self.cfg.manual_peers {
            let addr = with_port(m, self.cfg.port);
            let connected_here = self.peers.values().any(|p| addr_matches(&addr, &p.addr));
            if !connected_here {
                targets.push(addr);
            }
        }
        for t in targets {
            self.dial(t);
        }
        for p in self.peers.values() {
            p.send(Message::Ping);
        }
        // Screen geometry may change (monitor plugged, resolution changed).
        let screen = self
            .opts
            .screen
            .unwrap_or_else(|| input::screen_bounds(self.cfg.settings.fallback_screen));
        if screen != self.screen {
            self.screen = screen;
            if let Some(i) = self.injector.as_mut() {
                i.set_bounds(screen);
            }
            self.broadcast_trusted(&Message::Screen(screen));
            self.dirty = true;
        }
        if !self.capture_started {
            self.try_start_capture();
        }
        let perms = permissions::check();
        let changed = {
            let st = self.state.read();
            st.permissions.iter().map(|p| p.granted).ne(perms.iter().map(|p| p.granted))
        };
        if changed {
            self.dirty = true;
        }
    }

    // ------------------------------------------------------------ pairing

    fn on_pair_request(&mut self, conn: ConnId) {
        let accept = self.cfg.settings.accept_pairing;
        let Some(p) = self.peers.get(&conn) else { return };
        let blocked = p.node_id().is_none_or(|n| self.cfg.is_blocked(n, &p.fingerprint));
        if blocked || !accept {
            // Same answer either way, so a blocked machine can't tell.
            p.send(Message::PairResult { ok: false, reason: DECLINED.into() });
            if !blocked {
                let name = p.hello.as_ref().map(|h| h.name.clone()).unwrap_or_default();
                self.log(format!("Declined pairing request from {name} (pairing requests are turned off)"));
            }
            return;
        }
        let Some(p) = self.peers.get_mut(&conn) else { return };
        p.pairing = PairState::Showing { attempts: 0 };
        let name = p.hello.as_ref().map(|h| h.name.clone()).unwrap_or_default();
        self.log(format!("{name} wants to pair — enter the code shown here on {name}"));
    }

    fn on_pair_code(&mut self, conn: ConnId, code: String) {
        let my_fp = self.identity.fingerprint.clone();
        let Some(p) = self.peers.get_mut(&conn) else { return };
        let PairState::Showing { attempts } = p.pairing.clone() else {
            p.send(Message::PairResult { ok: false, reason: "no pairing in progress".into() });
            return;
        };
        let expected = pairing_code(&my_fp, &p.fingerprint);
        if code.trim() == expected {
            p.pairing = PairState::None;
            p.send(Message::PairResult { ok: true, reason: String::new() });
            self.trust(conn);
        } else if attempts + 1 >= MAX_PAIR_ATTEMPTS {
            p.pairing = PairState::None;
            p.send(Message::PairResult { ok: false, reason: "too many wrong codes".into() });
            self.log("Pairing cancelled after too many wrong codes");
        } else {
            p.pairing = PairState::Showing { attempts: attempts + 1 };
            p.send(Message::PairResult { ok: false, reason: "wrong code".into() });
        }
        self.dirty = true;
    }

    fn on_pair_result(&mut self, conn: ConnId, ok: bool, reason: String) {
        let Some(p) = self.peers.get_mut(&conn) else { return };
        if !ok && reason == "cancelled" {
            // The other side cancelled / rejected: drop whatever we were showing.
            if !matches!(p.pairing, PairState::None) {
                p.pairing = PairState::None;
                let name = p.hello.as_ref().map(|h| h.name.clone()).unwrap_or_default();
                self.log(format!("Pairing with {name} was cancelled"));
            }
            return;
        }
        if !ok && reason == DECLINED && matches!(p.pairing, PairState::Initiated { .. }) {
            p.pairing = PairState::Declined;
            self.dirty = true;
            return;
        }
        if !matches!(p.pairing, PairState::Submitted) {
            return;
        }
        if ok {
            p.pairing = PairState::None;
            self.trust(conn);
        } else if reason == "wrong code" {
            p.pairing = PairState::Initiated { attempts: 0, error: Some("The other machine rejected the code".into()) };
        } else {
            p.pairing = PairState::None;
            self.log(format!("Pairing failed: {reason}"));
        }
        self.dirty = true;
    }

    fn trust(&mut self, conn: ConnId) {
        let Some(p) = self.peers.get_mut(&conn) else { return };
        let Some(h) = p.hello.clone() else { return };
        p.trusted = true;
        let addr = format!("{}:{}", p.addr.ip(), h.port);
        self.cfg.trusted.retain(|t| t.node_id != h.node_id);
        self.cfg.trusted.push(TrustedPeer {
            node_id: h.node_id,
            name: h.name.clone(),
            fingerprint: p.fingerprint.clone(),
            last_addr: Some(addr),
        });
        // Place the new machine in the layout and share it.
        if self.cfg.layout.ensure(h.node_id, &h.name) {
            self.cfg.layout.version = now_ms();
        }
        let layout = self.cfg.layout.clone();
        self.save();
        self.log(format!("Paired with {}", h.name));
        self.adopt_layout(h.layout);
        if self.cfg.layout.version >= layout.version {
            self.broadcast_trusted(&Message::Layout(self.cfg.layout.clone()));
        }
    }

    // ------------------------------------------------------------ layout

    fn adopt_layout(&mut self, l: Layout) {
        if l.version > self.cfg.layout.version && l.validate().is_ok() {
            self.cfg.layout = l;
            let (id, name) = (self.cfg.node_id, self.cfg.name.clone());
            self.cfg.layout.ensure(id, &name);
            self.save();
            self.log("Layout updated from another machine");
        } else if l.version < self.cfg.layout.version {
            self.broadcast_trusted(&Message::Layout(self.cfg.layout.clone()));
        }
    }

    // ------------------------------------------------------------ commands

    fn on_command(&mut self, cmd: Command) {
        match cmd {
            Command::SetLayout(mut l) => {
                if let Err(e) = l.validate() {
                    self.log(format!("Invalid layout: {e}"));
                    return;
                }
                l.version = now_ms().max(self.cfg.layout.version + 1);
                self.cfg.layout = l;
                let (id, name) = (self.cfg.node_id, self.cfg.name.clone());
                self.cfg.layout.ensure(id, &name);
                self.save();
                self.broadcast_trusted(&Message::Layout(self.cfg.layout.clone()));
                self.log("Layout saved and shared");
            }
            Command::Pair(node) => {
                let Some((conn, _)) = self.peer_by_node(node) else {
                    self.log("That machine is not connected");
                    return;
                };
                let p = self.peers.get_mut(&conn).unwrap();
                p.pairing = PairState::Initiated { attempts: 0, error: None };
                p.send(Message::PairRequest);
                self.dirty = true;
            }
            Command::SubmitCode(node, code) => {
                let my_fp = self.identity.fingerprint.clone();
                let Some((conn, _)) = self.peer_by_node(node) else { return };
                let p = self.peers.get_mut(&conn).unwrap();
                let PairState::Initiated { attempts, .. } = p.pairing.clone() else { return };
                let expected = pairing_code(&my_fp, &p.fingerprint);
                let code: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
                if code == expected {
                    p.pairing = PairState::Submitted;
                    p.send(Message::PairCode(code));
                } else if attempts + 1 >= MAX_PAIR_ATTEMPTS {
                    p.pairing = PairState::None;
                    self.log("Pairing cancelled: the code did not match. If it keeps failing, the connection may be intercepted.");
                } else {
                    p.pairing = PairState::Initiated {
                        attempts: attempts + 1,
                        error: Some("Code does not match. Check the code shown on the other machine.".into()),
                    };
                }
                self.dirty = true;
            }
            Command::CancelPair(node) => {
                if let Some((conn, _)) = self.peer_by_node(node) {
                    let p = self.peers.get_mut(&conn).unwrap();
                    if !matches!(p.pairing, PairState::None) {
                        p.pairing = PairState::None;
                        p.send(Message::PairResult { ok: false, reason: "cancelled".into() });
                    }
                }
                self.dirty = true;
            }
            Command::Unpair(node) => {
                self.cfg.trusted.retain(|t| t.node_id != node);
                self.cfg.layout.placements.retain(|p| p.node_id != node);
                self.cfg.layout.version = now_ms();
                if let Some((conn, _)) = self.peer_by_node(node) {
                    self.peers.get_mut(&conn).unwrap().trusted = false;
                    if let Control::Remote { conn: c, .. } = self.control {
                        if c == conn {
                            let center = self.screen.center();
                            self.return_local(center);
                        }
                    }
                }
                let name = self.node_name(node);
                self.save();
                self.broadcast_trusted(&Message::Layout(self.cfg.layout.clone()));
                self.log(format!("Removed {name}"));
            }
            Command::Block(node) => {
                let Some((name, fingerprint)) = self
                    .peer_by_node(node)
                    .map(|(_, p)| (p.hello.as_ref().map(|h| h.name.clone()).unwrap_or_default(), p.fingerprint.clone()))
                    .or_else(|| self.cfg.trusted.iter().find(|t| t.node_id == node).map(|t| (t.name.clone(), t.fingerprint.clone())))
                else {
                    return;
                };
                if self.cfg.trusted.iter().any(|t| t.node_id == node) {
                    self.on_command(Command::Unpair(node));
                }
                if let Some((conn, _)) = self.peer_by_node(node) {
                    let p = self.peers.get_mut(&conn).unwrap();
                    if !matches!(p.pairing, PairState::None) {
                        p.pairing = PairState::None;
                        p.send(Message::PairResult { ok: false, reason: "cancelled".into() });
                    }
                }
                self.cfg.blocked.retain(|b| b.node_id != node);
                self.cfg.blocked.push(BlockedPeer { node_id: node, name: name.clone(), fingerprint });
                self.save();
                self.log(format!("Blocked {name}: its pairing requests will be ignored"));
                self.dirty = true;
            }
            Command::Unblock(node) => {
                if let Some(i) = self.cfg.blocked.iter().position(|b| b.node_id == node) {
                    let b = self.cfg.blocked.remove(i);
                    self.save();
                    self.log(format!("Unblocked {}", b.name));
                }
                self.dirty = true;
            }
            Command::Connect(addr) => {
                let addr = addr.trim().to_string();
                if addr.is_empty() {
                    return;
                }
                if !self.cfg.manual_peers.contains(&addr) {
                    self.cfg.manual_peers.push(addr.clone());
                    self.save();
                }
                let full = with_port(&addr, self.cfg.port);
                self.log(format!("Connecting to {full}…"));
                self.dial(full);
            }
            Command::RemoveManualPeer(addr) => {
                self.cfg.manual_peers.retain(|a| a != &addr);
                self.save();
            }
            Command::SetSettings(s) => {
                self.cfg.settings = s;
                self.save();
            }
            Command::SetName(name) => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return;
                }
                self.cfg.name = name.clone();
                let id = self.cfg.node_id;
                if let Some(p) = self.cfg.layout.placements.iter_mut().find(|p| p.node_id == id) {
                    p.name = name;
                }
                self.cfg.layout.version = now_ms();
                self.save();
                let hello = self.hello();
                for p in self.peers.values() {
                    p.send(hello.clone());
                }
                self.broadcast_trusted(&Message::Layout(self.cfg.layout.clone()));
            }
            Command::ReturnHome => {
                let c = self.screen.center();
                self.return_local(c);
            }
            Command::StartTransfer(id) => {
                self.log("Fetching the copied files to the clipboard (requested in the control panel)");
                self.start_incoming(id, None, None)
            }
            Command::SaveTransferTo(id, folder) => {
                self.log(format!("Saving the copied files to {} (chosen with Save to…)", folder.display()));
                self.start_incoming(id, Some(folder), None)
            }
            Command::PasteResolved(id, target) => {
                self.paste_resolving = None;
                self.log(match &target {
                    PasteTarget::Folder(f) => format!("Paste into {} — fetching the copied files there", f.display()),
                    PasteTarget::App(_) => "Paste — fetching the copied files to the clipboard".to_string(),
                });
                match target {
                    PasteTarget::Folder(f) => self.start_incoming(id, Some(f), None),
                    PasteTarget::App(app) => self.start_incoming(id, None, app),
                }
            }
            Command::PruneCache(in_use) => {
                // Folders still being downloaded into are kept too.
                let keep: Vec<std::path::PathBuf> = self
                    .transfers
                    .iter()
                    .filter(|t| t.ui.incoming && t.ui.state == TransferState::Running && t.ui.saved_to.is_none())
                    .filter_map(|t| t.dir.clone())
                    .collect();
                let root = self.cache_root();
                tokio::task::spawn_blocking(move || {
                    let freed = transfer::prune_cache(&root, &keep, &in_use);
                    if freed > 0 {
                        tracing::info!("deleted {} of received copies no longer on the clipboard", human_bytes(freed));
                    }
                });
            }
            Command::Repaste(app) => {
                if self.opts.os_input && paste::front_app() == Some(app) {
                    let m = if cfg!(target_os = "macos") { ev::LEFTMETA } else { ev::LEFTCTRL };
                    for (code, down) in [(m, true), (ev::V, true), (ev::V, false), (m, false)] {
                        self.inject(InputEvent::Key { code, down });
                    }
                }
            }
            Command::OfferFiles(paths) => {
                let tx = self.offer_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = tx.send(transfer::scan(&paths).map_err(|e| e.to_string()));
                });
            }
            Command::CancelTransfer(id) => self.cancel_transfer(id, "cancelled"),
            Command::DismissTransfer(id) => {
                self.transfers.retain(|t| t.ui.id != id || matches!(t.ui.state, TransferState::Running));
                self.dirty = true;
            }
            Command::RefreshPermissions => {
                self.try_start_capture();
                self.dirty = true;
            }
        }
    }

    // ------------------------------------------------------------ capture

    fn on_captured(&mut self, ev: CapturedEvent) {
        match ev {
            CapturedEvent::Stopped(reason) => {
                self.capture_started = false;
                self.capture_error = Some(reason.clone());
                self.log(format!("Input capture stopped: {reason}"));
                let c = self.screen.center();
                self.return_local(c);
            }
            CapturedEvent::Key { code, down } => self.on_key(code, down),
            CapturedEvent::Paste => self.on_paste(),
            CapturedEvent::Button { button, down } => {
                if let Control::Remote { conn, .. } = self.control {
                    if down {
                        self.remote_buttons.insert(button);
                    } else if !self.remote_buttons.remove(&button) {
                        return;
                    }
                    self.send_to(conn, Message::Input(InputEvent::MouseButton { button, down }));
                }
            }
            CapturedEvent::Scroll { dx, dy } => {
                if let Control::Remote { conn, .. } = self.control {
                    self.send_to(conn, Message::Input(InputEvent::Scroll { dx, dy }));
                }
            }
            CapturedEvent::Motion { x, y, dx, dy } => self.on_motion(x, y, dx, dy),
        }
    }

    fn send_to(&self, conn: ConnId, m: Message) {
        if let Some(p) = self.peers.get(&conn) {
            p.send(m);
        }
    }

    fn on_key(&mut self, code: u16, down: bool) {
        let is_mod = keymap::is_ctrl(code) || keymap::is_alt(code) || keymap::is_shift(code) || keymap::is_meta(code);
        if is_mod {
            if down {
                self.mods.insert(code);
            } else {
                self.mods.remove(&code);
            }
        }
        // Hotkeys: Ctrl+Alt+Shift + Arrow / Enter.
        if down && self.cfg.settings.hotkeys_enabled && self.hotkey_mods_held() {
            let dir = match code {
                ev::LEFT => Some(Direction::Left),
                ev::RIGHT => Some(Direction::Right),
                ev::UP => Some(Direction::Up),
                ev::DOWN => Some(Direction::Down),
                _ => None,
            };
            if let Some(dir) = dir {
                self.hotkey_switch(dir);
                return;
            }
            if code == ev::ENTER || code == ev::KPENTER {
                let c = self.screen.center();
                self.return_local(c);
                return;
            }
        }
        if let Control::Remote { conn, os, .. } = self.control {
            let mut out = code;
            if self.cfg.settings.swap_cmd_ctrl && is_mac(Os::current()) != is_mac(os) {
                out = keymap::swap_cmd_ctrl(code);
            }
            if down {
                self.remote_keys.insert(out);
            } else if !self.remote_keys.remove(&out) {
                return;
            }
            self.send_to(conn, Message::Input(InputEvent::Key { code: out, down }));
        }
    }

    fn hotkey_mods_held(&self) -> bool {
        self.mods.iter().any(|c| keymap::is_ctrl(*c))
            && self.mods.iter().any(|c| keymap::is_alt(*c))
            && self.mods.iter().any(|c| keymap::is_shift(*c))
    }

    fn hotkey_switch(&mut self, dir: Direction) {
        let from = match &self.control {
            Control::Local => self.cfg.node_id,
            Control::Remote { node, .. } => *node,
        };
        let Some(next) = self.cfg.layout.neighbor(from, dir) else { return };
        if next == self.cfg.node_id {
            let c = self.screen.center();
            self.return_local(c);
        } else if let Some(rect) = self.peer_rect(next) {
            self.switch_to(next, rect.center());
        }
    }

    fn peer_rect(&self, node: Uuid) -> Option<Rect> {
        self.peer_by_node(node).filter(|(_, p)| p.trusted).and_then(|(_, p)| p.screen)
    }

    fn on_motion(&mut self, x: f64, y: f64, dx: f64, dy: f64) {
        match self.control {
            Control::Local => {
                if x.is_nan() || y.is_nan() || self.shared.any_local_button() {
                    return;
                }
                let b = self.screen;
                let dir = if x <= b.x && dx < 0.0 {
                    Some(Direction::Left)
                } else if x >= b.right() - 1.0 && dx > 0.0 {
                    Some(Direction::Right)
                } else if y <= b.y && dy < 0.0 {
                    Some(Direction::Up)
                } else if y >= b.bottom() - 1.0 && dy > 0.0 {
                    Some(Direction::Down)
                } else {
                    None
                };
                let Some(dir) = dir else {
                    self.edge_push = 0.0;
                    return;
                };
                let Some(next) = self.cfg.layout.neighbor(self.cfg.node_id, dir) else { return };
                let Some(rect) = self.peer_rect(next) else { return };
                self.edge_push += dx.abs() + dy.abs();
                if self.edge_push < self.cfg.settings.edge_resistance_px {
                    return;
                }
                self.edge_push = 0.0;
                let entry = entry_point(b, (x, y), dir, rect);
                self.switch_to(next, entry);
            }
            Control::Remote { node, conn, rect, pos, .. } => {
                let speed = self.cfg.settings.pointer_speed;
                let (nx, ny) = (pos.0 + dx * speed, pos.1 + dy * speed);
                let dir = if nx < rect.x {
                    Some(Direction::Left)
                } else if nx >= rect.right() {
                    Some(Direction::Right)
                } else if ny < rect.y {
                    Some(Direction::Up)
                } else if ny >= rect.bottom() {
                    Some(Direction::Down)
                } else {
                    None
                };
                if let Some(dir) = dir {
                    if self.remote_buttons.is_empty() {
                        if let Some(next) = self.cfg.layout.neighbor(node, dir) {
                            let (cx, cy) = rect.clamp(nx, ny);
                            if next == self.cfg.node_id {
                                let entry = entry_point(rect, (cx, cy), dir, self.screen);
                                self.return_local(entry);
                                return;
                            }
                            if let Some(nrect) = self.peer_rect(next) {
                                let entry = entry_point(rect, (cx, cy), dir, nrect);
                                self.switch_to(next, entry);
                                return;
                            }
                        }
                    }
                }
                let (cx, cy) = rect.clamp(nx, ny);
                if let Control::Remote { pos, .. } = &mut self.control {
                    *pos = (cx, cy);
                }
                self.send_to(conn, Message::Input(InputEvent::MouseMove { x: cx, y: cy }));
            }
        }
    }

    fn release_remote(&mut self, conn: ConnId) {
        for code in std::mem::take(&mut self.remote_keys) {
            self.send_to(conn, Message::Input(InputEvent::Key { code, down: false }));
        }
        for button in std::mem::take(&mut self.remote_buttons) {
            self.send_to(conn, Message::Input(InputEvent::MouseButton { button, down: false }));
        }
    }

    fn switch_to(&mut self, node: Uuid, entry: (f64, f64)) {
        let Some((conn, peer)) = self.peer_by_node(node) else { return };
        let Some(rect) = peer.screen else { return };
        let os = peer.hello.as_ref().map(|h| h.os).unwrap_or(Os::Other);
        match self.control {
            Control::Remote { conn: old, .. } => {
                self.release_remote(old);
                self.send_to(old, Message::Leave);
            }
            Control::Local => self.set_grab(true),
        }
        let entry = rect.clamp(entry.0, entry.1);
        self.send_to(conn, Message::Enter { x: entry.0, y: entry.1 });
        self.control = Control::Remote { node, conn, rect, pos: entry, os };
        self.dirty = true;
    }

    fn return_local(&mut self, at: (f64, f64)) {
        if let Control::Remote { conn, .. } = self.control {
            self.release_remote(conn);
            self.send_to(conn, Message::Leave);
            self.control = Control::Local;
            self.set_grab(false);
            let (x, y) = self.screen.clamp(at.0, at.1);
            self.warp(x, y);
            self.dirty = true;
        }
    }

    // ------------------------------------------------------------ being controlled

    fn inject_remote(&mut self, ev: InputEvent) {
        match ev {
            InputEvent::Key { code: ev::V, down } if self.remote_paste_v || (down && self.paste_armed() && keymap::is_paste_chord(&self.injected_keys)) => {
                // Paste with files from another machine waiting: fetch them
                // first instead of pasting the old clipboard.
                if down && !self.remote_paste_v {
                    self.on_paste();
                }
                self.remote_paste_v = down;
                return;
            }
            InputEvent::Key { code, down } => {
                if down {
                    self.injected_keys.insert(code);
                } else {
                    self.injected_keys.remove(&code);
                }
            }
            InputEvent::MouseButton { button, down } => {
                if down {
                    self.injected_buttons.insert(button);
                } else {
                    self.injected_buttons.remove(&button);
                }
            }
            _ => {}
        }
        self.inject(ev);
    }

    fn end_controlled(&mut self) {
        self.controlled_by = None;
        let keys: Vec<u16> = self.injected_keys.drain().collect();
        let buttons: Vec<MouseButton> = self.injected_buttons.drain().collect();
        for code in keys {
            self.inject(InputEvent::Key { code, down: false });
        }
        for button in buttons {
            self.inject(InputEvent::MouseButton { button, down: false });
        }
        self.dirty = true;
    }

    // ------------------------------------------------------------ clipboard

    fn read_clipboard(&mut self) {
        if !self.cfg.settings.clipboard_enabled || !self.peers.values().any(|p| p.trusted) {
            // Not shared, but received copies no longer on the clipboard
            // still have to go.
            self.check_cache();
            return;
        }
        let Some(cb) = self.clipboard.clone() else { return };
        let opts = ReadOptions { files: self.cfg.settings.clipboard_files_enabled };
        let tx = self.clip_tx.clone();
        let offer_tx = self.offer_tx.clone();
        let cmd_tx = self.cmd_tx.clone();
        let root = self.cache_root();
        if !opts.files {
            // Files on the clipboard aren't listed by this read.
            self.check_cache();
        }
        let lists_files = opts.files;
        tokio::task::spawn_blocking(move || match cb.read(opts) {
            Ok(Clip::Data(d)) => {
                if lists_files {
                    let _ = cmd_tx.send(Command::PruneCache(Vec::new()));
                }
                if !d.is_empty() {
                    let _ = tx.send(d);
                }
            }
            // Only names and sizes are collected here; contents are streamed
            // later to the machine that needs them.
            Ok(Clip::Files(paths)) => {
                let _ = cmd_tx.send(Command::PruneCache(paths.clone()));
                // Our own received copy back on the clipboard: not news.
                if !paths.iter().all(|p| p.starts_with(&root)) {
                    let _ = offer_tx.send(transfer::scan(&paths).map_err(|e| e.to_string()));
                }
            }
            Err(e) => tracing::warn!("clipboard read failed: {e}"),
        });
    }

    fn cache_root(&self) -> std::path::PathBuf {
        self.opts.download_root.clone().unwrap_or_else(transfer::default_download_root)
    }

    /// Read which files are on the clipboard, then prune the cache.
    fn check_cache(&self) {
        let root = self.cache_root();
        let (cb, cmd_tx) = (self.clipboard.clone(), self.cmd_tx.clone());
        tokio::task::spawn_blocking(move || {
            if !root.read_dir().is_ok_and(|mut d| d.next().is_some()) {
                return;
            }
            let in_use = match cb.map(|cb| cb.read(ReadOptions { files: true })) {
                Some(Ok(Clip::Files(paths))) => paths,
                Some(Ok(Clip::Data(_))) | None => Vec::new(),
                // Unsure what is on the clipboard: keep everything.
                Some(Err(_)) => return,
            };
            let _ = cmd_tx.send(Command::PruneCache(in_use));
        });
    }

    fn broadcast_clipboard(&mut self, data: ClipboardData) {
        let hash = clip_hash(&data);
        if self.clip_last_hash == Some(hash) {
            return;
        }
        self.clip_last_hash = Some(hash);
        self.supersede_local_offer();
        self.cancel_incoming("replaced by a newer copy");
        let what = describe_clip(&data);
        self.broadcast_trusted(&Message::Clipboard(data));
        tracing::info!("shared clipboard ({what})");
    }

    fn write_clipboard(&mut self, data: ClipboardData) {
        if !self.cfg.settings.clipboard_enabled {
            return;
        }
        let Some(cb) = self.clipboard.clone() else { return };
        self.clip_last_hash = Some(clip_hash(&data));
        self.clip_ignore_until = Instant::now() + Duration::from_millis(1500);
        self.clip_pending = None;
        tokio::task::spawn_blocking(move || {
            if let Err(e) = cb.write(data) {
                tracing::warn!("clipboard write failed: {e}");
            }
        });
    }

    // ------------------------------------------------------------ file transfers

    fn xfer(&mut self, id: Uuid) -> Option<&mut Xfer> {
        self.transfers.iter_mut().find(|t| t.ui.id == id)
    }

    fn push_transfer(&mut self, x: Xfer) {
        self.transfers.insert(0, x);
        // Keep the list short: drop the oldest finished entries.
        while self.transfers.len() > 8 {
            match self.transfers.iter().rposition(|t| !matches!(t.ui.state, TransferState::Running | TransferState::Pending)) {
                Some(i) => {
                    self.transfers.remove(i);
                }
                None => break,
            }
        }
        self.dirty = true;
    }

    /// A new local copy (or a remote one) makes our previous file offer stale:
    /// stop serving it and abort uploads in progress.
    /// A new copy makes our previous file offer stale: machines that haven't
    /// pasted it yet can no longer fetch it. Downloads already started (the
    /// user pasted) keep going.
    fn supersede_local_offer(&mut self) {
        self.local_offer = None;
    }

    /// Drop files copied elsewhere that haven't been pasted yet. Downloads
    /// already started (the user pasted) keep going.
    fn cancel_incoming(&mut self, why: &str) {
        let ids: Vec<Uuid> = self
            .transfers
            .iter()
            .filter(|t| t.ui.incoming && t.ui.state == TransferState::Pending)
            .map(|t| t.ui.id)
            .collect();
        for id in ids {
            self.cancel_transfer(id, why);
        }
    }

    fn cancel_transfer(&mut self, id: Uuid, why: &str) {
        let Some(t) = self.xfer(id) else { return };
        if !matches!(t.ui.state, TransferState::Pending | TransferState::Running) {
            return;
        }
        if let Some(a) = t.abort.take() {
            a.abort();
        }
        t.ui.state = TransferState::Cancelled;
        t.ui.error = Some(why.to_string());
        t.ui.finished_ms = Some(now_ms());
        if let Some(dir) = t.dir.take() {
            tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir));
        }
        self.dirty = true;
    }

    fn on_local_offer(&mut self, res: Result<LocalOffer, String>) {
        let local = match res {
            Ok(l) => l,
            Err(e) => {
                // Shown as a failed copy (not just in the log) so it is seen
                // here, where the files were copied, instead of failing later
                // on the machine that pastes them.
                self.supersede_local_offer();
                self.push_transfer(Xfer {
                    ui: UiTransfer {
                        id: Uuid::new_v4(),
                        incoming: false,
                        peer: String::new(),
                        label: "Copied files".into(),
                        files: 0,
                        total: 0,
                        done: 0,
                        state: TransferState::Failed,
                        error: Some(e.clone()),
                        started_ms: None,
                        finished_ms: Some(now_ms()),
                        saved_to: None,
                        paths: Vec::new(),
                    },
                    abort: None,
                    dir: None,
                    from: None,
                    repaste: None,
                });
                self.log(format!("Could not share the copied files: {e}"));
                return;
            }
        };
        if local.offer.entries.is_empty() {
            return;
        }
        self.supersede_local_offer();
        self.cancel_incoming("replaced by a newer copy");
        let offer = local.offer.clone();
        self.local_offer = Some(Arc::new(local));
        // Copying the same files twice in a row should not re-announce.
        self.clip_last_hash = None;
        self.broadcast_trusted(&Message::ClipboardFiles(offer.clone()));
        tracing::info!(
            "copied {} ({} files, {}) — offered to paired machines",
            offer.label(),
            offer.file_count(),
            human_bytes(offer.total)
        );
    }

    fn on_remote_offer(&mut self, conn: ConnId, offer: FileOffer) {
        if !self.cfg.settings.clipboard_enabled || !self.cfg.settings.clipboard_files_enabled {
            return;
        }
        let Some(h) = self.peers.get(&conn).and_then(|p| p.hello.clone()) else { return };
        self.supersede_local_offer();
        self.cancel_incoming("replaced by a newer copy");
        self.push_transfer(Xfer {
            ui: UiTransfer {
                id: offer.id,
                incoming: true,
                peer: h.name.clone(),
                label: offer.label(),
                files: offer.file_count(),
                total: offer.total,
                done: 0,
                state: TransferState::Pending,
                error: None,
                started_ms: None,
                finished_ms: None,
            saved_to: None,
            paths: Vec::new(),
            },
            abort: None,
            dir: None,
            from: Some((h.node_id, offer)),
            repaste: None,
        });
        let (label, total) = (self.transfers[0].ui.label.clone(), self.transfers[0].ui.total);
        self.log(format!("{} copied {label} ({}) — paste here to fetch it", h.name, human_bytes(total)));
    }

    /// The paste shortcut was pressed (locally or from the controlling
    /// machine) while copied files wait: find out where it goes, then fetch.
    fn on_paste(&mut self) {
        let Some(id) = self.transfers.iter().find(|t| t.ui.incoming && t.ui.state == TransferState::Pending).map(|t| t.ui.id) else {
            return;
        };
        if self.paste_resolving.is_some() {
            return;
        }
        self.paste_resolving = Some(id);
        if !self.opts.os_input {
            self.on_command(Command::PasteResolved(id, PasteTarget::App(None)));
            return;
        }
        let tx = self.cmd_tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(Command::PasteResolved(id, paste::target()));
        });
    }

    fn fail_transfer(&mut self, id: Uuid, err: String) {
        if let Some(t) = self.xfer(id) {
            t.ui.state = TransferState::Failed;
            t.ui.error = Some(err.clone());
            t.ui.finished_ms = Some(now_ms());
            if let Some(dir) = t.dir.take() {
                tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir));
            }
        }
        self.log(format!("File transfer failed: {err}"));
    }

    /// Files from another machine wait for a paste (intercept Cmd/Ctrl+V).
    fn paste_armed(&self) -> bool {
        self.transfers.iter().any(|t| t.ui.incoming && t.ui.state == TransferState::Pending)
    }

    /// Fetch a pending transfer. `folder`: save straight there (otherwise to
    /// the clipboard cache and onto the clipboard). `repaste`: app to paste
    /// into again once the files are on the clipboard.
    fn start_incoming(&mut self, id: Uuid, folder: Option<std::path::PathBuf>, repaste: Option<i64>) {
        let Some((node, offer)) = self
            .xfer(id)
            .filter(|t| t.ui.incoming && t.ui.state == TransferState::Pending)
            .and_then(|t| t.from.clone())
        else {
            return;
        };
        let Some((_, peer)) = self.peer_by_node(node).filter(|(_, p)| p.trusted) else {
            self.fail_transfer(id, "the machine that copied the files is not connected".into());
            return;
        };
        let Some(port) = peer.hello.as_ref().map(|h| h.port) else { return };
        let addr = SocketAddr::new(peer.addr.ip(), port);
        let fingerprint = peer.fingerprint.clone();

        let dir = match &folder {
            // Staged next to the destination (same disk, so moving it into
            // place at the end is instant), hidden until complete.
            Some(f) => {
                let d = f.join(format!(".smk-incoming-{}", id.simple()));
                std::fs::create_dir_all(&d).map(|_| d)
            }
            None => {
                transfer::new_transfer_dir(&self.cache_root())
            }
        };
        let dir = match dir {
            Ok(d) => d,
            Err(e) => {
                self.fail_transfer(id, format!("cannot create download folder: {e}"));
                return;
            }
        };
        // No size limit, but there must be room for it.
        if let Ok(free) = fs4::available_space(&dir) {
            let need = offer.total + 64 * 1024 * 1024;
            if free < need {
                let _ = std::fs::remove_dir_all(&dir);
                self.fail_transfer(
                    id,
                    format!("not enough disk space: needs {}, {} free", human_bytes(offer.total), human_bytes(free)),
                );
                return;
            }
        }

        let connector = self.connector.clone();
        let tx = self.xfer_tx.clone();
        let task_dir = dir.clone();
        let task_folder = folder.clone();
        let task = tokio::spawn(async move {
            let result = async {
                let mut stream = net::dial_transfer(addr, connector, &fingerprint, id).await?;
                let mut last = Instant::now();
                let ptx = tx.clone();
                let paths = transfer::receive(&mut stream, &offer, &task_dir, move |done| {
                    if last.elapsed() >= Duration::from_millis(150) {
                        last = Instant::now();
                        let _ = ptx.send(XferEvent::Progress { id, done });
                    }
                })
                .await?;
                match task_folder {
                    Some(f) => Ok(tokio::task::spawn_blocking(move || {
                        let moved = transfer::move_into(&paths, &f);
                        let _ = std::fs::remove_dir_all(&task_dir);
                        moved
                    })
                    .await??),
                    None => Ok(paths),
                }
            }
            .await;
            let result: anyhow::Result<Vec<std::path::PathBuf>> = result;
            let _ = tx.send(XferEvent::Finished { id, result: result.map(Some).map_err(|e| format!("{e:#}")) });
        });
        if let Some(t) = self.xfer(id) {
            t.ui.state = TransferState::Running;
            t.ui.started_ms = Some(now_ms());
            t.ui.saved_to = folder.map(|f| f.display().to_string());
            t.abort = Some(task.abort_handle());
            t.dir = Some(dir);
            t.repaste = repaste;
        }
        self.dirty = true;
    }

    fn on_fetch_request(&mut self, fingerprint: String, id: Uuid, mut stream: net::BoxStream) {
        let Some(t) = self.cfg.trusted_by_fingerprint(&fingerprint).cloned() else {
            tracing::warn!("file request from an unpaired machine ignored");
            return;
        };
        let Some(local) = self.local_offer.clone().filter(|l| l.offer.id == id) else {
            // Stale request (the clipboard changed since): just close it.
            tokio::spawn(async move {
                let _ = crate::protocol::write_frame(&mut stream, &Message::FilesError("the clipboard has changed".into())).await;
            });
            return;
        };
        let tx = self.xfer_tx.clone();
        // One upload per offer and machine: a retry replaces the old one.
        let upload_id = Uuid::from_u128(id.as_u128() ^ t.node_id.as_u128());
        if let Some(old) = self.xfer(upload_id) {
            if let Some(a) = old.abort.take() {
                a.abort();
            }
        }
        self.transfers.retain(|x| x.ui.id != upload_id);
        let task = tokio::spawn(async move {
            let mut last = Instant::now();
            let ptx = tx.clone();
            let result = transfer::send(&mut stream, &local, move |done| {
                if last.elapsed() >= Duration::from_millis(150) {
                    last = Instant::now();
                    let _ = ptx.send(XferEvent::Progress { id: upload_id, done });
                }
            })
            .await;
            let _ = tx.send(XferEvent::Finished { id: upload_id, result: result.map(|_| None).map_err(|e| format!("{e:#}")) });
        });
        let offer = &self.local_offer.as_ref().expect("checked above").offer;
        let ui = UiTransfer {
            id: upload_id,
            incoming: false,
            peer: t.name.clone(),
            label: offer.label(),
            files: offer.file_count(),
            total: offer.total,
            done: 0,
            state: TransferState::Running,
            error: None,
            started_ms: Some(now_ms()),
            finished_ms: None,
            saved_to: None,
            paths: Vec::new(),
        };
        self.push_transfer(Xfer { ui, abort: Some(task.abort_handle()), dir: None, from: None, repaste: None });
    }

    fn on_xfer(&mut self, ev: XferEvent) {
        match ev {
            XferEvent::Progress { id, done } => {
                if let Some(t) = self.xfer(id) {
                    if t.ui.state == TransferState::Running {
                        t.ui.done = done;
                        self.dirty = true;
                    }
                }
            }
            XferEvent::Finished { id, result } => {
                let Some(t) = self.xfer(id) else { return };
                if t.ui.state != TransferState::Running {
                    return; // cancelled meanwhile
                }
                t.abort = None;
                match result {
                    Ok(paths) => {
                        t.ui.state = TransferState::Done;
                        t.ui.done = t.ui.total;
                        t.ui.finished_ms = Some(now_ms());
                        let (label, peer, incoming) = (t.ui.label.clone(), t.ui.peer.clone(), t.ui.incoming);
                        let (saved_to, repaste) = (t.ui.saved_to.clone(), t.repaste);
                        if let Some(p) = &paths {
                            t.ui.paths = p.iter().map(|p| p.display().to_string()).collect();
                        }
                        // The download folder is kept until the next copy.
                        if saved_to.is_some() {
                            t.dir = None;
                        }
                        if let (Some(folder), Some(_)) = (&saved_to, &paths) {
                            self.log(format!("Saved {label} from {peer} to {folder}"));
                        } else if let (Some(paths), Some(cb)) = (paths, self.clipboard.clone()) {
                            self.clip_ignore_until = Instant::now() + Duration::from_millis(1500);
                            self.clip_pending = None;
                            let cmd_tx = self.cmd_tx.clone();
                            tokio::task::spawn_blocking(move || match cb.write_files(paths.clone()) {
                                Ok(()) => {
                                    // The copy it replaces on the clipboard isn't needed any more.
                                    let _ = cmd_tx.send(Command::PruneCache(paths));
                                    if let Some(app) = repaste {
                                        let _ = cmd_tx.send(Command::Repaste(app));
                                    }
                                }
                                Err(e) => tracing::warn!("putting files on the clipboard failed: {e}"),
                            });
                            self.log(format!("Received {label} from {peer} — ready to paste"));
                        } else if !incoming {
                            self.log(format!("Sent {label} to {peer}"));
                        }
                    }
                    Err(e) if !t.ui.incoming => {
                        // Usually the receiver cancelled or went away.
                        tracing::info!("upload to {} stopped: {e}", t.ui.peer);
                        t.ui.state = TransferState::Cancelled;
                        t.ui.error = Some(format!("stopped by {}", t.ui.peer));
                        t.ui.finished_ms = Some(now_ms());
                    }
                    Err(e) => self.fail_transfer(id, e),
                }
                self.dirty = true;
            }
        }
    }

    // ------------------------------------------------------------ UI snapshot

    fn publish(&mut self) {
        self.dirty = false;
        // Local Cmd/Ctrl+V can only be held back where the hook may swallow
        // keys without grabbing the keyboard (macOS, Windows).
        if self.opts.os_input && cfg!(any(target_os = "macos", target_os = "windows")) {
            self.shared.arm_paste(self.paste_armed(), &self.capture_tx);
        }
        let mut peers: Vec<UiPeer> = Vec::new();
        for p in self.peers.values() {
            let Some(h) = &p.hello else { continue };
            if self.cfg.is_blocked(h.node_id, &p.fingerprint) {
                continue;
            }
            let pairing = match &p.pairing {
                PairState::None => None,
                PairState::Initiated { error, .. } => Some(UiPairing::EnterCode { error: error.clone() }),
                PairState::Submitted => Some(UiPairing::EnterCode { error: None }),
                PairState::Declined => Some(UiPairing::Declined),
                PairState::Showing { .. } => Some(UiPairing::ShowCode {
                    code: pairing_code(&self.identity.fingerprint, &p.fingerprint),
                }),
            };
            peers.push(UiPeer {
                node_id: h.node_id,
                name: h.name.clone(),
                os: Some(h.os),
                addr: Some(p.addr.to_string()),
                connected: true,
                trusted: p.trusted,
                fingerprint: p.fingerprint.clone(),
                screen: p.screen,
                pairing,
            });
        }
        for t in &self.cfg.trusted {
            if !peers.iter().any(|p| p.node_id == t.node_id) {
                peers.push(UiPeer {
                    node_id: t.node_id,
                    name: t.name.clone(),
                    os: None,
                    addr: t.last_addr.clone(),
                    connected: false,
                    trusted: true,
                    fingerprint: t.fingerprint.clone(),
                    screen: None,
                    pairing: None,
                });
            }
        }
        peers.sort_by_key(|p| p.name.to_lowercase());
        let blocked = self
            .cfg
            .blocked
            .iter()
            .map(|b| UiBlocked {
                node_id: b.node_id,
                name: b.name.clone(),
                online: self.peers.values().any(|p| p.node_id() == Some(b.node_id) || p.fingerprint == b.fingerprint),
            })
            .collect();
        let controlling = match &self.control {
            Control::Local => None,
            Control::Remote { node, .. } => Some(self.node_name(*node)),
        };
        let controlled_by = self
            .controlled_by
            .and_then(|c| self.peers.get(&c))
            .and_then(|p| p.hello.as_ref())
            .map(|h| h.name.clone());
        {
            let mut st = self.state.write();
            st.me.name = self.cfg.name.clone();
            st.me.screen = self.screen;
            st.peers = peers;
            st.layout = self.cfg.layout.clone();
            st.settings = self.cfg.settings.clone();
            st.manual_peers = self.cfg.manual_peers.clone();
            st.blocked = blocked;
            st.controlling = controlling;
            st.controlled_by = controlled_by;
            st.permissions = permissions::check();
            st.capture_error = self.capture_error.clone();
            st.inject_error = self.inject_error.clone();
            st.log = self.log.iter().cloned().collect();
            st.transfers = self.transfers.iter().map(|t| t.ui.clone()).collect();
        }
        (self.on_change)();
    }
}

/// Map a point leaving `from` through edge `dir` to the matching point just
/// inside the opposite edge of `to`, preserving the relative position.
fn entry_point(from: Rect, p: (f64, f64), dir: Direction, to: Rect) -> (f64, f64) {
    let fy = ((p.1 - from.y) / from.h).clamp(0.0, 1.0);
    let fx = ((p.0 - from.x) / from.w).clamp(0.0, 1.0);
    match dir {
        Direction::Left => (to.right() - 2.0, to.y + fy * to.h),
        Direction::Right => (to.x + 1.0, to.y + fy * to.h),
        Direction::Up => (to.x + fx * to.w, to.bottom() - 2.0),
        Direction::Down => (to.x + fx * to.w, to.y + 1.0),
    }
}

fn with_port(addr: &str, default_port: u16) -> String {
    if addr.parse::<SocketAddr>().is_ok() || (addr.contains(':') && !addr.contains("::") && addr.rsplit(':').next().is_some_and(|p| p.parse::<u16>().is_ok())) {
        addr.to_string()
    } else {
        format!("{addr}:{default_port}")
    }
}

fn addr_matches(a: &str, b: &SocketAddr) -> bool {
    a == b.to_string() || a.split(':').next() == Some(&b.ip().to_string())
}

fn clip_hash(d: &ClipboardData) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(d.text.as_deref().unwrap_or("").as_bytes());
    h.update([0]);
    h.update(d.html.as_deref().unwrap_or("").as_bytes());
    h.update([0]);
    h.update(d.image_png.as_deref().map(|b| b.len().to_le_bytes()).unwrap_or_default());
    h.finalize().into()
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

fn describe_clip(d: &ClipboardData) -> String {
    if d.image_png.is_some() {
        "image".into()
    } else {
        "text".into()
    }
}
