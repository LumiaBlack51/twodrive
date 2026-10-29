# TwoDrive Development desktop

The current dev Flutter desktop supports paired devices, WebDAV/NAS, OneDrive, Google Drive, iCloud Drive and Baidu Netdisk with separate private account state. See [connection setup and actual verification](storage-connections-dev.md). Its installer does not manage the stable OneDrive daemon. The linked 0.1.1 release is the earlier preview; later local/source changes are recorded separately.

Download the [Development 0.1.1 Linux/Windows release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.1), then verify its SHA256SUMS before running. The stable Linux daemon is separately available in [0.2.12-1](https://github.com/LumiaBlack51/twodrive/releases/tag/v0.2.12-1).

Current Linux adaptation and explicitly scoped validation are recorded in [the PR47 Linux guide](linux-zorin-2026-10-29.md).

## Provider boundary (0.1.1)

The desktop and shared file/backup/mount services now use a provider contract.
Account fields, labels and capabilities come from engine IPC; protocol and
credential details stay in registered adapters. Existing profiles and cache
bindings are preserved. See [the architecture and regression record](provider-boundaries-2026-10-13.md).
Successful automatic proxy fallback displays a yellow warning triangle. Actual
connection or synchronization failures keep the error icon.

## User interface

- Click the cloud tray icon to open a compact provider/account status panel.
  The gear menu offers settings, folder backups, pause/resume, sign-out and quit.
- The panel shows actual manual, FUSE and backup transfers. A green backup status
  requires a completed backup scan; a connection alone is not proof of backup.
- Use Connections to add a paired device or an HTTPS WebDAV account. Each account
  offers automatic physical egress with reported fallback, strict physical direct
  routing, or system routing/proxy. Disconnect before editing a connection.
- Use Backups to select local directories and remote destinations. Nothing is
  selected automatically. Backups upload additions and modifications; stopping a
  backup or deleting a local source does not delete the server copy.
- Pause persists across restarts. It stops new sync requests and backup streaming;
  an already executing filesystem request may finish. New local FUSE writes remain
  durable and resume through the existing recovery loop (up to its 60-second scan).
- Settings contains mount/unmount, clean-cache release and login autostart.
  **Autostart defaults off and installation never enables it.**

## Build and install

Requirements: Flutter 3.41.8, Rust 1.96, GTK 3 development files, clang, CMake,
Ninja, pkg-config, FUSE 3 and a session D-Bus StatusNotifierItem host. Current
native acceptance now includes isolated GNOME 46 GTK X11 and native GTK Wayland.
The optional TwoDrive-only GNOME component provides actual icon geometry and compositor positioning; first installation may need the next desktop login to load. The launcher selects native Wayland when that component is available, preserving explicit GDK_BACKEND. Other Wayland compositors are not verified. See [positioning, installation and boundaries](tray-positioning-2026-10-14.md).

```bash
cargo build --locked --release --manifest-path experiments/twodrive-dev/Cargo.toml --features mount
cd apps/dav
flutter pub get --enforce-lockfile
flutter build linux --release
cd ../..
# Keep the pinned supporting component beside the portable runtime.
python3 scripts/install-rclone-component.py --platform linux-amd64 --destination apps/dav/build/linux/x64/release/bundle
python3 scripts/install-dav-desktop.py \
  --bundle apps/dav/build/linux/x64/release/bundle \
  --engine-dir experiments/twodrive-dev/target/release \
  --gnome-anchor packaging/gnome-tray-anchor
~/.local/bin/twodrive-dav
```

Public product name for this Linux source branch: **TwoDrive**. Internal `dav` paths and binary
names are retained for isolation and do not rename the product. Runtime lives in
`~/.local/share/twodrive-dav/runtime`; private profiles/cache/backup metadata live
under `~/.local/share/twodrive-dav/state`. The autostart entry, if explicitly enabled,
is `~/.config/autostart/twodrive-dav.desktop`.

## Safety and recovery

File actions bind both profile ID and revision before any remote request. Cache
binding includes the actual paired device identity or DAV origin/account. The
client refuses to adopt an already mounted directory, remove pending caches or
unmount with unfinished/conflicting writes. Downloads use a temporary sibling and
no-clobber rename. Credentials are private and omitted from profile listing IPC.

Folder backup configurations never delete source or remote files. Symlinks,
overlapping mappings, home/root, mount and private-state sources are rejected.
A private snapshot is streamed with bounded memory; durable upload intent is
written before PUT. A restarted worker reconciles an ambiguous committed PUT by
remote content hash before accepting its new ETag. Remote version conflicts stop
overwrites and remain visible. Source scans run every 10 seconds, skip unchanged
size/mtime fingerprints, and are bounded at 100000 entries per scan. Files changed
without any size/mtime change are outside this detection contract.

Safe reads have at most two retries for transient transport/502–504 failures,
before any response body is delivered. PUT, MOVE, DELETE and MKCOL are never
silently replayed. Physical HTTPS uses the existing bound DNS/CONNECT adapter;
automatic system/proxy fallback is selected by a safe connection probe before
mutations. Changing a route requires reconnecting. Numeric loopback gateways
always bypass external proxies; paired network traffic is governed by the
selected peer route policy.

## Provider delivery verification (0.1.1, 2026-10-13)

- Linux Development Rust: 49 tests; Flutter: 13 tests, analyzer and native release build.
- Windows: native engine/Flutter build, 13 Flutter tests, isolated IPC/backup/autostart and synthetic tray/focus/Escape checks.
- Non-DAV memory provider: opaque IDs, individual capabilities, conditional upload, actual IPC and backup recovery. Restoring old path-as-ID expressions fails both regressions.
- Final CI Linux bundle: real isolated FUSE plus loopback WebDAV smoke, including pause/resume, backup conflict protection and committed-upload recovery.
- Stable 0.2.12-1 deb: 151 default Rust tests, 25 Nautilus and 4 Settings tests; six ignored FUSE regressions explicitly pass with the packaged CLI, plus isolated packaged CLI/daemon smoke.

All four final CI workflows passed on 4554021, whose tree matches the dev merge.
This round used no actual cloud account and did not replace installed clients.
Native CI and synthetic tray events do not establish user hardware acceptance.
See [incidents TD-20261013-15 through 18](incidents.md) and the
[sanitized verification summary](provider-validation-2026-10-13.json).

## Earlier desktop verification (before provider decoupling, 2026-10-13)

- Rust: 28 unit, 11 integration and 2 signed-control tests; strict Clippy and format.
- Flutter: 9 model/widget tests, analyzer and native release build.
- Local real FUSE: existing-mount ownership, file round trip, clean-cache release,
  pending writes retained while paused and uploaded after resume.
- Actual paired Windows: 2097356-byte SHA-256 round trip, upload/download, MOVE,
  FUSE read/write, backup create/update/pause/resume and remote conflict protection.
- Three route selections each connected to the actual Windows server. Diagnostics
  reported physical-only, system routing, and physical-first automatic respectively.
- Native D-Bus tray registration/Activate and repeated exit code 0 checks. The
  former GTK implicit-view destruction assertion is avoided by SystemNavigator.pop.
- A first Windows run encountered a transient 502. A synthetic regression fails
  before safe-read retry and passes afterward, while confirming PUT is not replayed.
  The deeper cause of that particular 502 is still unknown; later success is not
  proof that all network failures have disappeared.

Automated desktop checks use disposable local/remote UUID directories and never
print credentials or private listings. The recovery test reconstructs a durable
committed-upload intent in isolated state; it is not a whole-machine power-loss test.
See [incidents TD-20261013-01 through 14](incidents.md) for old/fixed evidence and
remaining boundaries. Yesterday's Windows native proxy fallback validation is
separately recorded in [its report](peer-windows-egress-2026-10-12.json).

The source includes Linux desktop CI and a portable bundle artifact. The installed
Development runtime connected to the real Windows gateway (PROPFIND 207); the
tray panel displayed TwoDrive / Windows and “文件已就绪”, with no synthetic backup
success. Autostart remained absent. Sanitized checks are stored in the
[validation report](linux-desktop-validation-2026-10-13.json). The installed and isolated Linux native fixtures now also compare the settled
window with the live pointer and workarea, accounting for GNOME icon-local event
coordinates and the mapped window rectangle. Installed opening took 28–54ms,
closing 4ms, with a stable 390×570 logical rectangle at 2× scaling. These are
synthetic host events, not hardware input or multi-monitor acceptance. [Final Linux CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38354827650)
passed the GTK rectangle fix; the Windows source is unchanged from the successful
[native Windows CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38353525532).
The earlier shared engine/provider workflows also passed. Local cleanup removed
12036763648 allocated bytes of owned build directories (11.2 GiB), 50 temporary
paths, the empty obsolete mount directory and the failed Windows diagnostic
artifact. Installed runtime, device pairing and stable daemon were retained. No stable
release is replaced by this Development build.

## Windows desktop

The current opt-in native filesystem experiment is documented in
[Windows CFAPI adapter](windows-cfapi-2026-10-21.md). It is separate from the
desktop and does not enable Windows mounting or claim Explorer acceptance.

The same Flutter UI and IPC engine now have a native Windows runner, cloud tray,
per-user paths, owned process jobs and opt-in startup entry. Windows CI builds the
portable directory and exercises local WebDAV file operations and folder backups.
[Final Windows/Linux CI](https://github.com/LumiaBlack51/twodrive/actions/runs/38364410735) passed shared engine checks, 13 Flutter tests, release build,
background tray toggles, outside-focus dismissal and Escape. The fixture uses
owned windows and notification messages; physical Windows GUI acceptance is not
claimed. The earlier first-activation access violation was reproduced before
window-manager taskbar initialization and absent afterward.

Extract the complete portable directory and run `twodrive_dav.exe`, or run its
`install-desktop-windows.ps1` to install in LocalAppData and create a Start Menu
shortcut. Installation does not enable startup. WebDAV browsing, file transfer
and upload-only folder backups are shared. FUSE mounting is a Linux capability;
the Windows UI does not present unsupported mount/cache actions. A Windows virtual
drive driver and macOS runner are outside this delivery.

Current icon-centered positioning and its separate X11/Wayland evidence are recorded in [the 2026-10-14 guide](tray-positioning-2026-10-14.md). Earlier pointer-based positioning evidence remains historical.
