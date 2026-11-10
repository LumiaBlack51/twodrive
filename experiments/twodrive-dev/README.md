# TwoDrive dev: WebDAV and encrypted device sharing

[简体中文](README.zh-CN.md) · [Design](../../doc/dev-webdav-peer-design.md) · [Incident records and verification](../../doc/incidents.md)

The `dev` branch introduces the independent `twodrive-dev` executable. The installed stable TwoDrive, its configuration/database and the original main checkout are unchanged. The dev branch also combines the Windows preview and OneDrive peer-control sources; see the [combined guide](../../doc/dev-integration.md). The experiment reuses the filesystem engine through a separate Cargo workspace, lockfile, device state and mount cache.

Implemented: existing HTTPS WebDAV connections, explicit directory sharing, single-use device invitations, QUIC/TLS encryption, reliable UDP/direct connectivity with NAT traversal and encrypted relay fallback, device revocation, an authenticated loopback WebDAV gateway, and optional Linux FUSE mounts. No VPN, virtual network interface or routing configuration is required. This is remote filesystem access, with no Syncthing-style bidirectional directory mirroring. The latest dev [Development desktop](../../doc/storage-connections-dev.md) has a Sharing page to choose a directory, start/stop hosting and export a connection file. The other PC imports it from Add connection; the CLI remains available. This GUI is newer than released 0.1.4. See [connection flow, update checks and distribution preparation](../../doc/distribution-preparation.md).

## Build

Requires Rust 1.91+. Linux mounting requires `/dev/fuse` and `fusermount3`.

```bash
cd experiments/twodrive-dev
cargo build --release --locked --features mount
```

Output: `target/release/twodrive-dev`. Omit `--features mount` on Windows or when only using WebDAV/device sharing; Windows produces `twodrive-dev.exe`. GitHub Actions builds native Linux/Windows workflow artifacts, distinct from stable release packages. Both devices use this dev executable; no OneDrive sign-in is required.

## Pair two devices

Choose a dedicated share directory on the server. Read-only is the default:

```bash
twodrive-dev --state ~/.local/share/twodrive-dev/server serve \
  --root /path/to/shared --invite-file ./invite.json
```

Transfer the private invitation through a trusted channel, preserving mode 0600 on Linux, then connect within ten minutes:

```bash
twodrive-dev --state ~/.local/share/twodrive-dev/client connect \
  --ticket-file ./invite.json
```

The client prints the pinned server public key, current transport, loopback URL `http://127.0.0.1:4918/`, and private credentials JSON path. Use its username/password with a WebDAV client. Local HTTP requests travel inside authenticated QUIC streams between devices; remote traffic remains encrypted. The server does not expose public HTTP.

Reconnect without `--ticket-file`. Issue a new invitation for another device, replacing any outstanding invitation, or revoke an existing device:

```bash
twodrive-dev --state ~/.local/share/twodrive-dev/server invite --out ./invite2.json
twodrive-dev --state ~/.local/share/twodrive-dev/server peers
twodrive-dev --state ~/.local/share/twodrive-dev/server revoke DEVICE_ID
```

Revocation denies subsequent requests on existing connections. An in-flight operation can finish; stop the server for immediate interruption. Invitations are single-use bearer secrets: their holder can redeem them first. Keep them private and delete them after pairing. Grants bind to the share directory and permission mode; use new state when changing either.

For writes, add server `--write`. On Linux, add client `--mount /path/to/empty-mount`; a writable mount also requires client `--write`. Its database/cache live under the client's `mount/` state, separate from stable OneDrive paths.

```bash
twodrive-dev --state ~/.local/share/twodrive-dev/client connect \
  --mount /path/to/empty-mount --write
fusermount3 -u /path/to/empty-mount
```

A successful local save and completed remote upload remain separate events. Writable mounts reuse TwoDrive's durable pending-work queue and retry failed network operations. Close open files and unmount the dev target before exiting. Default read-only mounts reject writes in the kernel.

## Existing WebDAV services and local sharing

Put `{"username":"...","password":"..."}` in a private `auth.json` (0600 on Linux). Do not put passwords in URLs, command arguments or logs.

```bash
twodrive-dev webdav --url https://dav.example.org/files/ --auth-file ./auth.json list
twodrive-dev webdav --url https://dav.example.org/files/ --auth-file ./auth.json get /report.pdf ./report.pdf
twodrive-dev webdav --url https://dav.example.org/files/ --auth-file ./auth.json --write put ./new.txt /new.txt
twodrive-dev --state ~/.local/share/twodrive-dev/dav-mount webdav \
  --url https://dav.example.org/files/ --auth-file ./auth.json mount /path/to/empty-mount
```

Also available: `mkdir`, `mv`, `rm`. PUT creates by default; overwrites require the current ETag from `list` using `--if-match`, preserving quotes. Missing, malformed or weak ETags cannot authorize conditional overwrites. Downloads do not overwrite an existing local destination.

For a standalone authenticated local WebDAV service:

```bash
twodrive-dev --state ~/.local/share/twodrive-dev/local-dav dav-serve \
  --root /path/to/shared --listen 127.0.0.1:4919
```

This is read-only by default and prints a private credentials path. Plain HTTP is restricted to numeric loopback addresses; use `serve`/`connect` for encrypted remote access. The WebDAV client refuses redirects and insecure remote HTTP, and verifies TLS certificates.

## NAT and relays

The default uses iroh/N0 public relays and address discovery. Ordinary outbound connectivity is needed; public IPs and port forwarding are unnecessary. Relays can see public device IDs, IPs, timing and traffic size, but cannot decrypt file content. Public infrastructure availability and speed are outside this project's guarantees.

- Self-hosting: use `--relay https://your-iroh-relay.example/` on both sides, repeatable. This mode does not use public N0 DNS. Deploy an [official iroh relay](https://github.com/n0-computer/iroh/tree/main/iroh-relay) with reachable HTTPS.
- LAN/offline tests: use `--no-relay` on both sides, disabling public relays and discovery. New invitations refresh UDP address hints after restarts.
- Relay diagnostics: use `--relay-only` on both sides. Direct IP transports are disabled and the connection reports `transport=relay`.

`serve` and `connect` default to `--network-route auto`: select an active physical interface and constrain peer-sharing DNS, UDP, relay TCP, and signed address publication to that egress. The first direct attempt ignores HTTP proxy environment settings and TUN fake-IP DNS. A private loopback connector, authenticated with random credentials kept in memory, carries opaque relay TLS; certificate and device identity verification remain enabled. No global proxy or routing changes are required.

On physical setup, DNS, TCP, publication, or initial peer transport failure, `auto` permits system/proxy fallback and prints `network: route=system/proxy fallback` with the stage. Peer transport retries happen before authorization; expired/consumed invitations or revoked grants do not trigger that retry. `connect` also prints the selected path and RTT.

- `--network-route direct`: disable automatic fallback, useful for checking physical connectivity alone. Unavailable interface binding can fail the attempt.
- `--network-route system`: deliberately use system routing and iroh-supported HTTP(S) proxy environment settings.
- `--network-interface NAME`: select a physical adapter by OS name, Windows friendly name, or index. In `auto`, an unavailable adapter reports system/proxy fallback; in `direct`, it fails. This also permits a controlled fallback test without changing global routes.

System fallback prefers explicit HTTP(S) proxy environment settings. On Windows it then reads the current user's static HTTP(S) proxy configuration, preserving bypass rules for the configured relay hosts. It reports the proxy source and presence flags without printing addresses, credentials or PAC contents. Automatic PAC/WPAD execution and SOCKS proxy support are outside this implementation. A/AAAA errors or empty results on the system route also try the native OS resolver, with bounded waits and outstanding calls; this allows TUN fake-IP there. Physical DNS and physical TCP continue to reject fake-IP. TXT keeps the existing resolver.

For a live Windows verification, `scripts/verify-network.ps1` accepts a disposable share, its existing private state, and a separately verified executable SHA-256. It temporarily replaces exactly the matching test server, measures physical-source TCP and unchanged routes, and tests a missing-adapter fallback. An already paired Linux verifier sends only bounded `next`/`finish`/`abort` signals through the share. The script restores the original server and deletes its own executable, script, and probe files; the verifier collects and deletes the remaining sanitized report. For autonomous retries, use the remote test process controller described below; this earlier staged script restarts the transport server.

Bypass is best effort. Linux TCP/DNS use interface and source-address constraints; Windows uses interface indices. iroh UDP currently binds physical source addresses. Forced VPN, WFP, or transparent gateway policies may remain effective; recognizing every virtual adapter and arbitrary SOCKS fallback are not guaranteed. The interface is selected at startup; restart dev after switching networks to reselect it. This policy covers experimental peer sharing; external WebDAV mounts and stable OneDrive egress are outside this change.

The [2026-10-11 live record](../../doc/peer-direct-egress-2026-10-11.json) covers local FlClash TUN bypass, real Windows-peer data verification, and cleanup in `direct` and final default `auto` modes. Bypassing a local proxy can still use an encrypted relay and does not establish direct UDP hole punching. [Native Windows and live dual-TUN verification](../../doc/peer-windows-egress-2026-10-11.json) passed default physical TCP egress and Linux fallback; Windows fallback startup exited and remains under investigation (TD-20261011-10).

Complex NAT or blocked UDP can require a relay, whose underlying transport may use TCP/TLS. Direct UDP cannot be guaranteed in every network. Discovery and relays do not store files; device secret keys stay local.

## Verification and boundaries

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked --features mount -- -D warnings
cargo test --locked --features mount
python3 scripts/smoke.py --binary target/release/twodrive-dev --network lan --mount
python3 scripts/smoke.py --binary target/release/twodrive-dev --network lan --mount --read-only
python3 scripts/smoke.py --binary target/release/twodrive-dev --network relay-only
```

The smoke script runs two real processes with disposable states/files and unmounts only its own target. It never changes an installed service or OneDrive state. Relay-only smoke needs public infrastructure; default automated tests do not contact it.

WebDAV polls full snapshots: maximum 100,000 resources and 16 MiB per metadata response. It has no native delta or persistent remote file ID; moves change path IDs. The built-in server exports regular files/directories, hides symlinks in listings and confines filesystem operations to a directory capability. Full PUTs default to an 8 GiB limit (`--max-upload`); completed data is synced and atomically replaced, while interrupted transfers preserve original content. The paired backend negotiates binary delta uploads for versioned files (including PDF/Word/PowerPoint), with full PUT fallback and an explicit full-upload policy. Generic external WebDAV clients still use full PUT. Resumable PUT and Content-Range remain unsupported. See [delta protocol, frontend contract and verification limits](../../doc/peer-delta-upload.md). Network mutations serialize conditional checks; local writers are outside this lock. External WebDAV atomicity depends on that service.

Unix state directories are 0700 and files 0600. Windows inherits directory ACLs; choose private current-user state directories. Files at rest are not encrypted. Local QUIC, public relay and real geographically separated NAT tests are distinct verification scopes. See the incident records/PR for actual results; same-host or mock tests do not establish real OneDrive or all-network NAT behavior.

## Remote test process control

`test-agent` is an explicitly started, leased controller carried by an existing paired writable TwoDrive share. It leaves the original `serve` running. The pinned control owner must already be authorized in the original private state. Owner-signed requests and server-signed responses bind a random session, increasing sequence and short expiry, rejecting tampering and replay.

Place a native build and `scripts/start-test-agent.ps1` in a fresh shared `td-control-<32 hexadecimal characters>` directory. Verify its SHA256 independently, then launch the bootstrap once with the original `-State`, public `-Owner` and `-ExpectedSha256`. The default lease is two hours and cannot be extended remotely. `scripts/control-test-agent.py` then sends `start`, `snapshot`, `stop` and `finish` through an already connected loopback WebDAV gateway. Snapshots retain startup errors, exit codes, bounded logs and Windows TCP/route samples. The control transport stays available after a test child fails, supporting diagnosis, retry and candidate upload.

Each test server has fresh private identity, share and temporary client authorization. Use a separate temporary client identity from the control gateway to avoid concurrent publication of the same device address. Start requests accept fixed network options and a candidate SHA256; fixed `candidate.bin` is copied into private runtime and verified before execution. There are no arbitrary shell or host path parameters. An authorized signer can approve candidate code with the launcher's OS permissions; this is not an OS sandbox. Owned Windows process Jobs terminate test descendants if the controller crashes.

Finish, lease expiry or owner revocation reaps the child and deletes temporary child state. Bootstrap exit removes its private executable copy and shared executable payload, preserving the original identity and grants. The Linux controller collects the signed result and small cleanup receipt before deleting its shared control directory. Legacy servers only transfer files, so first enablement still requires one Windows bootstrap; subsequent tests and retries use the controller.

`scripts/verify-live-test-agent.py` is the versioned two-device verification entry point. It waits for the signed ready response, uses fresh client identities, and checks both default physical routes and a missing-interface fallback on each device. Every stage verifies a 2 MiB full hash, move, 256 KiB range and deletion, and saves bounded diagnostics to `--output`. Supply the actual current `--network-context`; campus Wi-Fi alone does not establish independent NAT. Failed stages stop their temporary child but keep the controller available; inspect the signed snapshot and use `--resume` for an intentional retry. A complete successful run sends `finish`; collect the bootstrap cleanup receipt and remove the owned shared bundle separately. Keep reports outside `/tmp` when work must survive a reboot.

Follow-up on 2026-10-12: the same native version passed both physical defaults, Linux fallback and Windows fallback with a 2 MiB full hash, MOVE, Range and DELETE 404. The [live record](../../doc/peer-windows-egress-2026-10-12.json) includes the Windows native DNS fallback repair and cleanup receipts from both devices. Both devices used campus Wi-Fi this round; independent NAT was not established. The previous hotspot/campus evidence remains separate.

## Google Drive

Google desktop OAuth and ordinary-file read/write now use the desktop provider interface, including Linux FUSE and folder backup. See [setup, security and verification](../../doc/google-drive-dev.md). Google editor documents appear as browser shortcuts.

The Development desktop provides WebDAV/NAS, OneDrive, Google Drive and PC/NAS connection setup. LAN discovery is opt-in (`serve --discoverable --device-name NAME`); pairing still requires a trusted invitation. See [connection setup and validation](../../doc/storage-connections-dev.md).
