//! Desktop notifications (org.freedesktop.Notifications).

use std::collections::HashMap;

use zbus::zvariant::Value;

use crate::notifications::{ACCEPT, ALWAYS, CANCEL, REJECT};

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

/// Notification bodies may be rendered as markup.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Shows the daemon's notifications; their buttons are reported as
/// `NotificationEvent`s.
pub struct Notifier(NotificationsProxy<'static>);

impl Notifier {
    pub fn new(proxy: NotificationsProxy<'static>) -> Self {
        Self(proxy)
    }

    /// Persistent notification with Accept / Decline (/ Always) buttons.
    pub async fn ask(&self, summary: &str, body: &str, offer_always: bool) -> anyhow::Result<u32> {
        Ok(self.0.ask(summary, body, offer_always).await?)
    }

    /// A transfer in progress, with a Cancel button and its percentage.
    pub async fn transfer(&self, replaces: u32, summary: &str, body: &str, percent: u32) -> u32 {
        self.0
            .show(replaces, summary, body, &[CANCEL, "Cancel"], Some(percent))
            .await
    }

    /// How something ended, with an optional `[key, label]` button.
    pub async fn conclude(
        &self,
        replaces: u32,
        summary: &str,
        body: &str,
        action: Option<[&str; 2]>,
    ) -> u32 {
        let actions = action.as_ref().map_or(&[][..], |a| &a[..]);
        self.0.show(replaces, summary, body, actions, None).await
    }

    pub async fn inform(&self, replaces: u32, summary: &str, body: &str) -> u32 {
        self.0.inform(replaces, summary, body).await
    }

    pub async fn close(&self, id: u32) {
        let _ = self.0.close_notification(id).await;
    }
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
