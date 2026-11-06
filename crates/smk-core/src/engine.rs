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

use crate::clipboard::{ClipboardHandle, ReadOptions};
use crate::config::{Config, Settings, TrustedPeer};
use crate::discovery::{self, Discovered};
use crate::input::{self, CaptureShared, CapturedEvent, Injector};
use crate::keymap::{self, ev};
use crate::layout::{Direction, Layout};
use crate::net::{self, ConnId, NetEvent};
use crate::permissions::{self, Permission};
use crate::protocol::{ClipboardData, Hello, InputEvent, Message, MouseButton, Os, Rect, PROTOCOL_VERSION};
use crate::tls::{pairing_code, Identity};

const MAX_PAIR_ATTEMPTS: u32 = 3;

// ---------------------------------------------------------------- UI state

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiPairing {
    /// We asked the peer to pair; the user must type the code shown there.
    EnterCode { error: Option<String> },
    /// The peer asked to pair; show this code to the user.
    ShowCode { code: String },
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
    /// Name of the machine currently receiving our input, if not local.
    pub controlling: Option<String>,
    /// Name of the machine currently sending us input.
    pub controlled_by: Option<String>,
    pub permissions: Vec<Permission>,
    pub capture_error: Option<String>,
    pub inject_error: Option<String>,
    pub log: Vec<LogEntry>,
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
    Connect(String),
    RemoveManualPeer(String),
    SetSettings(Settings),
    SetName(String),
    ReturnHome,
    RefreshPermissions,
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
    let mut cfg = Config::load_or_create(&opts.config_dir)?;
    if let Some(p) = opts.port {
        cfg.port = p;
    }
    let identity = Arc::new(Identity::load_or_create(&opts.config_dir)?);
    let (net_tx, net_rx) = unbounded_channel();
    let (cmd_tx, cmd_rx) = unbounded_channel();
    let (capture_tx, capture_rx) = unbounded_channel();
    let (disc_tx, disc_rx) = unbounded_channel();
    let (clip_change_tx, clip_change_rx) = unbounded_channel();
    let (clip_tx, clip_rx) = unbounded_channel();

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
        controlling: None,
        controlled_by: None,
        permissions: permissions::check(),
        capture_error: None,
        inject_error: None,
        log: vec![],
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

    tokio::spawn(engine.run(net_rx, cmd_rx, capture_rx, disc_rx, clip_change_rx, clip_rx));
    Ok(EngineHandle { cmd: cmd_tx, state, capture: capture_tx })
}

impl Engine {
    async fn run(
        mut self,
        mut net_rx: UnboundedReceiver<NetEvent>,
        mut cmd_rx: UnboundedReceiver<Command>,
        mut capture_rx: UnboundedReceiver<CapturedEvent>,
        mut disc_rx: UnboundedReceiver<Discovered>,
        mut clip_change_rx: UnboundedReceiver<()>,
        mut clip_rx: UnboundedReceiver<ClipboardData>,
    ) {
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
                self.dialing.retain(|a| !addr_matches(a, &addr));
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
            NetEvent::DialFailed { addr, reason } => {
                self.dialing.remove(&addr);
                tracing::debug!("dial {addr} failed: {reason}");
            }
            NetEvent::Message { conn, msg } => self.on_message(conn, msg),
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
            Message::Clipboard(data) => self.write_clipboard(data),
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
        peer.hello = Some(h);
        if trusted {
            self.log(format!("Connected to {name}"));
            self.adopt_layout(layout);
            self.save();
        } else {
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
            return;
        }
        let Some(cb) = self.clipboard.clone() else { return };
        let opts = ReadOptions {
            files: self.cfg.settings.clipboard_files_enabled,
            max_file_bytes: self.cfg.settings.clipboard_max_file_mb * 1_000_000,
        };
        let tx = self.clip_tx.clone();
        tokio::task::spawn_blocking(move || match cb.read(opts) {
            Ok(d) if !d.is_empty() => {
                let _ = tx.send(d);
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("clipboard read failed: {e}"),
        });
    }

    fn broadcast_clipboard(&mut self, data: ClipboardData) {
        let hash = clip_hash(&data);
        if self.clip_last_hash == Some(hash) {
            return;
        }
        self.clip_last_hash = Some(hash);
        let what = describe_clip(&data);
        self.broadcast_trusted(&Message::Clipboard(data));
        tracing::info!("shared clipboard ({what})");
    }

    fn write_clipboard(&mut self, data: ClipboardData) {
        if !self.cfg.settings.clipboard_enabled {
            return;
        }
        if !data.files.is_empty() && !self.cfg.settings.clipboard_files_enabled {
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

    // ------------------------------------------------------------ UI snapshot

    fn publish(&mut self) {
        self.dirty = false;
        let mut peers: Vec<UiPeer> = Vec::new();
        for p in self.peers.values() {
            let Some(h) = &p.hello else { continue };
            let pairing = match &p.pairing {
                PairState::None => None,
                PairState::Initiated { error, .. } => Some(UiPairing::EnterCode { error: error.clone() }),
                PairState::Submitted => Some(UiPairing::EnterCode { error: None }),
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
            st.controlling = controlling;
            st.controlled_by = controlled_by;
            st.permissions = permissions::check();
            st.capture_error = self.capture_error.clone();
            st.inject_error = self.inject_error.clone();
            st.log = self.log.iter().cloned().collect();
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
    for f in &d.files {
        h.update(f.rel_path.as_bytes());
        h.update(f.data.len().to_le_bytes());
    }
    h.finalize().into()
}

fn describe_clip(d: &ClipboardData) -> String {
    if !d.files.is_empty() {
        format!("{} file(s)", d.files.iter().filter(|f| !f.is_dir).count())
    } else if d.image_png.is_some() {
        "image".into()
    } else {
        "text".into()
    }
}
