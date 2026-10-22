//! Client of the tuxdropd session D-Bus API.
//!
//! When tuxdropd runs, it owns the port other devices answer discovery on, so
//! the CLI must ask it for nearby devices instead of listening itself.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tuxdrop_core::Node;
use tuxdrop_core::discovery::Peer;
use tuxdrop_core::proto::Protocol;

use crate::{describe, human_size};

#[zbus::proxy(
    interface = "org.tuxdrop.Daemon1",
    default_service = "org.tuxdrop.Daemon",
    default_path = "/org/tuxdrop/Daemon"
)]
pub trait Daemon {
    #[allow(clippy::type_complexity)]
    fn list_peers(&self) -> zbus::Result<Vec<(String, String, String, String, String, u32)>>;
    fn announce(&self) -> zbus::Result<()>;
    fn list_pending(&self) -> zbus::Result<Vec<(String, String, bool, u32, u64)>>;
    fn accept(&self, id: &str) -> zbus::Result<()>;
    fn reject(&self, id: &str) -> zbus::Result<()>;
}

/// The running daemon; `Ok(None)` if it is not running, an error if there is
/// no session bus at all.
pub async fn connect() -> Result<Option<DaemonProxy<'static>>> {
    let conn = zbus::Connection::session()
        .await
        .context("cannot reach the D-Bus session bus")?;
    let running = zbus::fdo::DBusProxy::new(&conn)
        .await?
        .name_has_owner("org.tuxdrop.Daemon".try_into()?)
        .await?;
    Ok(if running {
        Some(DaemonProxy::new(&conn).await?)
    } else {
        None
    })
}

async fn require() -> Result<DaemonProxy<'static>> {
    connect()
        .await?
        .context("tuxdropd is not running (systemctl --user start tuxdropd)")
}

/// Devices known to the daemon, with seconds since each was last seen. Their
/// local names come from the same file the daemon reads ([`Node::name_store`]).
async fn known_peers(daemon: &DaemonProxy<'_>) -> Result<Vec<(Peer, u32)>> {
    Ok(daemon
        .list_peers()
        .await?
        .into_iter()
        .filter_map(|(alias, _name, addr, fingerprint, protocol, age)| {
            let addr: SocketAddr = addr.parse().ok()?;
            let protocol = Protocol::from_scheme(&protocol)?;
            Some((Peer::known(alias, fingerprint, addr, protocol), age))
        })
        .collect())
}

/// Makes the daemon announce, then returns the devices that answered within
/// `timeout`.
pub async fn discover(daemon: &DaemonProxy<'_>, timeout: Duration) -> Result<Vec<Peer>> {
    daemon.announce().await?;
    tokio::time::sleep(timeout).await;
    Ok(known_peers(daemon)
        .await?
        .into_iter()
        .filter(|(_, age)| u64::from(*age) <= timeout.as_secs())
        .map(|(peer, _)| peer)
        .collect())
}

/// Looks `target` up in the daemon's table, announcing and waiting up to
/// `timeout` if it is not there yet.
pub async fn find(
    daemon: &DaemonProxy<'_>,
    target: &str,
    timeout: Duration,
) -> Result<Option<Peer>> {
    let names = Node::name_store().load();
    let lookup = async || -> Result<Option<Peer>> {
        Ok(known_peers(daemon)
            .await?
            .into_iter()
            .map(|(peer, _)| peer)
            .find(|peer| peer.matches(target, &names)))
    };
    if let Some(peer) = lookup().await? {
        return Ok(Some(peer));
    }
    daemon.announce().await?;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(300)).await;
        if let Some(peer) = lookup().await? {
            return Ok(Some(peer));
        }
    }
    Ok(None)
}

pub async fn status() -> Result<()> {
    let daemon = require().await?;
    let pending = daemon.list_pending().await?;
    if pending.is_empty() {
        println!("no pending transfer");
    }
    for (id, alias, verified, files, size) in pending {
        let identity = if verified { "verified" } else { "unverified" };
        println!(
            "pending {id}  from {alias} ({identity}): {files} file(s), {}",
            human_size(size)
        );
    }
    println!("devices seen in the last 5 minutes:");
    let names = Node::name_store().load();
    for (peer, age) in known_peers(&daemon).await? {
        println!("  {}  {age}s ago", describe(&peer, &names));
    }
    Ok(())
}

pub async fn decide(id: &str, accept: bool) -> Result<()> {
    let daemon = require().await?;
    if accept {
        daemon.accept(id).await?;
    } else {
        daemon.reject(id).await?;
    }
    Ok(())
}

/// For commands that also work without the daemon (no session bus, as over
/// SSH, counts as not running).
pub async fn running() -> Option<DaemonProxy<'static>> {
    connect().await.ok().flatten()
}
