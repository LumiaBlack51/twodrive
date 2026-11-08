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


An immutable publication source now serves a verified private-cache lease for the
entire CFAPI fetch. The bound hydration claim compares the published local/cloud
identity, path, ETag, size and mtime without importing fresh discovery. A newer
remote version before first download is refused; corruption remains recoverable
local work. One digest check precedes bounded 64 KiB transfers, with cancellation
registered before downloading and checked during verification. Generated Windows
regressions cover the retained lease, blocked writes, cancellation, unpublished
remote changes and corruption. The real personal OneDrive generated-file smoke
was rerun using this source; all four sizes, native acknowledgements, stale ETag
refusal and scope cleanup passed. This is still a component test, without a
persistent root projection/delta coordinator.

Dot's definite-pre-transfer rejection finding is fixed: a local pause before
Provider invocation returns the exact upload claim to Dirty using generation,
owner and digest checks. Actual provider errors remain uncertain; an intervening
native edit is retained and blocks a stale retry. See TD-20261108-22.


The account-checked Graph namespace boundary now uses explicit drive, item and
parent IDs, atomic fail-on-name-conflict creation, and If-Match moves/deletes.
Ordinary deletion uses the service recycle bin. A mismatched embedded drive ID
requires independent retrieval inside the exact bound drive; identifiers are not
case-folded. That authority readback never substitutes a newer content ETag for
the operation receipt. HTTP regressions and a generated real personal OneDrive
run passed directory creation, duplicate/name conflict preservation, cross-parent
move with stable ID/full bytes, rejection of a deliberately noncurrent tag, and
exact generated-scope cleanup. The native mutation consumer and concurrent
folder-delete/child-insert handling are still required. No original root changed.

The CFAPI reader initialization now checks offline/stopping/cancelled after
active-request registration and before any begin_read. Callback-body regressions
use a recording engine and replace only the OS transfer boundary; old ordering
opened the reader once, while fixed offline/stopping states open it zero times.
The per-chunk checks remain. See TD-20261108-23 and 24.

Relevant service contracts: [conditional move](https://learn.microsoft.com/en-us/graph/api/driveitem-move?view=graph-rest-1.0),
[conditional recycle](https://learn.microsoft.com/en-us/graph/api/driveitem-delete?view=graph-rest-1.0),
and [case sensitivity](https://learn.microsoft.com/en-us/onedrive/developer/rest-api/concepts/case-sensitivity?view=odsp-graph-online).


A read-only Windows name check of that catalogue snapshot found no unsupported
names/device aliases or CompareStringOrdinal(ignore-case) sibling collisions.
The maximum depth is 19; eleven complete paths are at least 260 UTF-16 code units
and the longest is 295. No contents were read. This is metadata preflight, not
Office long-path or full namespace publication acceptance. Native publication now
shares a portable leaf validator, including superscript COM/LPT device aliases
and console devices; its previous byte-length predicate failed the new alias
regression before correction. See TD-20261108-25.


An actual generated CFAPI root passed nested directory/file publication beyond
295 UTF-16 units, hydration, metadata inventory, disconnect/resume and exact
fixture retirement. Its data source was generated bytes, not OneDrive. The
catalogue's 284 DOCX/XLSX/PPTX files have a maximum complete path of 157 UTF-16
units; none are among those eleven long paths. This does not certify arbitrary
long-path Office saves.


The reusable namespace planner now verifies an explicit cloud root ID and builds
parent-first paths before registration, rejecting duplicate IDs, unknown parents,
unreachable cycles, unsupported objects, unusable versions, reserved names,
case-insensitive sibling conflicts and path/depth limits. Windows comparison uses
CompareStringOrdinal; comparison failure rejects the plan. Portable regressions
passed. An account-verified metadata audit against the private committed catalogue
accepted all 20,158 non-root entries with depth 19 and maximum complete length 295.
This is a plan for that snapshot, not fresh-delta reconciliation or publication.
It also found and fixed the shared root metadata URL's empty root:/ suffix; see
TD-20261108-26.


Native publication now has durable Preparing/Published records containing a
private opaque tag, account-owned root binding, cloud version and physical
volume/file ID. Schema 4 upgrades 2/3; older owners refuse it. Changed baselines,
pending native events and duplicate physical/tag claims cannot complete a
publication. Windows/Linux journal regressions verify restart and these vetoes.

BoundPublishedSource checks the kernel callback's registration bytes, root ID,
volume, physical file ID and size against that durable publication before opening
a reader. Context-free reads are refused. A generated actual CFAPI test hydrated
the published file, rejected a second physical placeholder bearing its tag,
authorized OS dehydration under the clean private-cache lease, and rehydrated
after recreating both engine and native connection. Private cache remained
retained; this does not establish automatic eviction or real-cloud transfer for
this new bound source. Both generated registrations were retired.

The source bridge's cloud-version comparison also now permits a preserved local
mtime to differ from the server's timestamp when identity/ETag/path/size match.
Its local metadata CAS still includes mtime. Old-code failure and generated
regression are recorded in TD-20261108-27. The lightweight cache-contract suite
now compiles the actual shared bridge on Linux as well as Windows.

The writable bound-source coordinator, projection advancement after local edits,
namespace consumer and final takeover remain unfinished.


The final-publication metadata binding now holds effective namespace leases
through the database callback. File leases request DELETE permission to engage
share-access checks without reading content; directory leases remain NT
DirectoryOnly. The old attributes-only file helper allowed an actual rename.
Its focused regression now refuses rename with zero hydration. Long-directory
and bound-reader/dehydrate/restart tests passed with these leases.

The shared upload-session store now records a full SHA-256 source fingerprint.
Same-size/same-mtime snapshots and legacy entries without a fingerprint cannot
resume each other's fragments. An old predicate regression failed; the corrected
Windows/Linux regression passed. A separate generated 20 MiB personal-cloud
race observed session HTTP 404 after a concurrent update; retry with the original
ETag failed with 412, and the concurrent full bytes/ETag survived. All generated
scopes and isolated credential copies were cleaned. This does not prove every
possible server race; see TD-20261108-28 and 29.

Windows native-sync without the Shell fixture now compiles and runs its 21 shared
bridge contracts and has a dedicated hosted CI step. A generated writable-map
probe also confirmed that the current capture refused entry after the original
file handle closed but a writable view remained; the registration was retired.
These remain prerequisites for, not completion of, the writable coordinator.


The initial coordinator now materializes a whole parent-first native tree with
persistent tags, directory observation, a verified full initial inventory and
sequence-bounded acknowledgement. Provider-owned initial publications precede
arming the watcher, then its mandatory rescan reconciles the gap without filling
the bounded queue with setup traffic. A newer watcher epoch cannot be acknowledged
by an older scan watermark. NativeSession seals account/drive/root configuration
with per-user DPAPI and uses the retained expected checkpoint for interrupted
registration; this is still not the installed startup/cutover path.

NativeAccount defers opening credentials until network I/O and verifies the exact
configured account/drive for each operation. Cached verified bytes remain readable
without credentials. Read-only authority failure before any data mutation returns
the upload lease to Dirty; ambiguous data errors remain fenced. A generated native
restart test used the sealed configuration with missing credentials: dehydrated
native data rehydrated from its private cache, while an uncached file was refused.
No normal Office/user profile was modified.

Projected native edits now retain their private tag and record the captured
physical binding atomically with each immutable snapshot, including cold complete
replacement before first hydration. Native acknowledgement advances publication
version/physical ID only while native, private-file, generation and SQLite leases
remain held. A generated actual coordinator test passed watcher-driven in-place
edit/upload/acknowledgement and a separate-process atomic replacement preserving
the cloud object, followed by native readback. The backend for this test was mock.
Namespace events veto new uploads/native in-sync state, while an immutable upload
already in flight can retain its data receipt for later namespace reconciliation.
Automatic new-file and nested directory creation now run parent-first. Directory
creation persists its physical claim and dispatch fence before cloud I/O; only
a definite pre-dispatch authority failure can reset it. An ambiguous create
remains fenced across owner restart rather than adopting a same-name folder.
Generated actual native validation passed a two-level ordinary directory plus
file through conversion, mock create-only receipts, publication and rehydration.
A separate generated-only actual personal OneDrive run also passed automatic
parent-first directory/file creation and independent native/cloud full-byte
readback; all generated cloud/native scopes and copied credentials were retired.
Bound-ID file/subtree rename now persists intent and dispatch/receipt phases,
then composes the shipped Linux subtree/pin routines with native journal and
publication advancement. A partial local commit is fenced and replayable.
Generated native external-process directory/file renames and a separate actual
personal OneDrive run preserve object IDs and complete bytes. Ambiguous move
responses remain fenced; no blind replay is enabled. Idle local workers use the
acknowledged watcher epoch/generation plus a durable-work veto, and full scans
use path/tag/physical maps rather than repeated whole-tree searches.
File delete consumption now requires durable matching root/volume/tag/physical
request and completion evidence plus the compared cloud version. A short prepared
phase can coalesce replacements or cancel a restored object before dispatch.
Conditional recycle receipts retire visible metadata but retain private revisions
under an independent archive journal key, protected from retired-cache pruning.
Generated actual native and actual personal OneDrive file-delete runs passed.
Directory deletion now waits for known descendants, verifies the bound parent
and an observed empty directory, and uses its service version for recycling.
This personal account omits directory cTag even with explicit selection: eTag
fallback protects the directory object and does not claim an atomic subtree
snapshot. Already observed nonempty directories are retained. Generated actual
Windows and personal-cloud two-level directory recycling passed.
Remote delta and pin/eviction consumers remain incomplete;
production takeover is not enabled by these tests.

The coordinator integration exposed a shutdown tail: native disconnect returned
before a private cache reader's Rust destructor completed. Explicit callback-scope
counting and a Condvar drain now precede source destruction. The regression checks
owner-lock reacquisition with no cleanup sleep; see TD-20261108-33.

Replacement upload recovery is now exercised through synchronize_once after
closing/reopening the native owner. An existing captured generation is reconciled
before another snapshot, then full native/private hashes gate publication. A
concurrent remote outcome remains Conflict and repeated passes create no extra
snapshots. The previous source order failed the native regression by retaining
three extra captures across three retries; see TD-20261108-34.


The Windows `native-sync` build also provides `twodrive-native-dev`. Its `prepare`
command accepts only a new state/root plus an account-verified private catalogue;
`verify` reconnects and checks native metadata/physical identities; `observe`
keeps hydration and durable notifications available for a bounded period while
exposing no cloud mutation consumer. These are staging diagnostics, not autostart
installation or an existing OneDrive root adoption command. Example arguments:

```powershell
twodrive-native-dev prepare --account <independent-account> --expected-owner <account-id> --catalogue <private-catalogue> --native-state <new-private-state> --root <new-root>
twodrive-native-dev verify --native-state <private-state>
twodrive-native-dev observe --native-state <private-state> --seconds 30
```

A complete-account side root was staged independently of the running Microsoft
client: 12,735 files and 7,425 directories, 20,160 published entries and
105,679,168,641 logical bytes. Initial/full independent-process inventories passed,
as did an actual limited, unpackaged Task Scheduler launch (package query 15700)
using the canonical protected state. The one-shot task was removed. Bounded
observation and a subsequent full inventory passed with zero durable work/native
events and zero private content-cache files. No user file contents were downloaded
for these staging checks. This is not an actual source cutover.
Only untracked ordinary Office lock/TMP/system artifacts are excluded from new
creation; existing cloud publications are retained and all underlying root/reparse
checks still run. The files are preserved locally. See TD-20261108-37.

Artifact exclusion preserves existing same-root creation claims, including an
interruption before conversion/metadata. A native upgrade regression failed the
previous early rejection; converted/paused and prepared-only creation restarts now
recover with a mock provider. See TD-20261108-38. This does not promise every
service accepts every historically claimed reserved filename.

The delete integration exposed completion RequestKey=0 on this host; the original
request key remains echoed in CfExecute responses. Historical matching accepts a
zero completion key only with the other durable identity/order/version conditions,
and rejects different nonzero keys. Failed/present-object request histories need
a held no-delete-sharing namespace lease for no-op consumption. The generated
regression holds DELETE access to prove the decision waits for that handle to
close. See TD-20261108-39. A file-ID lookup failure is never deletion authority.

ReplaceFileW with a retained backup previously moved the cloud target's name to
the backup; the old native regression failed. The target is now captured under
its logical identity, both complete revisions are independently owned before
reverting the old backup's cloud tag, and the obsolete physical binding is cleared
until final target acknowledgement. An interruption after native revert is
recoverable; independent backup bytes remain protected from pruning. Actual
personal-cloud generated validation preserves target ID/path and backup bytes.
Private cache schema 5 prevents older owners from bypassing these decisions.
Deletion preparation consumes its covered same-object history in the same FULL
transaction; retained decisions can replay legacy ACK boundaries. A rename/delete
batch with a prepare interruption passed actual native restart validation. See
TD-20261108-40 through -42 for evidence and service limits.
