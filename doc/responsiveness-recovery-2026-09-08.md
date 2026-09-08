# Responsive mounts and restart recovery (0.2.3)

## Problems addressed

- Startup waited for cloud metadata, pending uploads and pinned downloads before mounting.
  A multi-gigabyte recovery backlog made the mount unavailable for the entire transfer.
- FUSE served cloud downloads inline, so one content read stopped unrelated filesystem requests.
- Directory pagination refreshed all children for every page, repeatedly scanned the inode table
  and sorted siblings. Every write also scanned child records across the whole drive.
- Nautilus resolved complete FUSE paths and queried SQLite on its main thread.
- A temporary upload-session status failure discarded the persisted session. Backup retries also
  retained obsolete source snapshots and could wait indefinitely when periodic rescans were disabled.
- Idle I/O scheduling applied to foreground FUSE operations as well as background transfers.

## Behavior

The mount starts from SQLite and the cache without waiting for network requests. Interrupted
`writing` records recover the actual cache length and become durable dirty work. The background
recovery scan reuses the normal item-serialized workers and coalesces queued work. Failures remain
queued and are retried; OneDrive delta import runs separately from foreground requests.

Read-only opens reserve handles immediately. Cloud reads run on four background workers and
retain their cache descriptors. Hydration is serialized per file across read handles and pin jobs.
An open directory gets a stable snapshot; pagination uses that snapshot without querying or sorting
again. File writes update their inode size directly; file creation allocates monotonically increasing
inode numbers and sorts only when listing. Nautilus status queries run in a background thread and
apply UI updates via GLib. Shortcut resolution stops before traversing the FUSE mount.

An upload-session status request only permits starting over when the server reports a missing or
expired session (404/410). Network failures, throttling and server errors retain the original session.
The next attempt queries the server for its confirmed offset. Session/state replacements fsync their
parent directory as well as the temporary file.

Known-folder watchers are registered before startup recovery. Pending backup jobs retry even with
rescans disabled, and use a fresh source snapshot if the source changed while offline. Jobs are
persisted before cloud folder creation or upload. They read the original source directly, do not
create a second local cache copy, and never propagate source deletion to OneDrive. Deleting a source
before it has finished uploading cannot preserve bytes that were never uploaded; missing pending
sources are removed from the local queue without deleting any cloud item.

The packaged service uses normal CPU priority and best-effort I/O so foreground filesystem requests
are not starved by other disk activity.

## Verification

Run:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 -m unittest discover -s packaging/nautilus -p 'test_*.py'
cargo test -p twodrive-fs mounted_large_listing_and_copy -- --ignored --nocapture
cargo build --workspace --release
```

The explicitly enabled FUSE regression uses an isolated temporary mount and a mock backend. With a
3-second download in progress, listing 10,000 entries and copying 16 MiB completed in about 311 ms.
It also verifies unique directory entries and exact copied bytes. It requires `/dev/fuse` and
`fusermount3`; the ordinary test suite does not require a mount.

Other regressions cover recovery without a close notification, upload-session expiry versus
transient failure, stable directory inodes, queue coalescing, changed backup sources, absence of
backup cache copies, retained cloud backups after source deletion, and shortcut resolution without
FUSE probes. Existing tests cover persisted fragment offsets and interrupted uploading states.

A local installation check on 2026-09-08 confirmed that two approximately 17 GB uploads resumed
while the live mount stayed available: Downloads, Pictures and the reported tinyml directory listed
in under 1 ms; the largest direct-child directory (403 entries) listed in about 2.2 ms. These are local
observations, not universal performance guarantees. The machine was not powered off for testing.

## Upgrade

Stop the daemon before replacing its binary. Back up SQLite (including any WAL/SHM files),
`known-folders-state.json` and `upload-sessions.json`; keep the existing cache intact. Install the
binary, Nautilus extension and service scheduling changes, then restart the daemon. Restart Nautilus
when convenient to load the new extension. Upload session files contain capability URLs and must
remain private; they must not be added to a source commit or diagnostics report.
