//! Windows: toast notifications (WinRT) with buttons and a progress bar,
//! native dialogs (task dialog, file dialog, a small text prompt), the
//! clipboard, Explorer, and starting at login (a `Run` registry value).
//!
//! Toasts need an AppUserModelID: an unpackaged program declares its own in
//! the registry, with the name and icon shown on its toasts. Their buttons
//! are reported by the toast's `Activated` event while the daemon runs.
//!
//! The Share sheet entry is another program, `tuxdrop-share.exe`, which a
//! sparse package declares as a share target (`packaging/windows`); it hands
//! the shared files to the daemon's `share` request.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{
    NotificationData, ToastActivatedEventArgs, ToastDismissalReason, ToastDismissedEventArgs,
    ToastNotification, ToastNotificationManager, ToastNotifier,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
};
use windows::core::{HSTRING, IInspectable, Interface};

use crate::notifications::{ACCEPT, ALWAYS, CANCEL, COPY, NotificationEvent, REJECT};

pub const CLIPBOARD_HELP: &str = "The clipboard is busy, try again";
pub const DIALOG_HELP: &str = "The dialog could not be shown, see tuxdropd.log";

/// Identifies the daemon's toasts; declared under
/// `HKCU\Software\Classes\AppUserModelId`.
const AUMID: &str = "tuxDrop.Daemon";
const TOAST_GROUP: &str = "tuxdrop";

/// `%LOCALAPPDATA%\tuxdrop`: the log and the toast icon.
fn local_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tuxdrop")
}

/// Whether another tuxdropd serves the control pipe: started again (Start
/// menu, login, Share sheet), this one leaves it running.
pub fn already_running() -> bool {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .is_ok_and(|runtime| {
            runtime
                .block_on(tuxdrop_core::ipc::running())
                .unwrap_or(false)
        })
}

/// Where the logs go when tuxdropd has no console (always, unless its
/// standard error is redirected): `%LOCALAPPDATA%\tuxdrop\tuxdropd.log`,
/// the previous run's in `tuxdropd.old.log`.
pub fn log_file() -> Option<std::fs::File> {
    use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
    // SAFETY: no pointer involved.
    let stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    if stderr.is_ok_and(|h| !h.is_invalid() && !h.0.is_null()) {
        return None;
    }
    let dir = local_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let log = dir.join("tuxdropd.log");
    let _ = std::fs::rename(&log, dir.join("tuxdropd.old.log"));
    std::fs::File::create(log).ok()
}

/// A blocking alert, for when the daemon stops.
pub fn alert(summary: &str, body: &str) {
    // SAFETY: both strings outlive the call.
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(body),
            &HSTRING::from(summary),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }
}

/// `<`, `>`, `&` and quotes escaped for toast XML.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The XML of a toast: a title, a body, buttons given as `[key, label]`.
/// `extra` goes in the binding (a progress bar), `attributes` on `<toast>`.
fn toast_xml(
    summary: &str,
    body: &str,
    extra: &str,
    buttons: &[[&str; 2]],
    attributes: &str,
    silent: bool,
) -> String {
    let mut xml = format!(
        r#"<toast {attributes}><visual><binding template="ToastGeneric"><text>{}</text>"#,
        xml_escape(summary)
    );
    if !body.is_empty() {
        xml.push_str(&format!("<text>{}</text>", xml_escape(body)));
    }
    xml.push_str(extra);
    xml.push_str("</binding></visual>");
    if !buttons.is_empty() {
        xml.push_str("<actions>");
        for [key, label] in buttons {
            xml.push_str(&format!(
                r#"<action content="{}" arguments="{}" activationType="foreground"/>"#,
                xml_escape(label),
                xml_escape(key)
            ));
        }
        xml.push_str("</actions>");
    }
    if silent {
        xml.push_str(r#"<audio silent="true"/>"#);
    }
    xml.push_str("</toast>");
    xml
}

struct Toast {
    toast: ToastNotification,
    /// Has a progress bar, updated in place.
    progress: bool,
}

/// Shows the daemon's toasts; clicked buttons are reported as
/// `NotificationEvent`s.
pub struct Notifier {
    /// `None` if toasts are unavailable: what they would show is logged.
    notifier: Option<ToastNotifier>,
    next_id: AtomicU32,
    toasts: Arc<Mutex<HashMap<u32, Toast>>>,
    /// Orders progress updates.
    sequence: AtomicU32,
    events: mpsc::UnboundedSender<NotificationEvent>,
}

impl Notifier {
    pub fn new(events: mpsc::UnboundedSender<NotificationEvent>) -> Self {
        let notifier = (|| -> Result<ToastNotifier> {
            // Started by tuxdrop-share, the daemon may run with the
            // package's identity, whose own AppUserModelID it must use.
            if has_package_identity() {
                return Ok(ToastNotificationManager::CreateToastNotifier()?);
            }
            register_aumid()?;
            Ok(ToastNotificationManager::CreateToastNotifierWithId(
                &HSTRING::from(AUMID),
            )?)
        })()
        .inspect_err(|e| tracing::warn!("toast notifications unavailable: {e:#}"))
        .ok();
        Self {
            notifier,
            next_id: AtomicU32::new(1),
            toasts: Arc::default(),
            sequence: AtomicU32::new(1),
            events,
        }
    }

    /// Shows a toast; returns its id, 0 if it could not be shown.
    fn show(&self, xml: &str, progress: Option<NotificationData>) -> u32 {
        let Some(notifier) = &self.notifier else {
            return 0;
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let shown = (|| -> Result<ToastNotification> {
            let doc = XmlDocument::new()?;
            doc.LoadXml(&HSTRING::from(xml))?;
            let toast = ToastNotification::CreateToastNotification(&doc)?;
            toast.SetTag(&HSTRING::from(id.to_string()))?;
            toast.SetGroup(&HSTRING::from(TOAST_GROUP))?;
            if let Some(data) = &progress {
                toast.SetData(data)?;
            }
            let events = self.events.clone();
            toast.Activated(&windows::Foundation::TypedEventHandler::new(
                move |_, args: windows::core::Ref<IInspectable>| {
                    let key = args
                        .as_ref()
                        .and_then(|a| a.cast::<ToastActivatedEventArgs>().ok())
                        .and_then(|a| a.Arguments().ok())
                        .map(|k| k.to_string_lossy())
                        .unwrap_or_default();
                    // A click on the toast itself has no key.
                    if !key.is_empty() {
                        let _ = events.send(NotificationEvent::Action { id, key });
                    }
                    Ok(())
                },
            ))?;
            let events = self.events.clone();
            let toasts = self.toasts.clone();
            toast.Dismissed(&windows::Foundation::TypedEventHandler::new(
                move |_, args: windows::core::Ref<ToastDismissedEventArgs>| {
                    // A toast that timed out moves to the notification
                    // center, where its buttons still work.
                    let gone = args.as_ref().and_then(|a| a.Reason().ok())
                        != Some(ToastDismissalReason::TimedOut);
                    if gone {
                        toasts.lock().unwrap().remove(&id);
                        let _ = events.send(NotificationEvent::Closed { id });
                    }
                    Ok(())
                },
            ))?;
            notifier.Show(&toast)?;
            Ok(toast)
        })();
        match shown {
            Ok(toast) => {
                let progress = progress.is_some();
                self.toasts
                    .lock()
                    .unwrap()
                    .insert(id, Toast { toast, progress });
                id
            }
            Err(e) => {
                tracing::warn!("cannot show a notification: {e:#}");
                0
            }
        }
    }

    fn progress_data(&self, body: &str, percent: u32) -> Result<NotificationData> {
        let data = NotificationData::new()?;
        let values = data.Values()?;
        values.Insert(
            &HSTRING::from("progressValue"),
            &HSTRING::from(format!("{:.2}", f64::from(percent.min(100)) / 100.0)),
        )?;
        values.Insert(&HSTRING::from("progressStatus"), &HSTRING::from(body))?;
        data.SetSequenceNumber(self.sequence.fetch_add(1, Ordering::Relaxed))?;
        Ok(data)
    }

    /// Accept / Decline (/ Always) toast, kept on screen until answered.
    pub async fn ask(&self, summary: &str, body: &str, offer_always: bool) -> Result<u32> {
        if self.notifier.is_none() {
            bail!("notifications are unavailable, see the tray icon");
        }
        let mut buttons = vec![[ACCEPT, "Accept"], [REJECT, "Decline"]];
        if offer_always {
            buttons.push([ALWAYS, "Always accept"]);
        }
        let xml = toast_xml(summary, body, "", &buttons, r#"scenario="reminder""#, false);
        match self.show(&xml, None) {
            0 => bail!("the notification could not be shown"),
            id => Ok(id),
        }
    }

    /// A transfer in progress, with a progress bar and a Cancel button:
    /// updates toast `replaces` if it is one, else replaces it.
    pub async fn transfer(&self, replaces: u32, summary: &str, body: &str, percent: u32) -> u32 {
        if self.notifier.is_none() {
            if percent == 0 {
                tracing::info!("{summary}: {body}");
            }
            return 0;
        }
        let data = match self.progress_data(body, percent) {
            Ok(data) => data,
            Err(e) => {
                tracing::warn!("cannot update a notification: {e:#}");
                return replaces;
            }
        };
        let in_place = self
            .toasts
            .lock()
            .unwrap()
            .get(&replaces)
            .is_some_and(|t| t.progress);
        if in_place {
            if let Some(notifier) = &self.notifier {
                // Fails harmlessly once the user dismissed the toast.
                let _ = notifier.UpdateWithTagAndGroup(
                    &data,
                    &HSTRING::from(replaces.to_string()),
                    &HSTRING::from(TOAST_GROUP),
                );
            }
            return replaces;
        }
        self.close(replaces).await;
        let bar = r#"<progress value="{progressValue}" status="{progressStatus}"/>"#;
        let xml = toast_xml(summary, "", bar, &[[CANCEL, "Cancel"]], "", true);
        self.show(&xml, Some(data))
    }

    /// How something ended, with a button (`[key, label]`) if any.
    pub async fn conclude(
        &self,
        replaces: u32,
        summary: &str,
        body: &str,
        action: Option<[&str; 2]>,
    ) -> u32 {
        self.close(replaces).await;
        if self.notifier.is_none() {
            tracing::info!("{summary}: {body}");
            return 0;
        }
        let buttons: Vec<[&str; 2]> = action.into_iter().collect();
        // A message stays longer, to leave time to copy it.
        let attributes = if action.is_some_and(|[key, _]| key == COPY) {
            r#"duration="long""#
        } else {
            ""
        };
        self.show(&toast_xml(summary, body, "", &buttons, attributes, false), None)
    }

    pub async fn inform(&self, replaces: u32, summary: &str, body: &str) -> u32 {
        self.conclude(replaces, summary, body, None).await
    }

    /// Removes toast `id` if it is still shown.
    pub async fn close(&self, id: u32) {
        let toast = self.toasts.lock().unwrap().remove(&id);
        if let (Some(toast), Some(notifier)) = (toast, &self.notifier) {
            let _ = notifier.Hide(&toast.toast);
        }
    }
}

fn has_package_identity() -> bool {
    use windows::Win32::Foundation::APPMODEL_ERROR_NO_PACKAGE;
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    let mut len = 0;
    // SAFETY: a null buffer of length 0 only asks for the length.
    let status = unsafe { GetCurrentPackageFullName(&mut len, None) };
    status != APPMODEL_ERROR_NO_PACKAGE
}

/// Declares the AppUserModelID of the toasts, with the name and icon they
/// show.
fn register_aumid() -> Result<()> {
    let icon = local_dir().join("tuxdrop.png");
    std::fs::create_dir_all(local_dir())?;
    write_icon(64, &icon)?;
    let key = format!(r"Software\Classes\AppUserModelId\{AUMID}");
    registry::set(&key, "DisplayName", "tuxDrop")?;
    registry::set(&key, "IconUri", &icon.to_string_lossy())?;
    Ok(())
}

/// Writes the tray icon's drop as a PNG of `size` pixels (the toast icon,
/// and the package logos: `tuxdropd --write-icon <size> <file>`).
pub fn write_icon(size: u32, path: &Path) -> Result<()> {
    let rgba = super::tray::drop_rgba(size, false);
    std::fs::write(path, png::encode(size, size, &rgba))
        .with_context(|| format!("writing {}", path.display()))
}

/// Just enough PNG: 8-bit RGBA, stored (uncompressed) deflate blocks.
mod png {
    pub fn encode(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut raw = Vec::with_capacity(rgba.len() + height as usize);
        for row in rgba.chunks(width as usize * 4) {
            raw.push(0); // filter: none
            raw.extend_from_slice(row);
        }
        let mut zlib = vec![0x78, 0x01];
        let blocks: Vec<&[u8]> = raw.chunks(0xFFFF).collect();
        for (i, block) in blocks.iter().enumerate() {
            zlib.push(u8::from(i == blocks.len() - 1));
            let len = block.len() as u16;
            zlib.extend_from_slice(&len.to_le_bytes());
            zlib.extend_from_slice(&(!len).to_le_bytes());
            zlib.extend_from_slice(block);
        }
        zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

        let mut header = Vec::new();
        header.extend_from_slice(&width.to_be_bytes());
        header.extend_from_slice(&height.to_be_bytes());
        header.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        chunk(&mut png, b"IHDR", &header);
        chunk(&mut png, b"IDAT", &zlib);
        chunk(&mut png, b"IEND", &[]);
        png
    }

    fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let mut crc = !0u32;
        for &byte in kind.iter().chain(data) {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        png.extend_from_slice(&(!crc).to_be_bytes());
    }

    fn adler32(data: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in data {
            a = (a + u32::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn png_has_valid_structure() {
            let png = super::encode(2, 1, &[255, 0, 0, 255, 0, 0, 255, 128]);
            assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
            // IHDR length, type, width 2, height 1.
            assert_eq!(&png[8..24], b"\0\0\0\x0dIHDR\0\0\0\x02\0\0\0\x01");
            assert!(png.ends_with(b"IEND\xae\x42\x60\x82"));
            assert_eq!(super::adler32(b"Wikipedia"), 0x11E6_0398);
        }
    }
}

/// String values under `HKEY_CURRENT_USER`.
mod registry {
    use anyhow::Result;
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW,
        RegSetKeyValueW,
    };
    use windows::core::HSTRING;

    pub fn set(key: &str, name: &str, value: &str) -> Result<()> {
        let data: Vec<u16> = value.encode_utf16().chain([0]).collect();
        // SAFETY: `data` is a NUL-terminated string of the given byte size.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(key),
                &HSTRING::from(name),
                REG_SZ.0,
                Some(data.as_ptr().cast()),
                (data.len() * 2) as u32,
            )
            .ok()?;
        }
        Ok(())
    }

    pub fn exists(key: &str, name: &str) -> bool {
        // SAFETY: only asks whether the value exists.
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(key),
                &HSTRING::from(name),
                RRF_RT_REG_SZ,
                None,
                None,
                None,
            )
            .is_ok()
        }
    }

    /// Succeeds if the value does not exist.
    pub fn delete(key: &str, name: &str) -> Result<()> {
        use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
        // SAFETY: plain strings.
        let status =
            unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(name)) };
        if status != ERROR_FILE_NOT_FOUND {
            status.ok()?;
        }
        Ok(())
    }
}

/// Starting at login: the `tuxDrop` value of the user's `Run` key, which
/// the installer sets.
pub mod login {
    use anyhow::{Context, Result};

    use super::registry;

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "tuxDrop";

    pub fn enabled() -> Option<bool> {
        Some(registry::exists(RUN, NAME))
    }

    pub fn set(enabled: bool) -> Result<()> {
        if !enabled {
            return registry::delete(RUN, NAME);
        }
        let exe = std::env::current_exe().context("cannot find tuxdropd.exe")?;
        registry::set(RUN, NAME, &format!("\"{}\"", exe.display()))
    }
}

/// Nothing to set up at each start: the installer registers the Share
/// sheet entry.
pub async fn integrate() {}

/// Explorer.
pub struct FileManager;

impl FileManager {
    /// Opens `folder`.
    pub async fn open_folder(&self, folder: &Path) {
        explorer(&[folder.as_os_str().to_owned()]).await;
    }

    /// Opens the folder holding the first of `items`, with it selected.
    pub async fn select(&self, items: &[impl AsRef<Path>]) {
        if let Some(first) = items.first() {
            let mut arg = std::ffi::OsString::from("/select,");
            arg.push(first.as_ref());
            explorer(&[arg]).await;
        }
    }
}

async fn explorer(args: &[std::ffi::OsString]) {
    // Explorer exits with 1 even when it succeeds.
    if let Err(e) = Command::new("explorer").args(args).status().await {
        tracing::warn!("cannot run explorer: {e}");
    }
}

/// Runs `f` on a thread of its own: dialogs run a modal message loop, and
/// COM wants a single-threaded apartment for them.
async fn on_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    let (tx, rx) = oneshot::channel();
    std::thread::Builder::new()
        .name("dialog".into())
        .spawn(move || {
            let _ = tx.send(f());
        })?;
    rx.await.context("the dialog thread stopped")
}

pub async fn copy_to_clipboard(text: &str) -> Result<()> {
    let text: Vec<u16> = text.encode_utf16().chain([0]).collect();
    on_thread(move || dialogs::set_clipboard(&text)).await?
}

/// Asks for a line of text; `None` if cancelled.
pub async fn ask_text(prompt: &str, initial: &str) -> Result<Option<String>> {
    let (prompt, initial) = (prompt.to_owned(), initial.to_owned());
    on_thread(move || dialogs::ask_text(&prompt, &initial)).await?
}

/// Lets the user pick files, or folders; `None` if cancelled.
pub async fn pick_paths(title: &str, folders: bool) -> Result<Option<Vec<PathBuf>>> {
    let title = title.to_owned();
    on_thread(move || dialogs::pick_paths(&title, folders)).await?
}

/// Lets the user pick one of `labels`; its index, `None` if cancelled.
pub async fn pick_device(prompt: &str, labels: &[String]) -> Result<Option<usize>> {
    let (prompt, labels) = (prompt.to_owned(), labels.to_vec());
    on_thread(move || dialogs::pick_one(&prompt, &labels)).await?
}

/// The blocking parts, each run on its own thread ([`on_thread`]).
mod dialogs {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use anyhow::{Context, Result, bail};
    use windows::Win32::Foundation::{
        ERROR_CANCELLED, GlobalFree, HANDLE, HWND, LPARAM, S_OK, WPARAM,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
        CoInitializeEx, CoTaskMemFree, CoUninitialize,
    };
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::CF_UNICODETEXT;
    use windows::Win32::UI::Controls::{
        TASKDIALOG_BUTTON, TASKDIALOG_NOTIFICATIONS, TASKDIALOGCONFIG, TDCBF_CANCEL_BUTTON,
        TDF_ALLOW_DIALOG_CANCELLATION, TDF_USE_COMMAND_LINKS, TDN_CREATED,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::Shell::{
        FOS_ALLOWMULTISELECT, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog,
        IFileOpenDialog, SIGDN_FILESYSPATH,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DLGTEMPLATE, DialogBoxIndirectParamW, EndDialog, GetDlgItem, GetDlgItemTextW,
        GetWindowTextLengthW, IDCANCEL, IDOK, SendDlgItemMessageW, SetDlgItemTextW,
        SetForegroundWindow, WM_COMMAND, WM_INITDIALOG,
    };
    use windows::core::{BOOL, HRESULT, HSTRING, PCWSTR, s, w};

    pub fn set_clipboard(text: &[u16]) -> Result<()> {
        // SAFETY: the clipboard is opened and closed here, and the memory
        // handed to it holds `text` (NUL-terminated).
        unsafe {
            let mut tries = 0;
            while OpenClipboard(None).is_err() {
                tries += 1;
                if tries == 20 {
                    bail!("{}", super::CLIPBOARD_HELP);
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            let copied = (|| -> Result<()> {
                EmptyClipboard()?;
                let memory = GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of_val(text))?;
                let target = GlobalLock(memory).cast::<u16>();
                if target.is_null() {
                    let _ = GlobalFree(Some(memory));
                    bail!("cannot lock the clipboard memory");
                }
                std::ptr::copy_nonoverlapping(text.as_ptr(), target, text.len());
                let _ = GlobalUnlock(memory);
                // The clipboard owns the memory once this succeeds.
                if let Err(e) = SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(memory.0)))
                {
                    let _ = GlobalFree(Some(memory));
                    return Err(e.into());
                }
                Ok(())
            })();
            let _ = CloseClipboard();
            copied
        }
    }

    pub fn pick_paths(title: &str, folders: bool) -> Result<Option<Vec<PathBuf>>> {
        // SAFETY: COM is initialized on this thread for the dialog's
        // lifetime; returned strings are freed after copying.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok()?;
            let picked = (|| -> Result<Option<Vec<PathBuf>>> {
                let dialog: IFileOpenDialog =
                    CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
                let mut options = dialog.GetOptions()? | FOS_ALLOWMULTISELECT | FOS_FORCEFILESYSTEM;
                if folders {
                    options |= FOS_PICKFOLDERS;
                }
                dialog.SetOptions(options)?;
                dialog.SetTitle(&HSTRING::from(title))?;
                match dialog.Show(None) {
                    Err(e) if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => return Ok(None),
                    shown => shown?,
                }
                let items = dialog.GetResults()?;
                let mut paths = Vec::new();
                for i in 0..items.GetCount()? {
                    let name = items.GetItemAt(i)?.GetDisplayName(SIGDN_FILESYSPATH)?;
                    let path = name.to_string();
                    CoTaskMemFree(Some(name.0 as _));
                    paths.push(PathBuf::from(path?));
                }
                Ok(Some(paths))
            })();
            CoUninitialize();
            picked
        }
    }

    type TaskDialogIndirect = unsafe extern "system" fn(
        *const TASKDIALOGCONFIG,
        *mut i32,
        *mut i32,
        *mut BOOL,
    ) -> HRESULT;

    /// Version 6 of the common controls (selected by tuxdropd's manifest)
    /// has task dialogs; older ones do not.
    fn task_dialog_indirect() -> Result<TaskDialogIndirect> {
        // SAFETY: the symbol has this signature in comctl32 6.
        unsafe {
            let comctl = LoadLibraryW(w!("comctl32.dll"))?;
            let f = GetProcAddress(comctl, s!("TaskDialogIndirect"))
                .context("no task dialogs: comctl32 6 is not loaded (missing manifest?)")?;
            Ok(std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                TaskDialogIndirect,
            >(f))
        }
    }

    unsafe extern "system" fn bring_forward(
        hwnd: HWND,
        msg: TASKDIALOG_NOTIFICATIONS,
        _: WPARAM,
        _: LPARAM,
        _: isize,
    ) -> HRESULT {
        if msg == TDN_CREATED {
            // SAFETY: `hwnd` is the dialog being created.
            let _ = unsafe { SetForegroundWindow(hwnd) };
        }
        S_OK
    }

    /// A task dialog with one command link per label.
    pub fn pick_one(prompt: &str, labels: &[String]) -> Result<Option<usize>> {
        const FIRST_ID: i32 = 1000;
        let task_dialog = task_dialog_indirect()?;
        let texts: Vec<HSTRING> = labels.iter().map(HSTRING::from).collect();
        let buttons: Vec<TASKDIALOG_BUTTON> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| TASKDIALOG_BUTTON {
                nButtonID: FIRST_ID + i as i32,
                pszButtonText: PCWSTR(text.as_ptr()),
            })
            .collect();
        let title = HSTRING::from("tuxDrop");
        let instruction = HSTRING::from(prompt);
        let config = TASKDIALOGCONFIG {
            cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
            dwFlags: TDF_USE_COMMAND_LINKS | TDF_ALLOW_DIALOG_CANCELLATION,
            dwCommonButtons: TDCBF_CANCEL_BUTTON,
            pszWindowTitle: PCWSTR(title.as_ptr()),
            pszMainInstruction: PCWSTR(instruction.as_ptr()),
            cButtons: buttons.len() as u32,
            pButtons: buttons.as_ptr(),
            // The dialog takes the focus: a key pressed for another window
            // must not send to the first device.
            nDefaultButton: IDCANCEL.0,
            pfCallback: Some(bring_forward),
            ..Default::default()
        };
        let mut clicked = 0;
        // SAFETY: `config` and everything it points to outlive the call.
        unsafe { task_dialog(&config, &mut clicked, std::ptr::null_mut(), std::ptr::null_mut()) }
            .ok()?;
        let index = clicked - FIRST_ID;
        Ok((0..labels.len() as i32)
            .contains(&index)
            .then_some(index as usize))
    }

    const EDIT_ID: i32 = 100;

    thread_local! {
        /// The text prompt's initial text, then what was typed (`None` if
        /// cancelled).
        static PROMPT: RefCell<(Vec<u16>, Option<String>)> = RefCell::default();
    }

    unsafe extern "system" fn prompt_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        _: LPARAM,
    ) -> isize {
        // SAFETY: `hwnd` is the dialog, which has an edit control EDIT_ID.
        unsafe {
            match msg {
                WM_INITDIALOG => {
                    PROMPT.with_borrow(|(initial, _)| {
                        let _ = SetDlgItemTextW(hwnd, EDIT_ID, PCWSTR(initial.as_ptr()));
                    });
                    // EM_SETSEL: select everything.
                    SendDlgItemMessageW(hwnd, EDIT_ID, 0x00B1, WPARAM(0), LPARAM(-1));
                    let _ = SetForegroundWindow(hwnd);
                    if let Ok(edit) = GetDlgItem(Some(hwnd), EDIT_ID) {
                        let _ = SetFocus(Some(edit));
                    }
                    0 // focus set here
                }
                WM_COMMAND => {
                    let id = (wparam.0 & 0xFFFF) as i32;
                    if id == IDOK.0 {
                        let edit = GetDlgItem(Some(hwnd), EDIT_ID).unwrap_or_default();
                        let mut text = vec![0u16; GetWindowTextLengthW(edit) as usize + 1];
                        let len = GetDlgItemTextW(hwnd, EDIT_ID, &mut text) as usize;
                        let typed = String::from_utf16_lossy(&text[..len]);
                        PROMPT.with_borrow_mut(|(_, answer)| *answer = Some(typed));
                        let _ = EndDialog(hwnd, 1);
                    } else if id == IDCANCEL.0 {
                        let _ = EndDialog(hwnd, 0);
                    }
                    1
                }
                _ => 0,
            }
        }
    }

    /// An in-memory dialog template: a label, a text field, OK and Cancel.
    /// Sizes are in dialog units, which follow the font.
    fn prompt_template(prompt: &str) -> Vec<u16> {
        const DS_SETFONT: u32 = 0x40;
        const DS_MODALFRAME: u32 = 0x80;
        const DS_CENTER: u32 = 0x800;
        const WS_POPUP: u32 = 0x8000_0000;
        const WS_CAPTION: u32 = 0x00C0_0000;
        const WS_SYSMENU: u32 = 0x0008_0000;
        const WS_CHILD: u32 = 0x4000_0000;
        const WS_VISIBLE: u32 = 0x1000_0000;
        const WS_TABSTOP: u32 = 0x0001_0000;
        const WS_BORDER: u32 = 0x0080_0000;
        const ES_AUTOHSCROLL: u32 = 0x80;
        const BS_DEFPUSHBUTTON: u32 = 1;
        const BUTTON: u16 = 0x80;
        const EDIT: u16 = 0x81;
        const STATIC: u16 = 0x82;

        fn push_u32(t: &mut Vec<u16>, v: u32) {
            t.extend([v as u16, (v >> 16) as u16]);
        }
        fn push_str(t: &mut Vec<u16>, s: &str) {
            t.extend(s.encode_utf16().chain([0]));
        }
        let item = |t: &mut Vec<u16>, style: u32, rect: [i16; 4], id: u16, class: u16, text: &str| {
            if t.len() % 2 == 1 {
                t.push(0); // items are DWORD-aligned
            }
            push_u32(t, WS_CHILD | WS_VISIBLE | style);
            push_u32(t, 0);
            t.extend(rect.map(|v| v as u16));
            t.extend([id, 0xFFFF, class]);
            push_str(t, text);
            t.push(0); // no creation data
        };

        let mut t = Vec::new();
        push_u32(
            &mut t,
            DS_SETFONT | DS_MODALFRAME | DS_CENTER | WS_POPUP | WS_CAPTION | WS_SYSMENU,
        );
        push_u32(&mut t, 0);
        t.extend([4, 0, 0, 250, 66]); // 4 items, x, y, width, height
        t.extend([0, 0]); // no menu, default class
        push_str(&mut t, "tuxDrop");
        t.push(9);
        push_str(&mut t, "Segoe UI");
        item(&mut t, 0, [8, 8, 234, 10], 0xFFFF, STATIC, prompt);
        let edit = WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL;
        item(&mut t, edit, [8, 22, 234, 13], EDIT_ID as u16, EDIT, "");
        let ok = WS_TABSTOP | BS_DEFPUSHBUTTON;
        item(&mut t, ok, [138, 44, 50, 14], IDOK.0 as u16, BUTTON, "OK");
        item(&mut t, WS_TABSTOP, [192, 44, 50, 14], IDCANCEL.0 as u16, BUTTON, "Cancel");
        t
    }

    pub fn ask_text(prompt: &str, initial: &str) -> Result<Option<String>> {
        let template = prompt_template(prompt);
        PROMPT.set((initial.encode_utf16().chain([0]).collect(), None));
        // A template must be DWORD-aligned, which a Vec<u16> may not be.
        let aligned: Vec<u32> = template
            .chunks(2)
            .map(|c| u32::from(c[0]) | (u32::from(*c.get(1).unwrap_or(&0)) << 16))
            .collect();
        // SAFETY: `aligned` holds a valid DLGTEMPLATE and its items.
        let result = unsafe {
            DialogBoxIndirectParamW(
                None,
                aligned.as_ptr().cast::<DLGTEMPLATE>(),
                None,
                Some(prompt_proc),
                LPARAM(0),
            )
        };
        if result == -1 {
            return Err(windows::core::Error::from_thread().into());
        }
        Ok(PROMPT.take().1)
    }
}