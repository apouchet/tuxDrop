//! Client of the running tuxdropd: its session D-Bus API on Linux, its local
//! socket on macOS and Windows ([`tuxdrop_core::ipc`]).
//!
//! When tuxdropd runs, it owns the port other devices answer discovery on, so
//! the CLI must ask it for nearby devices instead of listening itself.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tuxdrop_core::Node;
use tuxdrop_core::discovery::Peer;
use tuxdrop_core::proto::Protocol;

use crate::{describe, human_size};

pub use backend::Daemon;

#[cfg(freedesktop)]
const NOT_RUNNING: &str = "tuxdropd is not running (systemctl --user start tuxdropd)";
#[cfg(target_os = "macos")]
const NOT_RUNNING: &str = "tuxdropd is not running (open tuxDrop.app)";
#[cfg(windows)]
const NOT_RUNNING: &str = "tuxdropd is not running (start tuxDrop from the Start menu)";
#[cfg(not(any(freedesktop, target_os = "macos", windows)))]
const NOT_RUNNING: &str = "tuxdropd is not running";

async fn require() -> Result<Daemon> {
    Daemon::connect().await?.context(NOT_RUNNING)
}

/// Devices known to the daemon, with seconds since each was last seen. Their
/// local names come from the same file the daemon reads ([`Node::name_store`]).
async fn known_peers(daemon: &Daemon) -> Result<Vec<(Peer, u32)>> {
    Ok(daemon
        .list_peers()
        .await?
        .into_iter()
        .filter_map(|p| {
            let addr: SocketAddr = p.addr.parse().ok()?;
            let protocol = Protocol::from_scheme(&p.protocol)?;
            Some((Peer::known(p.alias, p.fingerprint, addr, protocol), p.age))
        })
        .collect())
}

/// Makes the daemon announce, then returns the devices that answered within
/// `timeout`.
pub async fn discover(daemon: &Daemon, timeout: Duration) -> Result<Vec<Peer>> {
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
pub async fn find(daemon: &Daemon, target: &str, timeout: Duration) -> Result<Option<Peer>> {
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
    for p in pending {
        let identity = if p.verified { "verified" } else { "unverified" };
        println!(
            "pending {}  from {} ({identity}): {} file(s), {}",
            p.id,
            p.alias,
            p.files,
            human_size(p.size)
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
        daemon.accept(id).await
    } else {
        daemon.reject(id).await
    }
}

/// The daemon asks which device to send `paths` to and sends them.
pub async fn share(paths: &[PathBuf]) -> Result<()> {
    let daemon = require().await?;
    // The daemon runs in another directory.
    let paths = paths
        .iter()
        .map(|p| {
            let p = std::path::absolute(p)?;
            p.try_exists()?
                .then(|| p.to_string_lossy().into_owned())
                .with_context(|| format!("{}: no such file or folder", p.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    daemon.share(paths).await
}

/// For commands that also work without the daemon (no session bus, as over
/// SSH, counts as not running).
pub async fn running() -> Option<Daemon> {
    Daemon::connect().await.ok().flatten()
}

#[cfg(freedesktop)]
mod backend {
    use anyhow::{Context, Result};
    use tuxdrop_core::ipc::{PeerEntry, PendingEntry};

    #[zbus::proxy(
        interface = "org.tuxdrop.Daemon1",
        default_service = "org.tuxdrop.Daemon",
        default_path = "/org/tuxdrop/Daemon"
    )]
    trait Daemon {
        #[allow(clippy::type_complexity)]
        fn list_peers(&self) -> zbus::Result<Vec<(String, String, String, String, String, u32)>>;
        fn announce(&self) -> zbus::Result<()>;
        fn list_pending(&self) -> zbus::Result<Vec<(String, String, bool, u32, u64)>>;
        fn accept(&self, id: &str) -> zbus::Result<()>;
        fn reject(&self, id: &str) -> zbus::Result<()>;
        fn share(&self, paths: &[String]) -> zbus::Result<()>;
    }

    pub struct Daemon(DaemonProxy<'static>);

    impl Daemon {
        /// The running daemon; `Ok(None)` if it is not running, an error if
        /// there is no session bus at all.
        pub async fn connect() -> Result<Option<Self>> {
            let conn = zbus::Connection::session()
                .await
                .context("cannot reach the D-Bus session bus")?;
            let running = zbus::fdo::DBusProxy::new(&conn)
                .await?
                .name_has_owner("org.tuxdrop.Daemon".try_into()?)
                .await?;
            Ok(if running {
                Some(Self(DaemonProxy::new(&conn).await?))
            } else {
                None
            })
        }

        pub async fn list_peers(&self) -> Result<Vec<PeerEntry>> {
            Ok(self
                .0
                .list_peers()
                .await?
                .into_iter()
                .map(
                    |(alias, name, addr, fingerprint, protocol, age)| PeerEntry {
                        alias,
                        name,
                        addr,
                        fingerprint,
                        protocol,
                        age,
                    },
                )
                .collect())
        }

        pub async fn announce(&self) -> Result<()> {
            Ok(self.0.announce().await?)
        }

        pub async fn list_pending(&self) -> Result<Vec<PendingEntry>> {
            Ok(self
                .0
                .list_pending()
                .await?
                .into_iter()
                .map(|(id, alias, verified, files, size)| PendingEntry {
                    id,
                    alias,
                    verified,
                    files,
                    size,
                })
                .collect())
        }

        pub async fn accept(&self, id: &str) -> Result<()> {
            Ok(self.0.accept(id).await?)
        }

        pub async fn reject(&self, id: &str) -> Result<()> {
            Ok(self.0.reject(id).await?)
        }

        pub async fn share(&self, paths: Vec<String>) -> Result<()> {
            Ok(self.0.share(&paths).await?)
        }
    }
}

#[cfg(not(freedesktop))]
mod backend {
    use anyhow::{Context, Result, bail};
    use tuxdrop_core::ipc::{self, PeerEntry, PendingEntry, Request, Response};

    pub struct Daemon;

    impl Daemon {
        /// The running daemon; `Ok(None)` if it is not running.
        pub async fn connect() -> Result<Option<Self>> {
            Ok(ipc::running()
                .await
                .context("cannot reach tuxdropd")?
                .then_some(Self))
        }

        async fn call(&self, request: Request) -> Result<Response> {
            match ipc::call(&request).await.context("cannot reach tuxdropd")? {
                Some(Response::Error(e)) => bail!(e),
                Some(response) => Ok(response),
                None => bail!("tuxdropd stopped"),
            }
        }

        pub async fn list_peers(&self) -> Result<Vec<PeerEntry>> {
            match self.call(Request::ListPeers).await? {
                Response::Peers(peers) => Ok(peers),
                other => bail!("unexpected answer from tuxdropd: {other:?}"),
            }
        }

        pub async fn announce(&self) -> Result<()> {
            self.call(Request::Announce).await.map(drop)
        }

        pub async fn list_pending(&self) -> Result<Vec<PendingEntry>> {
            match self.call(Request::ListPending).await? {
                Response::Pending(pending) => Ok(pending),
                other => bail!("unexpected answer from tuxdropd: {other:?}"),
            }
        }

        pub async fn accept(&self, id: &str) -> Result<()> {
            let id = id.to_owned();
            self.call(Request::Accept { id }).await.map(drop)
        }

        pub async fn reject(&self, id: &str) -> Result<()> {
            let id = id.to_owned();
            self.call(Request::Reject { id }).await.map(drop)
        }

        pub async fn share(&self, paths: Vec<String>) -> Result<()> {
            self.call(Request::Share { paths }).await.map(drop)
        }
    }
}
