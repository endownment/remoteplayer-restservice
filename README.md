# remoteplayer-restservice

REST API for **remoteplayer**: a Raspberry Pi 3 attached to a TV that plays
video with mpv on the hardware decoder. The service controls that mpv
instance, manages its settings and the media library, and reports on the
hardware around it (audio outputs, display, HDMI-CEC, board health).

It is one of the remoteplayer components:

| Component | Role |
|---|---|
| `remoteplayer-mpv.service` | mpv itself, fullscreen on DRM, with a JSON IPC socket (unit in [`deploy/systemd`](deploy/systemd)) |
| **remoteplayer-restservice** | this repository: the HTTP API |
| [remoteplayer-mpvwatcher](https://github.com/endownment/remoteplayer-mpvwatcher) | follows mpv's state and keeps the snapshot to restore from (`last` on its socket), also across reboots when it can write |
| remoteplayer-webcontrol | the web UI (planned) |

Other mpv IPC clients (for example jellyfin-mpv-shim) can share the same mpv
socket; the service does not assume it is the only one talking to mpv.

## Features

- **Player** — typed commands (play from the library or a URL, pause, seek,
  volume, playlist, audio/subtitle tracks), full state, a WebSocket with live
  events, and a guarded raw IPC passthrough.
- **Settings** — every mpv option the service manages, changed through
  `PUT`/`PATCH` (JSON Merge Patch). Options mpv accepts at runtime are applied
  live; the others restart mpv and **restore playlist, position, pause, volume
  and tracks**. A failed restart rolls back to the previous settings.
- **Automatic restore** — when mpv dies and systemd restarts it, and when
  the Pi boots, playback is restored from the watcher's snapshot.
- **Read-only systems** — everything the service writes lives on one
  writable partition; without it the service runs read-only and says so
  (see "Read-only SD card").
- **Library** — first-level folders below one upload root; streaming uploads
  (raw `PUT` or multipart) with MD5, free-space checks and atomic commit;
  listing, moving, deleting; MD5 of any file, cached in an extended attribute.
- **Audio outputs** — mpv's real device list grouped per sound card, with
  the value that actually works on a Pi marked as recommended (HDMI, 3.5 mm
  jack, S/PDIF HATs, USB).
- **Display** — connectors, supported modes and TV identity (EDID) from DRM
  sysfs, plus the current mode from mpv.
- **HDMI-CEC** — polls the bus through the kernel CEC API and reports every
  device: role, physical address, name, vendor, power status.
- **System** — temperature, under-voltage/throttling flags, memory, load;
  reboot and power off.
- Bearer-token authentication, Problem Details (RFC 9457) errors, systemd
  `Type=notify`, sandboxed unit, mDNS announcement.

The full endpoint reference is in [docs/API.md](docs/API.md).

## Quick start on a development machine

No Raspberry Pi needed: mpv runs in a normal window, CEC and power control
use stubs. You need Rust (see [BUILD.md](BUILD.md)) and mpv ≥ 0.38.

```sh
cargo run -- --config config/dev.toml
```

In another terminal:

```sh
TOKEN=dev-token-0123456789abcdef
curl -s localhost:8080/api/v1/health
curl -s -H "Authorization: Bearer $TOKEN" -T movie.mkv localhost:8080/api/v1/folders/movies/files/movie.mkv
curl -s -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
     -d '{"type": "play", "folder": "movies", "name": "movie.mkv"}' localhost:8080/api/v1/player/command
curl -s -H "Authorization: Bearer $TOKEN" localhost:8080/api/v1/player/state
```

What is stubbed and how to test real hardware paths without a Pi is described
in [dev-notes/09-testing-without-pi.md](dev-notes/09-testing-without-pi.md).

## Installation on the Raspberry Pi

Target: Raspberry Pi OS (Debian 13 "trixie") Lite, mpv 0.40, user `coop`
(replace it in the unit files and the polkit rule if yours differs). Build the
binary first, see [BUILD.md](BUILD.md).

```sh
# Binary
sudo install -m 0755 target/release/remoteplayer-restservice /usr/local/bin/

# Configuration — then edit the token: openssl rand -hex 24
sudo install -d /etc/remoteplayer
sudo install -m 0640 -g coop config/restservice.toml /etc/remoteplayer/restservice.toml

# The writable part (see "Read-only SD card"): media library and settings.
# On a fully read-only system skip this; the service then runs read-only.
sudo install -d -o coop -g coop /srv/remoteplayer/media /srv/remoteplayer/state/restservice

# Groups: /dev/cec0 and vcgencmd (video), the mpv log (systemd-journal)
sudo usermod -aG video,render,audio,systemd-journal coop

# systemd units, polkit rule, mDNS announcement
sudo install -m 0644 deploy/systemd/remoteplayer-mpv.service /etc/systemd/system/
sudo install -m 0644 deploy/systemd/remoteplayer-restservice.service /etc/systemd/system/
sudo install -m 0644 deploy/polkit/50-remoteplayer.rules /etc/polkit-1/rules.d/
sudo install -m 0644 deploy/avahi/remoteplayer.service /etc/avahi/services/   # if avahi-daemon is installed

sudo systemctl daemon-reload
sudo systemctl enable --now remoteplayer-mpv.service remoteplayer-restservice.service
```

Install remoteplayer-mpvwatcher as well; without it there is no automatic
restore.

If you used the earlier hand-written `remoteplayer-mpd.service`, disable it
first: `sudo systemctl disable --now remoteplayer-mpd.service`. The new unit
keeps the same mpv command line, but the IPC socket moves to
`/run/remoteplayer-mpv/mpv.sock` and the tunable options come from
`/srv/remoteplayer/state/restservice/mpv.env`, which this service writes.

Check it:

```sh
systemctl status remoteplayer-restservice
curl -s http://raspberrypi.local:8080/api/v1/health
curl -s -H "Authorization: Bearer $TOKEN" http://raspberrypi.local:8080/api/v1/storage   # "mode": "read-write"?
journalctl -u remoteplayer-restservice -f
```

### Read-only SD card

To be able to unplug the Pi at any time, the SD card is mounted read-only
and everything that has to be written goes to one place, `/srv/remoteplayer`:

```
/srv/remoteplayer/
├── media/                      the library (storage.upload-root)
└── state/
    ├── restservice/            settings.json, mpv.env (read by remoteplayer-mpv.service)
    └── mpvwatcher/             state.json (the watcher's snapshot)
```

A typical setup:

1. A separate ext4 partition or a USB drive, mounted in `/etc/fstab`, e.g.
   `UUID=... /srv/remoteplayer ext4 defaults,noatime,nofail,x-systemd.device-timeout=10s 0 2`.
   `nofail` lets the Pi boot without it.
2. The root filesystem read-only, e.g. `raspi-config` → Performance Options →
   Overlay File System. Writes to the root then go to RAM and vanish at
   power-off. Check with `findmnt /srv/remoteplayer` that the partition is
   mounted from its device, not as part of the overlay.

Both services check where they can really write — they create and delete a
probe file, they do not trust permission bits — and adapt:

| | Writable partition | No write access, or not configured |
|---|---|---|
| uploads, folders, move/delete | work | 403 "read-only" / 501 "not configured"; listing and playback still work |
| settings | saved, applied | `PATCH`/`PUT` answer 403, nothing changes; mpv keeps running with what it has |
| restore after an mpv crash | yes | yes (from the watcher's memory) |
| restore after reboot or unplugging | yes | no: the watcher answers "state unknown", nothing is restored |

Two traps are detected explicitly:

- **RAM-backed filesystems.** On tmpfs or an overlay a write succeeds but
  lands in RAM. Uploads there would fill the Pi's 1 GB, so they are refused
  (`storage.allow-volatile` overrides this); `GET /storage`, `GET /settings`
  and the watcher's `status` report `"volatile": true`.
- **Stale files.** A state file found in a place the watcher cannot write is
  ignored: it may be arbitrarily old.

The units have no `StateDirectory=`, which systemd cannot create on a read-only
root, and list `/srv/remoteplayer` as an optional `ReadWritePaths=-…`, so they
start with or without the partition. An upload interrupted by a power cut
leaves a hidden partial file; it is removed at the next start.

### Why mpv's options live in an EnvironmentFile

The unit file is static and installed once by root. The service only writes
`/srv/remoteplayer/state/restservice/mpv.env`, which the unit reads through
`EnvironmentFile=` and expands as `$REMOTEPLAYER_MPV_ARGS`. So:

- the service never needs write access to `/etc/systemd` or `daemon-reload`;
- the settings cannot change *how* mpv runs (user, groups, sandbox), only
  its options — letting a service write unit files would amount to root;
- mpv is started with plain command-line options and no extra config file,
  so every IPC client sees the same, explicit mpv;
- values are validated (no whitespace or quoting characters), so a setting
  cannot smuggle extra arguments into the command line.

Enabling the unit at boot is part of the installation above for the same
reason: `EnableUnitFiles` cannot be limited to one unit by polkit.

## Configuration

[`config/restservice.toml`](config/restservice.toml) documents every key. The
main sections:

| Section | Purpose |
|---|---|
| `listen`, `[auth]` | address and bearer token (≥ 16 characters) |
| `[storage]` | upload root (optional), default folder, free-space reserve, size limit, `allow-volatile` |
| `[mpv]` | IPC socket, settings and EnvironmentFile paths, timeouts, raw IPC switch |
| `[service-manager]` | `systemd` (the Pi) or `child-process` (development) |
| `[cec]` | `linux` with a device, `stub`, or `disabled` |
| `[display]`, `[system]` | sysfs paths, preferred connector, power control |
| `[restore]` | automatic restore after a crash / on start, the watcher's socket |
| `[player-defaults]` | overrides of the built-in mpv defaults |

## Security notes

- The token protects everything except `/api/v1/health`. There is no TLS:
  keep the API on the home network, or put a reverse proxy in front of it.
- `POST /api/v1/player/ipc` reaches mpv directly. A deny-list blocks process
  spawning, script loading and file-writing options, but mpv's IPC is not a
  security boundary. Set `mpv.raw-ipc-enabled = false` if untrusted people can
  reach the API.
- The polkit rule grants the service user start/stop/restart of
  `remoteplayer-mpv.service` and reboot/power-off, nothing more.

## Repository layout

```
src/            the service (start reading at main.rs, then routes/mod.rs)
config/         restservice.toml for the Pi, dev.toml for a desktop
deploy/         systemd units, polkit rule, Avahi service file
docs/API.md     endpoint reference
dev-notes/      how it is built and why, written as Rust learning material
```
