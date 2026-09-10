# Download completion could restore an empty placeholder

## Evidence

The reported `intattention.pdf` and `2511.21513v2.pdf` records pointed to
zero-byte caches, while orphaned download caches still held the complete
712,364-byte, 14-page IntAttention PDF. The mounted inode initially advertised
a nonzero size even though the database and selected cache were empty.
Renaming refreshed the inode from that database record, exposing size zero.
Logs also contained upload acknowledgements for temporary local identities
that had already been removed by atomic replacement.

## Cause

A browser reserves an empty destination and writes its download to a temporary
file. The upload worker reads the empty destination's record, then performs a
remote path preflight. While that network operation runs, FUSE replaces the
destination with the completed temporary file. Replacement preserves the
destination's local identity but changes its cache and state.

Previously, the worker unconditionally marked that local identity `uploading`
and uploaded the old empty cache. The acknowledgement checked only path and
state, so it could accept the empty upload as current and restore the old cache
pointer and zero size. The complete cache became unreachable through the mount.

## Fix

`Database::begin_upload` atomically checks the worker's snapshot (identity,
path, cache, state, size, mtime and cloud version) before claiming the upload.
A stale snapshot is skipped; the existing queue/recovery scan processes the
current dirty record. Upload acknowledgement additionally requires the current
cache path to match before replacing local content metadata.

## Validation and recovery

- Added a regression reproducing a stale empty-destination upload after a
  completed download replaces it. It failed before the fix and passes after it;
  the test also verifies the final backend content.
- Added coverage for reopened writers, replacement between claim and
  acknowledgement, and stale cache acknowledgements.
- `cargo test --workspace`: 77 passed; 4 existing environment-dependent mounted
  tests remain ignored by default. `cargo build --release` succeeded.
- Installed the rebuilt daemon after checking for open mount writers.
- Eight real OneDrive mount checks reserved an empty destination, replaced it
  with the completed PDF, renamed it, waited for upload processing, and verified
  byte-for-byte equality. Disposable files were deleted afterwards.
- Recovered both reported PDF paths from the preserved complete cache. Recovery
  material is retained outside the mount under the application's recovery
  directory. Cloud readback verification is recorded below.
- Released the restored `intattention.pdf` cache and read it back from OneDrive;
  SHA-256 matched the recovery copy:
  `7704fee7488a4226f6218227a51ad1e1d447f7b986ffaa036404f84d6c518c25`.
  `pdfinfo` successfully parsed all document metadata and reported 14 pages.
