# Everyday use and safety

[Home](../README.md) · [简体中文](usage.zh-CN.md) · [Installation](getting-started.md)

## Browse and edit

Open `~/TwoDrive/OneDrive` in GNOME Files (Nautilus). Directory entries come from SQLite metadata. Ordinary browsing and Nautilus/GIO MIME probes do not prefetch content; an application that actually reads or copies a cloud-only file triggers a download. Unknown formats may display a generic icon until opened.

Create, edit, rename, move, and delete through the mount. Changes are committed locally before background Graph operations finish. Watch the [status emblems](../README.md#files-on-demand-at-a-glance), rather than treating an editor's successful save as cloud completion. Reading uncached content requires connectivity.

## Keep or release content

Cloud deletion can remain pending after a path disappears locally. `twodrive status`
reports retained/deferred deletions; `twodrive status-path /old/path` shows
`state=delete_pending`, the failure category and next retry time while that path
has no newer local object. Permission/authentication failures retry after 5 minutes,
doubling up to 6 hours; temporary failures start at 1 minute, up to 1 hour. A longer
server Retry-After is respected. Restarting does not erase this schedule or the task.
After resolving access/sign-in issues, `twodrive retry-deletes` makes retained tasks
eligible on the next background scan, or run `twodrive sync` afterward for a pass.
This does not confirm cloud completion, change deletion targets, or discard tasks/cache.
Ordinary `sync` respects existing retry schedules. HTTP diagnostics expose only
bounded, allowlisted fields; never share tokens, private databases or raw unrelated logs.

| Nautilus action | Behavior |
| --- | --- |
| **Always keep on this device** | Sets an explicit pin policy and downloads existing eligible files, recursively for folders. Pinned content is protected from automatic cache cleanup. |
| **Cancel always keep on this device** | Cancels this item's explicit and inherited pin policy while keeping its cache. For folders, ancestor policy stops at that folder; independently pinned descendants remain pinned. |
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

Pinning a folder applies to existing descendants. Descendants can inherit that policy unless an intervening folder blocks inheritance; **newly discovered cloud files remain online-only**, even beneath an already pinned folder. Folder emblems summarize descendants, not guaranteed offline availability of every child. Confirm downloads before going offline; `twodrive status-path /Documents/report.pdf` reports effective pin state.

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

The daemon applies this policy on startup and in its background maintenance loop (60 seconds between passes). Superseded saves and clean invalidated remote versions are reclaimed once no durable reference or open file lock needs their bytes. Unknown pre-existing cache files and explicit recovery directories are preserved.

Linux trash at the mount root (`/.Trash-<numeric UID>` or `/.Trash/<numeric UID>`) is exempt from automatic cache retention. Both background maintenance and `cache prune` preserve existing caches for `.trashinfo` recovery metadata and trashed files/directories. This does not change pin policies or download cloud-only content. Emptying the trash or deleting an entry still propagates deletion; restoring an item outside the trash restores ordinary cache retention. An explicit **Release space** can still release these caches; restoring uncached content still requires a connection. Ordinary 30-day cache expiration only releases local bytes and does not empty either Linux or OneDrive trash.

## Tray and Settings

The homepage's [real screenshot](../README.md) shows the tray **at idle**, not during a transfer. With activity present, the implementation lists the active count and up to five uploading/downloading filenames, with byte progress when available. `Sync: idle` is an activity snapshot, not proof that all queued operations succeeded.

**Open TwoDrive folder**, **Sync now**, cache pruning, and **Settings** are available from the menu. **Quit tray** leaves the daemon running. **Quit tray and stop sync** also requests a user-service stop; close applications using the mount first.

**Pause sync is currently display-only.** It changes the tray label/icon, not the daemon or transfers. To actually stop after closing mounted files, use `systemctl --user stop twodrive-daemon.service`; restart with `systemctl --user start twodrive-daemon.service`. Stopping the daemon also makes the mount unavailable; it is not an offline pause mode.

Settings is a **GTK 4 overview with an upload source chooser** for token-file state, paths, measured cache usage, configured values and source diagnostics. It can change upload sources; other configuration remains read-only. “Pinned used” remains a placeholder, and source checks are not a complete error history. A displayed token path is not a live authentication check.

## Current limitations and safety

TwoDrive is experimental, not a backup system. Keep independent copies. Local recovery and conflict handling reduce specific risks; they do not guarantee against data loss.

Renaming during an upload keeps the local identity and replays the latest cloud destination. Atomic saves into a renamed file retain that pending move. Names containing unsupported characters stay local until corrected; rename and folder creation validate names before contacting OneDrive.

On an ETag conflict, TwoDrive attempts to retain the cloud version under its original name and upload the local edit as a `TwoDrive conflict` copy. If an upload returns a data-endpoint 404 and a separate lookup confirms that its bound cloud ID is absent, TwoDrive also preserves the local edit as a conflict copy. It does not recreate the original name or overwrite a replacement at that name. Once the copy is acknowledged and recorded, only the matching old generation is retired; a concurrent newer save remains pending. This is not document merging. Existing duplicate cloud directories need separate reconciliation. Unsupported OneDrive filenames, including `:` or `?`, can block uploads while retaining local content; inspect the error and rename appropriately.

Tokens are plaintext JSON with mode `0600`; Secret Service is not implemented. Do not share tokens, OAuth callback URLs, or upload-session URLs. `twodrive logout` removes the local token file only: stop the daemon first, since a running process can retain tokens in memory. It does not revoke account-side consent.

The mount's recovery/metadata loop waits 60 seconds between passes and uses `ac_upload_concurrency` for the upload pool. Displayed AC/battery intervals do **not** currently implement adaptive power scheduling. `cache.max_size` is stored/displayed but is **not an enforced disk quota**. Do not rely on those fields to limit traffic, battery use, or disk consumption.

File and directory rwx permissions are stored locally and enforced on this mount, including executable files and `chmod`; they survive remounts but do not sync to OneDrive. Cloud-only imports and existing entries default to 0644 (files) or 0755 (directories). Full POSIX ownership, special mode bits, and directory timestamp semantics are not preserved; the mount root mode remains fixed. The client operates against the signed-in user's `/me/drive`; do not assume multi-account management or arbitrary SharePoint library support.

## Filename upload exclusions

Office owner files whose names start with `~$` stay local in FUSE. Applications can create, read, write, lock and delete them normally; they do not create upload activity or cloud retry jobs. Local writes and pending uploads are not network transfers: the tray shows uploading only while the backend transfer runs, and byte progress comes from that transfer.

Add optional filename rules to `~/.config/twodrive/config.toml`:

```toml
[sync]
exclude_name_regexes = ['(?i)\.bak$', '^scratch-[0-9]+\.txt$']
```

These are Rust regular expressions, matched against the filename (not its full path). Matching searches anywhere unless anchored with `^`/`$`, and is case sensitive unless `(?i)` is used. TOML single quotes preserve the backslash. Lookaround and backreferences are unsupported; invalid or oversized patterns reject configuration loading. The default custom list is empty; the built-in `~$` rule always applies. Close mounted files before restarting TwoDrive to apply changes.

The same regexes filter known-folder scans, events and saved upload jobs. Existing exact-name/suffix rules remain specific to known folders; those scanners also prune matching ancestor names. FUSE regexes apply to files, not directory trees. Excluded local bytes remain durable across restart and are protected from cache release. Renaming a temporary file to a nonexcluded final name, including atomic replacement of an existing document, restores normal upload after active writers close. Removing a custom rule restores upload on restart. Previously uploaded cloud copies are not removed by exclusion; explicitly deleting a cloud-backed file still uses the normal cloud deletion queue. Reading existing cloud files still works; this is an upload exclusion, not selective download or a remote cleanup command. A missing local-only cache is reported for recovery rather than replaced with older cloud bytes.

## Optional known-folder uploads

Known-folder handling is **disabled by default**, separate from the read/write mount, and supports `upload_only`. Configured local folders upload directly without an extra TwoDrive content cache. Local source deletion does not delete the cloud copy; deletion propagation is ignored even if requested in config.

Read [config.example.toml](../config.example.toml) before enabling it. The example disables startup/rescans (`startup_scan = false`, `rescan_interval = "0s"`); generated defaults use `true` and `"15m"`. The watcher baselines existing files, then handles additions, content changes, renames and configured scans. It is not bidirectional backup or a promise to upload all existing files immediately. Temporary/hidden files and symlinks are skipped. Keep sources until uploads succeed.

Local sources are explicit paths: changing the desktop language or renaming a directory does not change an upload mapping. Open `twodrive settings` to inspect sources and choose a replacement, or use `twodrive known-folders status` for JSON diagnostics. Missing sources and skipped symbolic links are reported alongside differences from the system's XDG special directories. These differences can also be intentional custom mappings. A system special directory reset to `$HOME` must be repaired separately; TwoDrive does not rewrite `user-dirs.dirs`, move local contents, or migrate links from another OneDrive client.

Use the zero-based mapping index from the diagnostics to update one source, for example `twodrive known-folders set-source 1 ~/Downloads`. The command validates the directory, preserves cloud destinations and other settings, and saves atomically. Restart TwoDrive after saving (close files on the mount first; a service installation can use `systemctl --user restart twodrive-daemon.service`). Until restart the daemon retains its previous mapping. Sources missing at startup also require a restart after being restored. The home directory, filesystem root, and sources inside or containing the TwoDrive mount are rejected to avoid uploading the whole home or feeding the cloud mount back into uploads. Existing configuration files are diagnosed without automatically changing paths. See [TD-20261006-01](incidents.md#td-20261006-01系统语言切换后特殊目录与上传来源错位) for evidence and verification limits.

`exclude_names` defaults to `["node_modules"]`. Names and existing suffix rules apply to every component below a configured source, for scans, file events and persisted work. To exclude a whole development tree, add its directory name. Changing exclusions takes effect after restart; excluded local bookkeeping is retired without deleting source files or existing cloud copies.

Editor data changes and atomic saves share the configured debounce (default 5 seconds): consecutive saves coalesce into the latest source generation, with a maximum wait of `max(debounce, 30 seconds)` during continuous activity. Local saving never waits for cloud admission. First uploads and retries of an already admitted generation do not consume churn budgets. Changed generations have a sliding limit of 25 per file in 20 seconds. Ordinary first edits of separate files do not consume the project budget; repeated changes have a separate limit of 180 per detected hot project. Deferred work remains durable and retries the latest source, without reducing configured upload concurrency or capping uploads per batch. Set either `max_*_uploads_per_window` to 0 to disable that budget; `upload_rate_window` controls the shared window.

Normal events only update in-memory counters. Project discovery runs only after file churn reaches its limit or aggregate repeated changes reach the project threshold. It checks for the nearest `.git`, `package.json`, `Cargo.toml`, `pyproject.toml`, `go.mod`, `pom.xml`, `build.gradle` or `build.gradle.kts`, at most eight directories upward and never above the configured source. Positive and negative results are cached for the window. At most eight uncached ancestor searches run per window (at most 512 marker metadata checks); additional unknown roots retain file protection until a later window. There is no background project search. Unrecognized projects retain file protection only. Optional `upload_rate_roots` (absolute or `~/` paths) override discovery, with the most specific matching root taking priority. Budgets and discovery caches reset on daemon restart; they protect resource use rather than enforce a durable quota.

Known-folder state now uses `known-folders-state.sqlite3` in the data directory. A legacy `known-folders-state.json` is imported in one transaction and retained as a rollback snapshot; subsequent progress is stored in SQLite, so that JSON is no longer a live queue. Preserve the SQLite state, legacy snapshot and existing recovery state during backup or rollback. Completed uploads update only their own snapshot and queue row. No pending source content is copied into this database.

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
