# TwoDrive reliability repair report

Date: 2026-07-24

> Update: the 2026-09-03 local-first redesign supersedes the foreground metadata-operation limits
> described here. See [local-first-sync-2026-09-03.md](local-first-sync-2026-09-03.md).

## Reproduction and root causes

The previous database represented "always keep" only as the current `state = pinned`. Pinning a
directory rewrote the current descendants once, so later local creates, Graph delta additions, and
moves had no durable policy to inherit.

FUSE marked a file `uploading` as soon as it was created and made `flush` perform the network
upload. This coupled editor durability to network availability and caused rename/unlink to reject
files as "changing". Atomic replacement also deleted the destination before moving the temporary
file. Graph move requests sent `parentReference.path`, which produced `badArgument` on the observed
account.

Dirty local content had no startup replay path and could conflict with delta metadata at the same
path. The default FUSE `fsync` and `setattr` implementations returned `ENOSYS`, which made editor and
tar workflows unreliable.

## Changes

- Added idempotent `pin_explicit` and `pin_origin_remote_id` migration.
- Migrated legacy contiguous pinned trees to one explicit root and inherited descendants.
- Recomputed effective pinning for metadata additions, subtree moves, unpin, and deletion.
- Used exact case-sensitive path-component matching.
- Hydrated newly inherited pinned files after delta sync and retained failed hydration for retry.
- Protected effective pinned entries from release and cache pruning.
- Changed writes to `dirty`, implemented local `flush`/`fsync`, random-offset writes, and truncate.
- Added upload-on-release with failure rollback to durable `dirty` state.
- Added startup/manual replay for dirty and interrupted `uploading` cache entries and protected
  them from delta replacement.
- Added a durable pending-delete table so local unlink can succeed while Graph is temporarily
  unavailable; delta metadata for queued deletes stays hidden until replay finishes.
- Added editor-style temporary-file replacement without deleting the remote destination first.
- Allowed rename and unlink of a locally open dirty handle.
- Reused dirty local cache for concurrent reads and read-only `fsync` requests.
- Serialized in-process database migration, added a SQLite busy timeout, and tested concurrent init.
- Resolved Graph move parents to item IDs, added ETag-protected overwrite uploads, and added page
  count/timing diagnostics without logging delta links or credentials.
- Retried transient Graph 429/5xx failures and honored bounded `Retry-After` delays.
- Added upload sessions for files larger than 10 MiB, using sequential 10 MiB fragments that are
  valid 320 KiB multiples and recover fragment progress after transient failures.
- Preserved ETag conflicts as stable cloud conflict copies, refreshed the winning remote metadata,
  and replayed interrupted conflict-copy work after restart.
- Added real read-only FUSE handles and delayed unlinked-cache cleanup until the final concurrent
  read/write handle closes.
- Accepted POSIX metadata updates as local compatibility hints; OneDrive POSIX metadata limitations
  are documented.
- Added `--version` handling and removed invalid package documentation metadata.

## Automated evidence

The workspace tests cover:

- new local/remote metadata inheriting a pinned ancestor;
- moving a subtree into and out of a pinned ancestor;
- nested explicit pins surviving parent unpin;
- exact `/foo` versus `/foobar` and case-sensitive policy boundaries;
- legacy database migration and retry;
- dirty local generation protection from delta metadata;
- pending remote delete hiding and replay after failure/restart;
- Graph parent lookup and path encoding;
- ETag-protected overwrite conflict-copy handling and restart replay;
- sequential Graph upload-session byte ranges and final metadata;
- read-only open/unlink/read/release lifetime;
- bounded `Retry-After` parsing for transient Graph failures;
- random-offset write plus local fsync;
- editor temporary-file atomic replacement;
- interrupted dirty/uploading replay.

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Upgrade and rollback

Stop the user service before replacing binaries. Keep a copy of
`~/.local/share/twodrive/twodrive.sqlite3` plus its `-wal` and `-shm` files while the service is
stopped. Install 0.2.2 and restart the service. Migration is idempotent and runs during database
initialization.

Version 0.2.2 is a forward-compatible reliability update over 0.2.1. It does not require a new
database migration and keeps the existing pending-delete, pin-policy, and dirty-cache semantics.

For rollback, stop the service, restore the previous binaries and the matching stopped-service
database backup, then restart. An older binary does not understand the new policy columns, although
SQLite will otherwise leave them intact.

## Remaining limits

No destructive test was run against the user's existing OneDrive. Real-account acceptance still
needs an explicitly approved disposable cloud directory. The later local-first redesign added
stable local identities plus durable create-folder/move and delete queues; file content continues
to replay from dirty cache. Upload sessions persist their resumable state. OneDrive does not store
full POSIX mode, uid, gid, or directory timestamps, so TwoDrive acknowledges those metadata
operations for compatibility but does not sync them as remote filesystem metadata.
