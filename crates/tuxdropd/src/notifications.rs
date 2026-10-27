//! What the user can answer to the daemon's notifications, on every
//! platform. Each platform's `Notifier` shows them its own way.

pub const ACCEPT: &str = "accept";
pub const REJECT: &str = "reject";
pub const ALWAYS: &str = "always";
pub const CANCEL: &str = "cancel";
pub const OPEN: &str = "open";
pub const COPY: &str = "copy";

/// Reported by a `Notifier`, in the order the user acted.
#[derive(Debug)]
pub enum NotificationEvent {
    /// A button of notification `id` was clicked.
    Action { id: u32, key: String },
    /// Notification `id` went away; its buttons can no longer be clicked.
    Closed { id: u32 },
}
