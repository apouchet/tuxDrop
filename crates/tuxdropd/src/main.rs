//! tuxdropd: receives in the background, asks through desktop notifications
//! and exposes `org.tuxdrop.Daemon1` on the session bus.

mod notifications;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tuxdrop_core::client::{self, SendEvent, SendOutcome, SendRequest};
use tuxdrop_core::discovery::Peer;
use tuxdrop_core::receive::{IncomingRequest, Outcome, ReceiveEvent};
use tuxdrop_core::{DECISION_TIMEOUT, Node, SWEEP_DELAY, random_id};
use zbus::fdo;
use zbus::object_server::SignalEmitter;

use crate::notifications::{ACCEPT, ALWAYS, NotificationsProxy};

const BUS_NAME: &str = "org.tuxdrop.Daemon";
const OBJECT_PATH: &str = "/org/tuxdrop/Daemon";
const PEER_TTL: Duration = Duration::from_secs(300);

struct Pending {
    request: IncomingRequest,
    reply: oneshot::Sender<bool>,
    notification: Option<u32>,
}

struct Shared {
    node: Node,
    pending: Mutex<HashMap<String, Pending>>,
    peers: Mutex<HashMap<String, (Peer, Instant)>>,
    notifier: NotificationsProxy<'static>,
}

impl Shared {
    /// Applies the user's choice; returns false if `id` is not pending.
    async fn decide(&self, id: &str, choice: &str) -> bool {
        let Some(pending) = self.pending.lock().unwrap().remove(id) else {
            return false;
        };
        let always = choice == ALWAYS && pending.request.is_verified();
        if always {
            let sender = &pending.request.sender;
            if let Err(e) = Node::trust_store().add(&sender.fingerprint, &sender.alias) {
                tracing::error!("cannot save trusted device: {e:#}");
            }
        }
        let _ = pending.reply.send(choice == ACCEPT || always);
        if let Some(n) = pending.notification {
            let _ = self.notifier.close_notification(n).await;
        }
        true
    }

    /// Peers seen within `PEER_TTL`, with when they were last seen.
    fn live_peers(&self) -> Vec<(Peer, Instant)> {
        let mut peers = self.peers.lock().unwrap();
        peers.retain(|_, (_, seen)| seen.elapsed() < PEER_TTL);
        peers.values().cloned().collect()
    }

    /// Multicasts an announcement; if no device answers within
    /// `SWEEP_DELAY`, repeats it once by unicast.
    fn announce(self: &Arc<Self>) {
        let started = Instant::now();
        if let Err(e) = self.node.announce() {
            tracing::warn!("announce failed: {e:#}");
        }
        let shared = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SWEEP_DELAY).await;
            let answered = shared
                .peers
                .lock()
                .unwrap()
                .values()
                .any(|(_, seen)| *seen >= started);
            if !answered {
                shared.node.sweep();
            }
        });
    }

    async fn find_peer(&self, target: &str) -> Result<Peer> {
        if let Some(addr) = client::parse_address(target) {
            return client::resolve_address(&self.node.identity, &self.node.me, addr).await;
        }
        self.live_peers()
            .into_iter()
            .map(|(p, _)| p)
            .find(|p| p.matches(target))
            .with_context(|| format!("device {target:?} not found"))
    }
}

struct Api {
    shared: Arc<Shared>,
}

#[zbus::interface(name = "org.tuxdrop.Daemon1")]
impl Api {
    /// (alias, address, fingerprint, "https"/"http", seconds since last seen)
    /// for devices seen in the last 5 minutes.
    fn list_peers(&self) -> Vec<(String, String, String, String, u32)> {
        self.shared
            .live_peers()
            .into_iter()
            .map(|(p, seen)| {
                let addr = SocketAddr::new(p.addr, p.port).to_string();
                let age = seen.elapsed().as_secs() as u32;
                let protocol = p.protocol.scheme().to_owned();
                (p.info.alias, addr, p.info.fingerprint, protocol, age)
            })
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
            .pending
            .lock()
            .unwrap()
            .iter()
            .map(|(id, p)| {
                let r = &p.request;
                (
                    id.clone(),
                    r.sender.alias.clone(),
                    r.is_verified(),
                    r.files.len() as u32,
                    r.total_size,
                )
            })
            .collect()
    }

    async fn accept(&self, id: &str) -> fdo::Result<()> {
        self.choose(id, ACCEPT).await
    }

    async fn reject(&self, id: &str) -> fdo::Result<()> {
        self.choose(id, notifications::REJECT).await
    }

    /// Starts sending in the background; returns the transfer id used by
    /// the `Progress` and `TransferFinished` signals.
    async fn send(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        target: &str,
        paths: Vec<String>,
    ) -> fdo::Result<String> {
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        let files =
            client::collect_files(&paths).map_err(|e| fdo::Error::InvalidArgs(format!("{e:#}")))?;
        let peer = self
            .shared
            .find_peer(target)
            .await
            .map_err(|e| fdo::Error::Failed(format!("{e:#}")))?;
        let id = random_id();
        let emitter = emitter.into_owned();
        let shared = self.shared.clone();
        let transfer_id = id.clone();
        tokio::spawn(async move {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let progress: client::ProgressFn = Arc::new(move |e| {
                let _ = tx.send(e);
            });
            let req = SendRequest {
                identity: &shared.node.identity,
                me: &shared.node.me,
                peer: &peer,
                files,
                pin: None,
            };
            let transfer = client::send(req, progress, CancellationToken::new());
            tokio::pin!(transfer);
            let mut last_percent = u64::MAX;
            let result = loop {
                tokio::select! {
                    r = &mut transfer => break r,
                    Some(SendEvent::Progress { file_name, sent, total }) = rx.recv() => {
                        let percent = (sent * 100).checked_div(total).unwrap_or(100);
                        if percent != last_percent {
                            last_percent = percent;
                            let _ = Api::progress(&emitter, &transfer_id, &file_name, sent, total).await;
                        }
                    }
                }
            };
            let outcome = match result {
                Ok(SendOutcome::Completed | SendOutcome::NothingToSend) => "completed".to_owned(),
                Err(e) => format!("failed: {e:#}"),
            };
            let _ = Api::transfer_finished(&emitter, &transfer_id, &outcome).await;
        });
        Ok(id)
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

impl Api {
    async fn choose(&self, id: &str, choice: &str) -> fdo::Result<()> {
        if self.shared.decide(id, choice).await {
            Ok(())
        } else {
            Err(fdo::Error::InvalidArgs(format!("no pending transfer {id}")))
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let node = Node::load()?;
    let (peers_tx, mut peers_rx) = mpsc::channel(64);
    let mut events = node.start_receiver(peers_tx.clone())?;
    node.start_discovery(peers_tx)?;

    let conn = zbus::Connection::session()
        .await
        .context("cannot reach the D-Bus session bus (run tuxdropd inside a user session)")?;
    let notifier = NotificationsProxy::new(&conn).await?;
    let mut actions = notifier.receive_action_invoked().await?;
    tracing::info!(
        "receiving as {:?} ({}) into {}",
        node.me.alias,
        node.me.fingerprint,
        node.config.download_dir.display()
    );
    let shared = Arc::new(Shared {
        node,
        pending: Mutex::default(),
        peers: Mutex::default(),
        notifier,
    });
    conn.object_server()
        .at(
            OBJECT_PATH,
            Api {
                shared: shared.clone(),
            },
        )
        .await?;
    conn.request_name(BUS_NAME)
        .await
        .context("another tuxdropd already owns the bus name")?;
    let emitter = SignalEmitter::new(&conn, OBJECT_PATH)?;

    let peers = shared.clone();
    tokio::spawn(async move {
        while let Some(peer) = peers_rx.recv().await {
            peers.peers.lock().unwrap().insert(
                peer.info.fingerprint.to_ascii_uppercase(),
                (peer, Instant::now()),
            );
        }
    });

    let on_action = shared.clone();
    tokio::spawn(async move {
        while let Some(signal) = actions.next().await {
            let Ok(args) = signal.args() else { continue };
            let session = on_action
                .pending
                .lock()
                .unwrap()
                .iter()
                .find(|(_, p)| p.notification == Some(args.id))
                .map(|(id, _)| id.clone());
            if let Some(session) = session {
                on_action.decide(&session, &args.action_key).await;
            }
        }
    });

    while let Some(event) = events.recv().await {
        handle(&shared, &emitter, event).await;
    }
    Ok(())
}

async fn handle(shared: &Arc<Shared>, emitter: &SignalEmitter<'_>, event: ReceiveEvent) {
    match event {
        ReceiveEvent::Request { request, reply } => {
            let id = request.session_id.clone();
            let verified = request.is_verified();
            let summary = format!(
                "{} wants to send {} file(s)",
                request.sender.alias,
                request.files.len()
            );
            let mut body: Vec<String> = request
                .files
                .iter()
                .take(5)
                .map(|f| f.file_name.clone())
                .collect();
            if request.files.len() > 5 {
                body.push(format!("… and {} more", request.files.len() - 5));
            }
            body.push(format!(
                "{} MiB total",
                request.total_size.div_ceil(1024 * 1024)
            ));
            if !verified {
                body.push("Sender identity not verified".to_owned());
            }
            let _ = Api::incoming_request(
                emitter,
                &id,
                &request.sender.alias,
                verified,
                request.files.len() as u32,
                request.total_size,
            )
            .await;
            shared.pending.lock().unwrap().insert(
                id.clone(),
                Pending {
                    request,
                    reply,
                    notification: None,
                },
            );
            match shared
                .notifier
                .ask(&summary, &body.join("\n"), verified)
                .await
            {
                Ok(n) => {
                    if let Some(p) = shared.pending.lock().unwrap().get_mut(&id) {
                        p.notification = Some(n);
                    }
                }
                Err(e) => {
                    tracing::warn!("cannot show notification ({e}); use `tuxdrop accept {id}`")
                }
            }
            // The core declines on its own after the timeout; drop our entry too.
            let shared = shared.clone();
            tokio::spawn(async move {
                tokio::time::sleep(DECISION_TIMEOUT).await;
                let expired = shared.pending.lock().unwrap().remove(&id);
                if let Some(n) = expired.and_then(|p| p.notification) {
                    let _ = shared.notifier.close_notification(n).await;
                }
            });
        }
        ReceiveEvent::AutoAccepted { request } => {
            shared
                .notifier
                .inform(
                    &format!("Receiving from {}", request.sender.alias),
                    &format!("{} file(s) from a trusted device", request.files.len()),
                )
                .await;
        }
        ReceiveEvent::Progress {
            session_id,
            file_name,
            received,
            total,
        } => {
            let _ = Api::progress(emitter, &session_id, &file_name, received, total).await;
        }
        ReceiveEvent::FileReceived { path, .. } => tracing::info!("saved {}", path.display()),
        ReceiveEvent::Finished {
            session_id,
            outcome,
        } => {
            let (summary, body, signal) = match &outcome {
                Outcome::Completed => (
                    "Files received",
                    format!("Saved in {}", shared.node.config.download_dir.display()),
                    "completed".to_owned(),
                ),
                Outcome::Cancelled => (
                    "Transfer cancelled",
                    "The sender cancelled the transfer".to_owned(),
                    "cancelled".to_owned(),
                ),
                Outcome::Failed(reason) => (
                    "Transfer failed",
                    reason.clone(),
                    format!("failed: {reason}"),
                ),
            };
            tracing::info!("transfer {session_id}: {signal}");
            shared.notifier.inform(summary, &body).await;
            let _ = Api::transfer_finished(emitter, &session_id, &signal).await;
        }
    }
}
