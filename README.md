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

Then, as each user who wants to receive files:

```sh
systemctl --user enable --now tuxdropd
```

The packages ship firewall definitions but do not open anything. Port 53317 is used for UDP
discovery and TCP transfers:

```sh
sudo ufw allow from 192.168.0.0/16 to any app tuxDrop   # ufw, local network only
sudo firewall-cmd --permanent --add-service=tuxdrop && sudo firewall-cmd --reload  # firewalld
```

Fedora Workstation's default zone already accepts ports above 1024, so firewalld usually needs
nothing.

## Build packages

Requires Docker. Compiles in Debian bookworm (glibc 2.36) and packages with nfpm:

```sh
scripts/package.sh   # -> dist/tuxdrop_<version>_amd64.deb, dist/tuxdrop-<version>-1.x86_64.rpm
```

The version comes from `Cargo.toml` (`[workspace.package]`).

## Use

```sh
tuxdrop peers                       # nearby devices
tuxdrop send iPhone photo.jpg dir/  # alias, fingerprint prefix or IP[:port]
tuxdrop status                      # pending incoming transfers (daemon)
tuxdrop accept <id> / reject <id>   # also offered as notification buttons
tuxdrop recv                        # receive in the foreground, without the daemon
tuxdrop trust <fingerprint>         # auto-accept this device; see `tuxdrop trusted`
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
  (`ListPeers`, `ListPending`, `Accept`, `Reject`, `Send`; signals `IncomingRequest`, `Progress`,
  `TransferFinished`)
