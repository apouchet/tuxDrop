//! Client of the tuxdropd session D-Bus API.

use anyhow::{Result, bail};

use crate::human_size;

#[zbus::proxy(
    interface = "org.tuxdrop.Daemon1",
    default_service = "org.tuxdrop.Daemon",
    default_path = "/org/tuxdrop/Daemon"
)]
trait Daemon {
    fn list_peers(&self) -> zbus::Result<Vec<(String, String, String)>>;
    fn list_pending(&self) -> zbus::Result<Vec<(String, String, bool, u32, u64)>>;
    fn accept(&self, id: &str) -> zbus::Result<()>;
    fn reject(&self, id: &str) -> zbus::Result<()>;
}

async fn proxy() -> Result<DaemonProxy<'static>> {
    let conn = zbus::Connection::session().await?;
    let running = zbus::fdo::DBusProxy::new(&conn)
        .await?
        .name_has_owner("org.tuxdrop.Daemon".try_into()?)
        .await?;
    if !running {
        bail!("tuxdropd is not running (systemctl --user start tuxdropd)");
    }
    Ok(DaemonProxy::new(&conn).await?)
}

pub async fn status() -> Result<()> {
    let daemon = proxy().await?;
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
    println!("nearby devices:");
    for (alias, addr, fingerprint) in daemon.list_peers().await? {
        println!(
            "  {alias:<24} {addr:<40} {}",
            &fingerprint[..fingerprint.len().min(16)]
        );
    }
    Ok(())
}

pub async fn decide(id: &str, accept: bool) -> Result<()> {
    let daemon = proxy().await?;
    if accept {
        daemon.accept(id).await?;
    } else {
        daemon.reject(id).await?;
    }
    Ok(())
}
