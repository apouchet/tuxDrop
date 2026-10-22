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
  [trust / untrust / trusted](#tuxdrop-trust--untrust--trusted) · [name](#tuxdrop-name)
- [tuxdropd](#tuxdropd): [tray icon](#tray-icon) · [file manager menu](#file-manager-menu) ·
  [D-Bus API](#d-bus-api)
- [Files and configuration](#files-and-configuration)
- [Troubleshooting](#troubleshooting)

## How a transfer works

1. **Discovery.** A device multicasts a short JSON announcement (UDP `224.0.0.167:53317`) on
   every IPv4 interface. Devices that hear it answer with their alias, fingerprint and port:
   - **By HTTPS:** they `register` on the TCP port of the announcing device.
   - **By UDP:** used when the HTTPS answer is impossible; sent both to the multicast group and
     directly to the announcer.

   Wi-Fi access points deliver multicast to their clients without retransmission; on a laptop
   with Wi-Fi power saving, about half of the announcements are lost. So when nobody (or not the
   wanted device) answers within 1.5 seconds, the announcement is sent **once** more by unicast
   to every address of the local networks. Only physical interfaces are covered, and at most
   the /24 around each address.
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

Lists the devices that answer on the local network, each once, as alias, address and the first
16 characters of its fingerprint. A device you named with [`tuxdrop name`](#tuxdrop-name) shows
as `Name (alias)`.

- **When `tuxdropd` runs**, the daemon announces and collects the answers, because other
  devices answer on its port. `peers` waits `--timeout` seconds, then shows the devices that
  answered.
- **Without the daemon**, for example over SSH with no session bus, `peers` announces and
  listens itself.

In both cases the unicast fallback described in
[How a transfer works](#how-a-transfer-works) applies.

```
$ tuxdrop peers
iPhone de Marie          192.168.1.34:53317                       9F2C41D07AB3E5C8
alpha                    192.168.1.212:53317                      4CEE7EB4FCC499F6
```

### `tuxdrop send`

```
tuxdrop send [--pin <PIN>] [-t, --timeout <SECONDS>] <TARGET> <PATH>...
```

Sends one or more files or folders to one device.

**`TARGET`** can be:

| Form | Example | How it is found |
|---|---|---|
| local name or alias (not case-sensitive) | `alpha`, `"iPhone de Marie"` | the daemon's list of devices; if absent, discovery for up to `--timeout` seconds (default 5) |
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
| `transfer cancelled by the receiver` | the receiver pressed Cancel during the transfer |
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
devices seen in the last 5 minutes:
  bravo                    192.168.1.50:53317                       52A8984B9E5EB8A2  12s ago
```

`status` sends nothing on the network. It shows the daemon's list as it is; use `tuxdrop peers`
to refresh it.

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
tuxdrop trust <DEVICE> [--alias <ALIAS>]
tuxdrop untrust <DEVICE>
tuxdrop trusted
```

Manages the devices whose transfers are accepted without asking.
- **`trust`** adds a device. `DEVICE` is either the full 64-character fingerprint shown by
  `tuxdrop info` on that device (not case-sensitive), or a nearby device given as for
  [`tuxdrop send`](#tuxdrop-send): local name, alias, fingerprint prefix or address. Anything
  else is an error. The device's alias is recorded with it; `--alias` replaces it.
- **`untrust`** removes a trusted device, given by fingerprint, local name, alias or fingerprint
  prefix (6+ characters). It fails if no trusted device matches, or if several do.
- **`trusted`** lists the trusted devices.

```
$ tuxdrop trust bravo
trusted bravo (52A8984B9E5EB8A2F144A6907DC4284D1F9FE6A7D9B9FF53C02C1D07BA68F24D)
```

The tray icon has a **Trusted** checkbox for each nearby device that does the same.

The list lives in `~/.local/share/tuxdrop/trusted.json`. It is read at each request, so changes
apply immediately, including to a running daemon.

Trust only works with tuxDrop senders, because only they prove their fingerprint. A LocalSend app
always asks, even if its fingerprint is in the list.

You can also trust a device from a request itself: the **Always accept from this device** button
of the notification, or answer `a` in `tuxdrop recv`.

### `tuxdrop name`

```
tuxdrop name <DEVICE> [NAME]
```

Gives a device a name of your own, for devices whose alias is unclear (`DESKTOP-4F2K`,
`localhost`). The name stays on this machine; the device keeps announcing its own alias.
- **`DEVICE`**: as for `trust`. A trusted device can be named even when it is not around.
- **Without `NAME`**, the name is removed and the alias is shown again.

The name is then shown as `Name (alias)` by `peers`, `status`, `trusted`, the tray icon and the
Files menu, and can be used as a target by `send`, `trust` and `untrust`. Incoming-transfer
notifications use it only for verified senders, because anyone can claim a fingerprint.
Control characters are removed and names are cut at 64 characters.

```
$ tuxdrop name bravo "PC du salon"
52A8984B9E5EB8A2F144A6907DC4284D1F9FE6A7D9B9FF53C02C1D07BA68F24D is now named "PC du salon"
$ tuxdrop send "pc du salon" photo.jpg
```

Names live in `~/.local/share/tuxdrop/names.json`, read at each use. The tray icon's
**Rename…** entry does the same.

---

## tuxdropd

```
tuxdropd
```

The daemon has no options. It normally runs as a systemd user service:

```sh
systemctl --user enable --now tuxdropd   # start now and at each login (packages enable it for everyone)
systemctl --user restart tuxdropd        # after editing config.toml
systemctl --user disable --now tuxdropd  # stop receiving
journalctl --user -u tuxdropd -f         # logs
```

It reads `config.toml` only at startup. Set `RUST_LOG` (default `info`) for more or fewer logs,
e.g. `systemctl --user edit tuxdropd` and add `Environment=RUST_LOG=debug`.

**What it does:**

1. Loads the configuration and the device identity, creating the identity on first run.
2. Listens on the configured port: TCP (IPv4 and IPv6) for transfers, UDP for discovery. It
   announces itself once at startup, answers other devices' announcements, and keeps the list
   of devices seen in the last 5 minutes.
3. Registers `org.tuxdrop.Daemon` on the D-Bus session bus. Only one daemon can run per user.
   With several users logged in at once, only the first one gets the port.
4. For each incoming request:
   - **Trusted device:** the transfer starts at once.
   - **Any other device:** a persistent notification listing the first files, the total size,
     and "Sender identity not verified" when it applies. Buttons: **Accept**, **Decline**, and
     **Always accept from this device** (verified senders only). The same choices are in the
     [tray menu](#tray-icon).
   - **No answer within 60 seconds:** declined, and the notification is closed.
5. During a transfer, in either direction, a notification "Receiving from …" or "Sending to …"
   shows the percentage and the current file, refreshed every 5 %, at most once a second
   (servers that support it also draw a bar). Its **Cancel** button stops the transfer: the
   file being received is deleted, files already completed stay.
6. When a transfer ends, the same notification turns into:
   - **Received:** "Files received" with the folder, and an **Open folder** button that shows
     the received files (or the top folder of a folder) selected in the file manager.
   - **Sent:** "Sent to …".
   - **Otherwise:** "Transfer cancelled" (by you or by the sender), "Transfer failed" or
     "Sending to … failed" with the reason. An error before an outgoing transfer starts
     (device gone, unreadable file) gives "Sending failed".
7. **Text messages** sent from the LocalSend app (its "Message" option) are shown as "Message
   from …" with a **Copy** button; nothing is saved. Copy uses `wl-copy` (package
   wl-clipboard) on Wayland, `xclip` on X11. One address can show at most one message every
   2 seconds.
8. Shows a [tray icon](#tray-icon) listing the nearby devices.

### Tray icon

Clicking the icon lists the devices seen in the last 5 minutes and asks nearby devices to
answer; new ones appear in the open menu. **Refresh** asks again. **Open received files**
opens the download folder.

While a request waits for your answer, the icon asks for attention and the menu starts with
"… wants to send N file(s)" followed by **Accept**, **Decline** and, for verified senders,
**Always accept from this device**, as in the notification.

Each device is shown as its alias, or `Name (alias)` once named, followed by `· trusted` when
it is trusted. Its submenu has:
- **Fingerprint …**: the first 16 characters, to compare with `tuxdrop info` on the device.
- **Rename…**: opens a dialog (zenity) to give it a [local name](#tuxdrop-name). Leaving the
  field empty, or typing its alias, removes the name. Without zenity, a notification says so.
- **Trusted (accept without asking)**: checkbox, same as `tuxdrop trust` / `untrust`. It has no
  effect on LocalSend apps, which cannot prove their identity and are always asked.

Names and trust changed with the CLI show up the next time the menu is opened.

On GNOME the icon needs the AppIndicator extension (see the README); without it the daemon logs
`no system tray` and the icon appears as soon as the extension is enabled.

### File manager menu

In Files (Nautilus), right-click files or folders, then **tuxDrop**: the submenu lists the devices
known to the daemon, with their local name if any. Clicking one calls `Send` with the device fingerprint and the selected
paths. Opening the menu also calls `Announce` (at most every 5 seconds), so a device that was
missing appears when the menu is opened again.

- **"tuxdropd is not running"**: start the daemon.
- **"No device found"**: wait a couple of seconds and right-click again.
- Only local files are offered; the entry does not appear on network locations.

The extension is `/usr/share/nautilus-python/extensions/tuxdrop.py` and needs nautilus-python.
After installing or updating it, restart Files with `nautilus -q`.

### D-Bus API

The daemon exposes `org.tuxdrop.Daemon1` at `/org/tuxdrop/Daemon` on the session bus. `tuxdrop
status/accept/reject` use it, and so can scripts or a future graphical interface.

| Member | Signature | Description |
|---|---|---|
| `ListPeers()` | → `a(sssssu)` | devices seen in the last 5 minutes: alias, local name (empty if none), address, fingerprint, `https`/`http`, seconds since last seen |
| `Announce()` | → | ask nearby devices to answer (with the unicast fallback); they appear in `ListPeers` within ~2 s |
| `ListPending()` | → `a(ssbut)` | pending requests: id, alias, verified, file count, total bytes |
| `Accept(id)` | `s` → | accept a pending request |
| `Reject(id)` | `s` → | decline a pending request |
| `Send(target, paths)` | `sas` → `s` | start sending in the background; returns a transfer id |
| `Cancel(id)` | `s` → | cancel a transfer in progress: incoming (request id) or outgoing (id returned by `Send`) |
| signal `IncomingRequest` | `ssbut` | id, alias, verified, file count, total bytes |
| signal `Progress` | `sstt` | id, file name, bytes done, file size |
| signal `TransferFinished` | `ss` | id, `completed` / `cancelled` / `failed: <reason>` |

`Progress` and `TransferFinished` cover both incoming transfers (id = request id) and outgoing ones
(id returned by `Send`).

For `Send`, `target` takes the same forms as [`tuxdrop send`](#tuxdrop-send). Use absolute paths,
because the daemon does not run in your current directory. It cannot send with a PIN.

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
| `~/.local/share/tuxdrop/names.json` | names you gave to devices (`tuxdrop name`, tray **Rename…**) |
| `~/Downloads/tuxDrop/` | default download folder (follows `XDG_DOWNLOAD_DIR`) |
| `/usr/lib/systemd/user/tuxdropd.service` | user service (packages) |
| `/usr/share/nautilus-python/extensions/tuxdrop.py` | Files right-click menu (packages) |
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

**Discovery works one way only, or only sometimes.** See what each side receives with debug
logs:
- **CLI:** `RUST_LOG=tuxdrop_core=debug tuxdrop peers`
- **Daemon:** `systemctl --user edit tuxdropd`, add `Environment=RUST_LOG=tuxdrop_core=debug`,
  restart it, and watch `journalctl --user -u tuxdropd -f`.

The lines to look for:
- `announcement from <ip>`: a datagram was received.
- `register from <ip>`: an HTTPS answer arrived.
- `answered <ip> by register` or `answering by UDP`: what this device sent back.
- `sweeping N addresses`: the unicast fallback was used.

**`port 53317 is taken`.** `tuxdrop recv` cannot run next to `tuxdropd` or the LocalSend app.
Stop one of them.

**`tuxdropd is not running`.** Start it with `systemctl --user start tuxdropd`.

**No notification appears.** The daemon then logs `use tuxdrop accept <id>`. Use
`tuxdrop status` and `tuxdrop accept`.

**`cannot reach the D-Bus session bus`.** `tuxdropd` and `status/accept/reject` must run inside a
user session; in a bare SSH session there is no session bus. Use `tuxdrop recv` there instead.
