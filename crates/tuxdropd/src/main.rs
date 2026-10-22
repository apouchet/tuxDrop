//! tuxdropd: receives in the background, asks through desktop notifications
//! and exposes `org.tuxdrop.Daemon1` on the session bus.

mod desktop;
mod notifications;
mod tray;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tuxdrop_core::client::{self, SendEvent, SendOutcome, SendRequest};
use tuxdrop_core::discovery::Peer;
use tuxdrop_core::receive::{IncomingRequest, Outcome, ReceiveEvent, Receiver};
use tuxdrop_core::{DECISION_TIMEOUT, Node, SWEEP_DELAY, random_id};
use zbus::fdo;
use zbus::object_server::SignalEmitter;

use crate::desktop::FileManagerProxy;
use crate::notifications::{ACCEPT, ALWAYS, CANCEL, COPY, NotificationsProxy, OPEN};

const BUS_NAME: &str = "org.tuxdrop.Daemon";
const OBJECT_PATH: &str = "/org/tuxdrop/Daemon";
const PEER_TTL: Duration = Duration::from_secs(300);
/// A transfer notification is refreshed when its percentage grew this much,
/// and at most once per `PROGRESS_INTERVAL`.
const PROGRESS_STEP: u32 = 5;
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);
/// Longest message text shown in a notification; Copy copies all of it.
const MESSAGE_PREVIEW_CHARS: usize = 500;

struct Pending {
    request: IncomingRequest,
    reply: oneshot::Sender<bool>,
    notification: Option<u32>,
}

/// A transfer in progress, shown as a notification with a Cancel button.
struct Transfer {
    notification: u32,
    summary: String,
    total: u64,
    /// Bytes done per file name.
    done: HashMap<String, u64>,
    shown_percent: u32,
    shown_at: Instant,
    /// Files saved so far (incoming transfers), for "Open folder".
    saved: Vec<PathBuf>,
    /// Stops an outgoing transfer; incoming ones are cancelled by the receiver.
    cancel: Option<CancellationToken>,
    cancelled_here: bool,
}

impl Transfer {
    fn percent(&self) -> u32 {
        let done: u64 = self.done.values().sum();
        (done.saturating_mul(100))
            .checked_div(self.total)
            .unwrap_or(100)
            .min(100) as u32
    }
}

/// What the button of a notification that outlives its transfer does.
enum Followup {
    /// Shows these received items in the file manager.
    Select(Vec<PathBuf>),
    /// Copies a received message.
    Copy(String),
}

struct Shared {
    node: Node,
    receiver: Receiver,
    pending: Mutex<HashMap<String, Pending>>,
    peers: Mutex<HashMap<String, (Peer, Instant)>>,
    /// By transfer id: the session id for incoming ones, the id returned by
    /// `Send` for outgoing ones.
    transfers: Mutex<HashMap<String, Transfer>>,
    /// By notification id.
    followups: Mutex<HashMap<u32, Followup>>,
    /// Signalled when what the tray shows changes: a new device, a renamed
    /// one, a request pending or answered.
    tray_changed: Notify,
    notifier: NotificationsProxy<'static>,
    file_manager: FileManagerProxy<'static>,
    emitter: SignalEmitter<'static>,
}

impl Shared {
    /// Applies the user's choice; returns false if `id` is not pending.
    async fn decide(&self, id: &str, choice: &str) -> bool {
        let Some(pending) = self.pending.lock().unwrap().remove(id) else {
            return false;
        };
        self.tray_changed.notify_one();
        let request = &pending.request;
        let always = choice == ALWAYS && request.is_verified();
        if always {
            let sender = &request.sender;
            if let Err(e) = Node::trust_store().add(&sender.fingerprint, &sender.alias) {
                tracing::error!("cannot save trusted device: {e:#}");
            }
        }
        let accepted = choice == ACCEPT || always;
        if accepted {
            // Tracked before the sender is told, so no upload event comes first.
            // The request notification turns into the transfer's.
            self.start_transfer(
                id,
                pending.notification.unwrap_or(0),
                format!("Receiving from {}", sender_label(request)),
                &format!("{} file(s)", request.files.len()),
                request.total_size,
                None,
            )
            .await;
        }
        if pending.reply.send(accepted).is_err() && accepted {
            // Answered too late: the core already declined.
            let stale = self.transfers.lock().unwrap().remove(id);
            if let Some(t) = stale {
                let _ = self.notifier.close_notification(t.notification).await;
            }
        } else if let (false, Some(n)) = (accepted, pending.notification) {
            let _ = self.notifier.close_notification(n).await;
        }
        true
    }

    /// Starts tracking a transfer and shows it with a Cancel button,
    /// replacing notification `replaces` unless it is 0.
    async fn start_transfer(
        &self,
        id: &str,
        replaces: u32,
        summary: String,
        body: &str,
        total: u64,
        cancel: Option<CancellationToken>,
    ) {
        self.transfers.lock().unwrap().insert(
            id.to_owned(),
            Transfer {
                notification: replaces,
                summary: summary.clone(),
                total,
                done: HashMap::new(),
                shown_percent: 0,
                shown_at: Instant::now(),
                saved: Vec::new(),
                cancel,
                cancelled_here: false,
            },
        );
        let n = self
            .notifier
            .show(replaces, &summary, body, &[CANCEL, "Cancel"], Some(0))
            .await;
        if let Some(t) = self.transfers.lock().unwrap().get_mut(id) {
            t.notification = n;
        }
    }

    /// Records that `done` bytes of `file_name` were transferred and
    /// refreshes the notification now and then.
    async fn transfer_progress(&self, id: &str, file_name: &str, done: u64) {
        let (replaces, summary, percent) = {
            let mut transfers = self.transfers.lock().unwrap();
            let Some(t) = transfers.get_mut(id) else {
                return;
            };
            t.done.insert(file_name.to_owned(), done);
            let percent = t.percent();
            if percent < t.shown_percent + PROGRESS_STEP || t.shown_at.elapsed() < PROGRESS_INTERVAL
            {
                return;
            }
            t.shown_percent = percent;
            t.shown_at = Instant::now();
            (t.notification, t.summary.clone(), percent)
        };
        let body = format!("{percent}% · {file_name}");
        let n = self
            .notifier
            .show(
                replaces,
                &summary,
                &body,
                &[CANCEL, "Cancel"],
                Some(percent),
            )
            .await;
        if let Some(t) = self.transfers.lock().unwrap().get_mut(id) {
            t.notification = n;
        }
    }

    /// Cancels a transfer in progress; returns false if `id` is not one.
    async fn cancel(&self, id: &str) -> bool {
        let token = {
            let mut transfers = self.transfers.lock().unwrap();
            let Some(t) = transfers.get_mut(id) else {
                return false;
            };
            t.cancelled_here = true;
            t.cancel.clone()
        };
        match token {
            Some(token) => {
                token.cancel();
                true
            }
            None => self.receiver.cancel_local(id).await,
        }
    }

    /// Shows how something ended, replacing notification `replaces` unless
    /// it is 0; `followup` is what its button does.
    async fn conclude(&self, replaces: u32, summary: &str, body: &str, followup: Option<Followup>) {
        let actions: &[&str] = match &followup {
            Some(Followup::Select(_)) => &[OPEN, "Open folder"],
            Some(Followup::Copy(_)) => &[COPY, "Copy"],
            None => &[],
        };
        let n = self
            .notifier
            .show(replaces, summary, body, actions, None)
            .await;
        if let (Some(followup), true) = (followup, n != 0) {
            self.followups.lock().unwrap().insert(n, followup);
        }
    }

    /// A notification button was clicked.
    async fn on_action(&self, notification: u32, key: &str) {
        let pending = self
            .pending
            .lock()
            .unwrap()
            .iter()
            .find(|(_, p)| p.notification == Some(notification))
            .map(|(id, _)| id.clone());
        if let Some(id) = pending {
            self.decide(&id, key).await;
            return;
        }
        if key == CANCEL {
            let transfer = self
                .transfers
                .lock()
                .unwrap()
                .iter()
                .find(|(_, t)| t.notification == notification)
                .map(|(id, _)| id.clone());
            if let Some(id) = transfer {
                self.cancel(&id).await;
            }
            return;
        }
        let followup = self.followups.lock().unwrap().remove(&notification);
        match followup {
            Some(Followup::Select(items)) => self.file_manager.select(&items).await,
            Some(Followup::Copy(text)) => {
                if let Err(e) = desktop::copy_to_clipboard(&text).await {
                    tracing::warn!("cannot copy the message: {e:#}");
                    self.notifier
                        .inform(
                            0,
                            "Cannot copy the message",
                            "Copying needs wl-clipboard (sudo apt install wl-clipboard / \
                             sudo dnf install wl-clipboard)",
                        )
                        .await;
                }
            }
            None => {}
        }
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
        let names = Node::name_store().load();
        self.live_peers()
            .into_iter()
            .map(|(p, _)| p)
            .find(|p| p.matches(target, &names))
            .with_context(|| format!("device {target:?} not found"))
    }
}

/// How notifications call the sender: the local name only applies when the
/// TLS certificate proved the fingerprint, which anyone can claim otherwise.
fn sender_label(request: &IncomingRequest) -> String {
    let sender = &request.sender;
    if request.is_verified() {
        Node::name_store()
            .load()
            .label(&sender.alias, &sender.fingerprint)
    } else {
        sender.alias.clone()
    }
}

/// The entries of the download folder holding `saved` files: the files
/// themselves, or the top folder of a folder that was sent.
fn top_level(download_dir: &Path, saved: &[PathBuf]) -> Vec<PathBuf> {
    let mut items: Vec<PathBuf> = saved
        .iter()
        .filter_map(|path| {
            let first = path.strip_prefix(download_dir).ok()?.components().next()?;
            Some(download_dir.join(first))
        })
        .collect();
    items.sort();
    items.dedup();
    items
}

struct Api {
    shared: Arc<Shared>,
}

#[zbus::interface(name = "org.tuxdrop.Daemon1")]
impl Api {
    /// (alias, local name or "", address, fingerprint, "https"/"http",
    /// seconds since last seen) for devices seen in the last 5 minutes.
    fn list_peers(&self) -> Vec<(String, String, String, String, String, u32)> {
        let names = Node::name_store().load();
        self.shared
            .live_peers()
            .into_iter()
            .map(|(p, seen)| {
                let name = names.get(&p.info.fingerprint).unwrap_or("").to_owned();
                let addr = SocketAddr::new(p.addr, p.port).to_string();
                let age = seen.elapsed().as_secs() as u32;
                let protocol = p.protocol.scheme().to_owned();
                (p.info.alias, name, addr, p.info.fingerprint, protocol, age)
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

    /// Cancels a transfer in progress, incoming (request id) or outgoing
    /// (id returned by `Send`).
    async fn cancel(&self, id: &str) -> fdo::Result<()> {
        if self.shared.cancel(id).await {
            Ok(())
        } else {
            Err(fdo::Error::InvalidArgs(format!(
                "no transfer {id} in progress"
            )))
        }
    }

    /// Starts sending in the background; returns the transfer id used by
    /// the `Progress` and `TransferFinished` signals.
    async fn send(&self, target: &str, paths: Vec<String>) -> fdo::Result<String> {
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        let files = match client::collect_files(&paths) {
            Ok(files) => files,
            Err(e) => return Err(self.send_failed(fdo::Error::InvalidArgs, e).await),
        };
        let peer = match self.shared.find_peer(target).await {
            Ok(peer) => peer,
            Err(e) => return Err(self.send_failed(fdo::Error::Failed, e).await),
        };
        let id = random_id();
        let shared = self.shared.clone();
        let transfer_id = id.clone();
        tokio::spawn(async move {
            let alias = peer.info.alias.clone();
            let count = format!("{} file(s)", files.len());
            let cancel = CancellationToken::new();
            shared
                .start_transfer(
                    &transfer_id,
                    0,
                    format!("Sending to {alias}"),
                    &format!("{count}, waiting for {alias} to accept"),
                    files.iter().map(|f| f.meta.size).sum(),
                    Some(cancel.clone()),
                )
                .await;
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
            let transfer = client::send(req, progress, cancel.clone());
            tokio::pin!(transfer);
            let mut last_percent = u64::MAX;
            let result = loop {
                tokio::select! {
                    r = &mut transfer => break r,
                    // Matching `Progress` in the pattern would disable this
                    // branch at the first other event until the end.
                    Some(event) = rx.recv() => {
                        let SendEvent::Progress { file_name, sent, total } = event else {
                            continue;
                        };
                        let percent = (sent * 100).checked_div(total).unwrap_or(100);
                        if percent != last_percent {
                            last_percent = percent;
                            let _ = Api::progress(&shared.emitter, &transfer_id, &file_name, sent, total).await;
                        }
                        shared.transfer_progress(&transfer_id, &file_name, sent).await;
                    }
                }
            };
            let notification = shared
                .transfers
                .lock()
                .unwrap()
                .remove(&transfer_id)
                .map_or(0, |t| t.notification);
            let (summary, body, outcome) = match result {
                Ok(SendOutcome::Completed | SendOutcome::NothingToSend) => {
                    (format!("Sent to {alias}"), count, "completed".to_owned())
                }
                Err(_) if cancel.is_cancelled() => (
                    format!("Sending to {alias} cancelled"),
                    count,
                    "cancelled".to_owned(),
                ),
                Err(e) => (
                    format!("Sending to {alias} failed"),
                    format!("{e:#}"),
                    format!("failed: {e:#}"),
                ),
            };
            shared.conclude(notification, &summary, &body, None).await;
            let _ = Api::transfer_finished(&shared.emitter, &transfer_id, &outcome).await;
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

    /// Also shows `e` as a notification: callers such as the file manager
    /// menu have no other way to report it.
    async fn send_failed(&self, kind: fn(String) -> fdo::Error, e: anyhow::Error) -> fdo::Error {
        let message = format!("{e:#}");
        self.shared
            .notifier
            .inform(0, "Sending failed", &message)
            .await;
        kind(message)
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
    let (receiver, mut events) = node.start_receiver(peers_tx.clone())?;
    node.start_discovery(peers_tx)?;

    let conn = zbus::Connection::session()
        .await
        .context("cannot reach the D-Bus session bus (run tuxdropd inside a user session)")?;
    let notifier = NotificationsProxy::new(&conn).await?;
    let mut actions = notifier.receive_action_invoked().await?;
    let mut closed = notifier.receive_notification_closed().await?;
    tracing::info!(
        "receiving as {:?} ({}) into {}",
        node.me.alias,
        node.me.fingerprint,
        node.config.download_dir.display()
    );
    let shared = Arc::new(Shared {
        node,
        receiver,
        pending: Mutex::default(),
        peers: Mutex::default(),
        transfers: Mutex::default(),
        followups: Mutex::default(),
        tray_changed: Notify::new(),
        notifier,
        file_manager: FileManagerProxy::new(&conn).await?,
        emitter: SignalEmitter::new(&conn, OBJECT_PATH)?,
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

    let peers = shared.clone();
    tokio::spawn(async move {
        while let Some(peer) = peers_rx.recv().await {
            let previous = peers.peers.lock().unwrap().insert(
                peer.info.fingerprint.to_ascii_uppercase(),
                (peer, Instant::now()),
            );
            if previous.is_none() {
                peers.tray_changed.notify_one();
            }
        }
    });
    tokio::spawn(tray::run(shared.clone()));

    // One task, actions first: servers close a notification right after its
    // button is clicked, and the follow-up must not be dropped before it runs.
    let on_signal = shared.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                Some(signal) = actions.next() => {
                    if let Ok(args) = signal.args() {
                        on_signal.on_action(args.id, &args.action_key).await;
                    }
                }
                Some(signal) = closed.next() => {
                    if let Ok(args) = signal.args() {
                        on_signal.followups.lock().unwrap().remove(&args.id);
                    }
                }
                else => break,
            }
        }
    });

    while let Some(event) = events.recv().await {
        handle(&shared, event).await;
    }
    Ok(())
}

async fn handle(shared: &Arc<Shared>, event: ReceiveEvent) {
    match event {
        ReceiveEvent::Request { request, reply } => {
            let id = request.session_id.clone();
            let verified = request.is_verified();
            let summary = format!(
                "{} wants to send {} file(s)",
                sender_label(&request),
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
                &shared.emitter,
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
            shared.tray_changed.notify_one();
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
                if let Some(p) = expired {
                    shared.tray_changed.notify_one();
                    if let Some(n) = p.notification {
                        let _ = shared.notifier.close_notification(n).await;
                    }
                }
            });
        }
        ReceiveEvent::AutoAccepted { request } => {
            shared
                .start_transfer(
                    &request.session_id,
                    0,
                    format!("Receiving from {}", sender_label(&request)),
                    &format!("{} file(s) from a trusted device", request.files.len()),
                    request.total_size,
                    None,
                )
                .await;
        }
        ReceiveEvent::Message { request, text } => {
            tracing::info!("message from {}", request.sender.alias);
            let mut preview: String = text.chars().take(MESSAGE_PREVIEW_CHARS).collect();
            if preview.len() < text.len() {
                preview.push('…');
            }
            shared
                .conclude(
                    0,
                    &format!("Message from {}", sender_label(&request)),
                    &preview,
                    Some(Followup::Copy(text)),
                )
                .await;
        }
        ReceiveEvent::Progress {
            session_id,
            file_name,
            received,
            total,
        } => {
            let _ = Api::progress(&shared.emitter, &session_id, &file_name, received, total).await;
            shared
                .transfer_progress(&session_id, &file_name, received)
                .await;
        }
        ReceiveEvent::FileReceived { session_id, path } => {
            tracing::info!("saved {}", path.display());
            if let Some(t) = shared.transfers.lock().unwrap().get_mut(&session_id) {
                t.saved.push(path);
            }
        }
        ReceiveEvent::Finished {
            session_id,
            outcome,
        } => {
            let transfer = shared.transfers.lock().unwrap().remove(&session_id);
            let notification = transfer.as_ref().map_or(0, |t| t.notification);
            let download_dir = &shared.node.config.download_dir;
            let (summary, body, signal, followup) = match &outcome {
                Outcome::Completed => {
                    let items = transfer
                        .as_ref()
                        .map(|t| top_level(download_dir, &t.saved))
                        .unwrap_or_default();
                    (
                        "Files received",
                        format!("Saved in {}", download_dir.display()),
                        "completed".to_owned(),
                        (!items.is_empty()).then_some(Followup::Select(items)),
                    )
                }
                Outcome::Cancelled => {
                    let body = if transfer.as_ref().is_some_and(|t| t.cancelled_here) {
                        "You cancelled the transfer"
                    } else {
                        "The sender cancelled the transfer"
                    };
                    (
                        "Transfer cancelled",
                        body.to_owned(),
                        "cancelled".to_owned(),
                        None,
                    )
                }
                Outcome::Failed(reason) => (
                    "Transfer failed",
                    reason.clone(),
                    format!("failed: {reason}"),
                    None,
                ),
            };
            tracing::info!("transfer {session_id}: {signal}");
            shared
                .conclude(notification, summary, &body, followup)
                .await;
            let _ = Api::transfer_finished(&shared.emitter, &session_id, &signal).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_lists_each_received_entry_once() {
        let dl = Path::new("/home/me/Downloads/tuxDrop");
        let saved = [
            dl.join("trip/day1/a.jpg"),
            dl.join("trip/b.jpg"),
            dl.join("note (1).txt"),
            PathBuf::from("/elsewhere/x"),
        ];
        assert_eq!(
            top_level(dl, &saved),
            [dl.join("note (1).txt"), dl.join("trip")]
        );
    }
}
