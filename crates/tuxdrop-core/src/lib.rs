//! tuxDrop core: LocalSend v2 protocol (discovery, HTTPS upload API) with
//! fingerprint-pinned TLS identities.

pub mod client;
pub mod config;
pub mod discovery;
pub mod identity;
pub mod proto;
pub mod receive;
pub mod server;
pub mod tls;
pub mod trust;

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::mpsc;

use crate::config::{Config, data_dir};
use crate::discovery::{Discovery, Peer};
use crate::identity::Identity;
use crate::proto::{DeviceInfo, DeviceType, PROTOCOL_VERSION, Protocol};
use crate::receive::{ReceiveEvent, Receiver, ReceiverSettings};
use crate::trust::TrustStore;

/// How long an incoming transfer waits for the user before being declined.
pub const DECISION_TIMEOUT: Duration = Duration::from_secs(60);

/// How long to wait for answers to a multicast announcement before falling
/// back to [`Node::sweep`].
pub const SWEEP_DELAY: Duration = Duration::from_millis(1500);

/// 128-bit random identifier, hex encoded (session ids, tokens, file ids).
pub fn random_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS random generator unavailable");
    hex::encode(bytes)
}

pub fn local_device_info(alias: &str, fingerprint: &str, port: u16) -> DeviceInfo {
    DeviceInfo {
        alias: alias.to_owned(),
        version: PROTOCOL_VERSION.to_owned(),
        device_model: Some("Linux".to_owned()),
        device_type: Some(DeviceType::Desktop),
        fingerprint: fingerprint.to_owned(),
        port: Some(port),
        protocol: Some(Protocol::Https),
        download: false,
    }
}

/// This device: configuration, identity and how it presents itself.
pub struct Node {
    pub config: Config,
    pub identity: Arc<Identity>,
    pub me: DeviceInfo,
}

impl Node {
    pub fn load() -> Result<Self> {
        let config = Config::load()?;
        let identity = Arc::new(Identity::load_or_create(&data_dir())?);
        let me = local_device_info(&config.alias, &identity.fingerprint, config.port);
        Ok(Self {
            config,
            identity,
            me,
        })
    }

    pub fn trust_store() -> TrustStore {
        TrustStore::new(data_dir().join("trusted.json"))
    }

    /// Listens for announcements and multicasts our own presence once.
    pub fn start_discovery(&self, peers: mpsc::Sender<Peer>) -> Result<()> {
        let discovery = Discovery::bind(self.config.port, self.identity.clone(), self.me.clone())
            .context("binding the discovery port")?;
        tokio::spawn(discovery.run(peers));
        self.announce()
    }

    /// Asks nearby devices to answer; they show up on the discovery channel.
    pub fn announce(&self) -> Result<()> {
        discovery::multicast(&self.me, true, self.config.port)
    }

    /// Unicast fallback when [`Node::announce`] got no answer after
    /// [`SWEEP_DELAY`]: Wi-Fi often drops multicast.
    pub fn sweep(&self) {
        match discovery::sweep(&self.me, self.config.port) {
            Ok(n) => tracing::debug!("no answer to multicast, announced to {n} addresses"),
            Err(e) => tracing::warn!("unicast sweep failed: {e:#}"),
        }
    }

    /// Serves the upload API on every interface; transfer events arrive on
    /// the returned channel.
    pub fn start_receiver(
        &self,
        peers: mpsc::Sender<Peer>,
    ) -> Result<mpsc::Receiver<ReceiveEvent>> {
        let listener = server::bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, self.config.port)))
            .with_context(|| {
                format!(
                    "port {} is taken (is tuxdropd or LocalSend running?)",
                    self.config.port
                )
            })?;
        let (events_tx, events) = mpsc::channel(64);
        let receiver = Receiver::new(
            ReceiverSettings {
                download_dir: self.config.download_dir.clone(),
                pin: self.config.pin.clone(),
                trust: Self::trust_store(),
                decision_timeout: DECISION_TIMEOUT,
            },
            events_tx,
        );
        let identity = self.identity.clone();
        let me = self.me.clone();
        tokio::spawn(async move {
            if let Err(e) = server::serve(listener, &identity, me, receiver, peers).await {
                tracing::error!("server stopped: {e:#}");
            }
        });
        Ok(events)
    }
}
