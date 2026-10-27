//! Desktop integration, one implementation per platform family. Each provides
//! the same items:
//!
//! - `run_main(daemon)`: runs the daemon, with whatever the main thread must
//!   do meanwhile (the native tray's event loop).
//! - `Session::start()`: connects to the desktop; returns the `Notifier`, the
//!   `FileManager`, the API `Signals`, the notification events and the
//!   `ApiServer`, which serves [`crate::api`] once started.
//! - `tray::run(shared)`: shows the tray icon (menu: [`crate::tray`]).
//! - `copy_to_clipboard`, `ask_text`, `pick_paths`, `pick_device`, with
//!   `CLIPBOARD_HELP` and
//!   `DIALOG_HELP` shown when they fail.
//!
//! **freedesktop** (Linux, BSD): session D-Bus (notifications, FileManager1,
//! `org.tuxdrop.Daemon1`), StatusNotifierItem tray, wl-copy/xclip, zenity.
//!
//! **native** (macOS, Windows): menu bar / notification area icon
//! (tray-icon), control API on a local socket or named pipe
//! ([`tuxdrop_core::ipc`]), and per-OS dialogs and notifications:
//! AppleScript on macOS, toasts and Win32 dialogs on Windows.

#[cfg(freedesktop)]
mod freedesktop;
#[cfg(freedesktop)]
pub use freedesktop::*;

#[cfg(not(freedesktop))]
mod native;
#[cfg(not(freedesktop))]
pub use native::*;
