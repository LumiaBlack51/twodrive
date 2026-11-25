# iCloud/Baidu integration with existing TwoDriveFS

This change addresses the maintainer's feedback on [#74](https://github.com/LumiaBlack51/twodrive/pull/74#issuecomment-7161766382), [#71](https://github.com/LumiaBlack51/twodrive/pull/71#issuecomment-7161926829) and [#72](https://github.com/LumiaBlack51/twodrive/pull/72#issuecomment-7162172825).

Linux iCloud and Baidu use the existing TwoDriveFS and its SQLite metadata, durable local edits, recovery queue, cache leases and cache-aware GIO behavior. Rclone remains the iCloud protocol adapter and the existing Windows mount component. This introduces no new filesystem implementation or atomic-CAS capability.

Providers without delta metadata return validated complete shallow listings. A transaction reconciles one directory, preserves stable local IDs when opaque provider snapshots change, retires obsolete cache references and protects dirty/error generations, queued namespace operations and deletion tombstones. Invalid/partial listings never authorize pruning. Completed empty directories are recorded too. Cached listings return immediately; refreshes coalesce by directory and use separate metadata workers. IPC can publish the same cached metadata before the authoritative refresh. Flutter retains successful responses and rejects obsolete account/path/revision events; it does not enumerate mounts.

Migration refuses current-account dirty Rclone VFS metadata and editor recovery spools, leaving both untouched. Existing cache bindings remain checked. Disposable tests exercise the native filesystem; a user upgrade still requires inspecting the actual old queues before switching.

Default folder backup updates the original cloud path and never propagates local deletion. Baidu reuses its multipart overwrite/is_revision flow and before/edited recovery copies. iCloud journals a verified temporary upload, rechecks the original snapshot, moves the original to an unpredictable recovery path, then publishes and verifies the staged object. Lost responses are reconciled by stable object identity and content, rather than filename alone. Conflicting targets and different pending local generations are preserved. Completed receipts cannot authorize a different generation. Previous iCloud cloud objects remain available at recovery paths; they occupy space and are not automatically garbage collected. This is recoverable replacement, not an atomic server-side comparison.

Versioned filenames are a separate explicit backup mode. Switching a confirmed versioned manifest to default mode retains the old version confirmations separately and creates a new baseline for the original paths. An unconfirmed old version intent must be recovered in version mode first. An unknown original target is never adopted as an overwrite baseline.

Mounted files open through an engine-verified original mount path and do not create a second viewed-files cache. Unmounted accounts use explicit downloads. Busy mounts report holders and offer retry after normal application closure; no holder is signalled. GVfs mounting, recent-files based transfer inference and frontend mount enumeration are removed. The test Debian package's missing Nautilus assets are documented.

## Verification and limits

- A disposable real FUSE mount enumerates full GIO metadata with zero cold downloads, still sniffs fully cached MIME data, and hydrates content while all four metadata workers wait on independent stalled directories. Explicit cold reading downloads exactly once.
- Database regressions cover changed opaque snapshots, cache invalidation, dirty subtrees, tombstones and rejected malformed snapshots.
- Replacement tests interrupt before/after each of the three cloud mutations, retry completed receipts and preserve a concurrent foreign target together with staged/original content. Provider contract tests keep CAS disabled and honor progress cancellation. The actual backup worker selects same-path updates even when the provider also supports versions.
- Migration tests retain dirty metadata and unsynchronized editor bytes. UI regressions cover late cached IPC events, directory navigation, transfer history, quit retry and preview alignment.
- These are synthetic provider/FUSE tests, not writes to live Apple/Baidu accounts. Native Windows NTFS ADS/Explorer/Office checks and production account migration are not claimed as verified. The already confirmed local recovery runtime was kept running during development.
