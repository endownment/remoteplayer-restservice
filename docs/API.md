# remoteplayer-restservice API (v1)

Base path: `/api/v1`. Requests and responses are JSON with kebab-case keys;
setting keys are mpv option names, so `audio-device` is `--audio-device`.

**Authentication.** Every endpoint except `GET /health` requires
`Authorization: Bearer <token>`. WebSocket clients that cannot set headers may
pass `?access_token=<token>` instead.

**Errors** use [RFC 9457 Problem Details](https://www.rfc-editor.org/rfc/rfc9457)
with `Content-Type: application/problem+json`:

```json
{ "type": "about:blank", "title": "Conflict", "status": 409, "detail": "`movies/a.mkv` already exists" }
```

Failures around restarting mpv add a `log` array with mpv's last log lines.

| Status | Meaning |
|---|---|
| 400 | malformed request, invalid name, checksum mismatch |
| 401 | missing or wrong token |
| 403 | no write access (read-only mount, no permission, RAM-backed upload root), path outside the upload root, raw IPC command blocked or disabled |
| 404 | folder or file not found; the watcher knows no state to restore |
| 409 | already exists, folder not empty, another restart in progress |
| 413 | upload larger than `storage.max-upload-bytes` |
| 422 | invalid setting or command value, mpv refused a typed command |
| 501 | the feature is not configured (media library, CEC, power, watcher) |
| 502 / 503 / 504 | mpv, systemd, CEC or the watcher failed / unreachable (also: upload root not mounted) / timed out |
| 507 | not enough free space |

Examples below use:

```sh
API=http://raspberrypi.local:8080/api/v1
AUTH="Authorization: Bearer $TOKEN"
JSON="Content-Type: application/json"
```

---

## Health

### `GET /health` — no authentication

```json
{ "status": "ok", "version": "0.1.0", "mpv-connected": true }
```

---

## Settings

The managed mpv options, their built-in defaults (the tested Raspberry Pi 3
command line) and how each one is applied:

| Key | Default | Applied |
|---|---|---|
| `audio-device` | `alsa/hdmi:CARD=vc4hdmi,DEV=0` | live |
| `audio-spdif` | `""` (off), e.g. `ac3,dts` for passthrough | live |
| `vo` | `gpu` | restart |
| `gpu-context` | `drm` | restart |
| `hwdec` | `v4l2m2m` | live |
| `drm-draw-plane` | `overlay` | restart |
| `drm-drmprime-video-plane` | `primary` | restart |
| `drm-mode` | `""` (the TV's preferred mode), e.g. `1920x1080@50` | restart |
| `fullscreen` | `true` | live |
| `force-window` | `true` | restart |
| `script-opts` | `osc-idlescreen=no` | restart |
| `demuxer-max-bytes` | `40MiB` | live |
| `demuxer-max-back-bytes` | `10MiB` | live |

An empty string means "do not pass this option to mpv". Values may not contain
whitespace, quotes, `\`, `$` or backticks.

### `GET /settings`

```json
{
  "settings": { "audio-device": "alsa/hdmi:CARD=vc4hdmi,DEV=0", "vo": "gpu", "...": "..." },
  "defaults": { "...": "..." },
  "apply-modes": { "audio-device": "runtime", "vo": "restart", "...": "..." },
  "persistence": { "writable": true, "volatile": false, "reason": null }
}
```

`persistence` is probed on every request. With `"writable": false` (settings
directory read-only, missing or not permitted; `reason` says which) every
`PATCH`/`PUT` that would change something is answered with 403 and nothing
changes — neither the files nor the running mpv. `"volatile": true` means the
settings are saved to RAM (tmpfs/overlay) and are lost at power-off.

### `PATCH /settings` — change some settings

JSON Merge Patch: missing keys stay unchanged, `null` resets a key to its default.

```sh
curl -X PATCH -H "$AUTH" -H "$JSON" -d '{"audio-device": "alsa/plughw:CARD=Headphones,DEV=0"}' $API/settings
```

### `PUT /settings` — replace all settings

Missing keys get their default value: `PUT {}` restores all defaults.

### Response of `PATCH` and `PUT`

```json
{
  "settings": { "...": "the settings now in effect" },
  "changes": [
    { "key": "audio-device", "from": "alsa/hdmi:CARD=vc4hdmi,DEV=0", "to": "alsa/plughw:CARD=Headphones,DEV=0",
      "apply-mode": "runtime", "applied": "live" }
  ],
  "restart": null,
  "warnings": []
}
```

`applied` is one of `live`, `restart` or `next-start` (mpv is not
reachable; the value is used when it starts). If mpv refuses a live value, the request fails with 422, live
changes made by the same request are undone and nothing is saved. When any
changed key needs a restart, mpv is restarted once, the playback state is
restored and `restart` reports it:

```json
"restart": { "generation": 4, "restored-items": 3, "restore-warnings": [] }
```

If mpv does not come back with the new settings — it exits on an unknown
option or a malformed value, or, with `force-window`, its video output does
not open within 5 seconds — the previous settings are written back, mpv is
restarted with them, and the request fails with 502 and mpv's log.

---

## Player

### `GET /player/state`

```json
{
  "connected": true, "pid": 812, "mpv-version": "mpv 0.40.0",
  "idle": false, "paused": false,
  "path": "/srv/remoteplayer/media/movies/a.mkv", "media-title": "a.mkv",
  "time-pos": 83.2, "duration": 5400.0, "percent-pos": 1.5,
  "volume": 100.0, "muted": false, "speed": 1.0,
  "playlist-pos": 0,
  "playlist": [ { "filename": "/srv/remoteplayer/media/movies/a.mkv", "title": null, "current": true, "playing": true } ],
  "tracks": [ { "id": 1, "type": "audio", "title": null, "lang": "eng", "codec": "ac3",
                "selected": true, "default": true, "external": false } ],
  "audio-device": "alsa/hdmi:CARD=vc4hdmi,DEV=0", "audio-output": "alsa", "hwdec-current": "v4l2m2m"
}
```

When mpv is not running: `"connected": false` and empty values.

### `POST /player/command`

One typed command per request, selected by `type`. Returns `{"data": ...}`
with mpv's result (usually `null`).

| `type` | Fields | Notes |
|---|---|---|
| `play` | `folder`, `name`, `mode`? | a file from the library |
| `play-url` | `url`, `mode`? | http, https, rtsp, rtmp, rtmps, srt, udp, smb |
| `pause` / `resume` / `toggle-pause` | | |
| `stop` | | stops and clears the playlist |
| `seek` | `seconds`, `mode`? | `relative` (default), `absolute`, `absolute-percent`, `relative-percent` |
| `set-volume` | `volume` | 0–130 |
| `set-mute` | `muted` | |
| `set-speed` | `speed` | 0.01–100 |
| `playlist-next` / `playlist-previous` | | |
| `playlist-play` | `index` | |
| `playlist-remove` | `index` | |
| `playlist-move` | `from`, `to` | |
| `playlist-clear` | | keeps the current file |
| `set-audio-track` / `set-subtitle-track` | `track` | a track id, `"no"` or `"auto"` |
| `add-subtitle` | `folder`, `name` | loads and selects an external subtitle file |

`mode` for `play`/`play-url`: `replace` (default), `append`, `append-play`.

```sh
curl -H "$AUTH" -H "$JSON" -d '{"type": "play", "folder": "movies", "name": "a.mkv"}' $API/player/command
curl -H "$AUTH" -H "$JSON" -d '{"type": "seek", "seconds": -10}' $API/player/command
curl -H "$AUTH" -H "$JSON" -d '{"type": "set-subtitle-track", "track": "no"}' $API/player/command
```

### `POST /player/ipc` — raw mpv IPC

The body is `{"command": [...]}` in mpv's JSON IPC array form; the reply is
mpv's own reply, also when mpv reports an error:

```sh
curl -H "$AUTH" -H "$JSON" -d '{"command": ["get_property", "demuxer-cache-duration"]}' $API/player/ipc
# {"error": "success", "data": 12.4}
```

Blocked with 403: commands that start processes, load scripts or configs, or
write files (`run`, `subprocess`, `load-script`, `screenshot-to-file`,
`dump-cache`, `vf`, `af`, ...), and setting properties such as `script-opts`,
`scripts`, `ytdl-*`, `input-ipc-server`, `stream-record`, `log-file` — also
through `loadfile` per-file options. This is a deny-list, not a sandbox; see
the security notes in the README. Disabled entirely with
`mpv.raw-ipc-enabled = false`.

### `GET /player/events` — WebSocket

Text frames, one JSON object each:

| `type` | Payload | When |
|---|---|---|
| `state` | `state`: as `GET /player/state` | on connect, and whenever mpv (re)connects |
| `mpv-event` | `event`: mpv's event object | every mpv event, e.g. `property-change`, `file-loaded`, `end-file` |
| `connection` | `connected`: bool | mpv went away or came back |
| `lagged` | `skipped`: number | this client read too slowly and missed events |

Watched properties: `pause`, `idle-active`, `path`, `media-title`, `duration`,
`time-pos` (at most once per second), `volume`, `mute`, `speed`,
`playlist-pos`, `playlist-count`, `aid`, `sid`, `audio-device`, `eof-reached`.

```js
const socket = new WebSocket(`ws://raspberrypi.local:8080/api/v1/player/events?access_token=${token}`);
socket.onmessage = (message) => console.log(JSON.parse(message.data));
```

### `POST /player/restart`

Restarts mpv (the systemd unit) and restores playlist, position, pause state,
volume, mute, speed, tracks and delays. Returns the `restart` object described
under settings. 409 while another restart or settings change runs.

### `POST /player/restore`

Restores the snapshot that remoteplayer-mpvwatcher keeps (its `last` answer,
over `restore.watcher-socket`), regardless of which mpv process it came from.
404 when the watcher knows no state, 501 without a configured watcher, 502
when it cannot be reached.

```json
{ "captured-at-unix-ms": 1792976499186, "source-pid": 812, "restored-items": 3, "warnings": [] }
```

Without this call, playback is restored automatically whenever a new mpv
appears that this service did not start itself — after a crash
(`restore.after-crash`) and when this service starts, e.g. after a reboot
(`restore.on-start`). It acts only if the new mpv is still empty and the
snapshot contains a playlist from a *different* mpv process, so it never
overwrites what someone started and never undoes a deliberate stop. After a
reboot there is a snapshot only if the watcher has a writable state file.

### `GET /player/service`

```json
{ "manager": "systemd", "unit": "remoteplayer-mpv.service", "active-state": "active",
  "sub-state": "running", "autostart": "enabled", "main-pid": 812 }
```

`autostart` is systemd's `UnitFileState` (`enabled`, `disabled`, ...).

### `GET /player/log?lines=50`

mpv's last log lines (from the journal; at most 1000).

---

## Library

Files live in first-level folders below `storage.upload-root`; the root itself
holds no files and folders are not nested. Without `upload-root` every library
endpoint answers 501; when the root is configured but not mounted, 503. When it
cannot be written (read-only mount, no permission, or on a RAM-backed
filesystem without `storage.allow-volatile`), listing, MD5 and playback work
and every change is answered with 403. Names may not contain `/`, `\` or
control characters and may not start with `.`. Files whose names start with a
dot (including uploads in progress) are not listed.

### `GET /storage`

```json
{ "mode": "read-write", "root": "/srv/remoteplayer/media", "reason": null, "volatile": false,
  "total-bytes": 62000000000, "available-bytes": 41000000000, "reserved-bytes": 268435456 }
```

`mode`: `read-write`, `read-only` (with `reason`), `unavailable` (configured but
not mounted) or `not-configured`. Probed on every request by creating and
deleting a file, so it shows what really works right now.

### `GET /folders`

```json
[ { "name": "inbox", "modified-unix-ms": 1792976047993, "is-default": true },
  { "name": "movies", "modified-unix-ms": 1792976047825, "is-default": false } ]
```

### `POST /folders` — `{"name": "series"}` → 201

### `DELETE /folders/{folder}` → 204; 409 unless the folder is empty

### `GET /folders/{folder}/files`

```json
[ { "name": "a.mkv", "size": 1468006400, "modified-unix-ms": 1792976047717, "kind": "video" } ]
```

`kind`: `video`, `audio`, `subtitle`, `image`, `playlist` or `other`, by extension.

### `PUT /folders/{folder}/files/{name}` — upload the raw body

Creates the folder if needed. Query parameters: `overwrite=true` to replace an
existing file (otherwise 409), `md5=<hex>` to have the upload verified.

```sh
curl -H "$AUTH" -T a.mkv "$API/folders/movies/files/a.mkv?md5=$(md5sum a.mkv | cut -d' ' -f1)"
```

→ 201

```json
{ "folder": "movies", "name": "a.mkv", "size": 1468006400, "md5": "1e181317134debb3c176b212cc67dcc5" }
```

The body is streamed to a hidden temporary file in the target folder while
the MD5 is computed, synced to disk, and only then renamed to its final
name. An interrupted upload leaves nothing behind. With `Content-Length`, free
space is checked before the first byte is written.

### `POST /uploads?folder=<folder>&overwrite=<bool>` — multipart upload

For HTML forms. Every part with a file name is stored under that name in
`folder` (default: `storage.default-folder`). Returns a list of results as above.

```sh
curl -H "$AUTH" -F "file=@a.mkv" -F "file=@a.srt" "$API/uploads?folder=movies"
```

### `GET /folders/{folder}/files/{name}/md5`

```json
{ "folder": "movies", "name": "a.mkv", "size": 1468006400, "md5": "1e18...", "cached": true }
```

Works for files that arrived any way. The digest is cached in the extended
attribute `user.remoteplayer.md5` together with size and modification time;
on filesystems without xattrs (FAT, exFAT) it is recomputed every time.

### `POST /folders/{folder}/files/{name}/move`

```json
{ "folder": "archive", "name": "renamed.mkv", "overwrite": false }
```

`name` is optional (keeps the current name). Returns the file's new listing entry.

### `DELETE /folders/{folder}/files/{name}` → 204

---

## Hardware

### `GET /audio/devices`

```json
{
  "source": "mpv-ipc",
  "current": "alsa/hdmi:CARD=vc4hdmi,DEV=0",
  "outputs": [
    { "card": "Headphones", "kind": "analog", "description": "bcm2835 Headphones, bcm2835 Headphones",
      "recommended": "alsa/plughw:CARD=Headphones,DEV=0", "alternatives": ["alsa/default:CARD=Headphones"] },
    { "card": "vc4hdmi", "kind": "hdmi", "description": "vc4-hdmi, MAI PCM i2s-hifi-0 / HDMI Audio Output",
      "recommended": "alsa/hdmi:CARD=vc4hdmi,DEV=0", "alternatives": ["alsa/plughw:CARD=vc4hdmi,DEV=0", "..."] }
  ],
  "other": [ { "name": "auto", "description": "Autoselect device" } ]
}
```

`source` is `mpv-ipc` (asked the running mpv) or `mpv-help` (mpv was down, so
`mpv --audio-device=help` was run). `kind`: `hdmi`, `analog`, `spdif`, `usb`,
`other`. Use `recommended` as the `audio-device` setting. An S/PDIF HAT appears
only after its `dtoverlay` is enabled in `/boot/firmware/config.txt` and the
Pi was rebooted.

### `GET /display`

```json
{
  "connectors": [
    { "name": "HDMI-A-1", "card": "card1", "connected": true, "enabled": true,
      "preferred-mode": "1920x1080", "modes": ["1920x1080", "1280x720", "..."],
      "monitor": { "manufacturer": "GSM", "product-code": 49320, "name": "LG TV", "manufacture-year": 2021 } }
  ],
  "active-connector": "HDMI-A-1",
  "current-mode": { "width": 1920, "height": 1080, "refresh-hz": 50.0 }
}
```

`current-mode` comes from mpv (`display-width`, `display-height`,
`display-fps`) and is `null` while mpv is not running.

### `GET /cec` — poll the HDMI-CEC bus

```json
{
  "adapter": { "device": "/dev/cec0", "driver": "vc4_hdmi", "name": "vc4-hdmi",
               "physical-address": "1.0.0.0", "logical-addresses": [4], "claimed-now": false },
  "devices": [
    { "logical-address": 0, "role": "tv", "physical-address": "0.0.0.0", "osd-name": "TV",
      "vendor-id": "0x00e091", "vendor-name": "LG", "power-status": "standby", "cec-version": "1.4" }
  ],
  "warnings": []
}
```

All 15 logical addresses are polled; every device that answers is asked for
its physical address, OSD name, vendor, power status and CEC version. Fields
a device does not answer are `null`. `power-status`: `on`, `standby`,
`transitioning-to-on`, `transitioning-to-standby`, `unknown`. If the adapter
has no logical address yet, the service registers the Pi as a playback device
(`claim-logical-address`); with the TV off or unplugged there is no physical
address and a warning explains why no devices are listed. A scan takes from a
fraction of a second up to a few seconds.

### `GET /system`

```json
{
  "hostname": "raspberrypi", "uptime-seconds": 86400.5, "load-average": [0.4, 0.5, 0.5],
  "memory-total-bytes": 948060160, "memory-available-bytes": 512000000, "temperature-celsius": 52.1,
  "throttled": { "raw": "0x50000", "under-voltage-now": false, "frequency-capped-now": false,
                 "throttled-now": false, "soft-temperature-limit-now": false,
                 "under-voltage-occurred": true, "frequency-capped-occurred": false,
                 "throttled-occurred": true, "soft-temperature-limit-occurred": false }
}
```

`under-voltage-occurred` since boot is the usual cause of stuttering video on
a Pi 3: use a proper 2.5 A power supply.

### `POST /system/reboot`, `POST /system/poweroff` → 202

Through systemd-logind; 501 when `system.power` is `disabled`.
