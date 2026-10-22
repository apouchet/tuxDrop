//! Desktop notifications (org.freedesktop.Notifications).

use std::collections::HashMap;

use zbus::zvariant::Value;

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
pub trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;

    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
}

pub const ACCEPT: &str = "accept";
pub const REJECT: &str = "reject";
pub const ALWAYS: &str = "always";
pub const CANCEL: &str = "cancel";
pub const OPEN: &str = "open";
pub const COPY: &str = "copy";

/// Notification bodies may be rendered as markup.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl NotificationsProxy<'_> {
    /// Persistent notification with Accept / Decline (/ Always) buttons.
    pub async fn ask(&self, summary: &str, body: &str, offer_always: bool) -> zbus::Result<u32> {
        let mut actions = vec![ACCEPT, "Accept", REJECT, "Decline"];
        if offer_always {
            actions.extend([ALWAYS, "Always accept from this device"]);
        }
        let hints = HashMap::from([("urgency", Value::U8(2))]);
        self.notify(
            "tuxDrop",
            0,
            "document-send",
            summary,
            &escape(body),
            &actions,
            hints,
            0,
        )
        .await
    }

    /// Shows a notification without buttons, replacing `replaces` unless it
    /// is 0; returns its id, 0 if it could not be shown.
    pub async fn inform(&self, replaces: u32, summary: &str, body: &str) -> u32 {
        self.show(replaces, summary, body, &[], None).await
    }

    /// Like [`Self::inform`], with buttons (`[key, label, …]`) and a
    /// progress percentage, drawn as a bar by servers supporting it.
    pub async fn show(
        &self,
        replaces: u32,
        summary: &str,
        body: &str,
        actions: &[&str],
        percent: Option<u32>,
    ) -> u32 {
        let mut hints = HashMap::new();
        if let Some(percent) = percent {
            hints.insert("value", Value::I32(percent.min(100) as i32));
            hints.insert("suppress-sound", Value::Bool(true));
        }
        self.notify(
            "tuxDrop",
            replaces,
            "document-send",
            summary,
            &escape(body),
            actions,
            hints,
            -1,
        )
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("notification failed: {e}");
            0
        })
    }
}
