//! Sending side of the LocalSend upload API.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::CONTENT_LENGTH;
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;

use crate::discovery::Peer;
use crate::identity::Identity;
use crate::proto::{
    API_PREFIX, DeviceInfo, FileMeta, PrepareUploadRequest, PrepareUploadResponse, Protocol,
};
use crate::{random_id, tls};

fn http_client(identity: &Identity, expected_fingerprint: Option<&str>) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .use_preconfigured_tls(tls::client_config(identity, expected_fingerprint)?)
        .connect_timeout(Duration::from_secs(5))
        .build()?)
}

/// Client pinned to the peer's announced fingerprint (HTTPS only).
fn peer_client(identity: &Identity, peer: &Peer) -> Result<reqwest::Client> {
    let expected = (peer.protocol == Protocol::Https).then_some(peer.info.fingerprint.as_str());
    http_client(identity, expected)
}

pub async fn register(identity: &Identity, me: &DeviceInfo, peer: &Peer) -> Result<DeviceInfo> {
    let resp = peer_client(identity, peer)?
        .post(peer.api_url("register"))
        .timeout(Duration::from_secs(3))
        .json(me)
        .send()
        .await?
        .error_for_status()?;
    Ok(resp.json().await?)
}

/// Parses `ip` or `ip:port` (default port); `None` if `target` is not an address.
pub fn parse_address(target: &str) -> Option<SocketAddr> {
    target.parse().ok().or_else(|| {
        target
            .parse::<std::net::IpAddr>()
            .ok()
            .map(|ip| SocketAddr::new(ip, crate::proto::DEFAULT_PORT))
    })
}

/// Resolves a peer given only its address (trust on first use: the
/// fingerprint it reports is then pinned for the transfer).
pub async fn resolve_address(
    identity: &Identity,
    me: &DeviceInfo,
    addr: SocketAddr,
) -> Result<Peer> {
    let info: DeviceInfo = http_client(identity, None)?
        .post(format!("https://{addr}{API_PREFIX}/register"))
        .timeout(Duration::from_secs(5))
        .json(me)
        .send()
        .await
        .with_context(|| format!("no tuxDrop/LocalSend device answering on {addr}"))?
        .error_for_status()?
        .json()
        .await?;
    Ok(Peer {
        info,
        addr: addr.ip().to_canonical(),
        port: addr.port(),
        protocol: Protocol::Https,
    })
}

pub struct OutgoingFile {
    pub meta: FileMeta,
    pub path: PathBuf,
}

/// Expands files and directories into upload entries; files inside a
/// directory keep their path relative to the directory's parent.
pub fn collect_files(paths: &[PathBuf]) -> Result<Vec<OutgoingFile>> {
    let mut out = Vec::new();
    for path in paths {
        let meta = std::fs::metadata(path).with_context(|| format!("{}", path.display()))?;
        if meta.is_dir() {
            let root = path.parent().unwrap_or(Path::new(""));
            for entry in walkdir::WalkDir::new(path).follow_links(false) {
                let entry = entry?;
                if entry.file_type().is_file() {
                    let rel = entry.path().strip_prefix(root)?;
                    let name = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/");
                    out.push(outgoing(entry.path(), name, entry.metadata()?.len()));
                }
            }
        } else {
            let name = path
                .file_name()
                .with_context(|| format!("{} has no file name", path.display()))?
                .to_string_lossy()
                .into_owned();
            out.push(outgoing(path, name, meta.len()));
        }
    }
    if out.is_empty() {
        bail!("nothing to send");
    }
    Ok(out)
}

fn outgoing(path: &Path, file_name: String, size: u64) -> OutgoingFile {
    let id = random_id();
    OutgoingFile {
        meta: FileMeta {
            id,
            file_type: mime_guess::from_path(path)
                .first_or_octet_stream()
                .to_string(),
            file_name,
            size,
            sha256: None,
            preview: None,
            metadata: None,
        },
        path: path.to_owned(),
    }
}

#[derive(Debug, Clone)]
pub enum SendEvent {
    Accepted,
    Progress {
        file_name: String,
        sent: u64,
        total: u64,
    },
    FileSent {
        file_name: String,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Completed,
    /// The receiver already has every file (HTTP 204).
    NothingToSend,
}

pub type ProgressFn = Arc<dyn Fn(SendEvent) + Send + Sync>;

pub struct SendRequest<'a> {
    pub identity: &'a Identity,
    pub me: &'a DeviceInfo,
    pub peer: &'a Peer,
    pub files: Vec<OutgoingFile>,
    pub pin: Option<&'a str>,
}

/// Runs a full upload session. Cancelling `cancel` notifies the receiver.
pub async fn send(
    req: SendRequest<'_>,
    progress: ProgressFn,
    cancel: CancellationToken,
) -> Result<SendOutcome> {
    let client = peer_client(req.identity, req.peer)?;
    let body = PrepareUploadRequest {
        info: req.me.clone(),
        files: req
            .files
            .iter()
            .map(|f| (f.meta.id.clone(), f.meta.clone()))
            .collect(),
    };
    let mut prepare = client.post(req.peer.api_url("prepare-upload")).json(&body);
    if let Some(pin) = req.pin {
        prepare = prepare.query(&[("pin", pin)]);
    }
    // The receiver waits for its user to decide, so no response timeout here.
    let resp = tokio::select! {
        r = prepare.send() => r?,
        () = cancel.cancelled() => bail!("cancelled"),
    };
    let session: PrepareUploadResponse = match resp.status() {
        StatusCode::OK => resp.json().await?,
        StatusCode::NO_CONTENT => return Ok(SendOutcome::NothingToSend),
        StatusCode::UNAUTHORIZED => bail!("receiver requires a PIN, or the PIN is wrong"),
        StatusCode::FORBIDDEN => bail!("transfer declined by the receiver"),
        StatusCode::CONFLICT => bail!("receiver is busy with another transfer"),
        StatusCode::TOO_MANY_REQUESTS => bail!("too many requests, retry in a few seconds"),
        s => bail!("receiver answered {s}"),
    };
    progress(SendEvent::Accepted);

    let tokens: HashMap<_, _> = session.files;
    for file in &req.files {
        let Some(token) = tokens.get(&file.meta.id) else {
            continue;
        };
        let upload = upload_file(
            &client,
            req.peer,
            &session.session_id,
            file,
            token,
            &progress,
        );
        let result = tokio::select! {
            r = upload => r,
            () = cancel.cancelled() => Err(anyhow::anyhow!("cancelled")),
        };
        if let Err(e) = result {
            let _ = client
                .post(req.peer.api_url("cancel"))
                .query(&[("sessionId", &session.session_id)])
                .timeout(Duration::from_secs(3))
                .send()
                .await;
            return Err(e);
        }
        progress(SendEvent::FileSent {
            file_name: file.meta.file_name.clone(),
        });
    }
    Ok(SendOutcome::Completed)
}

async fn upload_file(
    client: &reqwest::Client,
    peer: &Peer,
    session_id: &str,
    file: &OutgoingFile,
    token: &str,
    progress: &ProgressFn,
) -> Result<()> {
    let reader = tokio::fs::File::open(&file.path)
        .await
        .with_context(|| format!("{}", file.path.display()))?;
    let total = file.meta.size;
    let name = file.meta.file_name.clone();
    let progress = progress.clone();
    let mut sent = 0u64;
    let stream = ReaderStream::with_capacity(reader, 256 * 1024).inspect(move |chunk| {
        if let Ok(bytes) = chunk {
            sent += bytes.len() as u64;
            progress(SendEvent::Progress {
                file_name: name.clone(),
                sent,
                total,
            });
        }
    });
    let resp = client
        .post(peer.api_url("upload"))
        .query(&[
            ("sessionId", session_id),
            ("fileId", file.meta.id.as_str()),
            ("token", token),
        ])
        .header(CONTENT_LENGTH, total)
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await?;
    match resp.status() {
        StatusCode::OK | StatusCode::NO_CONTENT => Ok(()),
        StatusCode::UNPROCESSABLE_ENTITY => bail!("{}: checksum mismatch", file.meta.file_name),
        StatusCode::CONFLICT => bail!("transfer cancelled by the receiver"),
        s => bail!("{}: receiver answered {s}", file.meta.file_name),
    }
}
