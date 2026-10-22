//! Device identity: a long-lived self-signed certificate whose SHA-256 is the
//! LocalSend fingerprint.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};

pub struct Identity {
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
    pub fingerprint: String,
}

impl Clone for Identity {
    fn clone(&self) -> Self {
        Self {
            cert: self.cert.clone(),
            key: self.key.clone_key(),
            fingerprint: self.fingerprint.clone(),
        }
    }
}

impl Identity {
    /// Loads `cert.pem`/`key.pem` from `dir`, generating them on first use.
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");
        if !cert_path.exists() || !key_path.exists() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            let generated = rcgen::generate_simple_self_signed(vec!["tuxdrop".to_string()])?;
            fs::write(&cert_path, generated.cert.pem())?;
            fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&key_path)?
                .write_all(generated.key_pair.serialize_pem().as_bytes())?;
        }
        let cert = rustls_pemfile::certs(&mut fs::read(&cert_path)?.as_slice())
            .next()
            .context("no certificate in cert.pem")??;
        let key = rustls_pemfile::private_key(&mut fs::read(&key_path)?.as_slice())?
            .context("no private key in key.pem")?;
        Ok(Self::from_der(cert, key))
    }

    /// Fresh in-memory identity (tests, ephemeral use).
    pub fn generate() -> Result<Self> {
        let generated = rcgen::generate_simple_self_signed(vec!["tuxdrop".to_string()])?;
        let key = PrivateKeyDer::try_from(generated.key_pair.serialize_der())
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(Self::from_der(generated.cert.der().clone(), key))
    }

    fn from_der(cert: CertificateDer<'static>, key: PrivateKeyDer<'static>) -> Self {
        let fingerprint = fingerprint_of(&cert);
        Self {
            cert,
            key,
            fingerprint,
        }
    }
}

/// Uppercase hex SHA-256 of a DER certificate.
pub fn fingerprint_of(cert: &[u8]) -> String {
    hex::encode_upper(Sha256::digest(cert))
}

pub fn same_fingerprint(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// A complete fingerprint: 64 hex digits, any case.
pub fn is_fingerprint(text: &str) -> bool {
    text.len() == 64 && text.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_identity_with_private_key_mode() {
        let dir = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(dir.path()).unwrap();
        let b = Identity::load_or_create(dir.path()).unwrap();
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_eq!(a.fingerprint.len(), 64);
        let mode = fs::metadata(dir.path().join("key.pem")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(mode.permissions().mode() & 0o777, 0o600);
    }
}
