use std::collections::HashMap;
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tuxdrop_core::Node;
use tuxdrop_core::client::{self, SendEvent, SendOutcome, SendRequest};
use tuxdrop_core::discovery::Peer;
use tuxdrop_core::receive::{IncomingRequest, Outcome, ReceiveEvent};

mod daemon;

#[derive(Parser)]
#[command(
    version,
    about = "Send and receive files with nearby devices (LocalSend compatible)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show this device's alias, fingerprint and download directory.
    Info,
    /// List nearby devices.
    Peers {
        /// Seconds to listen for answers.
        #[arg(short, long, default_value_t = 3)]
        timeout: u64,
    },
    /// Send files or folders to a device (alias, fingerprint prefix or IP[:port]).
    Send {
        target: String,
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// PIN required by the receiver.
        #[arg(long)]
        pin: Option<String>,
        /// Seconds to search for the target.
        #[arg(short, long, default_value_t = 5)]
        timeout: u64,
    },
    /// Receive in the foreground (when tuxdropd is not running).
    Recv,
    /// Pending incoming transfers and nearby devices known to tuxdropd.
    Status,
    /// Accept a pending incoming transfer (tuxdropd).
    Accept { id: String },
    /// Decline a pending incoming transfer (tuxdropd).
    Reject { id: String },
    /// Accept transfers from this device without asking.
    Trust {
        fingerprint: String,
        #[arg(long, default_value = "")]
        alias: String,
    },
    /// Stop trusting a device.
    Untrust { fingerprint: String },
    /// List trusted devices.
    Trusted,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    match Cli::parse().command {
        Command::Info => {
            let node = Node::load()?;
            println!("alias:        {}", node.me.alias);
            println!("fingerprint:  {}", node.me.fingerprint);
            println!("port:         {}", node.config.port);
            println!("download dir: {}", node.config.download_dir.display());
            println!(
                "PIN:          {}",
                if node.config.pin.is_some() {
                    "set"
                } else {
                    "none"
                }
            );
        }
        Command::Peers { timeout } => peers(timeout).await?,
        Command::Send {
            target,
            paths,
            pin,
            timeout,
        } => send(&target, &paths, pin.as_deref(), timeout).await?,
        Command::Recv => recv().await?,
        Command::Status => daemon::status().await?,
        Command::Accept { id } => daemon::decide(&id, true).await?,
        Command::Reject { id } => daemon::decide(&id, false).await?,
        Command::Trust { fingerprint, alias } => {
            Node::trust_store().add(&fingerprint, &alias)?;
            println!("trusted {fingerprint}");
        }
        Command::Untrust { fingerprint } => {
            if !Node::trust_store().remove(&fingerprint)? {
                bail!("{fingerprint} was not trusted");
            }
        }
        Command::Trusted => {
            for d in Node::trust_store().list()? {
                println!("{}  {}", d.fingerprint, d.alias);
            }
        }
    }
    Ok(())
}

fn describe(peer: &Peer) -> String {
    format!(
        "{:<24} {:<40} {}",
        peer.info.alias,
        SocketAddr::new(peer.addr, peer.port),
        &peer.info.fingerprint[..peer.info.fingerprint.len().min(16)]
    )
}

async fn peers(timeout: u64) -> Result<()> {
    let node = Node::load()?;
    let (tx, mut rx) = mpsc::channel(32);
    node.start_discovery(tx)?;
    let mut seen = HashMap::new();
    let deadline = tokio::time::sleep(Duration::from_secs(timeout));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            Some(peer) = rx.recv() => {
                if seen.insert(peer.info.fingerprint.clone(), ()).is_none() {
                    println!("{}", describe(&peer));
                }
            }
            () = &mut deadline => break,
        }
    }
    if seen.is_empty() {
        eprintln!("no device found");
    }
    Ok(())
}

async fn find_peer(node: &Node, target: &str, timeout: u64) -> Result<Peer> {
    if let Some(addr) = client::parse_address(target) {
        return client::resolve_address(&node.identity, &node.me, addr).await;
    }
    let (tx, mut rx) = mpsc::channel(32);
    node.start_discovery(tx)?;
    let search = async {
        while let Some(peer) = rx.recv().await {
            if peer.matches(target) {
                return Some(peer);
            }
        }
        None
    };
    match tokio::time::timeout(Duration::from_secs(timeout), search).await {
        Ok(Some(peer)) => Ok(peer),
        _ => bail!("device {target:?} not found (try `tuxdrop peers`)"),
    }
}

async fn send(target: &str, paths: &[PathBuf], pin: Option<&str>, timeout: u64) -> Result<()> {
    let node = Node::load()?;
    let files = client::collect_files(paths)?;
    let total: u64 = files.iter().map(|f| f.meta.size).sum();
    let peer = find_peer(&node, target, timeout).await?;
    eprintln!(
        "sending {} file(s), {} to {}; waiting for approval…",
        files.len(),
        human_size(total),
        peer.info.alias
    );

    let last_percent = Arc::new(Mutex::new((String::new(), u64::MAX)));
    let progress: client::ProgressFn = Arc::new(move |event| match event {
        SendEvent::Accepted => eprintln!("accepted"),
        SendEvent::Progress {
            file_name,
            sent,
            total,
        } => {
            let percent = (sent * 100).checked_div(total).unwrap_or(100);
            let mut last = last_percent.lock().unwrap();
            if last.0 != file_name || last.1 != percent {
                *last = (file_name.clone(), percent);
                eprint!("\r{file_name}: {percent:>3}%");
                let _ = std::io::stderr().flush();
            }
        }
        SendEvent::FileSent { file_name } => eprintln!("\r{file_name}: done"),
    });

    let cancel = CancellationToken::new();
    let on_ctrl_c = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            on_ctrl_c.cancel();
        }
    });
    let req = SendRequest {
        identity: &node.identity,
        me: &node.me,
        peer: &peer,
        files,
        pin,
    };
    match client::send(req, progress, cancel).await? {
        SendOutcome::Completed => eprintln!("transfer complete"),
        SendOutcome::NothingToSend => eprintln!("the receiver already has these files"),
    }
    Ok(())
}

pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn print_request(request: &IncomingRequest) {
    eprintln!(
        "\n{} ({}) wants to send {} file(s), {}:",
        request.sender.alias,
        request.addr,
        request.files.len(),
        human_size(request.total_size)
    );
    for file in request.files.iter().take(10) {
        eprintln!("  {}  ({})", file.file_name, human_size(file.size));
    }
    if request.files.len() > 10 {
        eprintln!("  … and {} more", request.files.len() - 10);
    }
    if request.is_verified() {
        eprintln!("  verified fingerprint {}", request.sender.fingerprint);
    } else {
        eprintln!("  identity not verified (LocalSend app or unknown device)");
    }
}

async fn recv() -> Result<()> {
    let node = Node::load()?;
    let (peers_tx, mut peers) = mpsc::channel(32);
    let mut events = node.start_receiver(peers_tx.clone())?;
    node.start_discovery(peers_tx)?;
    tokio::spawn(async move { while peers.recv().await.is_some() {} });
    eprintln!(
        "receiving as {:?} into {} (Ctrl+C to stop)",
        node.me.alias,
        node.config.download_dir.display()
    );

    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    while let Some(event) = events.recv().await {
        match event {
            ReceiveEvent::Request { request, reply } => {
                print_request(&request);
                let verified = request.is_verified();
                if verified {
                    eprint!("Accept? [y]es / [N]o / [a]lways from this device: ");
                } else {
                    eprint!("Accept? [y/N]: ");
                }
                let _ = std::io::stderr().flush();
                let answer = tokio::time::timeout(Duration::from_secs(55), stdin.next_line()).await;
                let answer = match answer {
                    Ok(Ok(Some(line))) => line.trim().to_ascii_lowercase(),
                    _ => {
                        eprintln!("\nno answer, declined");
                        String::new()
                    }
                };
                if verified && answer == "a" {
                    Node::trust_store().add(&request.sender.fingerprint, &request.sender.alias)?;
                }
                let _ = reply
                    .send(matches!(answer.as_str(), "y" | "yes") || (verified && answer == "a"));
            }
            ReceiveEvent::AutoAccepted { request } => {
                eprintln!(
                    "\naccepted {} file(s) from trusted device {}",
                    request.files.len(),
                    request.sender.alias
                );
            }
            ReceiveEvent::Progress {
                file_name,
                received,
                total,
                ..
            } => {
                let percent = (received * 100).checked_div(total).unwrap_or(100);
                eprint!("\r{file_name}: {percent:>3}%");
                let _ = std::io::stderr().flush();
            }
            ReceiveEvent::FileReceived { path, .. } => eprintln!("\rsaved {}", path.display()),
            ReceiveEvent::Finished { outcome, .. } => match outcome {
                Outcome::Completed => eprintln!("transfer complete"),
                Outcome::Cancelled => eprintln!("\ntransfer cancelled by the sender"),
                Outcome::Failed(reason) => eprintln!("\ntransfer failed: {reason}"),
            },
        }
    }
    Ok(())
}
