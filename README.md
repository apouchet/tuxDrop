# tuxDrop

AirDrop-like file sharing for Linux, speaking the [LocalSend v2 protocol](https://github.com/localsend/protocol):
works with the LocalSend app on iPhone, Android, Windows and macOS on the same network.

## Install

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

Configuration (optional), `~/.config/tuxdrop/config.toml`:

```toml
alias = "my-laptop"                       # default: hostname
download_dir = "/home/me/Downloads/tuxDrop"
port = 53317
pin = "1234"                              # senders must provide it
```

## Security model

- Each device has a self-signed certificate (`~/.local/share/tuxdrop/`, key mode 0600); its SHA-256
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

- `crates/tuxdrop-core`: protocol, discovery, HTTPS server/client, receive logic, trust store
- `crates/tuxdrop`: CLI
- `crates/tuxdropd`: daemon with notifications and the `org.tuxdrop.Daemon1` session D-Bus API
  (`ListPeers`, `Announce`, `ListPending`, `Accept`, `Reject`, `Send`; signals `IncomingRequest`, `Progress`,
  `TransferFinished`)
