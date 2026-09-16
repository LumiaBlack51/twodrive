# Peer implementation and relay-lab audit (2026-09-16)

Base: refactored provider branch `5b79afe`; independent worktree `twodrive-peer`, branch `codex/peer-control`. No stable installation, service, mount, configuration, database or cache is changed.

## Audit and integration

Reviewed the supplied relay-lab identity, encryption, OAuth, Graph, relay mailbox, transfer, VPS and CLI implementations. Kept the persistent Ed25519 + separate X25519 identity concept and ephemeral X25519 / HKDF-SHA256 / XChaCha20-Poly1305 envelope design. Implemented a dedicated control protocol rather than reusing file-chunk messages. The lab source is a reference, not another shipped runtime dependency.

Changes required by the untrusted-cloud boundary:

- Self-signed cloud manifests are discovery records, not evidence of device ownership. Trust requires the full fingerprint read from the other device's local console. Unknown devices cannot send accepted commands. No silent trust-on-first-use.
- Domain-separated signatures bind the entire message, destination, source/destination sessions, message ID and expiry. Per-process random sessions and bounded replay sets reject duplicates and old-session replay. Handshake challenge responses prove access to the pinned identity and encryption key. No claimed forward secrecy after recipient long-term key compromise.
- Strict Ed25519 verification and non-contributory X25519 rejection. JSON is size bounded, versioned, rejects unknown fields; cloud pagination cannot choose another bearer-token destination. Bounded object counts, message sizes, peers and pending ping windows.
- No arbitrary shell commands, file paths, filesystem reads, upload URLs, credentials or private keys in control payloads. Only typed presence, hello/ack, ping/pong, status and release tag notifications.
- VPS transport, chunk assembly, quota/deletion algorithms and duplicate OAuth/Graph code are not imported. In particular the prototype's check/upload/recheck quota is not a distributed reservation and must not be advertised as a strict concurrent hard cap.

`twodrive-backend::control::ControlStore` is an opaque bounded transport interface. The OneDrive implementation resides with Graph provider code, reuses its HTTP retry, token refresh, OAuth/PKCE and existing public client ID. The peer owns protocol, trust, local state and lifecycle. Future transports implement ControlStore without changing the device protocol. Peer is independent of FUSE and daemon crates.

OneDrive uses app-root `peer-control-v1/devices` and `peer-control-v1/in-<fingerprint>`. [Microsoft app-folder documentation](https://learn.microsoft.com/en-us/graph/onedrive-sharepoint-appfolder) describes the extra AppFolder scope requested only by the isolated peer login. Existing TwoDrive login defaults are unchanged. Presence expires after 180 seconds; encrypted control messages after 120 seconds. Cloud deletion, malicious reordering, flooding and outages can deny service; they cannot confer local trust or authorize executable installation.

Local secrets retain compatible Unix JSON for existing TwoDrive TokenStore consumers, now atomically persisted with 0600 creation. Windows tokens/identity are DPAPI-protected for the current user. Peer state defaults to its own per-user directory; paths are explicitly constructed rather than using TwoDrive AppPaths::discover. No installer is run.

## Update trust and publishing

The separately generated release Ed25519 public key is in `crates/twodrive-peer/src/release-public-key.hex`. The private key is outside this repository in the local publisher's protected directory; it is not in CI, artifacts or source. Back it up offline before publishing future versions. A compromised OneDrive account or paired device cannot substitute the release trust root.

Offline publisher utility:

```text
peer-release sign --private /secure/release-key.dat --binary twodrive-peer.exe --version 0.1.1 --platform windows-x86_64 --output twodrive-peer-windows-x86_64.json
```

Publish exactly that binary and manifest in the fixed repository under tag `peer-v0.1.1`. Peer only accepts stable semver versions strictly above both its compiled version and installed high-water mark. Product/schema/platform/file/size/hash/signature must match. CI deliberately has no signing private key; unsigned build artifacts are initial manual distribution, not automatic-update authorization.

Installation uses immutable version directories, an atomic pending/active/previous journal, independent re-verification before execution, bounded child health checks, and rollback on startup failure. Bootstrap and previous versions remain available. A killed installer before activation leaves the old pointer; staging can be retried. A successful health check does not prove every later workload is bug-free. Local same-user compromise, disk hardware failures and administrator tampering are outside this threat model. No Authenticode certificate is supplied; SmartScreen reputation is independent of this Ed25519 release verification.

## Verification status

In progress. Native CI run, artifact digests, regression results and remaining boundaries will be recorded after verification. Real Microsoft login and Windows/Linux OneDrive end-to-end require user-operated accounts/machines and are not claimed by mock tests.
