//! LAN discovery via UDP broadcast beacons.
//!
//! Every instance broadcasts a small JSON beacon every few seconds and listens
//! for beacons of others. Networks that block broadcast can still connect by
//! entering an address manually.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

use crate::config::DISCOVERY_PORT;
use crate::protocol::Os;

const APP_TAG: &str = "shared-mouse-keyboard";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Beacon {
    pub app: String,
    pub v: u32,
    pub node_id: Uuid,
    pub name: String,
    pub os: Os,
    pub port: u16,
    pub fingerprint: String,
}

#[derive(Debug, Clone)]
pub struct Discovered {
    pub beacon: Beacon,
    pub addr: SocketAddr,
}

fn bind() -> anyhow::Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    sock.set_broadcast(true)?;
    sock.set_nonblocking(true)?;
    sock.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT).into())?;
    Ok(UdpSocket::from_std(sock.into())?)
}

/// Run discovery forever: broadcast `beacon` and report others on `found`.
pub async fn run(beacon: Beacon, found: UnboundedSender<Discovered>) {
    let sock = loop {
        match bind() {
            Ok(s) => break s,
            Err(e) => {
                tracing::warn!("discovery bind failed: {e}; retrying");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };
    let payload = serde_json::to_vec(&beacon).expect("beacon serializes");
    let my_id = beacon.node_id;
    let mut buf = vec![0u8; 2048];
    let mut tick = tokio::time::interval(Duration::from_secs(3));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let dst = SocketAddrV4::new(Ipv4Addr::BROADCAST, DISCOVERY_PORT);
                if let Err(e) = sock.send_to(&payload, dst).await {
                    tracing::debug!("beacon send failed: {e}");
                }
            }
            res = sock.recv_from(&mut buf) => {
                let Ok((n, from)) = res else { continue };
                let Ok(b) = serde_json::from_slice::<Beacon>(&buf[..n]) else { continue };
                if b.app != APP_TAG || b.node_id == my_id {
                    continue;
                }
                let addr = SocketAddr::new(from.ip(), b.port);
                let _ = found.send(Discovered { beacon: b, addr });
            }
        }
    }
}

pub fn beacon(node_id: Uuid, name: &str, port: u16, fingerprint: &str) -> Beacon {
    Beacon {
        app: APP_TAG.into(),
        v: 1,
        node_id,
        name: name.into(),
        os: Os::current(),
        port,
        fingerprint: fingerprint.into(),
    }
}
