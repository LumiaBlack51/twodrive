# Combined TwoDrive dev preview

Integration date: 2026-10-07. The `dev` branch combines Windows [PR #9](https://github.com/LumiaBlack51/twodrive/pull/9), OneDrive `codex/peer-control`, and WebDAV/QUIC [PR #8](https://github.com/LumiaBlack51/twodrive/pull/8). Git merge ancestry retains both source branches. Your installed TwoDrive and the original `main` checkout are independent of these dev builds.

## Components and current behavior

| Component | Available behavior | Local state |
| --- | --- | --- |
| Windows Full/Lite, `twodrive-engine` and `twodrive-tray` | OneDrive sign-in, browsing, persistent metadata/delta index, explicit read-only downloads/resume, cache viewing; Full UI and Lite share one engine | Explicit Windows preview state root |
| `twodrive-peer` | Same-account OneDrive discovery, fingerprint trust, authenticated encrypted control messages, ping/pong, signed update verification | Dedicated peer state; DPAPI secrets on Windows |
| `twodrive-dev` | Existing WebDAV client, directory sharing, private single-use invitation, encrypted QUIC, NAT traversal/public relay fallback, loopback WebDAV gateway, Linux FUSE mount | Dedicated dev device state; separate mount cache |

The GUI still manages its OneDrive engine. Peer control and QUIC/WebDAV remain separate CLI protocols: cloud-control trust does not authorize a QUIC share, and the merge does not add automatic OneDrive-to-QUIC route switching. Pair QUIC devices using the single-use invitation. Cloud control messages cannot read or transfer files. CFAPI, bidirectional Windows synchronization, and GUI controls for WebDAV/QUIC remain future work.

A file saved in a newly created Linux mounted directory can wait for the existing 60-second background recovery cycle while the remote parent is being acknowledged; a successful local fsync is not a completed upload.

The shared Graph provider keeps PR #9's cancellation, callback validation, coordinated token refresh, strict browsing redirects and sanitized errors, alongside the peer's bounded opaque control store, pagination scope checks and deletion fallback. All historical incident entries from the three branches are retained.

## One Windows download for testing

The Windows dual preview workflow creates Full/Lite packages containing the same `twodrive-engine.exe`, `twodrive-tray.exe`, `twodrive-peer.exe`, and `twodrive-dev.exe`. Full also contains Flutter; Lite contains no Flutter/Dart runtime. Package manifests include hashes and an `includes_dev_tools` flag. These are unsigned development artifacts; this integration performs no installation or release publishing.

Build the combined packages on Windows:

```powershell
.\scripts\build-windows.ps1 -OutputDirectory C:\Temp\TwoDriveDevPackages -IncludeDevTools
```

The output directory must be new. Omit `-IncludeDevTools` to build the original Windows-only preview. Use `Start.ps1 -StateDirectory ABSOLUTE_DISPOSABLE_DIRECTORY` for Full/Lite. [Windows guide](https://github.com/LumiaBlack51/twodrive/blob/dev/doc/windows-preview.md).

## WebDAV/QUIC quick pairing

On the sharing Windows machine, choose an explicit directory and dedicated state; sharing defaults to read-only:

```powershell
$devServerState = Join-Path $env:LOCALAPPDATA 'twodrive-dev\server'
.\twodrive-dev.exe --state $devServerState serve --root D:\Share --invite-file .\invite.json
```

Transfer the private invitation through a trusted channel, then on the other Windows machine within ten minutes:

```powershell
$devClientState = Join-Path $env:LOCALAPPDATA 'twodrive-dev\client'
.\twodrive-dev.exe --state $devClientState connect --ticket-file .\invite.json
```

The client prints its loopback WebDAV URL and private credentials-file path. Use those credentials with a WebDAV client. Later reconnects omit `--ticket-file`; add server `--write` only when writes are intended. The cross-machine channel is always encrypted. Defaults use public discovery/relay assistance, with direct UDP when available; restrictive networks may use relay transport over TCP/TLS. No VPN is required.

[Chinese WebDAV/QUIC guide](https://github.com/LumiaBlack51/twodrive/blob/dev/experiments/twodrive-dev/README.zh-CN.md) covers existing WebDAV services, Linux mounting, custom relays and revocation.

## Existing OneDrive peer control

Run this separate workflow on both devices, logging in to the same account:

```powershell
$peerDevState = Join-Path $env:LOCALAPPDATA 'twodrive-peer-dev'
.\twodrive-peer.exe --state-dir $peerDevState init
.\twodrive-peer.exe --state-dir $peerDevState login
.\twodrive-peer.exe --state-dir $peerDevState run
```

From another terminal, run `peers` with the same `--state-dir`, verify the other device's full fingerprint against its local `init` output, and use `trust OTHER_FINGERPRINT` on both devices. Then `ping --to OTHER_FINGERPRINT` exercises control messages. Follow the [peer guide](https://github.com/LumiaBlack51/twodrive/blob/dev/doc/peer-quickstart.zh-CN.md); an unverified discovery record does not establish trust.

## Validation boundaries

Use temporary states, synthetic credentials and disposable files. Integration CI builds the current combined tree on native Windows and Linux, including Full Flutter analysis/tests/build, Rust tests, native IPC, peer update-process tests and encrypted two-process WebDAV transfer. Results are recorded in the [incident log](https://github.com/LumiaBlack51/twodrive/blob/dev/doc/incidents.md).

Historical source-branch live Graph evidence remains historical. This integration does not log in to a real account, modify cloud files, exercise the user's installed Windows machine, or validate two geographically separated NAT devices. Existing default-ignored FUSE tests and separately executed dev mount smoke are different verification scopes. A successful build or mock cloud test is not live OneDrive end-to-end evidence.
