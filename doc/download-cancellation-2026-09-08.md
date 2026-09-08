# Release cancels downloads (0.2.5)

## Behavior

Release space cancels an unpinned file's active download instead of waiting for the complete file
before freeing it. Releasing a directory includes its descendants without affecting sibling paths.
Partial download files are removed and the item becomes online-only. Existing read handles fail
rather than silently returning truncated content or restarting the transfer. Explicitly reopening
creates a new download request and is allowed to fetch the file again.

Writing/dirty/uploading data is still protected. Release on an upload queues cache removal after a
successful upload and closing open cache handles. Explicit/inherited pin protection is unchanged;
unpin an always-keep file before releasing it.

## Implementation and races

SQLite gains an idempotently migrated integer `download_generation` column. Release increments it
for eligible files, including online-only files whose read requests may still be queued. Each lazy
read handle captures the generation at open. Download work checks it before starting, during
progress, and before completion. This works across the CLI and daemon processes and survives
restarts. A new explicit open captures the new generation.

Download publication (temporary-file rename and cache metadata update) runs in the same SQLite
write transaction as the generation eligibility check. A release that wins just before completion
therefore cannot be undone by the final downloaded fragment. Conversely, a completed cache that
wins first is subject to the normal cache release/locking rules. Local write states are never
replaced by downloaded content.

The progress callback checks cancellation at 100 ms intervals, avoiding a database connection for
every small buffer. Graph range requests also check cancellation before requests and during retry
backoff. An already-blocked synchronous HTTP operation cannot be interrupted by the callback: it
must return or hit the existing request timeout (60 seconds) before cleanup completes. No subsequent
range or retry starts after cancellation is observed. This is not a guarantee of instantaneous
socket interruption.

Cancellation is returned internally as ECANCELED. Linux buffered FUSE reads may surface EIO to the
reading application. The error is intentional; reporting a successful short file would be unsafe.
The user can close and explicitly reopen the file to download again.

## Verification

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python3 -m unittest discover -s packaging/nautilus -p 'test_*.py'
bash scripts/build-deb.sh
TWODRIVE_TEST_CLI="$PWD/target/release/twodrive" cargo test -p twodrive-fs -- --ignored --nocapture
```

The streaming regression releases through an independent database connection while a mock backend
is sending data, then verifies early termination, no partial files, rejected old generations and a
successful fresh download. A transactional test covers release just before publication and a
restart. A retry test verifies that a 30-second server backoff is cancelled promptly.

The FUSE integration test invokes the newly built CLI as a separate process against an isolated
mount. It verifies cancellation of the live read, failure of an older unopened-for-read handle, and
successful explicit reopening with exact bytes. Existing upload preservation and deferred-release
tests remain in the release gate. These checks do not cancel or modify real user cloud content.
