# tuxDrop

AirDrop-like file sharing for Linux and macOS, speaking the
[LocalSend v2 protocol](https://github.com/localsend/protocol): works with the LocalSend app on iPhone,
Android, Windows and macOS on the same network. Windows support is prepared in the code but not
finished (see [Platforms](#platforms)).

## Install (Linux)

Packages (Ubuntu 24.04+ / Debian 12+, Fedora 37+):

```sh
sudo apt install ./tuxdrop_<version>_amd64.deb     # Ubuntu / Debian
sudo dnf install ./tuxdrop-<version>-1.x86_64.rpm  # Fedora
```

Without a package:

```sh
cargo build --release
sudo install -m755 target/release/tuxdrop target/release/tuxdropd /usr/local/bin/
sudo install -Dm644 packaging/tuxdropd.service /usr/lib/systemd/user/tuxdropd.service
```

The packages enable the daemon for every user (`systemctl --global enable`): it starts at the
next login. To start it right away, or after a manual install:

```sh
systemctl --user enable --now tuxdropd
```

To opt out, mask it (a disable would come back at the next package upgrade):
`systemctl --user mask tuxdropd` for yourself, `sudo systemctl --global mask tuxdropd` for
everyone.

The packages ship firewall definitions but do not open anything. Port 53317 is used for UDP
discovery and TCP transfers:

```sh
sudo ufw allow from 192.168.0.0/16 to any app tuxDrop   # ufw, local network only
sudo firewall-cmd --permanent --add-service=tuxdrop && sudo firewall-cmd --reload  # firewalld
```

Fedora Workstation's default zone already accepts ports above 1024, so firewalld usually needs
nothing.

## Desktop integration

- **Notifications**: transfers show their progress with a **Cancel** button; received files
  get **Open folder**; a text message sent from the LocalSend app is shown with **Copy**
  (needs wl-clipboard, or xclip on X11).
- **Tray icon**: `tuxdropd` shows an icon listing the nearby devices. Each device's submenu
  renames it (local name, needs zenity) and trusts it. A request waiting for an answer is shown
  at the top with Accept / Decline, and **Open received files** opens the download folder.
  GNOME needs the AppIndicator extension:
  - Ubuntu: preinstalled; if disabled, `gnome-extensions enable ubuntu-appindicators@ubuntu.com`.
  - Fedora: `sudo dnf install gnome-shell-extension-appindicator`, log out and in, then
    `gnome-extensions enable appindicatorsupport@rgcjonas.gmail.com`.
  - KDE Plasma: works as is.
- **Right-click in Files (Nautilus)**: a **tuxDrop** submenu on files and folders lists the
  nearby devices; click one to send. It needs nautilus-python (installed as a recommended
  package: `python3-nautilus` on Ubuntu, `nautilus-python` on Fedora). Restart Files once after
  installing: `nautilus -q`.

  Without a package: `install -Dm644 packaging/nautilus/tuxdrop.py
  ~/.local/share/nautilus-python/extensions/tuxdrop.py`.

Both need the daemon (`systemctl --user enable --now tuxdropd`).

## Install (macOS)

**From the disk image**: open `tuxDrop-<version>-macos-<arch>.dmg`, drag **tuxDrop** onto
**Applications**, then open it from Applications. It shows in the menu bar; tick **Open at Login**
in its menu to start it at each login. The Share menu entry is set up on first launch. For the
`tuxdrop` command:
`sudo ln -s /Applications/tuxDrop.app/Contents/MacOS/tuxdrop /usr/local/bin/tuxdrop`.

The disk image is not notarized by Apple unless built with a Developer ID (see below). If it
was downloaded, macOS refuses to open the app the first time: go to **System Settings > Privacy &
Security** and click **Open Anyway** (or run `xattr -dr com.apple.quarantine
/Applications/tuxDrop.app`).

**From the sources** requires Rust (`rustup`). Builds `tuxDrop.app`, installs it in `/Applications`, starts it at each
login (LaunchAgent `org.tuxdrop.tuxdropd`), adds tuxDrop to the Share menu and links the
`tuxdrop` command. Needs Xcode or the Command Line Tools (`xcode-select --install`) for the Share
menu extension:

```sh
scripts/install-macos.sh             # build, install and start
scripts/install-macos.sh uninstall   # stop and remove (keeps identity and settings)
```

On first use macOS asks to let tuxDrop find devices on the local network, then to use the
Downloads folder: accept both. If the macOS firewall is on, allow incoming connections for
tuxDrop. Logs go to `~/Library/Logs/tuxdrop.log`.

- **Menu bar icon**: nearby devices, each with **Send Files…**, **Send Folder…**, **Rename…** and
  **Trusted**; transfers in progress with their percentage and **Cancel**; a waiting request with
  **Accept** / **Decline** (the icon then shows a dot). Pointing at the icon refreshes the devices.
- **Share menu**: right-click files or folders in Finder, **Share > tuxDrop**: a window lists
  the nearby devices; pick one and click **Send**.
- **Incoming request**: a dialog with **Accept**, **Decline** and **Always Accept** (verified
  senders), declined after 60 seconds. The menu bar and `tuxdrop accept` close it.
- **Notifications**: banners when a transfer starts, ends or fails. macOS banners cannot carry
  buttons for an unsigned app, so a received text message is shown in a dialog with **Copy**,
  and received files are opened from **Open Received Files**.

`scripts/package-macos.sh` only builds `dist/tuxDrop.app`, the disk image
`dist/tuxDrop-<version>-macos-<arch>.dmg` and a zip of the app. Options: `UNIVERSAL=1` for an
arm64 + x86_64 build; `CODESIGN_IDENTITY="Developer ID Application: …"` to sign for
distribution (otherwise ad-hoc, which only this Mac trusts) and `NOTARY_PROFILE=<profile>` to
also notarize the disk image (`xcrun notarytool store-credentials` creates the profile).

## Platforms

| | Linux | macOS | Windows |
|---|---|---|---|
| CLI (`send`, `recv`, `peers`, `trust`, `name`…) | yes | yes | prepared |
| Daemon control (`status`, `accept`, `reject`) | D-Bus | local socket | named pipe, prepared |
| Request with buttons | notification | dialog | tray menu only |
| Progress, Cancel | notification and tray | menu bar | tray menu |
| Tray / menu bar icon | StatusNotifierItem | yes | yes (tray-icon) |
| Send from the file manager | Nautilus, tray | Finder Share menu, menu bar | tray: not yet (no file dialog) |
| Copy a received message | wl-copy / xclip | pbcopy | not yet |
| Autostart | systemd user service | LaunchAgent | not yet |

Everything OS-specific is in `crates/tuxdrop-core/src/sys.rs` (core) and
`crates/tuxdropd/src/platform/` (desktop): `freedesktop/` for Linux and BSD, `native/` for macOS
(`macos.rs`) and Windows (`windows.rs`, which lists what is left to do).
Check that other targets still compile with `cargo check --target x86_64-unknown-linux-gnu` or
`x86_64-pc-windows-msvc`.

## Build packages

Requires Docker. Compiles in Debian bookworm (glibc 2.36) and packages with nfpm:

```sh
scripts/package.sh   # -> dist/tuxdrop_<version>_amd64.deb, dist/tuxdrop-<version>-1.x86_64.rpm
```

The version comes from `Cargo.toml` (`[workspace.package]`).

## Use

Full reference for every command, the daemon and its D-Bus API: [docs/commands.md](docs/commands.md).

```sh
tuxdrop peers                       # nearby devices
tuxdrop send iPhone photo.jpg dir/  # local name, alias, fingerprint prefix or IP[:port]
tuxdrop status                      # pending incoming transfers (daemon)
tuxdrop accept <id> / reject <id>   # also offered as notification buttons
tuxdrop recv                        # receive in the foreground, without the daemon
tuxdrop trust <device>              # auto-accept this device; see `tuxdrop trusted`
tuxdrop name <device> "Salon"       # your own name for a device
tuxdrop info                        # own alias and fingerprint
```

Configuration (optional), `~/.config/tuxdrop/config.toml` (macOS:
`~/Library/Application Support/tuxdrop/config.toml`):

```toml
alias = "my-laptop"                       # default: hostname
download_dir = "/home/me/Downloads/tuxDrop"
port = 53317
pin = "1234"                              # senders must provide it
```

## Security model

- Each device has a self-signed certificate (`~/.local/share/tuxdrop/`, macOS:
  `~/Library/Application Support/tuxdrop/`, key mode 0600); its SHA-256
  is the LocalSend fingerprint. Senders pin the receiver's certificate to the announced fingerprint.
- tuxDrop senders also present their certificate to the receiver. Only a fingerprint proven this
  way can be trusted for auto-accept; the `fingerprint` field of the JSON request is never trusted
  on its own. Official LocalSend apps present no certificate, so they always ask.
- Received names are confined to the download directory (no `..`, no following symlinks), written
  to a `.part` file, checked against size and optional SHA-256, then renamed without overwriting
  (`name (1).ext`).
- One transfer at a time (409), a cooldown after a refusal or a wrong PIN (429), and a free-space
  check before accepting.

## Layout

- `crates/tuxdrop-core`: protocol, discovery, HTTPS server/client, receive logic, trust store,
  OS helpers (`sys.rs`), local control socket (`ipc.rs`)
- `crates/tuxdrop`: CLI
- `crates/tuxdropd`: daemon with notifications, tray icon and its control API: the
  `org.tuxdrop.Daemon1` session D-Bus API on Linux (`ListPeers`, `Announce`, `ListPending`,
  `Accept`, `Reject`, `Send`, `Cancel`; signals `IncomingRequest`, `Progress`,
  `TransferFinished`), the same requests on a local socket on macOS and Windows
