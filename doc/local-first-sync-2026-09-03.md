# TwoDrive local-first filesystem redesign

Date: 2026-09-03

## Why this change was required

The FUSE request loop previously performed Graph create-folder, rename, replacement upload, and
delete calls before replying to the kernel. A normal metadata operation could therefore block the
whole mounted filesystem for a network timeout or retry interval. PDF and Office applications also
exercise close/reopen, path-based truncate, and temporary-file replacement patterns that exposed a
race between the foreground filesystem and the upload workers.

The observed failure sequence was:

1. An application created and closed a temporary or newly saved file.
2. The close path queued an upload and returned.
3. The upload worker removed the temporary database identity and inserted the OneDrive identity.
4. A rapid reopen, truncate, or rename raced with that multi-step replacement and received `EIO`
   because its record disappeared.

The journal also showed SQLite snapshot-upgrade errors and Graph failures being surfaced through
foreground filesystem operations.

## New invariants

- `files.remote_id` is now the stable local identity for compatibility with the existing schema.
- `files.cloud_remote_id` is the optional OneDrive item identity. Existing cloud records are
  migrated automatically; local creates bind this value only after upload.
- Upload completion updates the existing row transactionally. It never deletes and recreates the
  local record.
- `write`, `flush`, `fsync`, and path-based `truncate` complete against the cache.
- `mkdir` and `rename` update the local namespace first and persist work in
  `pending_metadata_operations`.
- `unlink`/`rmdir` hide the item locally and persist the OneDrive deletion in `pending_deletes`.
- Graph work runs in background workers. Commands for the same local item are serialized.
- Metadata delta import skips records with pending local metadata operations, just as it already
  protects active content generations and pending deletes.
- Dirty cache files and pending operations survive daemon restart.

## User-visible durability contract

A successful save means the new generation is durable in TwoDrive's local cache and metadata
database. It does not promise that OneDrive has already accepted the generation. Cloud completion,
retry, conflict, and error are synchronization states and can be inspected separately.

This distinction allows editors to use ordinary POSIX save patterns even when the network is slow
or unavailable, while preserving eventual two-way synchronization.

## Database migration

Initialization adds:

```sql
ALTER TABLE files ADD COLUMN cloud_remote_id TEXT;

CREATE UNIQUE INDEX idx_files_cloud_remote_id
ON files(cloud_remote_id)
WHERE cloud_remote_id IS NOT NULL;

CREATE TABLE pending_metadata_operations (
    local_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    path TEXT NOT NULL,
    queued_unix INTEGER NOT NULL
);
```

Legacy non-placeholder rows receive `cloud_remote_id = remote_id`. The migration is idempotent.
Before upgrading a production installation, stop the daemon and back up the SQLite database together
with its `-wal` and `-shm` files.

## Recovery ordering

Daemon startup performs pending deletes and delta import, then replays pending folder/move operations
before dirty content uploads. Runtime workers use the same durable records. If a worker completed a
remote request but the local path changed concurrently, it does not declare the newer generation
synced; the current queued operation remains authoritative and is replayed next.

Recovery is idempotent at the remote boundary: a repeated delete treats an already-missing OneDrive
item as complete, and folder creation can re-discover and bind a folder that was created before its
acknowledgement was lost.

## Verification coverage

Automated tests cover:

- migration and stable local-to-cloud identity binding;
- immediate close/upload/reopen/write without losing the local record;
- path-based truncate followed by queued upload;
- editor temporary-file replacement;
- durable and coalesced folder/move operations;
- folder-create recovery after a lost remote acknowledgement;
- local-first delete with restart recovery;
- concurrent uploads, dirty replay, conflict copies, pinned moves, and delta protection.

The release gate remains:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

An isolated mock FUSE mount should additionally verify actual kernel `truncate`, rapid rewrite,
rename, mkdir, and unlink behavior before packaging.

The 2026-09-03 verification run completed path truncate, twenty immediate rewrite/truncate cycles,
mkdir, cross-directory rename, and unlink without `EIO`. The measured foreground metadata
operations completed in 9–15 ms on the test machine, and all durable metadata, delete, and dirty
queues drained to zero afterward.
