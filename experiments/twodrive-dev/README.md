# TwoDrive dev: WebDAV and encrypted device sharing

[简体中文](README.zh-CN.md) · [Design](../../doc/dev-webdav-peer-design.md) · [Incident records and verification](../../doc/incidents.md)

The `dev` branch introduces the independent `twodrive-dev` executable. Stable TwoDrive source, CLI, OneDrive service, configuration, database and packages are unchanged. The experiment reuses the filesystem engine through a separate Cargo workspace, lockfile, device state and mount cache.

Implemented: existing HTTPS WebDAV connections, explicit directory sharing, single-use device invitations, QUIC/TLS encryption, reliable UDP/direct connectivity with NAT traversal and encrypted relay fallback, device revocation, an authenticated loopback WebDAV gateway, and optional Linux FUSE mounts. No VPN, virtual network interface or routing configuration is required. This is remote filesystem access, with no Syncthing-style bidirectional directory mirroring or settings GUI.

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

WebDAV polls full snapshots: maximum 100,000 resources and 16 MiB per metadata response. It has no native delta or persistent remote file ID; moves change path IDs. The built-in server exports regular files/directories, hides symlinks in listings and confines filesystem operations to a directory capability. Full PUTs default to an 8 GiB limit (`--max-upload`); completed data is synced and atomically replaced, while interrupted transfers preserve original content. Resumable PUT, PATCH and Content-Range are unsupported. Network mutations serialize conditional checks; local writers are outside this lock. External WebDAV atomicity depends on that service.

Unix state directories are 0700 and files 0600. Windows inherits directory ACLs; choose private current-user state directories. Files at rest are not encrypted. Local QUIC, public relay and real geographically separated NAT tests are distinct verification scopes. See the incident records/PR for actual results; same-host or mock tests do not establish real OneDrive or all-network NAT behavior.
