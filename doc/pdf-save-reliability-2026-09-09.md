# PDF save reliability, 2026-09-09

## Failure and cause

Evince/Poppler can reject its first save after opening a newly uploaded PDF. The
mounted file's bytes are unchanged, but upload completion replaces the local
modification time with OneDrive's later server time. Directory refresh then
publishes that change to the open inode, and Poppler's source-file consistency
check returns “Failed to save document”. Saving to a different local directory
cannot avoid the source check.

This was reproduced on the real OneDrive mount using a disposable copy of the
reported PDF: open before upload completion, wait for the upload, refresh the
parent directory, add an annotation, save through Poppler. The pre-fix daemon
failed on save zero. The earlier fix only covered an already replaced inode;
its rapid mock saves missed the asynchronous upload timestamp transition.

## Changes

- Keep the local content mtime when committing an upload acknowledgement.
- Preserve that mtime for matching ETag/size delta echoes, in both single and
  batch metadata upserts. A genuinely new remote ETag still updates metadata.
- Keep file content mtime when acknowledging a locally requested metadata move.
- Publish completed local write size and mtime to the inode table before queuing
  upload, so reopen does not first observe stale metadata.
- The previous open-handle replacement fix remains: unlinked read handles retain
  their original attributes until release, including the original mtime.

## Reproducible verification

`python3 scripts/verify-pdf-save.py --mount MOUNT --source SOURCE.pdf`

Requires Python 3, Poppler GLib, GObject/GLib, and `gio`. It copies SOURCE into a
unique disposable file on MOUNT; it never overwrites SOURCE. On a real mount the
disposable file is uploaded, then deleted at cleanup. Four cycles add annotations,
save locally with Poppler, copy back using GIO, reopen the persisted output, and
check that annotation counts increase. The last cycle waits 65 seconds to cross
the normal delta interval. Local diagnostic outputs remain in a printed temporary
directory. `--settle-seconds` and `--last-wait-seconds` control those waits.

Core tests cover upload acknowledgement, single and batch same-version delta
echoes, and a changed remote ETag. Filesystem tests cover publication before
reopen/directory refresh and old inode attributes after atomic replacement.

## Verification result

The updated daemon was installed and restarted. On the real OneDrive mount, all
four annotated saves passed, including the 65-second final wait crossing a logged
delta sync. Reopened output PDFs contained 1, 2, 3, and 4 added annotations. The
disposable remote test files were removed. Workspace tests, Clippy with warnings
denied, formatting, and the new completed-write attribute regression passed.

Already-open documents whose source descriptor was invalidated by a previous
daemon restart need reopening; the fix cannot reconstruct unsaved editor memory.
