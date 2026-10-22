//! End-to-end transfers between two in-process nodes over loopback.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tuxdrop_core::client::{self, OutgoingFile, SendEvent, SendOutcome, SendRequest};
use tuxdrop_core::discovery::Peer;
use tuxdrop_core::identity::Identity;
use tuxdrop_core::proto::DeviceInfo;
use tuxdrop_core::receive::{Outcome, ReceiveEvent, Receiver, ReceiverSettings};
use tuxdrop_core::trust::TrustStore;
use tuxdrop_core::{local_device_info, server};

#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Request { verified: bool },
    AutoAccepted,
    File(PathBuf),
    Finished(Outcome),
}

struct Rx {
    peer: Peer,
    dir: tempfile::TempDir,
    log: Arc<Mutex<Vec<Seen>>>,
}

impl Rx {
    fn downloads(&self) -> PathBuf {
        self.dir.path().join("downloads")
    }

    async fn wait_for(&self, pred: impl Fn(&[Seen]) -> bool) {
        for _ in 0..100 {
            if pred(&self.log.lock().unwrap()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("condition not reached, log: {:?}", self.log.lock().unwrap());
    }
}

struct Tx {
    identity: Identity,
    me: DeviceInfo,
}

impl Tx {
    fn new() -> Self {
        let identity = Identity::generate().unwrap();
        let me = local_device_info("sender", &identity.fingerprint, 0);
        Self { identity, me }
    }

    async fn send(
        &self,
        rx: &Rx,
        files: Vec<OutgoingFile>,
        pin: Option<&str>,
    ) -> anyhow::Result<SendOutcome> {
        self.send_with(rx, files, pin, Arc::new(|_| {}), CancellationToken::new())
            .await
    }

    async fn send_with(
        &self,
        rx: &Rx,
        files: Vec<OutgoingFile>,
        pin: Option<&str>,
        progress: client::ProgressFn,
        cancel: CancellationToken,
    ) -> anyhow::Result<SendOutcome> {
        let req = SendRequest {
            identity: &self.identity,
            me: &self.me,
            peer: &rx.peer,
            files,
            pin,
        };
        client::send(req, progress, cancel).await
    }
}

/// Starts a receiver whose user always answers `accept` (after `delay`).
async fn start_rx(accept: bool, delay: Duration, trusted: Option<&str>, pin: Option<&str>) -> Rx {
    let dir = tempfile::tempdir().unwrap();
    let trust = TrustStore::new(dir.path().join("trusted.json"));
    if let Some(fp) = trusted {
        trust.add(fp, "sender").unwrap();
    }
    let identity = Identity::generate().unwrap();
    let listener = server::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
    let port = listener.local_addr().unwrap().port();
    let me = local_device_info("receiver", &identity.fingerprint, port);
    let (events_tx, mut events) = mpsc::channel(64);
    let receiver = Receiver::new(
        ReceiverSettings {
            download_dir: dir.path().join("downloads"),
            pin: pin.map(str::to_owned),
            trust,
            decision_timeout: Duration::from_secs(5),
        },
        events_tx,
    );
    let (peers_tx, _peers) = mpsc::channel(8);
    let served_me = me.clone();
    tokio::spawn(async move {
        let _keep = _peers;
        server::serve(listener, &identity, served_me, receiver, peers_tx)
            .await
            .unwrap();
    });

    let log = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            let seen = match event {
                ReceiveEvent::Request { request, reply } => {
                    let verified = request.is_verified();
                    tokio::spawn(async move {
                        tokio::time::sleep(delay).await;
                        let _ = reply.send(accept);
                    });
                    Seen::Request { verified }
                }
                ReceiveEvent::AutoAccepted { .. } => Seen::AutoAccepted,
                ReceiveEvent::FileReceived { path, .. } => Seen::File(path),
                ReceiveEvent::Finished { outcome, .. } => Seen::Finished(outcome),
                ReceiveEvent::Progress { .. } => continue,
            };
            sink.lock().unwrap().push(seen);
        }
    });

    Rx {
        peer: Peer::new(me, IpAddr::V4(Ipv4Addr::LOCALHOST)),
        dir,
        log,
    }
}

fn source_tree(root: &Path) -> Vec<PathBuf> {
    std::fs::write(root.join("a.txt"), "hello").unwrap();
    let folder = root.join("folder");
    std::fs::create_dir_all(folder.join("sub")).unwrap();
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(folder.join("sub/big.bin"), &big).unwrap();
    vec![root.join("a.txt"), folder]
}

fn no_part_files(dir: &Path) -> bool {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .all(|e| !e.file_name().to_string_lossy().ends_with(".part"))
}

#[tokio::test]
async fn accepted_transfer_with_folder() {
    let rx = start_rx(true, Duration::ZERO, None, None).await;
    let tx = Tx::new();
    let src = tempfile::tempdir().unwrap();
    let files = client::collect_files(&source_tree(src.path())).unwrap();
    assert_eq!(
        tx.send(&rx, files, None).await.unwrap(),
        SendOutcome::Completed
    );
    rx.wait_for(|log| log.contains(&Seen::Finished(Outcome::Completed)))
        .await;

    let log = rx.log.lock().unwrap().clone();
    assert_eq!(log[0], Seen::Request { verified: true });
    let dl = rx.downloads();
    assert_eq!(std::fs::read_to_string(dl.join("a.txt")).unwrap(), "hello");
    assert_eq!(
        std::fs::read(dl.join("folder/sub/big.bin")).unwrap(),
        std::fs::read(src.path().join("folder/sub/big.bin")).unwrap()
    );
    assert!(no_part_files(&dl));

    // Same names again: kept side by side, nothing overwritten.
    let files = client::collect_files(&[src.path().join("a.txt")]).unwrap();
    tx.send(&rx, files, None).await.unwrap();
    rx.wait_for(|log| {
        log.iter()
            .filter(|s| matches!(s, Seen::Finished(_)))
            .count()
            == 2
    })
    .await;
    assert!(dl.join("a (1).txt").exists());
}

#[tokio::test]
async fn declined_then_rate_limited() {
    let rx = start_rx(false, Duration::ZERO, None, None).await;
    let tx = Tx::new();
    let src = tempfile::tempdir().unwrap();
    let paths = source_tree(src.path());
    let err = tx
        .send(&rx, client::collect_files(&paths).unwrap(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("declined"), "{err}");
    let err = tx
        .send(&rx, client::collect_files(&paths).unwrap(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("too many"), "{err}");
}

#[tokio::test]
async fn trusted_sender_is_auto_accepted() {
    let tx = Tx::new();
    let rx = start_rx(false, Duration::ZERO, Some(&tx.identity.fingerprint), None).await;
    let src = tempfile::tempdir().unwrap();
    let files = client::collect_files(&source_tree(src.path())).unwrap();
    tx.send(&rx, files, None).await.unwrap();
    rx.wait_for(|log| log.contains(&Seen::Finished(Outcome::Completed)))
        .await;
    let log = rx.log.lock().unwrap();
    assert_eq!(log[0], Seen::AutoAccepted);
    assert!(!log.iter().any(|s| matches!(s, Seen::Request { .. })));
}

#[tokio::test]
async fn spoofed_fingerprint_is_not_trusted() {
    let trusted = Tx::new();
    let mut attacker = Tx::new();
    attacker.me.fingerprint = trusted.identity.fingerprint.clone();
    let rx = start_rx(
        false,
        Duration::ZERO,
        Some(&trusted.identity.fingerprint),
        None,
    )
    .await;
    let src = tempfile::tempdir().unwrap();
    let files = client::collect_files(&source_tree(src.path())).unwrap();
    assert!(attacker.send(&rx, files, None).await.is_err());
    let log = rx.log.lock().unwrap();
    assert_eq!(log[0], Seen::Request { verified: false });
}

#[tokio::test]
async fn checksum_mismatch_is_rejected_and_cleaned() {
    let rx = start_rx(true, Duration::ZERO, None, None).await;
    let tx = Tx::new();
    let src = tempfile::tempdir().unwrap();
    let mut files = client::collect_files(&source_tree(src.path())[..1]).unwrap();
    files[0].meta.sha256 = Some("00".repeat(32));
    let err = tx.send(&rx, files, None).await.unwrap_err();
    assert!(err.to_string().contains("checksum"), "{err}");
    rx.wait_for(|log| matches!(log.last(), Some(Seen::Finished(Outcome::Failed(_)))))
        .await;
    assert!(!rx.downloads().join("a.txt").exists());
    assert!(no_part_files(&rx.downloads()));
}

#[tokio::test]
async fn path_traversal_is_refused_before_asking() {
    let rx = start_rx(true, Duration::ZERO, None, None).await;
    let tx = Tx::new();
    let src = tempfile::tempdir().unwrap();
    let mut files = client::collect_files(&source_tree(src.path())[..1]).unwrap();
    files[0].meta.file_name = "../../evil.txt".into();
    let err = tx.send(&rx, files, None).await.unwrap_err();
    assert!(err.to_string().contains("400"), "{err}");
    assert!(rx.log.lock().unwrap().is_empty());
    assert!(!rx.dir.path().join("evil.txt").exists());
}

#[tokio::test]
async fn sender_cancel_cleans_partial_file() {
    let rx = start_rx(true, Duration::ZERO, None, None).await;
    let tx = Tx::new();
    let src = tempfile::tempdir().unwrap();
    let big = src.path().join("huge.bin");
    std::fs::write(&big, vec![7u8; 16 * 1024 * 1024]).unwrap();
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let progress: client::ProgressFn = Arc::new(move |e| {
        if matches!(e, SendEvent::Progress { sent, .. } if sent > 1024 * 1024) {
            trigger.cancel();
        }
    });
    let files = client::collect_files(&[big]).unwrap();
    assert!(
        tx.send_with(&rx, files, None, progress, cancel)
            .await
            .is_err()
    );
    rx.wait_for(|log| log.contains(&Seen::Finished(Outcome::Cancelled)))
        .await;
    let dl = rx.downloads();
    for _ in 0..100 {
        if no_part_files(&dl) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(no_part_files(&dl));
    assert!(!dl.join("huge.bin").exists());
}

#[tokio::test]
async fn second_sender_gets_busy_while_first_is_pending() {
    let rx = Arc::new(start_rx(true, Duration::from_millis(500), None, None).await);
    let src = tempfile::tempdir().unwrap();
    let paths = source_tree(src.path());
    let first = {
        let rx = rx.clone();
        let paths = paths.clone();
        tokio::spawn(async move {
            Tx::new()
                .send(&rx, client::collect_files(&paths).unwrap(), None)
                .await
        })
    };
    rx.wait_for(|log| !log.is_empty()).await;
    let err = Tx::new()
        .send(&rx, client::collect_files(&paths).unwrap(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("busy"), "{err}");
    assert_eq!(first.await.unwrap().unwrap(), SendOutcome::Completed);
}

#[tokio::test]
async fn pin_is_enforced() {
    let rx = start_rx(true, Duration::ZERO, None, Some("4242")).await;
    let tx = Tx::new();
    let src = tempfile::tempdir().unwrap();
    let paths = source_tree(src.path());
    let err = tx
        .send(&rx, client::collect_files(&paths).unwrap(), Some("0000"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("PIN"), "{err}");

    let rx = start_rx(true, Duration::ZERO, None, Some("4242")).await;
    tx.send(&rx, client::collect_files(&paths).unwrap(), Some("4242"))
        .await
        .unwrap();
}

#[tokio::test]
async fn resolve_by_address_pins_reported_fingerprint() {
    let rx = start_rx(true, Duration::ZERO, None, None).await;
    let tx = Tx::new();
    let addr = SocketAddr::new(rx.peer.addr, rx.peer.port);
    let peer = client::resolve_address(&tx.identity, &tx.me, addr)
        .await
        .unwrap();
    assert_eq!(peer.info.fingerprint, rx.peer.info.fingerprint);
}
