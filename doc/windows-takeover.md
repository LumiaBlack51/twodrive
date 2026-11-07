# Windows Development takeover work

This work targets `dev`, not stable TwoDrive. The existing Microsoft client must
keep serving the current root until the replacement passes native, cloud and
recovery acceptance. No fixture capability is promoted merely because it builds.

## Reuse of Linux fixes

The Windows implementation must use `twodrive-core::Database` and the existing
storage contract. It must not duplicate `MetadataEntry` or silently use path IDs.
Native changes are complete immutable snapshots; publication is one conditional
transaction. An upload started before an Office atomic replacement cannot claim
the new snapshot, and an older acknowledgement cannot reset its size, mtime or
dirty state.

| Historical mechanism | Shared code / Windows acceptance |
| --- | --- |
| Empty browser destination replaced by complete data | Linux `begin_upload`/`commit_uploaded`; native journal uses immutable capture generations, full native atomic-replace still required |
| Locally created files beneath a pending directory | Linux `create_local_file`, `has_pending_metadata_at_or_above`; native creation remains required |
| Parent move precedes children, jobs move with subtree | `move_subtree_and_queue`, metadata recovery; native rename/restart required |
| Source mtime must survive an upload/delta echo | `commit_uploaded`, metadata upsert; new native upload/reconciliation timestamp tests |
| Readers/writers, dirty cache and pending deletes veto release | Core cache locks, journal and Windows oplock/USN checks; native test required |
| Cloud delete denial survives restart and backs off | Shared `recover_pending_deletes`; Windows baseline test passes |
| Ambiguous remote upload result | Compare full remote bytes before retry/ack; native production bridge required |
| External replacement while downloading | Download generation and native file identity must both match at publication |

See [folder moves](folder-move-sync-2026-09-11.md),
[empty-placeholder race](download-placeholder-race-2026-09-10.md),
[PDF mtime](pdf-save-reliability-2026-09-09.md), and
[journal bridge](windows-local-journal-2026-10-21.md).

## Migration constraints

The current user's read-only metadata inventory contains roughly 12,700 files,
98 GiB logical content and 10,700 online/recall files. Full hydration would exceed
the source volume's available space. These are aggregate observations, not a
private filename manifest and not downloaded bytes.

Migration must retain on-demand data, authenticate the same cloud account using
TwoDrive's independent login, preserve the original root and locally changed
bytes, and have a tested rollback. Never call `CfUnregisterSyncRoot` on a foreign
root: Windows can permanently remove its unhydrated placeholders. Stopping the
Microsoft client is a final cutover step after validation, not a prerequisite for
development. Cloud placeholders and opaque Microsoft cache fragments are not
portable complete files.

## Current verification boundary

The real Windows generated-file scenario passes ordinary ReadFile hydration,
same-length local edit, a synchronous no-recall/no-follow exclusive capture,
durable journal publication, conditional upload to a simulated provider and
full-byte comparison. It unregisters only its own generated root after retaining
the file. This is not a live OneDrive upload or an existing-root migration.

Shared-engine regressions cover native snapshot generation, reader/path-binding
vetoes, upload/acknowledgement races and the Linux timestamp rule. The native
capture reuses the existing CacheJournal; no second parallel writable database
owner was enabled. The CFAPI harness now depends on the shared backend contract
instead of compiling a separate Windows model. Its lockfile adds that dependency
closure; ordinary CI tests do not contact a cloud account.

Production account/root authority, persistent native registration/reconnect,
file-size changes, create/rename/delete, remote delta conflict reconciliation,
cache eviction, installed Office integration, real-cloud end-to-end and the
source-folder rollback/cutover remain incomplete. Desktop writable/native-root
capabilities stay disabled. Do not treat this preparatory change as takeover
readiness or stop the Microsoft client on its basis.
