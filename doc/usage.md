# Everyday use and safety

[Home](../README.md) · [简体中文](usage.zh-CN.md) · [Installation](getting-started.md)

## Browse and edit

Open `~/TwoDrive/OneDrive` in GNOME Files (Nautilus). Directory entries come from SQLite metadata. Ordinary browsing and Nautilus/GIO MIME probes do not prefetch content; an application that actually reads or copies a cloud-only file triggers a download. Unknown formats may display a generic icon until opened.

Create, edit, rename, move, and delete through the mount. Changes are committed locally before background Graph operations finish. Watch the [status emblems](../README.md#files-on-demand-at-a-glance), rather than treating an editor's successful save as cloud completion. Reading uncached content requires connectivity.

## Keep or release content

| Nautilus action | Behavior |
| --- | --- |
| **Always keep on this device** | Sets an explicit pin policy and downloads existing eligible files, recursively for folders. Pinned content is protected from automatic cache cleanup. |
| **Cancel always keep on this device** | Removes this item's explicit policy but leaves cache in place. An inherited parent policy can still apply. |
| **Release space** | Cancels explicit and inherited pins for the selection (including folder descendants), then safely removes local cache. The OneDrive item and placeholder stay visible. |
| **Sync now** | Refreshes metadata, retries pending local operations/uploads, and hydrates pending pins. Not a strictly metadata-only command. |
| **View status** | Shows the selected item's local state. |
| **Copy path** | Copies absolute local paths, one per line for multiple selections. Also works outside TwoDrive; paths are not shell-quoted. |

These CLI commands use **cloud-relative paths beginning with `/`**, not paths under your home directory:

```bash
twodrive pin /Documents/report.pdf
twodrive pin /Courses
twodrive unpin /Documents/report.pdf
twodrive release /Documents/report.pdf
```

Pinning a folder applies to existing descendants. Locally created descendants can inherit that policy; **newly discovered cloud files remain online-only**, even beneath an already pinned folder. Folder emblems summarize descendants, not guaranteed offline availability of every child. Confirm downloads before going offline; `twodrive status-path /Documents/report.pdf` reports effective pin state.

**During download:** releasing a file cancels the download and removes partial data. A folder release includes descendants. Old handles cannot restart the transfer; an explicit new open can download again. A blocked network request must return or time out before cancellation finishes.

**During writing/upload:** the release request is persisted and shown as cloud + syncing. Cache is removed after successful upload and closure of open handles. Failed/conflicting uploads retain data. Automatic pruning still protects pinned content; Always Keep cancels a pending release.

> [!WARNING]
> **Delete and Release space are different.** Deleting inside the mount deletes the cloud item too. To reclaim disk space without deleting OneDrive content, use Release space—not Delete, `rm`, or manual cache removal.

To run a sync/recovery pass or prune expired unpinned cache:

```bash
twodrive sync
twodrive cache prune
```

Pruning uses `cache.retain_for` (default `30d`) and preserves protected/in-progress content. The tray's **Release unpinned cache** invokes this retention-based prune; it does not immediately clear every unpinned file.

## Tray and Settings

The homepage's [real screenshot](../README.md) shows the tray **at idle**, not during a transfer. With activity present, the implementation lists the active count and up to five uploading/downloading filenames, with byte progress when available. `Sync: idle` is an activity snapshot, not proof that all queued operations succeeded.

**Open TwoDrive folder**, **Sync now**, cache pruning, and **Settings** are available from the menu. **Quit tray** leaves the daemon running. **Quit tray and stop sync** also requests a user-service stop; close applications using the mount first.

**Pause sync is currently display-only.** It changes the tray label/icon, not the daemon or transfers. To actually stop after closing mounted files, use `systemctl --user stop twodrive-daemon.service`; restart with `systemctl --user start twodrive-daemon.service`. Stopping the daemon also makes the mount unavailable; it is not an offline pause mode.

Settings is a **read-only GTK 4 overview** of token-file state, paths, measured cache usage, and configured values. It cannot edit configuration. “Pinned used” and “Recent errors” are placeholders, not live accounting/error history. A displayed token path is not a live authentication check.

## Current limitations and safety

TwoDrive is experimental, not a backup system. Keep independent copies. Local recovery and conflict handling reduce specific risks; they do not guarantee against data loss.

On an ETag conflict, TwoDrive attempts to retain the cloud version under its original name and upload the local edit as a `TwoDrive conflict` copy. This is not document merging. Existing duplicate cloud directories need separate reconciliation. Unsupported OneDrive filenames, including `:` or `?`, can block uploads while retaining local content; inspect the error and rename appropriately.

Tokens are plaintext JSON with mode `0600`; Secret Service is not implemented. Do not share tokens, OAuth callback URLs, or upload-session URLs. `twodrive logout` removes the local token file only: stop the daemon first, since a running process can retain tokens in memory. It does not revoke account-side consent.

The mount's recovery/metadata loop waits 60 seconds between passes and uses `ac_upload_concurrency` for the upload pool. Displayed AC/battery intervals do **not** currently implement adaptive power scheduling. `cache.max_size` is stored/displayed but is **not an enforced disk quota**. Do not rely on those fields to limit traffic, battery use, or disk consumption.

Full POSIX uid/gid/mode and directory timestamp semantics are not preserved. The client operates against the signed-in user's `/me/drive`; do not assume multi-account management or arbitrary SharePoint library support.

## Optional known-folder uploads

Known-folder handling is **disabled by default**, separate from the read/write mount, and supports `upload_only`. Configured local folders upload directly without an extra TwoDrive content cache. Local source deletion does not delete the cloud copy; deletion propagation is ignored even if requested in config.

Read [config.example.toml](../config.example.toml) before enabling it. The example disables startup/rescans (`startup_scan = false`, `rescan_interval = "0s"`); generated defaults use `true` and `"15m"`. The watcher baselines existing files, then handles additions/renames and configured scans. It is not bidirectional backup or a promise to upload all existing files immediately. Temporary/hidden files and symlinks are skipped. Keep sources until uploads succeed.

## Paths and diagnostics

| Purpose | Default location |
| --- | --- |
| Mount | `~/TwoDrive/OneDrive` |
| Config / tokens | `~/.config/twodrive/config.toml` / `tokens.json` in the same directory |
| Metadata / recovery records | `~/.local/share/twodrive/twodrive.sqlite3` |
| Content cache | `~/.local/share/twodrive/cache` |
| Activity / upload sessions | `activity.json` / `upload-sessions.json` under `~/.local/share/twodrive` |
| Nautilus diagnostic log | `~/.local/state/twodrive/nautilus.log` |

Rust components honor `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, and `TWODRIVE_MOUNT_DIR`. **Nautilus and the packaged service still include default-path assumptions**; these development overrides are not a complete custom-location desktop setup.

```bash
twodrive status
twodrive status-path /Documents/report.pdf
journalctl --user -u twodrive-daemon.service -f
```

Close mounted files before restarting/unmounting. Stop the service before manually unmounting a remaining mount:

```bash
systemctl --user stop twodrive-daemon.service
fusermount3 -u ~/TwoDrive/OneDrive
```

The second command is needed only if it remains mounted. Do not erase the database/cache to reset a stalled upload. Reports should contain sanitized versions, environment, reproduction, and relevant logs—not the runtime data directory. Remove usernames, personal paths, filenames, tokens, and authorized URLs. [Reporting and development](../CONTRIBUTING.md)
