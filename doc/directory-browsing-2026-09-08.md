# Directory browsing and deferred-release emblems (0.2.4)

## Root cause and correction to the previous verification

The reported `resource` directory listed names and stat information in less than 1 ms. However,
`gio list -a 'standard::*,access::*'` stalled. A syscall trace showed GIO opening an unknown-extension
multi-gigabyte `.part.02` file with `O_RDONLY | O_NOATIME`, then reading its header for MIME detection.
TwoDrive 0.2.3 correctly moved the read to a worker, but that worker still hydrated the entire file.
The file manager waited for the read even though other FUSE requests remained responsive.
The 0.2.3 directory-name-only test did not cover this GIO behavior.

The implementation is based on the observed trace and GLib's
[local content-type detection](https://raw.githubusercontent.com/GNOME/glib/2.80.0/gio/glocalfileinfo.c).
GLib uses O_NOATIME for sniffing and falls back to its filename-based guess when the open fails
with ENODATA. Ordinary input streams and copies use normal read opens.

TwoDrive now declines this speculative open for uncached files when the executable is `nautilus`
or `gio` and the open flags match the probe. It does not block ordinary reads, tools such as `cp`
using O_NOATIME, or sniffing bytes that are already locally cached. The caller is identified through
`/proc/<request-pid>/exe`, including requests from GIO worker threads.

An unknown format can display a generic icon without downloading content. Explicit opens/copies
still hydrate normally. Thumbnail/indexer suppression and the asynchronous read workers remain.
A "Transport endpoint is not connected" error can also occur when a file manager retains handles
across a daemon restart; reopening the directory after the upgrade clears those old handles.

## Metadata and storage

Names, sizes, types, paths and cloud identities are already in SQLite. The reported installation has
about 16,000 entries and a roughly 15 MB metadata database. This release adds no new database
columns, no header cache, no speculative file download and no persistent directory snapshot cache.
Memory directory snapshots are released when their directory handle closes. Database size scales
with the number/path length of entries, not with the byte size of the remote files.

The metadata database is distinct from cached file contents. Unsynced writes must retain their
actual bytes until the cloud acknowledges them; selecting Release space queues that intent rather
than deleting the only complete copy. Existing retention/size policies still apply to ordinary,
unpinned content caches. Pinned and dirty data are not disposable metadata.

## Deferred-release emblems

The Nautilus background query now reads `release_pending`. Writing, dirty or uploading files with
this flag show two emblems: cloud plus syncing. Parent directories aggregate the same state using
an indexed descendant path range. After the upload commits and the last cache handle permits
release, the normal database transition to online-only leaves just the cloud emblem. Error/conflict
states retain their error emblem; ordinary uploads retain only the syncing emblem.

## Regression and release checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 -m unittest discover -s packaging/nautilus -p 'test_*.py'
cargo test -p twodrive-fs gio_directory_metadata -- --ignored --nocapture
cargo test -p twodrive-fs mounted_large_listing_and_copy -- --ignored --nocapture
bash scripts/build-deb.sh
```

The GIO regression mounts an isolated mock filesystem, enumerates all standard/access attributes
for 30 virtual 4 GB unknown-extension files, verifies zero backend downloads and zero cached-file
records, then explicitly copies a real file through GIO and verifies its bytes. On the development
machine the listing took about 31 ms. Both FUSE regressions require `/dev/fuse` and `fusermount3`;
the new one also requires `gio`. Python tests cover dual emblems, normal uploads, completion,
error priority, actual callback emblem additions and parent-directory aggregation.

Upgrade the daemon and Nautilus extension together. Stop/reopen Nautilus around a mount restart to
avoid stale mount handles. The deb release is `twodrive_0.2.4-1_amd64.deb`; install it with
`sudo apt install ./twodrive_0.2.4-1_amd64.deb`, then restart the user service and Nautilus.
Existing upload sessions and pending-release requests are preserved.
