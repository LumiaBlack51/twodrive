# Changelog

[Home](README.md) · [简体中文](CHANGELOG.zh-CN.md) · [Downloads and release notes](https://github.com/LumiaBlack51/twodrive/releases)

User-facing highlights moved from the former README. Releases hold published assets and verification notes; [debian/changelog](debian/changelog) retains package history. These entries describe changes at release time, not fresh test results or a guarantee of current behavior.

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
