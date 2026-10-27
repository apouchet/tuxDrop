//! Menu bar (macOS) / notification area (Windows) icon showing the menu of
//! [`crate::tray`].
//!
//! The icon belongs to the main thread, which runs the event loop; the daemon
//! runs on a Tokio runtime and sends each new menu through the loop's proxy.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tokio::runtime::{Handle, Runtime};
use tray_icon::menu::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::notifications::{ACCEPT, ALWAYS, REJECT};
use crate::tray::{self, Device, Snapshot};
use crate::{Shared, icon};

/// Announces at most this often when the pointer reaches the icon.
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(5);

enum UserEvent {
    /// The daemon started.
    Started(Arc<Shared>),
    Show(Snapshot),
    Menu(MenuEvent),
    Icon(TrayIconEvent),
    /// The daemon stopped.
    Exit(Result<()>),
}

static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();

/// What a menu entry does.
enum Action {
    Decide(String, &'static str),
    Cancel(String),
    SendTo(Device, bool),
    Rename(Device),
    ToggleTrust(Device),
    OpenReceived,
    /// Turns starting at login on or off.
    ToggleLogin,
    Refresh,
    Quit,
}

/// Runs `daemon` on a Tokio runtime and the event loop on this thread; exits
/// the process when the daemon stops or the user quits.
pub fn run_main(daemon: impl Future<Output = Result<()>> + Send + 'static) -> Result<()> {
    let runtime = Runtime::new()?;
    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        // A menu bar item only: no Dock icon, no menu bar of its own.
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }
    let proxy = event_loop.create_proxy();
    let _ = PROXY.set(proxy.clone());
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = menu_proxy.send_event(UserEvent::Menu(e));
    }));
    let icon_proxy = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |e| {
        let _ = icon_proxy.send_event(UserEvent::Icon(e));
    }));
    runtime.spawn(async move {
        let result = daemon.await;
        let _ = proxy.send_event(UserEvent::Exit(result));
    });

    let mut app = App {
        runtime,
        shared: None,
        icon: None,
        actions: HashMap::new(),
        last_announce: None,
    };
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {}
            Event::UserEvent(UserEvent::Started(shared)) => app.shared = Some(shared),
            Event::UserEvent(UserEvent::Show(snapshot)) => app.show(&snapshot),
            Event::UserEvent(UserEvent::Menu(e)) => app.activate(&e.id),
            Event::UserEvent(UserEvent::Icon(
                TrayIconEvent::Enter { .. } | TrayIconEvent::Click { .. },
            )) => app.refresh(false),
            Event::UserEvent(UserEvent::Exit(result)) => {
                app.remove_icon();
                let code = match result {
                    Ok(()) => 0,
                    Err(e) => {
                        tracing::error!("{e:#}");
                        super::os::alert("tuxDrop stopped", &format!("{e:#}"));
                        1
                    }
                };
                std::process::exit(code);
            }
            _ => {}
        }
    })
}

/// Shows the icon and keeps its menu up to date (called on the runtime).
pub async fn run(shared: Arc<Shared>) {
    let Some(proxy) = PROXY.get() else {
        return;
    };
    if proxy
        .send_event(UserEvent::Started(shared.clone()))
        .is_err()
    {
        return;
    }
    tray::watch(&shared, |snapshot| {
        std::future::ready(proxy.send_event(UserEvent::Show(snapshot)).is_ok())
    })
    .await;
}

struct App {
    runtime: Runtime,
    shared: Option<Arc<Shared>>,
    icon: Option<TrayIcon>,
    /// What the entries of the menu shown do.
    actions: HashMap<MenuId, Action>,
    last_announce: Option<Instant>,
}

impl App {
    fn handle(&self) -> &Handle {
        self.runtime.handle()
    }

    fn show(&mut self, snapshot: &Snapshot) {
        self.actions.clear();
        let menu = self.menu(snapshot);
        let attention = !snapshot.requests.is_empty();
        let tooltip = match snapshot.requests.first() {
            Some(request) => request.label.clone(),
            None => "tuxDrop".to_owned(),
        };
        match &self.icon {
            Some(icon) => {
                icon.set_menu(Some(Box::new(menu)));
                #[cfg(target_os = "macos")]
                let _ = icon.set_icon_templated(Some(drop_icon(attention)));
                #[cfg(not(target_os = "macos"))]
                let _ = icon.set_icon(Some(drop_icon(attention)));
                let _ = icon.set_tooltip(Some(tooltip));
            }
            None => {
                let builder = TrayIconBuilder::new()
                    .with_id("tuxdrop")
                    .with_tooltip(tooltip)
                    .with_menu(Box::new(menu));
                #[cfg(target_os = "macos")]
                let builder = builder.with_icon_templated(drop_icon(attention));
                #[cfg(not(target_os = "macos"))]
                let builder = builder.with_icon(drop_icon(attention));
                let built = builder.build();
                match built {
                    Ok(icon) => self.icon = Some(icon),
                    Err(e) => tracing::warn!("cannot create the menu bar icon: {e}"),
                }
            }
        }
    }

    /// Asks nearby devices to answer and reloads names and trust, at most
    /// every `ANNOUNCE_INTERVAL` unless `now`.
    fn refresh(&mut self, now: bool) {
        let Some(shared) = self.shared.clone() else {
            return;
        };
        if !now
            && self
                .last_announce
                .is_some_and(|t| t.elapsed() < ANNOUNCE_INTERVAL)
        {
            return;
        }
        self.last_announce = Some(Instant::now());
        self.handle().spawn(async move {
            shared.announce();
            shared.tray_changed.notify_one();
        });
    }

    fn activate(&mut self, id: &MenuId) {
        let Some(action) = self.actions.remove(id) else {
            return;
        };
        let Some(shared) = self.shared.clone() else {
            return;
        };
        let handle = self.handle().clone();
        match action {
            Action::Decide(id, choice) => {
                handle.spawn(async move { shared.decide(&id, choice).await });
            }
            Action::Cancel(id) => {
                handle.spawn(async move { shared.cancel(&id).await });
            }
            Action::SendTo(device, folders) => {
                handle.spawn(tray::send_to(shared, device, folders));
            }
            Action::Rename(device) => {
                handle.spawn(tray::rename(shared, device));
            }
            Action::ToggleTrust(device) => tray::toggle_trust(&shared, &device),
            Action::OpenReceived => {
                handle.spawn(tray::open_received(shared));
            }
            Action::ToggleLogin => {
                let enable = !super::os::login::enabled().unwrap_or(true);
                if let Err(e) = super::os::login::set(enable) {
                    tracing::error!("cannot change starting at login: {e:#}");
                }
            }
            Action::Refresh => self.refresh(true),
            Action::Quit => {
                self.remove_icon();
                std::process::exit(0)
            }
        }
    }

    /// Exiting skips destructors, and Windows keeps showing the icon of a
    /// program that did not remove it until the pointer reaches it.
    fn remove_icon(&mut self) {
        drop(self.icon.take());
    }

    /// A clickable entry doing `action`.
    fn item(&mut self, text: &str, action: Action) -> MenuItem {
        let item = MenuItem::new(label(text), true, None);
        self.actions.insert(item.id().clone(), action);
        item
    }

    fn menu(&mut self, snapshot: &Snapshot) -> Menu {
        let menu = Menu::new();
        let mut entries: Vec<Box<dyn IsMenuItem>> = Vec::new();
        for request in &snapshot.requests {
            entries.push(Box::new(disabled(&request.label)));
            let choices = [("Accept", ACCEPT), ("Decline", REJECT)].into_iter().chain(
                request
                    .verified
                    .then_some(("Always Accept from This Device", ALWAYS)),
            );
            for (text, choice) in choices {
                entries.push(Box::new(
                    self.item(text, Action::Decide(request.id.clone(), choice)),
                ));
            }
            entries.push(Box::new(PredefinedMenuItem::separator()));
        }
        for transfer in &snapshot.transfers {
            entries.push(Box::new(disabled(&transfer.label)));
            entries.push(Box::new(
                self.item("Cancel", Action::Cancel(transfer.id.clone())),
            ));
            entries.push(Box::new(PredefinedMenuItem::separator()));
        }
        if snapshot.devices.is_empty() {
            entries.push(Box::new(disabled("No device found")));
        }
        for device in &snapshot.devices {
            entries.push(Box::new(self.device_menu(device)));
        }
        entries.push(Box::new(PredefinedMenuItem::separator()));
        entries.push(Box::new(
            self.item("Open Received Files", Action::OpenReceived),
        ));
        entries.push(Box::new(self.item("Refresh", Action::Refresh)));
        entries.push(Box::new(PredefinedMenuItem::separator()));
        if let Some(enabled) = super::os::login::enabled() {
            let login = CheckMenuItem::new("Open at Login", true, enabled, None);
            self.actions.insert(login.id().clone(), Action::ToggleLogin);
            entries.push(Box::new(login));
        }
        entries.push(Box::new(self.item("Quit tuxDrop", Action::Quit)));
        for entry in &entries {
            let _ = menu.append(entry.as_ref());
        }
        menu
    }

    fn device_menu(&mut self, device: &Device) -> Submenu {
        let trusted = CheckMenuItem::new(
            "Trusted (Accept Without Asking)",
            true,
            device.trusted,
            None,
        );
        self.actions
            .insert(trusted.id().clone(), Action::ToggleTrust(device.clone()));
        let items: [&dyn IsMenuItem; 6] = [
            &disabled(&device.short_fingerprint()),
            &self.item("Send Files…", Action::SendTo(device.clone(), false)),
            &self.item("Send Folder…", Action::SendTo(device.clone(), true)),
            &PredefinedMenuItem::separator(),
            &self.item("Rename…", Action::Rename(device.clone())),
            &trusted,
        ];
        Submenu::with_items(label(&device.menu_label()), true, &items)
            .unwrap_or_else(|_| Submenu::new(label(&device.menu_label()), true))
    }
}

fn disabled(text: &str) -> MenuItem {
    MenuItem::new(label(text), false, None)
}

/// A single `&` marks a mnemonic in menu labels.
fn label(text: &str) -> String {
    text.replace('&', "&&")
}

/// The menu bar / notification area icon ([`crate::icon`]).
fn drop_icon(attention: bool) -> Icon {
    const SIZE: u32 = 44;
    // macOS draws template images in the menu bar's own color, from their
    // alpha only.
    #[cfg(target_os = "macos")]
    let rgba = icon::drop_rgba_in([0, 0, 0], SIZE, attention);
    #[cfg(not(target_os = "macos"))]
    let rgba = icon::drop_rgba(SIZE, attention);
    Icon::from_rgba(rgba, SIZE, SIZE).expect("valid icon size")
}
