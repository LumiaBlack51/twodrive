<p align="center">
  <img src="packaging/icons/hicolor/scalable/emblems/emblem-twodrive-cloud.svg" width="72" height="72" alt="TwoDrive cloud status emblem">
</p>
<h1 align="center">TwoDrive</h1>
<p align="center"><strong>OneDrive Files On-Demand, in GNOME Files.</strong></p>
<p align="center">
  English · <a href="README.zh-CN.md">简体中文</a>
</p>
<p align="center">
  <a href="doc/getting-started.md"><strong>Get started</strong></a> ·
  <a href="https://github.com/LumiaBlack51/twodrive/releases/latest">Download</a> ·
  <a href="doc/README.md">Documentation</a> ·
  <a href="CONTRIBUTING.md">Contribute</a>
</p>

TwoDrive is an **experimental Rust OneDrive client for GNOME/Linux**. Browse your cloud files in Nautilus without downloading the whole drive. Open what you need, keep selected files locally, and release cached content when you need the space. A read/write FUSE mount connects your applications to a local cache and background Microsoft Graph synchronization.

<p align="center">
  <img src="doc/assets/tray-menu.png" width="520" alt="Real TwoDrive tray menu at idle, with the local token-file path redacted">
  <br>
  <sub>The real tray menu at idle. The local path has been redacted.</sub>
</p>

## Your files, with a local-first workflow

| In GNOME Files | Behind the scenes |
| --- | --- |
| **Browse first, download on demand.** Cloud placeholders expose names, sizes, and folders; reading a file fetches its content. | **Save locally, sync in the background.** Create, edit, rename, move, and delete through the mount. Pending changes are recorded locally for retry. |
| **Choose what stays.** Use **Always keep on this device** and **Release space** from the right-click menu. | **Keep transfers visible.** Nautilus emblems show file state; the tray lists active uploads/downloads and byte progress. |
| **Use familiar desktop entry points.** Open the mount from the tray and inspect paths and cache usage in the read-only Settings window. | **Recover interrupted work.** Durable queues and resumable large-file uploads separate a successful local save from cloud completion. |

The default mount is `~/TwoDrive/OneDrive`. [Everyday use and desktop controls →](doc/usage.md)

## File manager integration

See file status at a glance and manage cloud files directly in Nautilus.

### Files On-Demand, at a glance

<p align="center">
  <img src="doc/assets/nautilus-file-status.png" width="960" alt="Nautilus folder showing TwoDrive's blue cloud, purple pin, orange in-progress and green locally available status emblems">
  <br>
  <sub>Online-only, always kept, in progress, and locally available — in the same folder.</sub>
</p>

<details>
<summary><strong>What each status indicator means</strong></summary>

| State | Meaning |
| --- | --- |
| <img src="packaging/icons/hicolor/scalable/emblems/emblem-twodrive-cloud.svg" width="28" height="28" alt="Cloud"> **Online-only** | Visible in the mount; content is not cached locally. Reading it requires a download. |
| <img src="packaging/icons/hicolor/scalable/emblems/emblem-twodrive-syncing.svg" width="28" height="28" alt="Syncing"> **In progress** | Downloading, being written, waiting to upload, or uploading—not necessarily an active network transfer. |
| <img src="packaging/icons/hicolor/scalable/emblems/emblem-twodrive-synced.svg" width="28" height="28" alt="Synced"> **Available locally** | Cached content is available locally and can be released when eligible. This is not an Always Keep policy. |
| <img src="packaging/icons/hicolor/scalable/emblems/emblem-twodrive-pinned.svg" width="28" height="28" alt="Pinned"> **Always keep** | Pinned content is protected from automatic cache cleanup; Release space cancels the pin. Confirm its download has finished before relying on it offline. |
| <img src="packaging/icons/hicolor/scalable/emblems/emblem-twodrive-error.svg" width="28" height="28" alt="Error"> **Needs attention** | An error or conflict needs inspection. Do not assume cloud synchronization has completed. |

<img src="packaging/icons/hicolor/scalable/emblems/emblem-twodrive-cloud.svg" width="22" height="22" alt="Cloud"> + <img src="packaging/icons/hicolor/scalable/emblems/emblem-twodrive-syncing.svg" width="22" height="22" alt="Syncing"> **Release pending:** a release was requested while changes were still being written or uploaded. Local data stays until upload succeeds and open handles close.

Folder emblems summarize descendants; they do not prove that every file is cached. Newly discovered cloud files remain online-only, including beneath an already pinned folder. [Pinning and release semantics →](doc/usage.md#keep-or-release-content)

</details>

### Manage files with a right-click

Keep selected content on your device, release its cache without deleting the cloud file, or check its status — without leaving the file manager.

<p align="center">
  <img src="doc/assets/nautilus-context-menu.png" width="460" alt="A selected file in Nautilus with TwoDrive context-menu actions: Copy path, Release space, Always keep on this device, Cancel always keep on this device, Sync now and View status">
  <br>
  <sub>Local availability, synchronization, and status controls in the native context menu.</sub>
</p>

[Explore the desktop controls →](doc/usage.md#keep-or-release-content)

## Get started

**Install → Sign in → Mount.**

Download the **amd64 `.deb` and checksum** from [Releases](https://github.com/LumiaBlack51/twodrive/releases/latest). The package targets Ubuntu/Zorin OS with GNOME and includes the daemon, CLI, Nautilus extension, tray, and Settings helper.

Since 0.2.8, TwoDrive includes its public Microsoft application ID: **no app registration or client secret is needed**. Follow the [installation and sign-in guide](doc/getting-started.md) to authorize your account with OAuth 2.0 + PKCE and start the service. Organization policies may require administrator approval. For other Linux setups, see [building from source](CONTRIBUTING.md#build-and-check).

## Before trusting it with your files

> [!WARNING]
> TwoDrive is experimental. Keep independent backups of important files. **Deleting inside the mount also deletes the cloud item; Release space does not.** A successful local save is not confirmation of a completed cloud upload.

Tokens are stored in a local JSON file with mode `0600`, not an encrypted keyring; Secret Service is not integrated. File and directory rwx permissions are stored locally and enforced on this mount, including executable files and `chmod`; they survive remounts but do not sync to OneDrive. Cloud-only imports and existing entries default to 0644 (files) or 0755 (directories). Full POSIX ownership, special mode bits, and directory timestamp semantics are not preserved; the mount root mode remains fixed.

The desktop helpers are still limited: **Settings is read-only**, and **Pause sync currently changes the tray display only**, not the daemon. See [current limitations and safety](doc/usage.md#current-limitations-and-safety) before use.

## Development

Contributions, reproducible bug reports, and documentation improvements are welcome. Start with [CONTRIBUTING](CONTRIBUTING.md) for the Rust workspace, test commands, and an isolated mock backend that does not touch your real OneDrive. [Release history](CHANGELOG.md) and [engineering notes](doc/README.md#engineering-notes) live outside this overview.

[MIT license](LICENSE). An independent project, not an official Microsoft client.

## Experimental device peer

The `codex/peer-control` branch adds a standalone Windows/Linux `twodrive-peer`
control process alongside the refactored provider architecture. It uses separate
local state and does not replace the installed mount or daemon. See the
[implementation and security boundary](doc/peer-implementation.md) and
[first two-machine test guide](doc/peer-quickstart.zh-CN.md). Native Windows
artifacts are built by the Peer native builds workflow. This stage implements
authenticated control messages and signed updates, not file backup transfer.
