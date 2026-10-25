//! Receiving side: one upload session at a time, user approval, and safe
//! writing into the download directory.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};

use crate::identity::same_fingerprint;
use crate::proto::{
    DeviceInfo, FileMeta, PrepareUploadRequest, PrepareUploadResponse, UploadQuery,
};
use crate::random_id;
use crate::sys::{available_space, rename_noreplace};
use crate::trust::TrustStore;

const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const REFUSAL_COOLDOWN: Duration = Duration::from_secs(5);
/// Bounds how often one address can show a message to the user.
const MESSAGE_COOLDOWN: Duration = Duration::from_secs(2);
const PROGRESS_STEP: u64 = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct IncomingRequest {
    pub session_id: String,
    pub sender: DeviceInfo,
    pub addr: IpAddr,
    /// Fingerprint of the TLS client certificate, if the sender presented one.
    pub verified_fingerprint: Option<String>,
    pub files: Vec<FileMeta>,
    pub total_size: u64,
}

impl IncomingRequest {
    /// The sender proved ownership of the fingerprint it announces.
    pub fn is_verified(&self) -> bool {
        self.verified_fingerprint
            .as_deref()
            .is_some_and(|fp| same_fingerprint(fp, &self.sender.fingerprint))
    }
}

#[derive(Debug)]
pub enum ReceiveEvent {
    /// Needs a decision: send `true` to accept. Dropping `reply` or letting
    /// the decision timeout expire rejects the transfer.
    Request {
        request: IncomingRequest,
        reply: oneshot::Sender<bool>,
    },
    /// Sent by a trusted device: accepted without asking.
    AutoAccepted {
        request: IncomingRequest,
    },
    /// A text message (LocalSend "message"): nothing is uploaded or saved.
    Message {
        request: IncomingRequest,
        text: String,
    },
    Progress {
        session_id: String,
        file_name: String,
        received: u64,
        total: u64,
    },
    FileReceived {
        session_id: String,
        path: PathBuf,
    },
    Finished {
        session_id: String,
        outcome: Outcome,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Cancelled,
    Failed(String),
}

pub struct ReceiverSettings {
    pub download_dir: PathBuf,
    pub pin: Option<String>,
    pub trust: TrustStore,
    pub decision_timeout: Duration,
}

#[derive(PartialEq, Eq)]
enum FileState {
    Pending,
    InProgress,
    Done,
}

struct SessionFile {
    meta: FileMeta,
    rel_path: PathBuf,
    token: String,
    state: FileState,
}

struct Session {
    id: String,
    sender_ip: IpAddr,
    files: HashMap<String, SessionFile>,
    cancelled: Arc<AtomicBool>,
    last_activity: Instant,
}

#[derive(Default)]
enum Slot {
    #[default]
    Idle,
    /// A `prepare-upload` is waiting for the user's decision.
    Deciding,
    Active(Session),
}

#[derive(Default)]
struct State {
    slot: Slot,
    /// Addresses refused until the given instant.
    cooldown: HashMap<IpAddr, Instant>,
}

struct Inner {
    settings: ReceiverSettings,
    state: Mutex<State>,
    events: mpsc::Sender<ReceiveEvent>,
}

#[derive(Clone)]
pub struct Receiver {
    inner: Arc<Inner>,
}

/// Returns the slot to `Idle` if the decision is abandoned, including when the
/// sender disconnects and the request future is dropped.
struct DecisionGuard<'a> {
    inner: &'a Inner,
}

impl Drop for DecisionGuard<'_> {
    fn drop(&mut self) {
        let mut st = lock(&self.inner.state);
        if matches!(st.slot, Slot::Deciding) {
            st.slot = Slot::Idle;
        }
    }
}

struct Refusal {
    status: StatusCode,
    cooldown: bool,
}

impl From<StatusCode> for Refusal {
    fn from(status: StatusCode) -> Self {
        Self {
            status,
            cooldown: false,
        }
    }
}

struct UploadJob {
    session_id: String,
    meta: FileMeta,
    rel_path: PathBuf,
    token: String,
    cancelled: Arc<AtomicBool>,
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Receiver {
    pub fn new(settings: ReceiverSettings, events: mpsc::Sender<ReceiveEvent>) -> Self {
        Self {
            inner: Arc::new(Inner {
                settings,
                state: Mutex::default(),
                events,
            }),
        }
    }

    pub async fn prepare_upload(
        &self,
        addr: IpAddr,
        verified_fingerprint: Option<String>,
        pin: Option<String>,
        req: PrepareUploadRequest,
    ) -> Result<Option<PrepareUploadResponse>, StatusCode> {
        self.reserve(addr)?;
        let guard = DecisionGuard { inner: &self.inner };
        match self.decide(addr, verified_fingerprint, pin, req).await {
            Ok(None) => {
                lock(&self.inner.state)
                    .cooldown
                    .insert(addr, Instant::now() + MESSAGE_COOLDOWN);
                Ok(None)
            }
            Ok(Some(session)) => {
                let response = PrepareUploadResponse {
                    session_id: session.id.clone(),
                    files: session
                        .files
                        .iter()
                        .map(|(id, f)| (id.clone(), f.token.clone()))
                        .collect(),
                };
                lock(&self.inner.state).slot = Slot::Active(session);
                drop(guard);
                Ok(Some(response))
            }
            Err(refusal) => {
                if refusal.cooldown {
                    lock(&self.inner.state)
                        .cooldown
                        .insert(addr, Instant::now() + REFUSAL_COOLDOWN);
                }
                Err(refusal.status)
            }
        }
    }

    fn reserve(&self, addr: IpAddr) -> Result<(), StatusCode> {
        let mut st = lock(&self.inner.state);
        let now = Instant::now();
        st.cooldown.retain(|_, until| *until > now);
        if st.cooldown.contains_key(&addr) {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        if let Slot::Active(s) = &st.slot
            && s.last_activity.elapsed() > SESSION_IDLE_TIMEOUT
        {
            s.cancelled.store(true, Ordering::Relaxed);
            let _ = self.inner.events.try_send(ReceiveEvent::Finished {
                session_id: s.id.clone(),
                outcome: Outcome::Failed("sender stopped responding".into()),
            });
            st.slot = Slot::Idle;
        }
        if !matches!(st.slot, Slot::Idle) {
            return Err(StatusCode::CONFLICT);
        }
        st.slot = Slot::Deciding;
        Ok(())
    }

    async fn decide(
        &self,
        addr: IpAddr,
        verified_fingerprint: Option<String>,
        pin: Option<String>,
        req: PrepareUploadRequest,
    ) -> Result<Option<Session>, Refusal> {
        let settings = &self.inner.settings;
        if let Some(expected) = &settings.pin
            && pin.as_deref() != Some(expected.as_str())
        {
            return Err(Refusal {
                status: StatusCode::UNAUTHORIZED,
                cooldown: true,
            });
        }
        if req.files.is_empty() {
            return Err(StatusCode::BAD_REQUEST.into());
        }
        if let Some(text) = message_text(&req.files) {
            let request = IncomingRequest {
                session_id: random_id(),
                sender: req.info,
                addr,
                verified_fingerprint,
                files: req.files.into_values().collect(),
                total_size: 0,
            };
            let _ = self
                .inner
                .events
                .send(ReceiveEvent::Message { request, text })
                .await;
            return Ok(None);
        }
        let mut files = HashMap::new();
        let mut total_size = 0u64;
        for (id, meta) in req.files {
            let rel_path = sanitize_relative_path(&meta.file_name).ok_or_else(|| {
                tracing::warn!(
                    "rejecting unsafe file name {:?} from {addr}",
                    meta.file_name
                );
                StatusCode::BAD_REQUEST
            })?;
            total_size = total_size
                .checked_add(meta.size)
                .ok_or(StatusCode::BAD_REQUEST)?;
            let token = random_id();
            files.insert(
                id,
                SessionFile {
                    meta,
                    rel_path,
                    token,
                    state: FileState::Pending,
                },
            );
        }
        let free = std::fs::create_dir_all(&settings.download_dir)
            .and_then(|()| available_space(&settings.download_dir))
            .map_err(|e| {
                tracing::error!("download dir {}: {e}", settings.download_dir.display());
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        if total_size > free {
            return Err(StatusCode::INSUFFICIENT_STORAGE.into());
        }

        let mut metas: Vec<FileMeta> = files.values().map(|f| f.meta.clone()).collect();
        metas.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        let request = IncomingRequest {
            session_id: random_id(),
            sender: req.info,
            addr,
            verified_fingerprint,
            files: metas,
            total_size,
        };
        let trusted = request.is_verified()
            && request
                .verified_fingerprint
                .as_deref()
                .is_some_and(|fp| settings.trust.is_trusted(fp));
        let session_id = request.session_id.clone();
        let accepted = if trusted {
            let _ = self
                .inner
                .events
                .send(ReceiveEvent::AutoAccepted { request })
                .await;
            true
        } else {
            let (reply, decision) = oneshot::channel();
            self.inner
                .events
                .send(ReceiveEvent::Request { request, reply })
                .await
                .is_ok()
                && matches!(
                    tokio::time::timeout(settings.decision_timeout, decision).await,
                    Ok(Ok(true))
                )
        };
        if !accepted {
            return Err(Refusal {
                status: StatusCode::FORBIDDEN,
                cooldown: true,
            });
        }
        Ok(Some(Session {
            id: session_id,
            sender_ip: addr,
            files,
            cancelled: Arc::default(),
            last_activity: Instant::now(),
        }))
    }

    pub async fn upload<S, E>(
        &self,
        addr: IpAddr,
        q: UploadQuery,
        body: S,
    ) -> Result<(), StatusCode>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        let job = self.start_upload(addr, &q)?;
        let result = self.write_file(&job, body).await;
        let mut events = Vec::new();
        {
            let mut st = lock(&self.inner.state);
            if let Slot::Active(s) = &mut st.slot
                && s.id == job.session_id
            {
                s.last_activity = Instant::now();
                match &result {
                    Ok(path) => {
                        if let Some(f) = s.files.get_mut(&q.file_id) {
                            f.state = FileState::Done;
                        }
                        events.push(ReceiveEvent::FileReceived {
                            session_id: s.id.clone(),
                            path: path.clone(),
                        });
                        if s.files.values().all(|f| f.state == FileState::Done) {
                            events.push(ReceiveEvent::Finished {
                                session_id: s.id.clone(),
                                outcome: Outcome::Completed,
                            });
                            st.slot = Slot::Idle;
                        }
                    }
                    Err(status) => {
                        s.cancelled.store(true, Ordering::Relaxed);
                        events.push(ReceiveEvent::Finished {
                            session_id: s.id.clone(),
                            outcome: Outcome::Failed(format!("{}: {status}", job.meta.file_name)),
                        });
                        st.slot = Slot::Idle;
                    }
                }
            }
        }
        for event in events {
            let _ = self.inner.events.send(event).await;
        }
        result.map(|_| ())
    }

    fn start_upload(&self, addr: IpAddr, q: &UploadQuery) -> Result<UploadJob, StatusCode> {
        let mut st = lock(&self.inner.state);
        let Slot::Active(s) = &mut st.slot else {
            return Err(StatusCode::FORBIDDEN);
        };
        if s.id != q.session_id {
            return Err(StatusCode::CONFLICT);
        }
        if s.sender_ip != addr {
            return Err(StatusCode::FORBIDDEN);
        }
        let file = s
            .files
            .get_mut(&q.file_id)
            .filter(|f| f.token == q.token)
            .ok_or(StatusCode::FORBIDDEN)?;
        if file.state != FileState::Pending {
            return Err(StatusCode::CONFLICT);
        }
        file.state = FileState::InProgress;
        s.last_activity = Instant::now();
        Ok(UploadJob {
            session_id: s.id.clone(),
            meta: file.meta.clone(),
            rel_path: file.rel_path.clone(),
            token: file.token.clone(),
            cancelled: s.cancelled.clone(),
        })
    }

    async fn write_file<S, E>(&self, job: &UploadJob, mut body: S) -> Result<PathBuf, StatusCode>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        let internal = |e: io::Error| {
            tracing::error!("writing {}: {e}", job.meta.file_name);
            StatusCode::INTERNAL_SERVER_ERROR
        };
        let base = &self.inner.settings.download_dir;
        let dir = ensure_dir(base, job.rel_path.parent()).map_err(internal)?;
        let file_name = job.rel_path.file_name().ok_or(StatusCode::BAD_REQUEST)?;
        let part = dir.join(format!(".tuxdrop-{}.part", job.token));
        let mut out = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)
            .await
            .map_err(internal)?;

        let written: Result<(), StatusCode> = async {
            let mut hasher = job.meta.sha256.as_ref().map(|_| Sha256::new());
            let mut received = 0u64;
            let mut reported = 0u64;
            while let Some(chunk) = body.next().await {
                let chunk = chunk.map_err(|e| {
                    tracing::warn!("upload of {} interrupted: {e}", job.meta.file_name);
                    StatusCode::BAD_REQUEST
                })?;
                if job.cancelled.load(Ordering::Relaxed) {
                    return Err(StatusCode::CONFLICT);
                }
                received += chunk.len() as u64;
                if received > job.meta.size {
                    return Err(StatusCode::BAD_REQUEST);
                }
                if let Some(h) = &mut hasher {
                    h.update(&chunk);
                }
                out.write_all(&chunk).await.map_err(internal)?;
                if received - reported >= PROGRESS_STEP || received == job.meta.size {
                    reported = received;
                    self.report_progress(job, received);
                }
            }
            if received != job.meta.size {
                return Err(StatusCode::BAD_REQUEST);
            }
            if let (Some(h), Some(expected)) = (hasher, &job.meta.sha256)
                && !hex::encode(h.finalize()).eq_ignore_ascii_case(expected)
            {
                return Err(StatusCode::UNPROCESSABLE_ENTITY);
            }
            out.sync_data().await.map_err(internal)?;
            Ok(())
        }
        .await;
        drop(out);

        let result = written.and_then(|()| move_unique(&part, &dir, file_name).map_err(internal));
        if result.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        result
    }

    fn report_progress(&self, job: &UploadJob, received: u64) {
        if let Slot::Active(s) = &mut lock(&self.inner.state).slot
            && s.id == job.session_id
        {
            s.last_activity = Instant::now();
        }
        // Progress is best effort: never stall the upload on a slow consumer.
        let _ = self.inner.events.try_send(ReceiveEvent::Progress {
            session_id: job.session_id.clone(),
            file_name: job.meta.file_name.clone(),
            received,
            total: job.meta.size,
        });
    }

    /// The sender's `cancel` request.
    pub async fn cancel(&self, addr: IpAddr, session_id: &str) -> Result<(), StatusCode> {
        if self.end_session(session_id, Some(addr)).await {
            Ok(())
        } else {
            Err(StatusCode::FORBIDDEN)
        }
    }

    /// Cancels the active session from this side: the file being received is
    /// deleted and the sender's next request fails. Returns false if
    /// `session_id` is not active.
    pub async fn cancel_local(&self, session_id: &str) -> bool {
        self.end_session(session_id, None).await
    }

    /// Ends the active session `session_id` as cancelled, if it comes from
    /// `sender` (any address when `None`).
    async fn end_session(&self, session_id: &str, sender: Option<IpAddr>) -> bool {
        let event = {
            let mut st = lock(&self.inner.state);
            match &st.slot {
                Slot::Active(s)
                    if s.id == session_id && sender.is_none_or(|addr| s.sender_ip == addr) =>
                {
                    s.cancelled.store(true, Ordering::Relaxed);
                    let event = ReceiveEvent::Finished {
                        session_id: s.id.clone(),
                        outcome: Outcome::Cancelled,
                    };
                    st.slot = Slot::Idle;
                    event
                }
                _ => return false,
            }
        };
        let _ = self.inner.events.send(event).await;
        true
    }
}

/// LocalSend sends a text message as a single text file whose content is
/// its `preview`; there is nothing to upload.
fn message_text(files: &HashMap<String, FileMeta>) -> Option<String> {
    let mut files = files.values();
    match (files.next(), files.next()) {
        (Some(file), None) if file.file_type == "text" || file.file_type.starts_with("text/") => {
            file.preview.clone()
        }
        _ => None,
    }
}

/// Turns a sender-supplied name (possibly `a/b/c.txt`) into a relative path
/// that cannot escape the download directory. Returns `None` if unsafe.
pub fn sanitize_relative_path(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in name.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.len() > 255 || part.chars().any(char::is_control) {
            return None;
        }
        out.push(part);
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Creates `rel` below `base` one component at a time, refusing to traverse
/// anything that is not a real directory (e.g. a pre-existing symlink).
fn ensure_dir(base: &Path, rel: Option<&Path>) -> io::Result<PathBuf> {
    let mut dir = base.to_path_buf();
    for component in rel.into_iter().flat_map(Path::components) {
        dir.push(component);
        match std::fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if !std::fs::symlink_metadata(&dir)?.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("{} exists and is not a directory", dir.display()),
                    ));
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(dir)
}

/// Renames `part` to `dir/name`, or `name (1).ext`… if taken. Never
/// overwrites an existing file.
fn move_unique(part: &Path, dir: &Path, name: &OsStr) -> io::Result<PathBuf> {
    let name = Path::new(name);
    let stem = name.file_stem().unwrap_or_default().to_string_lossy();
    let ext = name.extension().map(|e| e.to_string_lossy());
    for i in 0..1000 {
        let candidate = match (i, &ext) {
            (0, _) => dir.join(name),
            (_, Some(ext)) => dir.join(format!("{stem} ({i}).{ext}")),
            (_, None) => dir.join(format!("{stem} ({i})")),
        };
        match rename_noreplace(part, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free destination name",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rejects_traversal_and_keeps_folders() {
        assert_eq!(
            sanitize_relative_path("photo.jpg"),
            Some("photo.jpg".into())
        );
        assert_eq!(
            sanitize_relative_path("dir/sub/a.txt"),
            Some("dir/sub/a.txt".into())
        );
        assert_eq!(
            sanitize_relative_path("/etc/passwd"),
            Some("etc/passwd".into())
        );
        assert_eq!(sanitize_relative_path("a\\b.txt"), Some("a/b.txt".into()));
        assert_eq!(sanitize_relative_path("../x"), None);
        assert_eq!(sanitize_relative_path("a/../../x"), None);
        assert_eq!(sanitize_relative_path(""), None);
        assert_eq!(sanitize_relative_path("./"), None);
        assert_eq!(sanitize_relative_path("bad\nname"), None);
        assert_eq!(sanitize_relative_path(&"x".repeat(256)), None);
    }

    #[test]
    fn move_unique_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "old").unwrap();
        std::fs::write(dir.path().join("a (1).txt"), "old").unwrap();
        let part = dir.path().join(".p");
        std::fs::write(&part, "new").unwrap();
        let dest = move_unique(&part, dir.path(), OsStr::new("a.txt")).unwrap();
        assert_eq!(dest, dir.path().join("a (2).txt"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "old"
        );
        assert!(!part.exists());
    }

    #[cfg(unix)]
    #[test]
    fn ensure_dir_refuses_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        assert!(ensure_dir(dir.path(), Some(Path::new("link/x"))).is_err());
        let made = ensure_dir(dir.path(), Some(Path::new("a/b"))).unwrap();
        assert!(made.is_dir());
    }
}
