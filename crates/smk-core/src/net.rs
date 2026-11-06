//! TLS transport. Each peer connection gets a reader task feeding
//! [`NetEvent`]s to the engine and a writer task draining an unbounded message
//! queue. A connection whose first frame is `FetchFiles` is a dedicated file
//! transfer instead and is handed to the engine as a raw stream.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rustls_pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::protocol::{read_frame, write_frame, Message};
use crate::tls::fingerprint;

pub type ConnId = u64;

/// A TLS stream, type-erased.
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
pub type BoxStream = Box<dyn Stream>;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub enum NetEvent {
    Connected {
        conn: ConnId,
        fingerprint: String,
        addr: SocketAddr,
        outbound: bool,
        tx: UnboundedSender<Message>,
        abort: tokio::task::AbortHandle,
    },
    Message { conn: ConnId, msg: Message },
    Closed { conn: ConnId, reason: String },
    /// An outgoing dial to `addr` finished: it failed (`error` set) or the
    /// connection it produced has closed. The address can be dialled again.
    DialEnded { addr: String, error: Option<String> },
    /// A peer opened a transfer connection asking for the files of offer `id`.
    FetchRequest { fingerprint: String, id: uuid::Uuid, stream: BoxStream },
}

pub type EventTx = UnboundedSender<NetEvent>;

/// Listen for peers. If the port is taken (e.g. the macOS login-screen
/// helper is still shutting down) keep retrying in the background.
pub async fn listen(port: u16, acceptor: TlsAcceptor, events: EventTx) -> anyhow::Result<()> {
    tokio::spawn(async move {
        let mut warned = false;
        let listener = loop {
            match TcpListener::bind(("0.0.0.0", port)).await {
                Ok(l) => break l,
                Err(e) => {
                    if !warned {
                        tracing::warn!("port {port} unavailable ({e}); retrying");
                        warned = true;
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        };
        tracing::info!("listening on port {port}");
        loop {
            let Ok((tcp, addr)) = listener.accept().await else { continue };
            let acceptor = acceptor.clone();
            let events = events.clone();
            tokio::spawn(async move {
                let _ = tcp.set_nodelay(true);
                match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await {
                    Ok(Ok(tls)) => {
                        let fp = tls
                            .get_ref()
                            .1
                            .peer_certificates()
                            .and_then(|c| c.first())
                            .map(|c| fingerprint(c));
                        let Some(fp) = fp else { return };
                        let mut tls = tls;
                        // The first frame tells a peer connection from a transfer.
                        let first = match tokio::time::timeout(Duration::from_secs(10), read_frame(&mut tls)).await {
                            Ok(Ok(m)) => m,
                            _ => return,
                        };
                        if let Message::FetchFiles { id } = first {
                            let _ = events.send(NetEvent::FetchRequest { fingerprint: fp, id, stream: Box::new(tls) });
                        } else {
                            run_conn(tls, fp, addr, false, Some(first), events).await;
                        }
                    }
                    Ok(Err(e)) => tracing::debug!("TLS accept from {addr} failed: {e}"),
                    Err(_) => tracing::debug!("TLS accept from {addr} timed out"),
                }
            });
        }
    });
    Ok(())
}

pub async fn dial(addr: String, connector: TlsConnector, events: EventTx) {
    let res: anyhow::Result<()> = async {
        let sock_addr = tokio::net::lookup_host(&addr)
            .await?
            .find(|a| a.is_ipv4())
            .ok_or_else(|| anyhow::anyhow!("cannot resolve {addr}"))?;
        let tcp = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(sock_addr)).await??;
        tcp.set_nodelay(true)?;
        let name = ServerName::try_from("shared-mouse-keyboard")?;
        let tls = tokio::time::timeout(Duration::from_secs(10), connector.connect(name, tcp)).await??;
        let fp = tls
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|c| c.first())
            .map(|c| fingerprint(c))
            .ok_or_else(|| anyhow::anyhow!("peer sent no certificate"))?;
        run_conn(tls, fp, sock_addr, true, None, events.clone()).await;
        Ok(())
    }
    .await;
    let _ = events.send(NetEvent::DialEnded { addr, error: res.err().map(|e| e.to_string()) });
}

/// Open a transfer connection to `addr`, check that the other side is the
/// machine with fingerprint `expected_fp`, and request the files of offer `id`.
pub async fn dial_transfer(
    addr: SocketAddr,
    connector: TlsConnector,
    expected_fp: &str,
    id: uuid::Uuid,
) -> anyhow::Result<BoxStream> {
    let tcp = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await??;
    tcp.set_nodelay(true)?;
    let name = ServerName::try_from("shared-mouse-keyboard")?;
    let mut tls = tokio::time::timeout(Duration::from_secs(10), connector.connect(name, tcp)).await??;
    let fp = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|c| c.first())
        .map(|c| fingerprint(c))
        .ok_or_else(|| anyhow::anyhow!("peer sent no certificate"))?;
    anyhow::ensure!(fp == expected_fp, "the machine at {addr} is not the one that copied the files");
    write_frame(&mut tls, &Message::FetchFiles { id }).await?;
    Ok(Box::new(tls))
}

async fn run_conn<S>(
    stream: S,
    fp: String,
    addr: SocketAddr,
    outbound: bool,
    first: Option<Message>,
    events: EventTx,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let conn = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let (mut rd, mut wr) = tokio::io::split(stream);
    let (tx, mut rx) = unbounded_channel::<Message>();

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if write_frame(&mut wr, &msg).await.is_err() {
                break;
            }
        }
    });

    // The reader must not deliver anything before the engine has seen
    // `Connected` (and the first frame), or early messages would be dropped.
    let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
    let ev = events.clone();
    let reader = tokio::spawn(async move {
        if go_rx.await.is_err() {
            return "closed".to_string();
        }
        loop {
            match read_frame(&mut rd).await {
                Ok(msg) => {
                    if ev.send(NetEvent::Message { conn, msg }).is_err() {
                        return "engine stopped".to_string();
                    }
                }
                Err(e) => return e.to_string(),
            }
        }
    });

    let _ = events.send(NetEvent::Connected {
        conn,
        fingerprint: fp,
        addr,
        outbound,
        tx,
        abort: reader.abort_handle(),
    });
    if let Some(msg) = first {
        let _ = events.send(NetEvent::Message { conn, msg });
    }
    let _ = go_tx.send(());

    let reason = match reader.await {
        Ok(r) => r,
        Err(_) => "closed".to_string(),
    };
    writer.abort();
    let _ = events.send(NetEvent::Closed { conn, reason });
}

pub fn acceptor(cfg: rustls::ServerConfig) -> TlsAcceptor {
    TlsAcceptor::from(Arc::new(cfg))
}

pub fn connector(cfg: rustls::ClientConfig) -> TlsConnector {
    TlsConnector::from(Arc::new(cfg))
}
