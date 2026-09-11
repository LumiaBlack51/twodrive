# Folder moves racing child synchronization

## Investigation

At 15:09 on September 11, a move of `vision_quantization_papers` was followed
by repeated Graph `409 nameAlreadyExists` failures for the directory. Read-only
Graph lookups confirmed that the directory's tracked cloud identity was still
at its old parent, while a different directory identity already occupied the
requested destination and contained uploaded files. This is consistent with
child path uploads creating the destination before the parent move completed.
The local database recorded the parent move as pending.

At 15:09:45 and 15:10:46, metadata refresh also failed while decoding negative
size values (`-3116084` and `-11644867`) into `u64`. The full response was not
retained, so the historical item responsible cannot be identified. Folder
aggregate sizes are not used locally and should not prevent decoding a page.

There was no per-file ECCV read or rename error in the journal, so these findings
do not establish which PDF first failed or its exact error. No reported file
was recovered, and existing cloud directory conflicts were not merged or deleted.

## Changes

- Defer file uploads while the file or any ancestor has pending metadata work.
  Recheck this condition atomically when claiming an upload, after network
  preflight.
- Defer child metadata operations until their parents settle, and reject stale
  queued path snapshots.
- Retarget descendant metadata jobs in the same transaction as a local subtree
  move.
- Protect descendants and destination paths from delta updates while ancestor
  operations are pending, for both single and batch metadata ingestion.
- Decode Graph sizes as JSON numbers. Ignore aggregate directory sizes; retain
  nonnegative file sizes without converting negative values into huge lengths.
  Invalid negative file sizes do not become file metadata.

## Validation

Three new regression tests fail against the previous production code: negative
size decoding, stale descendant job paths, and claiming a child upload before
the parent move finishes. All pass with the fix. The filesystem regression also
checks that child operations resume afterwards and the uploaded bytes match.

`cargo test --workspace`: 80 passed, four existing mounted tests ignored.
The changes are source fixes; the investigation did not restart the user's
active FUSE mount or reconcile already-conflicting remote directories.
