"""tuxDrop submenu in the Nautilus context menu of files and folders.

Lists the devices known to tuxdropd and sends the selection to the one
clicked, through the org.tuxdrop.Daemon1 session D-Bus API. The daemon
reports progress and errors as notifications.

Needs nautilus-python 4 (Nautilus 43+).
"""

import time

from gi.repository import Gio, GLib, GObject, Nautilus

# Bounds how long a hung daemon can freeze the file manager.
LIST_TIMEOUT_MS = 500
# Opening the menu asks nearby devices to answer, at most this often.
ANNOUNCE_INTERVAL_S = 5


def _info(name, label):
    return Nautilus.MenuItem(name=name, label=label, sensitive=False)


def _label(alias, name):
    """Same as the tray: "Name (alias)" for a device named locally."""
    if not name or name == alias:
        return alias
    return f"{name} ({alias})" if alias else name


def _ignore_reply(proxy, result):
    try:
        proxy.call_finish(result)
    except GLib.Error:
        pass  # tuxdropd shows its own errors as notifications


class TuxDropMenu(GObject.GObject, Nautilus.MenuProvider):
    def __init__(self):
        super().__init__()
        self.daemon = None
        self.last_announce = 0.0

    def get_file_items(self, files):
        paths = [f.get_location().get_path() for f in files]
        if not paths or None in paths:
            return []  # only local files can be sent
        submenu = Nautilus.Menu()
        for item in self._device_items(paths):
            submenu.append_item(item)
        top = Nautilus.MenuItem(
            name="TuxDrop::Send", label="tuxDrop", tip="Send to a nearby device"
        )
        top.set_submenu(submenu)
        return [top]

    def _proxy(self):
        if self.daemon is None:
            flags = (
                Gio.DBusProxyFlags.DO_NOT_LOAD_PROPERTIES
                | Gio.DBusProxyFlags.DO_NOT_CONNECT_SIGNALS
                | Gio.DBusProxyFlags.DO_NOT_AUTO_START
            )
            try:
                self.daemon = Gio.DBusProxy.new_for_bus_sync(
                    Gio.BusType.SESSION,
                    flags,
                    None,
                    "org.tuxdrop.Daemon",
                    "/org/tuxdrop/Daemon",
                    "org.tuxdrop.Daemon1",
                    None,
                )
            except GLib.Error:
                return None
        return self.daemon

    def _device_items(self, paths):
        daemon = self._proxy()
        if daemon is None or daemon.get_name_owner() is None:
            return [_info("TuxDrop::Stopped", "tuxdropd is not running")]
        try:
            (peers,) = daemon.call_sync(
                "ListPeers", None, Gio.DBusCallFlags.NONE, LIST_TIMEOUT_MS, None
            ).unpack()
        except GLib.Error:
            return [_info("TuxDrop::Error", "tuxdropd is not responding")]
        self._announce(daemon)
        if not peers:
            return [_info("TuxDrop::None", "No device found")]
        labelled = [
            (_label(alias, name), fingerprint)
            for alias, name, _addr, fingerprint, _protocol, _age in peers
        ]
        items = []
        for label, fingerprint in sorted(labelled, key=lambda p: p[0].casefold()):
            item = Nautilus.MenuItem(name=f"TuxDrop::Send::{fingerprint}", label=label)
            item.connect("activate", self._send, daemon, fingerprint, paths)
            items.append(item)
        return items

    def _announce(self, daemon):
        now = time.monotonic()
        if now - self.last_announce >= ANNOUNCE_INTERVAL_S:
            self.last_announce = now
            daemon.call(
                "Announce", None, Gio.DBusCallFlags.NONE, -1, None, _ignore_reply
            )

    def _send(self, _item, daemon, fingerprint, paths):
        daemon.call(
            "Send",
            GLib.Variant("(sas)", (fingerprint, paths)),
            Gio.DBusCallFlags.NONE,
            -1,
            None,
            _ignore_reply,
        )
