# Archive extraction I/O error

## Root cause

The daemon reported `twodrive create upload error: created upload record disappeared`
while an archive extractor created an AppleDouble (`._`) file in a new nested folder.
The filename was incidental: ordinary files failed by the same mechanism.

`create_upload` used `upsert_metadata`, followed by separate cache and state updates.
The metadata import function deliberately ignores entries beneath pending folder
creates or moves, to prevent stale cloud deltas from undoing local work. During fast
extraction the parent was still pending, so the import returned success without
inserting the file. The later lookup failed and FUSE returned EIO. Separate inserts
and state updates also left an unnecessary intermediate online-only record visible
to background metadata/recovery work.

## Fix

A dedicated `Database::create_local_file` inserts local identity, path, cache,
access time and Writing state in one SQLite statement. It does not apply cloud
import filters. Unique constraints reject collisions without replacing existing
records. The cache remains locked by the open write handle. Upload recovery still
waits for pending parent operations; cloud import protections remain intact.
A path awaiting deletion of an older cloud identity can hold a new local write;
existing upload recovery waits for that deletion before reusing the cloud path.

## Verification

- The pending nested-folder regression fails against the previous `create_upload`
  with the exact production error, and passes with the fix.
- Both AppleDouble and ordinary files retain their bytes while parents are pending,
  reject stale delta replacement, and upload correctly after parent recovery.
- A database test checks pending-delete path reuse and collision preservation.
- A real isolated FUSE mount successfully extracts a ZIP containing nested source,
  AppleDouble metadata and an empty file while the root folder remains pending.
- All 87 default Rust tests and 19 Nautilus tests pass; formatting and Clippy with
  warnings denied pass. All five normally ignored mounted tests were also run and
  passed, using the newly built CLI where required.

The mount tests use a mock cloud backend, not a live Microsoft account. Existing
partial extractions and unrelated queued cloud conflicts are not merged or deleted
by this change. Retry extraction after upgrading.
