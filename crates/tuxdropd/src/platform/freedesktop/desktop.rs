//! Showing files in the file manager, copying text to the clipboard and
//! asking the user through zenity dialogs.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::notifications::escape;

/// Implemented by Nautilus, Dolphin, Nemo and others.
#[zbus::proxy(
    interface = "org.freedesktop.FileManager1",
    default_service = "org.freedesktop.FileManager1",
    default_path = "/org/freedesktop/FileManager1"
)]
trait FileManager1 {
    fn show_folders(&self, uris: &[&str], startup_id: &str) -> zbus::Result<()>;
    fn show_items(&self, uris: &[&str], startup_id: &str) -> zbus::Result<()>;
}

pub struct FileManager(FileManager1Proxy<'static>);

impl FileManager {
    pub async fn new(conn: &zbus::Connection) -> zbus::Result<Self> {
        Ok(Self(FileManager1Proxy::new(conn).await?))
    }

    /// Opens `folder`.
    pub async fn open_folder(&self, folder: &Path) {
        if let Err(e) = self.0.show_folders(&[&file_uri(folder)], "").await {
            tracing::warn!("cannot open {}: {e}", folder.display());
        }
    }

    /// Opens the folders holding `items`, with the items selected.
    pub async fn select(&self, items: &[impl AsRef<Path>]) {
        let uris: Vec<String> = items.iter().map(|p| file_uri(p.as_ref())).collect();
        let uris: Vec<&str> = uris.iter().map(String::as_str).collect();
        if let Err(e) = self.0.show_items(&uris, "").await {
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

/// Asks for a line of text; `None` if cancelled.
pub async fn ask_text(prompt: &str, initial: &str) -> Result<Option<String>> {
    let output = Command::new("zenity")
        .args(["--entry", "--title=tuxDrop"])
        .arg(format!("--text={}", escape(prompt)))
        .arg(format!("--entry-text={initial}"))
        .output()
        .await
        .context("cannot run zenity")?;
    Ok(output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .trim_end_matches('\n')
            .to_owned()
    }))
}

/// Lets the user pick one of `labels`; its index, `None` if cancelled.
pub async fn pick_device(prompt: &str, labels: &[String]) -> Result<Option<usize>> {
    let output = Command::new("zenity")
        .args([
            "--list",
            "--title=tuxDrop",
            "--column=Device",
            "--hide-header",
        ])
        .arg(format!("--text={}", escape(prompt)))
        .args(labels)
        .output()
        .await
        .context("cannot run zenity")?;
    let picked = String::from_utf8_lossy(&output.stdout);
    let picked = picked.trim_end_matches('\n');
    Ok(labels.iter().position(|l| l == picked))
}

/// Lets the user pick files, or folders; `None` if cancelled.
pub async fn pick_paths(title: &str, folders: bool) -> Result<Option<Vec<PathBuf>>> {
    let mut command = Command::new("zenity");
    command
        .args(["--file-selection", "--multiple", "--separator=\n"])
        .arg(format!("--title={title}"));
    if folders {
        command.arg("--directory");
    }
    let output = command.output().await.context("cannot run zenity")?;
    Ok(output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect()
    }))
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
