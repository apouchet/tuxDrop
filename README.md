# tuxDrop

AirDrop-like file sharing for Linux, speaking the [LocalSend v2 protocol](https://github.com/localsend/protocol):
works with the LocalSend app on iPhone, Android, Windows and macOS on the same network.

## Install

```sh
cargo install --path crates/tuxdrop
cargo install --path crates/tuxdropd
install -Dm644 packaging/tuxdropd.service ~/.config/systemd/user/tuxdropd.service
systemctl --user daemon-reload
systemctl --user enable --now tuxdropd
```

With ufw enabled, open the LocalSend port (discovery is UDP, transfers are TCP):

```sh
sudo ufw allow 53317
```

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
