//! Linux (and BSD) desktops, reached through the D-Bus session bus.

mod desktop;
mod notifications;
pub mod tray;

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use zbus::fdo;
use zbus::object_server::SignalEmitter;

pub use self::desktop::{FileManager, ask_text, copy_to_clipboard, pick_device, pick_paths};
use self::notifications::NotificationsProxy;
pub use self::notifications::Notifier;
use crate::Shared;
use crate::api::ApiError;
use crate::notifications::{ACCEPT, NotificationEvent, REJECT};

const BUS_NAME: &str = "org.tuxdrop.Daemon";
const OBJECT_PATH: &str = "/org/tuxdrop/Daemon";

pub const CLIPBOARD_HELP: &str =
    "Copying needs wl-clipboard (sudo apt install wl-clipboard / sudo dnf install wl-clipboard)";
pub const DIALOG_HELP: &str =
    "This needs zenity (sudo apt install zenity / sudo dnf install zenity)";

pub fn run_main(daemon: impl Future<Output = Result<()>>) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(daemon)
}

pub struct Session {
    pub notifier: Notifier,
    pub file_manager: FileManager,
    pub signals: Signals,
    pub events: mpsc::UnboundedReceiver<NotificationEvent>,
    pub api: ApiServer,
}

impl Session {
    pub async fn start() -> Result<Self> {
        let conn = zbus::Connection::session()
            .await
            .context("cannot reach the D-Bus session bus (run tuxdropd inside a user session)")?;
        let proxy = NotificationsProxy::new(&conn).await?;
        let mut actions = proxy.receive_action_invoked().await?;
        let mut closed = proxy.receive_notification_closed().await?;
        let (tx, events) = mpsc::unbounded_channel();
        // Actions first: servers close a notification right after its button
        // is clicked.
        tokio::spawn(async move {
            loop {
                let event = tokio::select! {
                    biased;
                    Some(signal) = actions.next() => match signal.args() {
                        Ok(args) => NotificationEvent::Action {
                            id: args.id,
                            key: args.action_key,
                        },
                        Err(_) => continue,
                    },
                    Some(signal) = closed.next() => match signal.args() {
                        Ok(args) => NotificationEvent::Closed { id: args.id },
                        Err(_) => continue,
                    },
                    else => break,
                };
                if tx.send(event).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            notifier: Notifier::new(proxy),
            file_manager: FileManager::new(&conn).await?,
            signals: Signals(SignalEmitter::new(&conn, OBJECT_PATH)?),
            events,
            api: ApiServer(conn),
        })
    }
}

/// Emits the signals of `org.tuxdrop.Daemon1`.
pub struct Signals(SignalEmitter<'static>);

impl Signals {
    pub async fn incoming_request(
        &self,
        id: &str,
        alias: &str,
        verified: bool,
        files: u32,
        size: u64,
    ) {
        let _ = Api::incoming_request(&self.0, id, alias, verified, files, size).await;
    }

    pub async fn progress(&self, id: &str, file_name: &str, done: u64, total: u64) {
        let _ = Api::progress(&self.0, id, file_name, done, total).await;
    }

    pub async fn transfer_finished(&self, id: &str, outcome: &str) {
        let _ = Api::transfer_finished(&self.0, id, outcome).await;
    }
}

/// Serves `org.tuxdrop.Daemon1` on the session bus.
pub struct ApiServer(zbus::Connection);

impl ApiServer {
    pub async fn serve(self, shared: Arc<Shared>) -> Result<()> {
        self.0
            .object_server()
            .at(OBJECT_PATH, Api { shared })
            .await?;
        self.0
            .request_name(BUS_NAME)
            .await
            .context("another tuxdropd already owns the bus name")?;
        Ok(())
    }
}

impl From<ApiError> for fdo::Error {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::InvalidArgs(message) => fdo::Error::InvalidArgs(message),
            ApiError::Failed(message) => fdo::Error::Failed(message),
        }
    }
}

struct Api {
    shared: Arc<Shared>,
}

#[zbus::interface(name = "org.tuxdrop.Daemon1")]
impl Api {
    /// (alias, local name or "", address, fingerprint, "https"/"http",
    /// seconds since last seen) for devices seen in the last 5 minutes.
    fn list_peers(&self) -> Vec<(String, String, String, String, String, u32)> {
        self.shared
            .list_peers()
            .into_iter()
            .map(|p| (p.alias, p.name, p.addr, p.fingerprint, p.protocol, p.age))
            .collect()
    }

    /// Asks nearby devices to answer; they appear in `ListPeers` within a
    /// couple of seconds.
    fn announce(&self) {
        self.shared.announce();
    }

    /// (id, sender alias, identity verified, file count, total bytes)
    fn list_pending(&self) -> Vec<(String, String, bool, u32, u64)> {
        self.shared
            .list_pending()
            .into_iter()
            .map(|p| (p.id, p.alias, p.verified, p.files, p.size))
            .collect()
    }

    async fn accept(&self, id: &str) -> fdo::Result<()> {
        Ok(self.shared.choose(id, ACCEPT).await?)
    }

    async fn reject(&self, id: &str) -> fdo::Result<()> {
        Ok(self.shared.choose(id, REJECT).await?)
    }

    /// Cancels a transfer in progress, incoming (request id) or outgoing
    /// (id returned by `Send`).
    async fn cancel(&self, id: &str) -> fdo::Result<()> {
        Ok(self.shared.cancel_transfer(id).await?)
    }

    /// Starts sending in the background; returns the transfer id used by
    /// the `Progress` and `TransferFinished` signals.
    async fn send(&self, target: &str, paths: Vec<String>) -> fdo::Result<String> {
        let paths = paths.into_iter().map(PathBuf::from).collect();
        Ok(self.shared.send(target, paths).await?)
    }

    /// Asks in a dialog which nearby device to send `paths` to, then sends
    /// them; returns once the paths are checked. For file managers without
    /// a menu of their own.
    fn share(&self, paths: Vec<String>) -> fdo::Result<()> {
        let paths = paths.into_iter().map(PathBuf::from).collect();
        Ok(self.shared.share(paths)?)
    }

    #[zbus(signal)]
    async fn incoming_request(
        emitter: &SignalEmitter<'_>,
        id: &str,
        alias: &str,
        verified: bool,
        files: u32,
        size: u64,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn progress(
        emitter: &SignalEmitter<'_>,
        id: &str,
        file_name: &str,
        done: u64,
        total: u64,
    ) -> zbus::Result<()>;

    /// `outcome`: "completed", "cancelled" or "failed: <reason>".
    #[zbus(signal)]
    async fn transfer_finished(
        emitter: &SignalEmitter<'_>,
        id: &str,
        outcome: &str,
    ) -> zbus::Result<()>;
}
