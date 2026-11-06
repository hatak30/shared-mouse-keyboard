//! TLS transport. Each connection gets a reader task feeding [`NetEvent`]s to
//! the engine and a writer task draining an unbounded message queue.

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

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
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
                        if let Some(fp) = fp {
                            run_conn(tls, fp, addr, false, events).await;
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
        run_conn(tls, fp, sock_addr, true, events.clone()).await;
        Ok(())
    }
    .await;
    let _ = events.send(NetEvent::DialEnded { addr, error: res.err().map(|e| e.to_string()) });
}

async fn run_conn<S>(stream: S, fp: String, addr: SocketAddr, outbound: bool, events: EventTx)
where
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

    let ev = events.clone();
    let reader = tokio::spawn(async move {
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
