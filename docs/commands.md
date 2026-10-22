# tuxDrop command reference

tuxDrop has two programs:

- **`tuxdropd`**: the daemon. It runs in your desktop session and receives files in the background.
  When a transfer arrives it asks you through a desktop notification.
- **`tuxdrop`**: the command-line tool. It sends files, lists nearby devices, manages trusted
  devices, and answers the daemon's pending requests. It can also receive on its own, in the
  foreground, when the daemon is not running.

Both speak the [LocalSend v2 protocol](https://github.com/localsend/protocol), so the other side can
be another tuxDrop or the LocalSend app (iPhone, Android, Windows, macOS). Both devices must be on
the same network.

- [How a transfer works](#how-a-transfer-works)
- [tuxdrop](#tuxdrop): [info](#tuxdrop-info) · [peers](#tuxdrop-peers) · [send](#tuxdrop-send) ·
  [recv](#tuxdrop-recv) · [status](#tuxdrop-status) · [accept / reject](#tuxdrop-accept--tuxdrop-reject) ·
  [trust / untrust / trusted](#tuxdrop-trust--untrust--trusted)
- [tuxdropd](#tuxdropd)
- [Files and configuration](#files-and-configuration)
- [Troubleshooting](#troubleshooting)

## How a transfer works

1. **Discovery.** A device multicasts a short JSON announcement (UDP `224.0.0.167:53317`) on
   every IPv4 interface. Devices that hear it answer with their alias, fingerprint and port.
2. **Request.** The sender asks the receiver over HTTPS (`prepare-upload`), listing the file
   names and sizes.
3. **Approval.** The receiver checks the request:
   - Any file name that could escape the download folder is refused, and the user is not asked.
   - The request is refused if there is not enough free space.
   - A trusted device is accepted at once. Otherwise the user is asked. With no answer after
     60 seconds, the transfer is declined.
4. **Upload.** The files are sent one after the other. Each one is written to a hidden
   `.tuxdrop-<token>.part` file, then checked for size (and SHA-256 when the sender provides it).
   It is then renamed to its final name. An existing file is never overwritten: the new one
   becomes `name (1).ext`, `name (2).ext`, and so on.

**Identity.** On first use each device creates a certificate. Its SHA-256 is the device's
**fingerprint**.
- The sender checks that the receiver's certificate matches the fingerprint it announced.
- A tuxDrop sender also presents its own certificate, which proves its fingerprint to the
  receiver. Such senders are shown as **verified**.
- The LocalSend apps present no certificate. They are always shown as **not verified** and can
  never be auto-accepted.

**Limits.** A receiver handles one transfer at a time; others get "busy". After a refusal or a
wrong PIN, the same address must wait 5 seconds; before that it gets "too many requests".

---

## tuxdrop

```
tuxdrop <COMMAND> [OPTIONS]
tuxdrop <COMMAND> --help
```

Errors are printed to stderr and the exit status is 1. Set `RUST_LOG=debug` for detailed logs.

### `tuxdrop info`

Shows how this device presents itself: alias, fingerprint, port, download folder and whether a PIN
is set.

```
$ tuxdrop info
alias:        my-laptop
fingerprint:  4CEE7EB4FCC499F69A1DEAAC2180E96434FA00638AD1B39404378DB6EAF86797
port:         53317
download dir: /home/me/Downloads/tuxDrop
PIN:          none
```

The first run creates the certificate and key. Give your fingerprint to someone who wants to add
you with `tuxdrop trust`.

### `tuxdrop peers`

```
tuxdrop peers [-t, --timeout <SECONDS>]     # default: 3
```

Lists the devices that answer on the local network.
- It sends one announcement, then listens for `--timeout` seconds.
- Each device appears once, as alias, address and the first 16 characters of its fingerprint.

```
$ tuxdrop peers
iPhone de Marie          192.168.1.34:53317                       9F2C41D07AB3E5C8
alpha                    192.168.1.212:53317                      4CEE7EB4FCC499F6
```

It works with or without the daemon: the UDP port is shared (`SO_REUSEPORT`). There is a known
limitation when `tuxdropd` is running on the same machine; see
[Troubleshooting](#troubleshooting).

### `tuxdrop send`

```
tuxdrop send [--pin <PIN>] [-t, --timeout <SECONDS>] <TARGET> <PATH>...
```

Sends one or more files or folders to one device.

**`TARGET`** can be:

| Form | Example | How it is found |
|---|---|---|
| alias (not case-sensitive) | `alpha`, `"iPhone de Marie"` | discovery, up to `--timeout` seconds (default 5) |
| fingerprint prefix, 6+ characters | `9F2C41` | discovery, as above |
| IP address | `192.168.1.34` | contacted directly on port 53317 |
| IP and port | `192.168.1.34:53318`, `[2001:db8::34]:53317` | contacted directly |

With an address, there is no discovery. tuxdrop asks the device for its fingerprint, then pins
that fingerprint for the transfer (trust on first use).

**`PATH`**: files and folders.
- A folder is sent recursively, and the receiver gets the same tree: sending `~/Photos/trip`
  creates `trip/…` on the other side.
- Symbolic links inside folders are skipped.

**`--pin`**: needed if the receiver has a PIN configured.

**What you see:**

```
$ tuxdrop send alpha report.pdf ~/Photos/trip
sending 12 file(s), 48.3 MiB to alpha; waiting for approval…
accepted
report.pdf: done
trip/IMG_0001.jpg:  57%
…
transfer complete
```

- **Waiting:** the command waits as long as the receiver takes to decide (60 seconds at most on
  a tuxDrop receiver).
- **Ctrl+C:** stops the transfer and tells the receiver, which deletes the file being received.
  Files already completed stay on the receiver.

**Errors:**

| Message | Meaning |
|---|---|
| `device "x" not found (try tuxdrop peers)` | nobody with that alias/prefix answered in time |
| `transfer declined by the receiver` | refused, or no answer within 60 s |
| `receiver requires a PIN, or the PIN is wrong` | use `--pin`, or check it |
| `receiver is busy with another transfer` | retry when the current transfer ends |
| `too many requests, retry in a few seconds` | 5 s cooldown after a refusal or wrong PIN |
| `receiver answered 400 Bad Request` | a file name was refused (e.g. contains `..`) |
| `receiver answered 507 Insufficient Storage` | not enough free space on the receiver |
| `peer certificate does not match its fingerprint` | the device is not who it announced; nothing was sent |

### `tuxdrop recv`

```
tuxdrop recv
```

Receives in the foreground, for when `tuxdropd` is not running (a server, an SSH session, a quick
test). It listens on the configured port (TCP for transfers, UDP for discovery) and announces
itself once. Received files go to the download folder.

For each request it prints the sender, its files and whether its identity is verified, then
asks:

```
bravo (192.168.1.50) wants to send 2 file(s), 19.1 MiB:
  big.bin  (19.1 MiB)
  note.txt  (3 B)
  verified fingerprint 52A8984B9E5EB8A251E21748A8BE9F51A42383B57C3C4AD9A91F423CFB81AE2D
Accept? [y]es / [N]o / [a]lways from this device:
```

- **`y`** accepts this transfer. **Enter** or anything else declines it.
- **`a`** (verified senders only) accepts it and adds the sender to the trusted devices.
- Without an answer within 55 seconds, the transfer is declined.
- Trusted devices are accepted without asking.

Stop with Ctrl+C. Stopping during a transfer can leave a hidden `.tuxdrop-*.part` file in the
download folder, which you can delete.

It fails with `port 53317 is taken` when `tuxdropd` or the LocalSend app is already running on
this machine. Only one program can receive on the port.

### `tuxdrop status`

```
tuxdrop status
```

Needs `tuxdropd`. It shows two things:
- **Pending requests:** the transfers waiting for your decision, with their id.
- **Nearby devices:** the devices the daemon has seen in the last 5 minutes.

```
$ tuxdrop status
pending 89d93ca544121debe565604c5150fd64  from bravo (verified): 1 file(s), 3 B
nearby devices:
  bravo                    192.168.1.50:53317                       52A8984B9E5EB8A2
```

Each call also makes the daemon announce itself, so devices that just answered appear on the next
call.

### `tuxdrop accept` / `tuxdrop reject`

```
tuxdrop accept <ID>
tuxdrop reject <ID>
```

Answers a pending request of `tuxdropd` without using the notification. Use them when
notifications are unavailable (SSH, no notification server). The `ID` comes from `tuxdrop
status`, or from the daemon log, which prints `use tuxdrop accept <id>` when it cannot show a
notification.

An id that is not pending (already answered, expired, or mistyped) gives
`no pending transfer <id>`.

### `tuxdrop trust` / `untrust` / `trusted`

```
tuxdrop trust <FINGERPRINT> [--alias <NAME>]
tuxdrop untrust <FINGERPRINT>
tuxdrop trusted
```

Manages the devices whose transfers are accepted without asking.
- **`trust`** adds a device. The fingerprint is the full 64-character value shown by
  `tuxdrop info` on that device, not case-sensitive. `--alias` is only a label for `trusted`.
- **`untrust`** removes it, and fails if it was not trusted.
- **`trusted`** lists the trusted devices.

The list lives in `~/.local/share/tuxdrop/trusted.json`. It is read at each request, so changes
apply immediately, including to a running daemon.

Trust only works with tuxDrop senders, because only they prove their fingerprint. A LocalSend app
always asks, even if its fingerprint is in the list.

You can also trust a device from a request itself: the **Always accept from this device** button
of the notification, or answer `a` in `tuxdrop recv`.

---

## tuxdropd

```
tuxdropd
```

The daemon has no options. It normally runs as a systemd user service:

```sh
systemctl --user enable --now tuxdropd   # start now and at each login
systemctl --user restart tuxdropd        # after editing config.toml
systemctl --user disable --now tuxdropd  # stop receiving
journalctl --user -u tuxdropd -f         # logs
```

It reads `config.toml` only at startup. Set `RUST_LOG` (default `info`) for more or fewer logs,
e.g. `systemctl --user edit tuxdropd` and add `Environment=RUST_LOG=debug`.

**What it does:**

1. Loads the configuration and the device identity, creating the identity on first run.
2. Listens on the configured port: TCP (IPv4 and IPv6) for transfers, UDP multicast for
   discovery. It announces itself once at startup and answers other devices' announcements.
3. Registers `org.tuxdrop.Daemon` on the D-Bus session bus. Only one daemon can run per user.
   With several users logged in at once, only the first one gets the port.
4. For each incoming request:
   - **Trusted device:** a "Receiving from …" notification, and the transfer starts.
   - **Any other device:** a persistent notification listing the first files, the total size,
     and "Sender identity not verified" when it applies. Buttons: **Accept**, **Decline**, and
     **Always accept from this device** (verified senders only).
   - **No answer within 60 seconds:** declined, and the notification is closed.
5. When a transfer ends, a notification says "Files received" (with the folder), "Transfer
   cancelled" or "Transfer failed" (with the reason).

### D-Bus API

The daemon exposes `org.tuxdrop.Daemon1` at `/org/tuxdrop/Daemon` on the session bus. `tuxdrop
status/accept/reject` use it, and so can scripts or a future graphical interface.

| Member | Signature | Description |
|---|---|---|
| `ListPeers()` | → `a(sss)` | nearby devices: alias, address, fingerprint. Also re-announces. |
| `ListPending()` | → `a(ssbut)` | pending requests: id, alias, verified, file count, total bytes |
| `Accept(id)` | `s` → | accept a pending request |
| `Reject(id)` | `s` → | decline a pending request |
| `Send(target, paths)` | `sas` → `s` | start sending in the background; returns a transfer id |
| signal `IncomingRequest` | `ssbut` | id, alias, verified, file count, total bytes |
| signal `Progress` | `sstt` | id, file name, bytes done, file size |
| signal `TransferFinished` | `ss` | id, `completed` / `cancelled` / `failed: <reason>` |

`Progress` and `TransferFinished` cover both incoming transfers (id = request id) and outgoing ones
(id returned by `Send`).

For `Send`, `target` takes the same forms as [`tuxdrop send`](#tuxdrop-send). Use absolute paths,
because the daemon does not run in your current directory. It cannot send with a PIN, and cannot
cancel a transfer once started.

Examples with `busctl`:

```sh
busctl --user call org.tuxdrop.Daemon /org/tuxdrop/Daemon org.tuxdrop.Daemon1 ListPending
busctl --user call org.tuxdrop.Daemon /org/tuxdrop/Daemon org.tuxdrop.Daemon1 \
    Send sas alpha 1 "$PWD/report.pdf"
busctl --user monitor org.tuxdrop.Daemon      # watch the signals
```

---

## Files and configuration

| Path | Content |
|---|---|
| `~/.config/tuxdrop/config.toml` | settings (optional) |
| `~/.local/share/tuxdrop/cert.pem`, `key.pem` | device identity; the key is private (mode 0600). Deleting them creates a new fingerprint, and devices that trusted you must trust you again. |
| `~/.local/share/tuxdrop/trusted.json` | trusted devices |
| `~/Downloads/tuxDrop/` | default download folder (follows `XDG_DOWNLOAD_DIR`) |
| `/usr/lib/systemd/user/tuxdropd.service` | user service (packages) |
| `/etc/ufw/applications.d/tuxdrop`, `/usr/lib/firewalld/services/tuxdrop.xml` | firewall definitions, not enabled by default |

`config.toml`: every key is optional, and an unknown key is an error.

```toml
alias = "my-laptop"                          # name shown to others; default: hostname
download_dir = "/home/me/Downloads/tuxDrop"  # created if missing
port = 53317                                 # TCP and UDP; other devices expect 53317
pin = "1234"                                 # senders must give it (tuxdrop send --pin)
```

`tuxdrop` and `tuxdropd` share these files, so they have the same alias and fingerprint.

---

## Troubleshooting

**No device found / other devices do not see me.**
- Check the firewall first; the port must accept UDP and TCP from the local network:
  `sudo ufw allow from 192.168.0.0/16 to any app tuxDrop`.
- Guest Wi-Fi and some company networks block traffic between clients.
- As a last resort, send to the IP address directly (`tuxdrop send 192.168.1.34 file`).

**`tuxdrop peers` / `send <alias>` miss devices while `tuxdropd` is running.**
Other tuxDrop or LocalSend devices answer an announcement by contacting the TCP port of the
machine that sent it. When the daemon is running, the daemon gets that answer, not the CLI. Use
`tuxdrop status` to see the devices the daemon knows, or send to the IP address.

**`port 53317 is taken`.** `tuxdrop recv` cannot run next to `tuxdropd` or the LocalSend app.
Stop one of them.

**`tuxdropd is not running`.** Start it with `systemctl --user start tuxdropd`.

**No notification appears.** The daemon then logs `use tuxdrop accept <id>`. Use
`tuxdrop status` and `tuxdrop accept`.

**`cannot reach the D-Bus session bus`.** `tuxdropd` and `status/accept/reject` must run inside a
user session; in a bare SSH session there is no session bus. Use `tuxdrop recv` there instead.
