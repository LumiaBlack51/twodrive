# Changelog

[Home](README.md) · [简体中文](CHANGELOG.zh-CN.md) · [Downloads and release notes](https://github.com/LumiaBlack51/twodrive/releases)

User-facing highlights moved from the former README. Releases hold published assets and verification notes; [debian/changelog](debian/changelog) retains package history. These entries describe changes at release time, not fresh test results or a guarantee of current behavior.

## Development 0.1.3 — Retained cloud deletion status

Includes stable 0.2.14 diagnostics and durable deletion backoff, plus the earlier stable local unlink fix. Linux mount status separates local deletion from unconfirmed/rejected cloud cleanup and offers an explicit retry action. Direct desktop file deletion retains its synchronous error behavior. Original-object access restrictions remain unresolved; permissions and targets are unchanged.

## 0.2.18 — Cache retirement and automatic retention

Superseded saves and clean remote versions enter a durable reclamation queue. Open readers/writers, upload claims and pending deletions defer removal; unknown cache and recovery files are preserved. The daemon now applies `cache.retain_for` (default 30 days) in its background maintenance cycle, with pin, in-progress and recent-access protection.

## 0.2.16 — Repeated moves preserve the file

A rename from an outdated directory view now recognizes when cloud metadata has already moved the same file to the destination. It refreshes the directory names while preserving file contents and open handles. Database replacement also preserves self-replacements, rejects moved targets, and rolls back when the target update does not take effect.

The stale-inode and database regressions fail with the previous implementation and pass with the fix; an isolated FUSE regression covers the system rename and an open descriptor. These controlled tests use a mock cloud, while recovery of the reported file was independently verified against real Graph bytes. See [TD-20261017-01](doc/incidents.md#td-20261017-01文件管理器移动-pdf-返回输入输出错误). The English and Chinese homepages now describe shipped stable features, with development previews documented separately.

## 0.2.15 — Metadata writer contention

Metadata imports through clones of the same Database now queue the complete write operation, including pin recomputation, to avoid exhausting SQLite's lock wait under shared storage contention. Reads remain concurrent; independent instances and processes still use SQLite's normal lock arbitration. The schema and recovery formats are unchanged.

The controlled regression failed on the old code and passes with the fix; Windows CI also repeats the core suite at higher concurrency. This does not establish a real OneDrive end-to-end test or eliminate every possible SQLite Busy error. See [TD-20261015-16](doc/incidents.md#td-20261015-16同实例元数据写者竞争耗尽-sqlite-锁等待).

## 0.2.14 — Diagnose and defer rejected cloud deletions

Ordinary Graph/OAuth failures retain bounded, safe error codes and request correlation without remote messages or URLs. Pending deletions back off durably across restart while retaining caches and same-name protection. Status distinguishes local removal from unconfirmed cloud deletion; `retry-deletes` makes retained work eligible again. This does not establish or bypass the original object's access restriction.

## 0.2.13 — Delete files awaiting synchronization

Closed dirty and uploading files can be removed immediately from the local namespace. Durable deletion records coordinate in-flight uploads, cloud cleanup, cached open handles and same-name recreation. After an interrupted unbound upload, recovery may finish that claimed transfer to obtain its cloud identity before deleting it. Includes the Nautilus pending-download feedback already shipped in 0.2.12.

Details and validation limits: [TD-20261014-12](doc/incidents.md#td-20261014-12关闭后的待上传文件不能删除导致-git-锁文件残留).

## 0.2.12 — Pending-download feedback

Nautilus immediately shows requested pin/download work, retains feedback across refresh and file-object replacement, and allows retry after failure.

## Development 0.1.2 — Cloud accounts and anchored tray

Google Drive desktop sign-in now supports ordinary-file read/write, conditional replacement, directory backups and Linux FUSE. The connection page can add and manage inactive WebDAV/NAS, PC/NAS sharing, Google Drive and OneDrive accounts while connected; failed configuration persistence preserves the current list. Devices use opt-in signed LAN discovery and trusted invitation pairing over QUIC.

GNOME/Zorin tray positioning uses actual icon bounds on X11 and native Wayland through the included TwoDrive component; first installation may require a new desktop login. Linux/Windows bundles include installers and the [provider, login, upload and supporting-component specification](doc/provider-integration-spec.md). Google editor documents remain browser shortcuts; other Wayland compositors, organization/shared drives and arbitrary cross-network NAT traversal have not been accepted.

Details: [Google Drive](doc/google-drive-dev.md), [connections](doc/storage-connections-dev.md), [tray positioning](doc/tray-positioning-2026-10-14.md), [incident evidence](doc/incidents.md).

## 0.2.10-2 — Internal architecture refactor

Separate storage, persistence, OneDrive/OAuth, Files On-Demand, service orchestration and presentation by responsibility. No intentional behavior or data-format changes. Branch prerelease; not merged into main. [Architecture and verification](doc/refactor-2026-09-16.md).

## 0.2.10 — Execute compiled programs on the mount

Preserve local file and directory rwx permissions, including create/umask and chmod, across renames, remounts, uploads and cloud metadata refreshes. Compiled programs can now execute directly on the mount. FUSE enforces local permissions. These permissions stay in this device's database and do not sync to OneDrive; existing files keep their default mode until changed with chmod.

Details: [incident TD-20260916-01](doc/incidents.md#td-20260916-01c-编译产物在挂载中无法执行).

## 0.2.9 — Archive extraction reliability

Local file creation now atomically records cache ownership and Writing state, independently of cloud delta filtering. Extracting archives into folders still awaiting cloud creation or moves no longer fails with an I/O error. Cloud updates remain blocked behind pending parent operations. Includes the previously unpublished Nautilus shortcut helper change to keep filesystem probes off the UI thread.

Details: [archive extraction investigation](doc/archive-extraction-2026-09-15.md).

## 0.2.8 — Sign in without app registration

TwoDrive now includes its public Microsoft application ID. New users can sign in directly; missing, blank, and legacy placeholder IDs use the built-in application on load. Custom registrations remain supported. Organization consent policies may still require administrator approval.

## 0.2.7 — Release pinned content

Release space now cancels explicit and inherited pins for the selected file or folder subtree before safely releasing cache. Released children stay online-only beneath pinned parents. Automatic pruning still protects pins; uploads and open handles still defer release until safe.

## 0.2.6 — Save and move reliability

Fixes stale placeholder upload acknowledgements overwriting completed downloads, PDF save timestamps, and backup-save synchronization races. Child uploads/metadata operations wait for parent moves; queued descendant paths follow local moves, and stale remote metadata no longer pulls pending descendants back. Negative Graph directory sizes no longer abort metadata decoding.

Existing duplicate cloud directories require separate reconciliation; this release does not automatically merge or delete them.

Details: [folder moves](doc/folder-move-sync-2026-09-11.md), [download completion](doc/download-placeholder-race-2026-09-10.md), [PDF saves](doc/pdf-save-reliability-2026-09-09.md).

## 0.2.5 — Release space cancels downloads

Release space cancels downloads of unpinned files and removes partial data, including descendants for folder releases. Old handles cannot restart cancelled transfers; an explicit new open can download again. Checks occur during transfer/backoff; a blocked request must return or time out before cancellation completes.

Uploads retain deferred-release protection: unuploaded data stays until cloud confirmation. Always Keep protection is unchanged.

Details: [cancellation and verification](doc/download-cancellation-2026-09-08.md).

## 0.2.4 — Directory browsing and deferred release

Nautilus/GIO MIME probes no longer download entire cloud-only files merely to identify unknown extensions. Browsing uses SQLite metadata without a new content/header prefetch cache; unknown formats can use generic icons until opened. Explicit reads and copies still download content.

A release requested during writing/uploading shows cloud + syncing, including parent summaries. It becomes cloud-only after successful upload and safe cache removal; errors/conflicts are not presented as completed releases.

Details: [root cause and verification](doc/directory-browsing-2026-09-08.md).

## 0.2.3 — Responsiveness and recovery

The mount becomes available from local metadata before network recovery. Directory snapshots/background reads keep browsing responsive, and Nautilus state queries run asynchronously. Transient failures retain resumable upload sessions. Upload-only known-folder handling retries without duplicating local content or propagating local deletions.

Details: [responsiveness and recovery](doc/responsiveness-recovery-2026-09-08.md).

## Earlier history

See [debian/changelog](debian/changelog), [Releases](https://github.com/LumiaBlack51/twodrive/releases), and [historical engineering notes](doc/README.md#engineering-notes).
