//! macOS: AppleScript dialogs and notifications (osascript), Finder (open),
//! clipboard (pbcopy). None of them needs a permission from the user.
//!
//! Notification banners cannot carry buttons without a signed app using the
//! UserNotifications framework, so whatever needs an answer is a dialog: an
//! incoming request, a received message (Copy). Transfers in progress are in
//! the menu bar menu rather than in banners.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use tuxdrop_core::DECISION_TIMEOUT;

use crate::notifications::{ACCEPT, ALWAYS, COPY, NotificationEvent, REJECT};

pub const CLIPBOARD_HELP: &str = "pbcopy failed, see the tuxdropd log";
pub const DIALOG_HELP: &str = "osascript failed, see the tuxdropd log";

/// How long a received message stays on screen.
const MESSAGE_TIMEOUT_SECS: u64 = 600;

/// Runs an AppleScript whose `run` handler gets `args` as `argv`, so they
/// never need quoting; returns its trimmed result, or `None` if the user
/// cancelled (error -128).
async fn osascript(script: &str, args: &[&str]) -> Result<Option<String>> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("cannot run osascript")?;
    if output.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&output.stdout)
                .trim_end_matches('\n')
                .to_owned(),
        ));
    }
    let error = String::from_utf8_lossy(&output.stderr);
    if error.contains("(-128)") {
        return Ok(None);
    }
    bail!("osascript: {}", error.trim())
}

/// Shows a dialog; returns the button clicked, `None` if it timed out.
/// `buttons` are given left to right, the last one is the default.
async fn dialog(text: &str, buttons: &[&str], timeout_secs: u64) -> Result<Option<String>> {
    const SCRIPT: &str = r#"on run argv
    activate
    set answer to display dialog (item 1 of argv) with title "tuxDrop" buttons (items 3 thru -1 of argv) default button (count of argv) - 2 giving up after (item 2 of argv as integer) with icon note
    if gave up of answer then return ""
    return button returned of answer
end run"#;
    let timeout = timeout_secs.to_string();
    let mut args = vec![text, timeout.as_str()];
    args.extend_from_slice(buttons);
    Ok(osascript(SCRIPT, &args)
        .await?
        .filter(|button| !button.is_empty()))
}

/// A banner in Notification Center.
fn banner(summary: &str, body: &str) {
    const SCRIPT: &str = r#"on run argv
    display notification (item 2 of argv) with title "tuxDrop" subtitle (item 1 of argv)
end run"#;
    let (summary, body) = (summary.to_owned(), body.to_owned());
    tokio::spawn(async move {
        if let Err(e) = osascript(SCRIPT, &[&summary, &body]).await {
            tracing::warn!("notification failed: {e:#}");
        }
    });
}

/// A blocking alert, for when the daemon stops.
pub fn alert(summary: &str, body: &str) {
    let _ = std::process::Command::new("osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "display alert (item 1 of argv) message (item 2 of argv) as critical",
            "-e",
            "end run",
            summary,
            body,
        ])
        .status();
}

/// Shows the daemon's notifications and dialogs; clicked buttons are
/// reported as `NotificationEvent`s.
pub struct Notifier {
    next_id: AtomicU32,
    /// Open dialogs, closed by sending on (or dropping) their sender.
    dialogs: Arc<Mutex<HashMap<u32, oneshot::Sender<()>>>>,
    events: mpsc::UnboundedSender<NotificationEvent>,
}

impl Notifier {
    pub fn new(events: mpsc::UnboundedSender<NotificationEvent>) -> Self {
        Self {
            next_id: AtomicU32::new(1),
            dialogs: Arc::default(),
            events,
        }
    }

    /// Shows a dialog in the background. Its buttons are `[key, label]`,
    /// the last one being the default; clicking reports an action unless
    /// the key is empty, and the dialog then reports being closed.
    fn open_dialog(&self, text: String, buttons: Vec<[&'static str; 2]>, timeout_secs: u64) -> u32 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (close, closed) = oneshot::channel();
        self.dialogs.lock().unwrap().insert(id, close);
        let events = self.events.clone();
        let dialogs = self.dialogs.clone();
        tokio::spawn(async move {
            let labels: Vec<&str> = buttons.iter().map(|[_, label]| *label).collect();
            // Dropping the dialog kills osascript.
            let clicked = tokio::select! {
                answer = dialog(&text, &labels, timeout_secs) => answer,
                _ = closed => return,
            };
            dialogs.lock().unwrap().remove(&id);
            match clicked {
                Ok(Some(label)) => {
                    let key = buttons.iter().find(|[_, l]| *l == label).map(|[k, _]| *k);
                    if let Some(key) = key.filter(|k| !k.is_empty()) {
                        let _ = events.send(NotificationEvent::Action {
                            id,
                            key: key.to_owned(),
                        });
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("dialog failed: {e:#}"),
            }
            let _ = events.send(NotificationEvent::Closed { id });
        });
        id
    }

    /// Accept / Decline (/ Always) dialog.
    pub async fn ask(&self, summary: &str, body: &str, offer_always: bool) -> Result<u32> {
        let mut buttons = vec![[REJECT, "Decline"]];
        if offer_always {
            buttons.push([ALWAYS, "Always Accept"]);
        }
        buttons.push([ACCEPT, "Accept"]);
        let text = format!("{summary}\n\n{body}");
        Ok(self.open_dialog(text, buttons, DECISION_TIMEOUT.as_secs()))
    }

    /// A transfer in progress: a banner when it starts, unless it answers a
    /// request (`replaces` is then that request's dialog); the menu bar menu
    /// shows the progress.
    pub async fn transfer(&self, replaces: u32, summary: &str, body: &str, percent: u32) -> u32 {
        if replaces != 0 {
            self.close(replaces).await;
        } else if percent == 0 {
            banner(summary, body);
        }
        0
    }

    /// How something ended. A message is shown in a dialog: banners cut long
    /// text and cannot carry the Copy button. Other buttons are dropped.
    pub async fn conclude(
        &self,
        replaces: u32,
        summary: &str,
        body: &str,
        action: Option<[&str; 2]>,
    ) -> u32 {
        self.close(replaces).await;
        if action.is_some_and(|[key, _]| key == COPY) {
            let text = format!("{summary}\n\n{body}");
            return self.open_dialog(
                text,
                vec![["", "Close"], [COPY, "Copy"]],
                MESSAGE_TIMEOUT_SECS,
            );
        }
        banner(summary, body);
        0
    }

    pub async fn inform(&self, replaces: u32, summary: &str, body: &str) -> u32 {
        self.close(replaces).await;
        banner(summary, body);
        0
    }

    /// Closes dialog `id` if it is still open.
    pub async fn close(&self, id: u32) {
        let close = self.dialogs.lock().unwrap().remove(&id);
        if let Some(close) = close {
            let _ = close.send(());
        }
    }
}

/// The bundle running this daemon, `None` outside one (`cargo run`).
fn app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.parent()?.parent()?.parent()?;
    (app.extension()? == "app").then(|| app.to_owned())
}

/// Starting at login: the LaunchAgent that scripts/install-macos.sh writes
/// too, for an app installed from the disk image.
pub mod login {
    use std::path::{Path, PathBuf};

    use anyhow::{Context, Result};

    const LABEL: &str = "org.tuxdrop.tuxdropd";
    const TEMPLATE: &str =
        include_str!("../../../../../packaging/macos/org.tuxdrop.tuxdropd.plist");

    fn agent() -> Option<PathBuf> {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        Some(home.join(format!("Library/LaunchAgents/{LABEL}.plist")))
    }

    /// `None` when not running from tuxDrop.app.
    pub fn enabled() -> Option<bool> {
        super::app_bundle()?;
        Some(agent()?.exists())
    }

    /// Takes effect at the next login: loading the agent now would start a
    /// second daemon.
    pub fn set(enabled: bool) -> Result<()> {
        let agent = agent().context("HOME is not set")?;
        if !enabled {
            return match std::fs::remove_file(&agent) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            };
        }
        let app = super::app_bundle().context("not running from tuxDrop.app")?;
        let home = agent
            .ancestors()
            .nth(3)
            .context("unexpected LaunchAgents path")?;
        let log = home.join("Library/Logs/tuxdrop.log");
        std::fs::create_dir_all(agent.parent().context("no parent")?)?;
        std::fs::write(&agent, render(&app, &log))?;
        Ok(())
    }

    fn render(app: &Path, log: &Path) -> String {
        let escape = |p: &Path| {
            p.to_string_lossy()
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        };
        TEMPLATE
            .replace("@APP@", &escape(app))
            .replace("@LOG@", &escape(log))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn agent_is_a_valid_plist() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("agent.plist");
            let app = Path::new("/Users/me/Apps & Tools/tuxDrop.app");
            std::fs::write(&file, render(app, Path::new("/Users/me/tuxdrop.log"))).unwrap();
            let lint = std::process::Command::new("plutil")
                .arg("-lint")
                .arg(&file)
                .output()
                .unwrap();
            assert!(lint.status.success(), "{lint:?}");
            let program = std::process::Command::new("plutil")
                .args(["-extract", "ProgramArguments.0", "raw", "-o", "-"])
                .arg(&file)
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8_lossy(&program.stdout).trim(),
                "/Users/me/Apps & Tools/tuxDrop.app/Contents/MacOS/tuxdropd"
            );
        }
    }
}

/// Makes the Share menu entry available right after the app is copied from
/// the disk image, without overriding a user who turned it off.
pub async fn integrate() {
    const SHARE: &str = "org.tuxdrop.tuxDrop.share";
    let Some(app) = app_bundle() else {
        return;
    };
    let extension = app.join("Contents/PlugIns/tuxDropShare.appex");
    if !extension.exists() {
        return;
    }
    let pluginkit = |args: &[&std::ffi::OsStr]| {
        let mut command = Command::new("pluginkit");
        command
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        async move { command.output().await.ok() }
    };
    let _ = pluginkit(&["-a".as_ref(), extension.as_os_str()]).await;
    // "+" in use, "-" turned off by the user, " " never chosen.
    let listed = pluginkit(&["-m".as_ref(), "-i".as_ref(), SHARE.as_ref()]).await;
    let never_chosen = listed.is_some_and(|out| out.stdout.starts_with(b" "));
    if never_chosen {
        let _ = pluginkit(&["-e".as_ref(), "use".as_ref(), "-i".as_ref(), SHARE.as_ref()]).await;
    }
}

/// Finder.
pub struct FileManager;

impl FileManager {
    /// Opens `folder`.
    pub async fn open_folder(&self, folder: &Path) {
        open(&[folder.as_os_str()], false).await;
    }

    /// Opens the folders holding `items`, with the items selected.
    pub async fn select(&self, items: &[impl AsRef<Path>]) {
        let items: Vec<_> = items.iter().map(|p| p.as_ref().as_os_str()).collect();
        open(&items, true).await;
    }
}

async fn open(paths: &[&std::ffi::OsStr], reveal: bool) {
    let mut command = Command::new("open");
    if reveal {
        command.arg("-R");
    }
    match command.args(paths).status().await {
        Ok(status) if status.success() => {}
        Ok(status) => tracing::warn!("open failed ({status})"),
        Err(e) => tracing::warn!("cannot run open: {e}"),
    }
}

pub async fn copy_to_clipboard(text: &str) -> Result<()> {
    let mut child = Command::new("pbcopy")
        // pbcopy reads the locale's encoding; launchd sets no locale.
        .env("LANG", "en_US.UTF-8")
        .stdin(Stdio::piped())
        .spawn()
        .context("cannot run pbcopy")?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    stdin.write_all(text.as_bytes()).await?;
    drop(stdin);
    let status = child.wait().await?;
    if !status.success() {
        bail!("pbcopy failed ({status})");
    }
    Ok(())
}

/// Asks for a line of text; `None` if cancelled.
pub async fn ask_text(prompt: &str, initial: &str) -> Result<Option<String>> {
    const SCRIPT: &str = r#"on run argv
    activate
    return text returned of (display dialog (item 1 of argv) with title "tuxDrop" default answer (item 2 of argv) buttons {"Cancel", "OK"} default button "OK" cancel button "Cancel")
end run"#;
    osascript(SCRIPT, &[prompt, initial]).await
}

/// Lets the user pick one of `labels`; its index, `None` if cancelled.
pub async fn pick_device(prompt: &str, labels: &[String]) -> Result<Option<usize>> {
    const SCRIPT: &str = r#"on run argv
    activate
    set picked to choose from list (items 2 thru -1 of argv) with title "tuxDrop" with prompt (item 1 of argv) default items {item 2 of argv} OK button name "Send"
    if picked is false then return ""
    return item 1 of picked
end run"#;
    let mut args = vec![prompt];
    args.extend(labels.iter().map(String::as_str));
    Ok(osascript(SCRIPT, &args)
        .await?
        .and_then(|picked| labels.iter().position(|l| *l == picked)))
}

/// Lets the user pick files, or folders; `None` if cancelled.
pub async fn pick_paths(title: &str, folders: bool) -> Result<Option<Vec<PathBuf>>> {
    const SCRIPT: &str = r#"on run argv
    activate
    if item 2 of argv is "folders" then
        set picked to choose folder with prompt (item 1 of argv) with multiple selections allowed
    else
        set picked to choose file with prompt (item 1 of argv) with multiple selections allowed
    end if
    set out to ""
    repeat with f in picked
        set out to out & POSIX path of f & linefeed
    end repeat
    return out
end run"#;
    let kind = if folders { "folders" } else { "files" };
    Ok(osascript(SCRIPT, &[title, kind]).await?.map(|out| {
        out.lines()
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect()
    }))
}
