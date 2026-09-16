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

OneDrive uses app-root `peer-control-v1/devices` and `peer-control-v1/in-<fingerprint>`. [Microsoft app-folder documentation](https://learn.microsoft.com/en-us/graph/onedrive-sharepoint-appfolder) describes the extra AppFolder scope requested only by the isolated peer login. Existing TwoDrive login defaults are unchanged. Presence expires after 180 seconds; encrypted control messages after 120 seconds. Consumed objects use the [documented drive-ID permanentDelete route](https://learn.microsoft.com/en-us/graph/api/driveitem-permanentdelete?view=graph-rest-1.0), with no recycle-bin fallback. Cloud deletion, malicious reordering, flooding and outages can deny service; they cannot confer local trust or authorize executable installation.

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

Local verification for this branch:

- 110 default Rust tests passed; formatting, Clippy with `-D warnings` and `git diff --check` passed.
- All six normally ignored FUSE tests explicitly passed with an isolated mock mount and the newly built CLI. Independent daemon smoke passed hydration, write/upload, move/delete, pin and release.
- 19 Nautilus Python tests passed.
- Peer tests cover two isolated state directories over an opaque mock cloud store, automatic discovery, no automatic trust, mutual challenge handshakes, bidirectional ping/pong, revocation, malformed messages, stale sessions, replay, low-order X25519 input and signature/routing tampering.
- Update tests cover normal installation, wrong signature/product/platform/version, file damage, post-staging corruption, launch failure, rollback and retained high-water state. The explicitly enabled native-process test starts a compiled fixture executable, then rejects a correctly signed non-executable package and preserves the previous version. Fixtures use ephemeral test signing keys, never the publisher private key.
- A real CLI process regression reproduced the relative-output health-check failure before fixing it. The old daemon test failed under a simulated 600ms startup delay; the revised barrier test passes the same delayed start while preserving the incremental-durability assertion. See TD-20260916-03/04 in the incident log.
- Original relay-lab copied into an isolated temporary directory failed both newly added security requirements (injected self-signed cloud identity and duplicate control envelope). New peer equivalent security tests pass. Original supplied source was not modified.
- Stable `twodrive-daemon.service` remains active, PID 204605, start time 2026-09-16 14:18:36 CST (before this development). No install, restart, unmount or existing account-file change was performed. The main worktree remains on its original commit with only the supplied untracked prototype.

Native CI and artifact hashes are recorded below.

## Explicit limits

No real Microsoft account login was performed in this task, and no Windows/Linux real OneDrive discovery, handshake or message round trip is claimed. CI validates native Windows code, DPAPI, protocol logic and process execution with synthetic data; it cannot establish tenant consent, AppFolder/permanentDelete availability, browser policies, proxy behavior or account-specific Graph reliability. The first real test remains the [two-machine guide](peer-quickstart.zh-CN.md). Keep both machine clocks synchronized.

No live signed GitHub Release update was published or downloaded end to end. Update validation/activation are tested with local synthetic signed packages and real native fixture processes, not with the user's installed Windows machine. A later publisher must sign a strictly newer version with the independent offline release key and publish the documented assets. No release private key is in Git, CI or artifacts. It is stored locally in the publisher's `~/.local/share/twodrive-peer-publisher/` directory and should be backed up offline.

This phase has no backup file-transfer or remote filesystem operation. Selected roots are local authorization records only; cloud messages cannot enumerate or read any directory. State and the outbox are per-user; control transport is bounded and best-effort, messages expire, and old process sessions deliberately cannot execute queued old-session messages. A verified ping/pong confirms a particular round trip; it does not promise exactly-once delivery under crashes.

The cloud can suppress/flood/reorder objects or erase presence, so availability against malicious cloud behavior is not promised. Fingerprints must be verified out of band once; a self-signed manifest alone does not establish ownership. Executable authenticity uses the pinned release key rather than Authenticode/SmartScreen reputation. Key rotation, comprehensive power-cut testing, long-running Windows service behavior and third-party security review remain future work.


## Verified delivery

- Build/source commit: [31ca30e](https://github.com/LumiaBlack51/twodrive/commit/31ca30e0433fbbf733930e14a88315058d472ec1), branch `codex/peer-control`, not merged into main.
- [Final native CI run](https://github.com/LumiaBlack51/twodrive/actions/runs/35068850290): both jobs successful. Windows: 68 default tests plus the explicitly enabled native update process test; release PE execution, identity persistence and health output passed. Linux: 110 defaults, explicit update process test, Python tests, format/Clippy and release health passed. Six FUSE tests were run locally, not on CI.
- Windows artifact `twodrive-peer-windows-x86_64`, artifact ID **10435477457**. CI ZIP digest: `5ff51411b8ad9efca30e19c0e50b9ea6992e3409865ef89f4396338655692e81`.
- Linux CI artifact `twodrive-peer-linux-x86_64`, ID **10435760184**. CI ZIP digest: `8e39b8c8a6fc507c53d8e2d3e0af9ce342f3e0fe7b96030289d5b4a408c01873`. The separately provided local Linux binary is built on this workstation for its own glibc compatibility; it is not asserted byte-identical to the Ubuntu CI build.
- Downloaded Windows exe: **7,656,448 bytes**, SHA-256 **`310f8ee544545e7bf7754f06a3c5e425d6ad3439bf6a688375f7e47eac59eaf5`**. Verified against CI SHA256SUMS, parsed PE machine 0x8664 (AMD64), and matching pinned public key. Native CI dependency inspection found only Windows system DLLs, no VCRUNTIME/MSVCP redistributable dependency.
- Local Linux exe SHA-256: `a1c181ad1240b6999fd2e76e78b1992be000e7cc0d99dac721e65c8aaa099e89`.
- Offline release manifests for these exact 0.1.0 binaries were signed after download/build with the independent publisher key. Their signatures and hashes were independently checked using Python cryptography. These manifests were not added to or substituted into the original CI artifact.
- Local enhanced Windows ZIP: `dist/twodrive-peer-0.1.0-windows-x86_64.zip`, SHA-256 `e00bab8237a1aac6840b30cde3c5b91eda890483680f43ed4c9e39f325b72e50`. It contains the unchanged CI exe, quickstart, public key, checksum, signed release manifest and build provenance; no state, token or private key.
- Original main/refactor worktrees and the running daemon were not modified. Delivery-only documentation updates after this build commit do not change the executable inputs. No GitHub Release, stable package or system installation was published.

Failure transparency: initial CI stopped at two Clippy findings, fixed before final verification. Subsequent release health output and a pre-existing scheduler-sensitive daemon test failure are recorded with reproductions in TD-20260916-03/04. Superseded in-flight runs were cancelled; only the final successful run is the delivery reference. CI emits a non-fatal action Node-runtime deprecation notice, with all required steps passing.
