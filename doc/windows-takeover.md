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

The shared core/fs/backend storage changes through Linux main `a64ce65` were
subsequently ported into this development worktree: untouched writable-open
handling, cloud cache-version invalidation, retired-cache references and trash
retention, missing-remote conflict recovery, and upload/rename identity ordering.
This is a selected source port, not a merge of stable releases or unrelated
known-folder throttling. Existing native journal guards were retained, and the
new retired-cache removal path additionally respects native journal references.
Linux backend/core/fs default tests and all eleven explicit mock FUSE checks
passed; Windows native journal tests passed. These establish shared recovery
behavior, not completed CFAPI create/rename/delete callbacks.

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

An opt-in `native-sync` platform owner now retains registration on disconnect and
resumes only the saved root identity, volume serial and directory file ID. A new
root now requires an external checkpoint, flushed before registration. Recovery
can complete an interrupted registration only for that exact empty ordinary
directory, and refuses a replaced directory or foreign registration. The
explicit generated test passed cached offline reads, retained online-only entries,
wrong-receipt refusal and resumed hydration. Its two generated files were retained
as ordinary files before that test alone unregistered its root. The production
type intentionally exposes no unregister/delete operation.

The companion Office test also passed native root binding and actual STA-timer
Word/Excel/PowerPoint saves on a generated CFAPI root. The fixture has no cloud
provider, and was unregistered only after its three generated documents had been
removed and its root confirmed empty. Neither test establishes account-backed
restart recovery or full writable native root support.

Production account/root authority and persistent engine composition,
file-size changes, create/rename/delete, remote delta conflict reconciliation,
cache eviction, installed Office integration, real-cloud end-to-end and the
source-folder rollback/cutover remain incomplete. Desktop writable/native-root
capabilities stay disabled. Do not treat this preparatory change as takeover
readiness or stop the Microsoft client on its basis.

## Account preflight, 2026-11-08

A read-only probe of an isolated copy of the older preview login succeeded, but
its returned drive/owner identity did not match the current personal client's CID.
It was not used for cloud writes or takeover. An independent explicit-account
PKCE login then completed with the user, and the resulting owner matched the
current personal client. No personal identifiers or credentials are recorded here.

`onedrive::open_local_session` obtains the service's account/drive identity through
the existing provider before opening a journal; a missing authority cannot reset
or recover an existing cache. The diagnostic probe exits unsuccessfully when an
explicit expected owner does not match. Account selection is explicit for the
migration login; the original login entry points retain their existing behavior.

An explicit 1,048,699-byte generated-file test subsequently passed real personal
OneDrive upload, native CFAPI hydration, same-length edit/capture, conditional
upload, complete cloud readback and stale-version refusal. Only a fresh generated
cloud directory was used; ordinary delete removed its file and then its verified
empty directory. The native fixture retained its bytes before unregistration and
was then removed. This does not establish the remaining full-root operations or
Office-document-to-cloud synchronization. See the [scope-specific results](windows-native-validation-2026-11-08.json).

Dot review follow-up: recovery-copy deletion and version precedence, untouched
writable-handle attributes, and cached read retention now have targeted
regressions. Four old-code regressions failed before their fixes. The native
checkpoint test exercises a real rejected second connection and recovery, and
nested publication accepts owned cloud directories while refusing ordinary and
junction parents. See incidents TD-20261108-07 through 09.

Native variable-size capture now passes actual generated Windows saves of short,
empty and larger content through the shared journal and conditional mock upload.
Native in-sync acknowledgement compares the current full digest, size and mtime
under the retained native lease and the acknowledged journal generation; a later
same-size edit is refused and marked dirty. Pinned acknowledgement does not grant
eviction. Live Graph variable-size/Office validation and worker integration remain
required; this does not enable production takeover capabilities.

The generated personal OneDrive validation subsequently passed 5-byte, empty,
1,056,892-byte and 1,048,699-byte saves with full cloud readback and native in-sync
state confirmation. The stale original ETag was rejected, and the generated cloud
folder and native root were cleaned. This still does not establish Office atomic
replacement, Explorer icon behavior or original-folder takeover.

Atomic replacement capture is now available behind `native-sync`: an ordinary
replacement is converted without dehydration or an in-sync claim, under the
owned parent chain and a unique resident-file lease. The bridge requires its
bound cloud path, persists native-dirty intent first, and publishes the immutable
snapshot before conditional upload. Actual Windows generated tests passed busy
reader deferral, replacement content preservation, foreign identity refusal, and
restart after conversion but before journal publication. They use MockProvider,
not real Office applications or a production worker.

Actual hidden Office probing found all three applications deny a share-none
reader while their document remains open after Save. They permit a read oplock,
but acquiring that oplock alone does not prove a coherent concurrent-write
snapshot. The current exclusive capture therefore defers cloud upload while a
document is held open; the local autosave timer still works. No weaker sharing
mode has been enabled in production on the basis of this diagnostic.

Windows CFAPI CI compiles the actual development bridge and its test source via
`tests/engine_cache_contract.rs` and `tests/desktop_contract.rs`, with all features.
The pinned-cache mismatch regression needs no registration and is included in
those default Windows test suites; native ignored tests remain opt-in.

The owned-root namespace layer now inventories ordinary additions and cloud
placeholders using metadata only, rejects junctions/unknown reparse states, and
retains directory identities during enumeration. File-ID lookup follows a known
identity through rename or move-out. Deleted identities can return an unresolved
Windows error; this is deliberately not cloud-deletion authorization. The actual
generated test observed zero hydration callbacks. No namespace mutation worker
is enabled by this observer alone.

Native placeholder rename/delete notifications can now be recorded through an
opt-in durable event sink. SQLite FULL commits precede allowing the operation;
completion observations survive restart, and acknowledgement removes only the
selected owner/sequence prefix. Actual Windows tests use a separate process:
a failed append denies its rename, while successful rename/delete produce
identity-bound request and completion records without hydration. Provider-owned
I/O and ordinary unconverted files did not produce these application callbacks.
An independent ordinary-file watcher, account-bound coordinator and cloud
mutation/recovery consumer remain required. Events alone enable no cloud writes.

Directory observation now uses directory-only NT opens relative to retained
parents, with enumeration access and no delete sharing. Queued directories carry
the complete expected ancestor identity chain. Two old-code race oracles failed;
three native regressions now pass, including replacement by an online-only file
without a hydration callback. Nested publication and replacement parents use the
same effective lease.

A fresh read-only catalogue audit of the independently authenticated personal
account completed 202 pages: 20,159 items, 12,733 files, 7,426 folder items, and
105,678,248,836 logical file bytes. The indexer classified no remote-item/package
objects as unsupported. This is metadata only, not file downloads, namespace
compatibility certification or migration. The private catalogue is retained
locally; no personal names, IDs or database are committed. The host resolved its
AppData output under the desktop package's LocalCache, so deployment must account
for the canonical state location instead of assuming an unpackaged process sees
the same logical AppData path.

The user confirmed Personal Vault is unused; its service feature is excluded from
the intended takeover scope. Ordinary account files still require the remaining
persistent coordinator and migration acceptance.

Actual Word/Excel/PowerPoint timer-generated OOXML subsequently passed the
personal OneDrive pipeline: a generated initial native-save baseline was uploaded,
then the timer's final content was captured under the owned CFAPI root, uploaded
conditionally, read back in full and acknowledged natively. Editors were closed
before exclusive capture. Three generated cloud files/folder, local documents,
baselines and native registration were retired. This proves the component data
path; it is not an installed COM/ribbon or unattended whole-root coordinator test.

Complete ordinary replacements before first hydration now establish a Dirty
initial cache journal without downloading the old version. Their observed cloud
baseline and pending-work checks remain mandatory; stale hydration cannot publish
over the new generation.


New native ordinary files now persist a physical/root-bound creation claim before
conversion, then establish an unbound Dirty snapshot. Atomic create-only upload
binds the real cloud ID without changing the native local identity. Schema 3
upgrades schema 2 and is refused by the older binary. Generated Windows tests
cover interruption before snapshot publication, restart, cloud binding and a
subsequent edit. Mock lost-reply reconciliation requires complete re-downloaded
bytes; a same-name conflict leaves both sides intact. A fourth generated TXT file
in the actual Office/cloud run passed new-file upload, full readback and native
acknowledgement; all generated scopes were retired.

The recursive ordinary-file watcher now reports physical file/parent IDs for
create/write/rename/delete without reading content. Its durable wrapper commits
whole batches before exposing a watermark; partial SQLite failure rolls back the
batch and stops the subscription. Restart and overflow require inventory, and a
scan cannot clear a changed epoch/generation. Native generated rename/delete
observations survived watcher recreation; cancellation drains pending OS buffer
references. These are observations, not permission to delete a cloud item.
The persistent coordinator and namespace mutation consumer remain outstanding.
