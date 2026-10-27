//! tuxdropd: receives in the background, asks through desktop notifications
//! and the tray icon, and serves the control API ([`api`]):
//! `org.tuxdrop.Daemon1` on the session bus on Linux, a local socket on macOS
//! and Windows. Everything specific to a desktop lives in [`platform`].

// No console window on Windows.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

mod api;
mod notifications;
mod platform;
mod tray;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tuxdrop_core::client;
use tuxdrop_core::discovery::Peer;
use tuxdrop_core::receive::{IncomingRequest, Outcome, ReceiveEvent, Receiver};
use tuxdrop_core::{DECISION_TIMEOUT, Node, SWEEP_DELAY};

use crate::notifications::{ACCEPT, ALWAYS, COPY, NotificationEvent, OPEN};
use crate::platform::{FileManager, Notifier, Signals};

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
    /// one, a request pending or answered, a transfer's progress.
    tray_changed: Notify,
    notifier: Notifier,
    file_manager: FileManager,
    /// Tells API clients about requests and transfers.
    signals: Signals,
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
            if let Some(t) = self.end_transfer(id) {
                self.notifier.close(t.notification).await;
            }
        } else if let (false, Some(n)) = (accepted, pending.notification) {
            self.notifier.close(n).await;
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
        self.tray_changed.notify_one();
        let n = self.notifier.transfer(replaces, &summary, body, 0).await;
        if let Some(t) = self.transfers.lock().unwrap().get_mut(id) {
            t.notification = n;
        }
    }

    /// Stops tracking a transfer that ended.
    fn end_transfer(&self, id: &str) -> Option<Transfer> {
        let transfer = self.transfers.lock().unwrap().remove(id);
        self.tray_changed.notify_one();
        transfer
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
        self.tray_changed.notify_one();
        let body = format!("{percent}% · {file_name}");
        let n = self
            .notifier
            .transfer(replaces, &summary, &body, percent)
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
        let action = match &followup {
            Some(Followup::Select(_)) => Some([OPEN, "Open folder"]),
            Some(Followup::Copy(_)) => Some([COPY, "Copy"]),
            None => None,
        };
        let n = self
            .notifier
            .conclude(replaces, summary, body, action)
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
        if key == notifications::CANCEL {
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
                if let Err(e) = platform::copy_to_clipboard(&text).await {
                    tracing::warn!("cannot copy the message: {e:#}");
                    self.notifier
                        .inform(0, "Cannot copy the message", platform::CLIPBOARD_HELP)
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

fn main() -> Result<()> {
    let logs = tracing_subscriber::fmt().with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
    );
    #[cfg(windows)]
    {
        // The package logos: `tuxdropd --write-icon <size> <file.png>`.
        let args: Vec<String> = std::env::args().skip(1).collect();
        if let [flag, size, path] = args.as_slice()
            && flag == "--write-icon"
        {
            return platform::write_icon(size.parse()?, Path::new(path));
        }
        if platform::already_running() {
            return Ok(());
        }
        // A program without a console has no standard error to log to.
        if let Some(file) = platform::log_file() {
            logs.with_writer(Mutex::new(file)).with_ansi(false).init();
            return platform::run_main(run());
        }
    }
    logs.init();
    platform::run_main(run())
}

async fn run() -> Result<()> {
    let node = Node::load()?;
    let (peers_tx, mut peers_rx) = mpsc::channel(64);
    let (receiver, mut events) = node.start_receiver(peers_tx.clone())?;
    node.start_discovery(peers_tx)?;

    let platform::Session {
        notifier,
        file_manager,
        signals,
        events: mut notification_events,
        api,
    } = platform::Session::start().await?;
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
        file_manager,
        signals,
    });
    api.serve(shared.clone())
        .await
        .context("starting the control API")?;

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
    tokio::spawn(platform::tray::run(shared.clone()));

    // One task, in order: servers close a notification right after its
    // button is clicked, and the follow-up must not be dropped before it runs.
    let on_event = shared.clone();
    tokio::spawn(async move {
        while let Some(event) = notification_events.recv().await {
            match event {
                NotificationEvent::Action { id, key } => on_event.on_action(id, &key).await,
                NotificationEvent::Closed { id } => {
                    on_event.followups.lock().unwrap().remove(&id);
                }
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
            shared
                .signals
                .incoming_request(
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
                        shared.notifier.close(n).await;
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
            shared
                .signals
                .progress(&session_id, &file_name, received, total)
                .await;
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
            let transfer = shared.end_transfer(&session_id);
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
            shared.signals.transfer_finished(&session_id, &signal).await;
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
