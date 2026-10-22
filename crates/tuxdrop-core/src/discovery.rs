//! LocalSend multicast discovery (UDP 224.0.0.167).
//!
//! On an announcement (`announce: true`) we answer through HTTP `register`,
//! falling back to a multicast reply with `announce: false`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;

use anyhow::Result;
use socket2::{Domain, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::client;
use crate::identity::{Identity, same_fingerprint};
use crate::proto::{API_PREFIX, Announcement, DEFAULT_PORT, DeviceInfo, MULTICAST_ADDR, Protocol};

#[derive(Clone, Debug)]
pub struct Peer {
    pub info: DeviceInfo,
    pub addr: IpAddr,
    pub port: u16,
    pub protocol: Protocol,
}

impl Peer {
    pub fn new(info: DeviceInfo, addr: IpAddr) -> Self {
        Self {
            port: info.port.unwrap_or(DEFAULT_PORT),
            protocol: info.protocol.unwrap_or(Protocol::Https),
            addr: addr.to_canonical(),
            info,
        }
    }

    pub fn api_url(&self, endpoint: &str) -> String {
        let host = match self.addr {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        };
        format!(
            "{}://{host}:{}{API_PREFIX}/{endpoint}",
            self.protocol.scheme(),
            self.port
        )
    }

    /// `target` is an alias (case-insensitive) or a fingerprint prefix of at
    /// least 6 characters.
    pub fn matches(&self, target: &str) -> bool {
        self.info.alias.eq_ignore_ascii_case(target)
            || (target.len() >= 6
                && self
                    .info
                    .fingerprint
                    .get(..target.len())
                    .is_some_and(|p| p.eq_ignore_ascii_case(target)))
    }
}

fn ipv4_interfaces() -> Vec<Ipv4Addr> {
    match if_addrs::get_if_addrs() {
        Ok(ifaces) => ifaces
            .into_iter()
            .filter_map(|i| match i.ip() {
                IpAddr::V4(ip) if !ip.is_loopback() => Some(ip),
                _ => None,
            })
            .collect(),
        Err(e) => {
            tracing::warn!("cannot list interfaces: {e}");
            Vec::new()
        }
    }
}

pub struct Discovery {
    socket: Arc<UdpSocket>,
    port: u16,
    identity: Arc<Identity>,
    me: DeviceInfo,
}

impl Discovery {
    /// Binds the shared multicast port. `SO_REUSEPORT` lets the CLI and the
    /// daemon listen at the same time.
    pub fn bind(port: u16, identity: Arc<Identity>, me: DeviceInfo) -> Result<Self> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(socket2::Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        socket.set_reuse_port(true)?;
        socket.set_nonblocking(true)?;
        socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into())?;
        let ifaces = ipv4_interfaces();
        if ifaces.is_empty() {
            socket.join_multicast_v4(&MULTICAST_ADDR, &Ipv4Addr::UNSPECIFIED)?;
        }
        for ip in ifaces {
            if let Err(e) = socket.join_multicast_v4(&MULTICAST_ADDR, &ip) {
                tracing::debug!("multicast join on {ip} failed: {e}");
            }
        }
        let socket = UdpSocket::from_std(socket.into())?;
        Ok(Self {
            socket: Arc::new(socket),
            port,
            identity,
            me,
        })
    }

    /// Receives datagrams forever, forwarding every other device to `peers`.
    pub async fn run(self, peers: mpsc::Sender<Peer>) {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let (len, src) = match self.socket.recv_from(&mut buf).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("discovery recv failed: {e}");
                    continue;
                }
            };
            let msg: Announcement = match serde_json::from_slice(&buf[..len]) {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!("ignoring datagram from {src}: {e}");
                    continue;
                }
            };
            if same_fingerprint(&msg.info.fingerprint, &self.me.fingerprint) {
                continue;
            }
            let peer = Peer::new(msg.info, src.ip());
            if msg.announce {
                self.answer(peer.clone());
            }
            if peers.send(peer).await.is_err() {
                return;
            }
        }
    }

    fn answer(&self, peer: Peer) {
        let identity = self.identity.clone();
        let me = self.me.clone();
        let port = self.port;
        tokio::spawn(async move {
            if let Err(e) = client::register(&identity, &me, &peer).await {
                tracing::debug!(
                    "register with {} failed ({e:#}), answering by UDP",
                    peer.addr
                );
                if let Err(e) = multicast(&me, false, port) {
                    tracing::warn!("multicast answer failed: {e:#}");
                }
            }
        });
    }
}

/// Multicasts `me` on every IPv4 interface; `announce: true` asks other
/// devices to answer. Uses a fresh socket per call, so setting the outgoing
/// interface cannot race with concurrent sends.
pub fn multicast(me: &DeviceInfo, announce: bool, port: u16) -> Result<()> {
    let payload = serde_json::to_vec(&Announcement {
        info: me.clone(),
        announce,
    })?;
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(socket2::Protocol::UDP))?;
    let dest: SocketAddr = SocketAddrV4::new(MULTICAST_ADDR, port).into();
    let ifaces = ipv4_interfaces();
    if ifaces.is_empty() {
        socket.send_to(&payload, &dest.into())?;
    }
    for ip in ifaces {
        let sent = socket
            .set_multicast_if_v4(&ip)
            .and_then(|()| socket.send_to(&payload, &dest.into()));
        if let Err(e) = sent {
            tracing::debug!("multicast send on {ip} failed: {e}");
        }
    }
    Ok(())
}
