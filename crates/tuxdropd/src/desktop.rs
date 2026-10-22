//! Showing files in the file manager and copying text to the clipboard.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Implemented by Nautilus, Dolphin, Nemo and others.
#[zbus::proxy(
    interface = "org.freedesktop.FileManager1",
    default_service = "org.freedesktop.FileManager1",
    default_path = "/org/freedesktop/FileManager1"
)]
pub trait FileManager {
    fn show_folders(&self, uris: &[&str], startup_id: &str) -> zbus::Result<()>;
    fn show_items(&self, uris: &[&str], startup_id: &str) -> zbus::Result<()>;
}

impl FileManagerProxy<'_> {
    /// Opens `folder`.
    pub async fn open_folder(&self, folder: &Path) {
        if let Err(e) = self.show_folders(&[&file_uri(folder)], "").await {
            tracing::warn!("cannot open {}: {e}", folder.display());
        }
    }

    /// Opens the folders holding `items`, with the items selected.
    pub async fn select(&self, items: &[impl AsRef<Path>]) {
        let uris: Vec<String> = items.iter().map(|p| file_uri(p.as_ref())).collect();
        let uris: Vec<&str> = uris.iter().map(String::as_str).collect();
        if let Err(e) = self.show_items(&uris, "").await {
            tracing::warn!("cannot show received files: {e}");
        }
    }
}

/// `file://` URI of an absolute path, percent-encoded.
pub fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

/// Copies `text` with wl-copy (Wayland) or xclip (X11); both keep serving
/// the clipboard in the background after this returns.
pub async fn copy_to_clipboard(text: &str) -> Result<()> {
    let (program, args): (&str, &[&str]) = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        ("wl-copy", &[])
    } else {
        ("xclip", &["-selection", "clipboard"])
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .with_context(|| format!("cannot run {program}"))?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    stdin.write_all(text.as_bytes()).await?;
    drop(stdin);
    let status = child.wait().await?;
    if !status.success() {
        bail!("{program} failed ({status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uri_escapes_reserved_and_non_ascii() {
        assert_eq!(
            file_uri(Path::new("/home/me/Téléchargements/a b#1.txt")),
            "file:///home/me/T%C3%A9l%C3%A9chargements/a%20b%231.txt"
        );
    }
}
