//! LocalSend discovery over UDP port 53317.
//!
//! An announcement (`announce: true`) is multicast to 224.0.0.167; devices
//! answer through HTTP `register`, falling back to a multicast reply with
//! `announce: false`. Multicast is unreliable towards Wi-Fi clients, so a
//! caller that gets no answer can repeat the announcement by unicast to every
//! address of the local networks ([`sweep`]).

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use socket2::{Domain, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::client;
use crate::identity::{Identity, same_fingerprint};
use crate::names::Names;
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

    /// A peer reported by a discovery table (tuxdropd) rather than by the
    /// device itself; the protocol version is unknown.
    pub fn known(alias: String, fingerprint: String, addr: SocketAddr, protocol: Protocol) -> Self {
        Self {
            info: DeviceInfo {
                alias,
                version: String::new(),
                device_model: None,
                device_type: None,
                fingerprint,
                port: Some(addr.port()),
                protocol: Some(protocol),
                download: false,
            },
            addr: addr.ip().to_canonical(),
            port: addr.port(),
            protocol,
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

    /// See [`designates`].
    pub fn matches(&self, target: &str, names: &Names) -> bool {
        designates(
            target,
            &self.info.alias,
            &self.info.fingerprint,
            names.get(&self.info.fingerprint),
        )
    }
}

/// Whether `target` designates a device: its local name or its alias
/// (case-insensitive), or a fingerprint prefix of at least 6 characters.
pub fn designates(target: &str, alias: &str, fingerprint: &str, name: Option<&str>) -> bool {
    name.is_some_and(|n| n.to_lowercase() == target.to_lowercase())
        || (!alias.is_empty() && alias.eq_ignore_ascii_case(target))
        || (target.len() >= 6
            && fingerprint
                .get(..target.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(target)))
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
            tracing::debug!(
                "announcement from {src}: {:?} (announce: {})",
                msg.info.alias,
                msg.announce
            );
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
            match client::register(&identity, &me, &peer).await {
                Ok(_) => tracing::debug!("answered {} by register", peer.addr),
                Err(e) => {
                    tracing::debug!(
                        "register with {} failed ({e:#}), answering by UDP",
                        peer.addr
                    );
                    // The protocol answers by multicast; the unicast copy is
                    // what reliably reaches a Wi-Fi client.
                    if let Err(e) = multicast(&me, false, port) {
                        tracing::warn!("multicast answer failed: {e:#}");
                    }
                    if let Err(e) = unicast(&me, SocketAddr::new(peer.addr, port)) {
                        tracing::debug!("unicast answer to {} failed: {e:#}", peer.addr);
                    }
                }
            }
        });
    }
}

/// Answers one device directly (`announce: false`).
fn unicast(me: &DeviceInfo, dest: SocketAddr) -> Result<()> {
    let payload = serde_json::to_vec(&Announcement {
        info: me.clone(),
        announce: false,
    })?;
    std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?.send_to(&payload, dest)?;
    Ok(())
}

/// Sends our announcement by unicast to every host of the local networks
/// (at most the /24 around each address) of physical interfaces; returns the
/// number of datagrams sent. Use it once, when multicast got no answer.
pub fn sweep(me: &DeviceInfo, port: u16) -> Result<usize> {
    let payload = serde_json::to_vec(&Announcement {
        info: me.clone(),
        announce: true,
    })?;
    let mut sent = 0;
    for iface in if_addrs::get_if_addrs()? {
        let if_addrs::IfAddr::V4(v4) = &iface.addr else {
            continue;
        };
        // Bridges, veth, VPN tunnels… have no backing device.
        let physical = Path::new("/sys/class/net")
            .join(&iface.name)
            .join("device")
            .exists();
        if !physical || v4.ip.is_loopback() || v4.ip.is_link_local() {
            continue;
        }
        let targets = sweep_targets(v4.ip, v4.prefixlen);
        tracing::debug!(
            "sweeping {} addresses from {} ({})",
            targets.len(),
            v4.ip,
            iface.name
        );
        // Datagrams to hosts awaiting ARP resolution (~1 KiB each) hold the
        // socket's send buffer until resolved or timed out: one non-blocking
        // socket per network, with a buffer large enough for a whole /24 (the
        // kernel caps it at twice net.core.wmem_max, 416 KiB by default).
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(socket2::Protocol::UDP))?;
        socket.set_send_buffer_size(512 * 1024)?;
        socket.set_nonblocking(true)?;
        socket.bind(&SocketAddrV4::new(v4.ip, 0).into())?;
        let socket = std::net::UdpSocket::from(socket);
        for target in targets {
            match socket.send_to(&payload, (target, port)) {
                Ok(_) => sent += 1,
                Err(e) => tracing::trace!("sweep to {target} failed: {e}"),
            }
        }
    }
    Ok(sent)
}

/// Host addresses of `ip`'s network, narrowed to its /24 when larger,
/// without `ip` itself.
fn sweep_targets(ip: Ipv4Addr, prefixlen: u8) -> Vec<Ipv4Addr> {
    let prefix = u32::from(prefixlen.clamp(24, 32));
    if prefix >= 31 {
        return Vec::new();
    }
    let mask = u32::MAX << (32 - prefix);
    let network = u32::from(ip) & mask;
    let broadcast = network | !mask;
    (network + 1..broadcast)
        .map(Ipv4Addr::from)
        .filter(|a| *a != ip)
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_covers_the_subnet_except_self() {
        let ip = Ipv4Addr::new(192, 168, 1, 212);
        let targets = sweep_targets(ip, 24);
        assert_eq!(targets.len(), 253);
        assert_eq!(targets[0], Ipv4Addr::new(192, 168, 1, 1));
        assert_eq!(targets[252], Ipv4Addr::new(192, 168, 1, 254));
        assert!(!targets.contains(&ip));
    }

    #[test]
    fn sweep_narrows_large_networks_and_keeps_small_ones() {
        let wide = sweep_targets(Ipv4Addr::new(10, 1, 2, 3), 8);
        assert_eq!(wide.len(), 253);
        assert!(wide.iter().all(|a| a.octets()[..3] == [10, 1, 2]));
        assert_eq!(sweep_targets(Ipv4Addr::new(10, 0, 0, 1), 30).len(), 1);
        assert!(sweep_targets(Ipv4Addr::new(10, 0, 0, 1), 31).is_empty());
    }

    #[test]
    fn known_peer_targets_reported_address() {
        let addr: SocketAddr = "192.168.1.16:53317".parse().unwrap();
        let peer = Peer::known("fedora".into(), "C41ADA".into(), addr, Protocol::Https);
        assert_eq!(
            peer.api_url("info"),
            "https://192.168.1.16:53317/api/localsend/v2/info"
        );
        assert!(peer.matches("FEDORA", &Names::default()));
    }

    #[test]
    fn designates_by_name_alias_or_prefix() {
        let fp = "C41ADA0123";
        assert!(designates(
            "Bureau d'Élise",
            "fedora",
            fp,
            Some("bureau d'élise")
        ));
        assert!(designates("FEDORA", "fedora", fp, Some("Bureau")));
        assert!(designates("c41ada", "fedora", fp, None));
        assert!(!designates("c41ad", "fedora", fp, None));
        assert!(!designates("", "", fp, None));
    }
}
