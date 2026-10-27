//! macOS and Windows: a menu bar / notification area icon (tray-icon, which
//! needs the main thread), the control API on a local socket (a named pipe
//! on Windows), and the OS's own dialogs and notifications.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as os;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as os;

pub mod tray;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;
use tuxdrop_core::ipc::{self, Request, Response};

pub use self::os::{
    CLIPBOARD_HELP, DIALOG_HELP, FileManager, Notifier, ask_text, copy_to_clipboard, pick_device,
    pick_paths,
};
#[cfg(windows)]
pub use self::os::{already_running, log_file, write_icon};
pub use self::tray::run_main;
use crate::Shared;
use crate::api::ApiError;
use crate::notifications::{ACCEPT, NotificationEvent, REJECT};

pub struct Session {
    pub notifier: Notifier,
    pub file_manager: FileManager,
    pub signals: Signals,
    pub events: mpsc::UnboundedReceiver<NotificationEvent>,
    pub api: ApiServer,
}

impl Session {
    pub async fn start() -> Result<Self> {
        let api = ipc::Listener::bind().await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                anyhow::anyhow!("another tuxdropd is already running")
            } else {
                anyhow::Error::new(e).context("cannot create the control socket")
            }
        })?;
        let (tx, events) = mpsc::unbounded_channel();
        tokio::spawn(os::integrate());
        Ok(Self {
            notifier: Notifier::new(tx),
            file_manager: FileManager,
            signals: Signals,
            events,
            api: ApiServer(api),
        })
    }
}

/// The local socket has no signals yet: clients only make requests.
pub struct Signals;

impl Signals {
    pub async fn incoming_request(
        &self,
        _id: &str,
        _alias: &str,
        _verified: bool,
        _files: u32,
        _size: u64,
    ) {
    }

    pub async fn progress(&self, _id: &str, _file_name: &str, _done: u64, _total: u64) {}

    pub async fn transfer_finished(&self, _id: &str, _outcome: &str) {}
}

/// Serves [`crate::api`] on the local socket ([`tuxdrop_core::ipc`]).
pub struct ApiServer(ipc::Listener);

impl ApiServer {
    pub async fn serve(self, shared: Arc<Shared>) -> Result<()> {
        let mut listener = self.0;
        tokio::spawn(async move {
            loop {
                let stream = match listener.accept().await {
                    Ok(stream) => stream,
                    Err(e) => {
                        tracing::warn!("control socket: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let shared = shared.clone();
                tokio::spawn(async move {
                    let served = ipc::serve_connection(stream, |request| answer(shared, request));
                    if let Err(e) = served.await {
                        tracing::debug!("control connection: {e}");
                    }
                });
            }
        });
        Ok(())
    }
}

async fn answer(shared: Arc<Shared>, request: Request) -> Response {
    let done = |result: Result<(), ApiError>| match result {
        Ok(()) => Response::Done,
        Err(ApiError::InvalidArgs(e) | ApiError::Failed(e)) => Response::Error(e),
    };
    match request {
        Request::ListPeers => Response::Peers(shared.list_peers()),
        Request::Announce => {
            shared.announce();
            Response::Done
        }
        Request::ListPending => Response::Pending(shared.list_pending()),
        Request::Accept { id } => done(shared.choose(&id, ACCEPT).await),
        Request::Reject { id } => done(shared.choose(&id, REJECT).await),
        Request::Cancel { id } => done(shared.cancel_transfer(&id).await),
        Request::Send { target, paths } => {
            let paths = paths.into_iter().map(PathBuf::from).collect();
            match shared.send(&target, paths).await {
                Ok(id) => Response::Id(id),
                Err(ApiError::InvalidArgs(e) | ApiError::Failed(e)) => Response::Error(e),
            }
        }
        Request::Share { paths } => {
            done(shared.share(paths.into_iter().map(PathBuf::from).collect()))
        }
    }
}
