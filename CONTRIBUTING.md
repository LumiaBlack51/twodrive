# Contributing to TwoDrive

[Home](README.md) · [中文首页](README.zh-CN.md) · [Documentation](doc/README.md)

Focused fixes, tests, translations, and documentation improvements are welcome. TwoDrive can change remote files: use disposable data and isolate development from your real account.

## Report a problem

[Open an issue](https://github.com/LumiaBlack51/twodrive/issues) with the version, distribution, desktop/Nautilus versions, installation method, reproduction, expected behavior, and sanitized output. Distinguish local saving from cloud completion. Never attach tokens, upload-session URLs, a whole database/cache directory, or unredacted personal paths. See [diagnostics](doc/usage.md#paths-and-diagnostics).

## Incident records

For every fault investigated from 2026-09-15 onward, follow [AGENTS.md](AGENTS.md) and add or update an entry in [the incident log](doc/incidents.md). Record evidence, root cause, the fix, regression coverage and verification limits alongside the fix. Mark unresolved causes explicitly; link recurring faults to their earlier records.

## Build and check

Use a recent stable Rust toolchain supporting **Rust 2024** and the locked dependencies. No minimum Rust version is currently declared. On Ubuntu/Zorin, install native build and desktop/runtime prerequisites:

```bash
sudo apt install build-essential pkg-config fuse3 sqlite3 python3-gi \
  gir1.2-gtk-3.0 gir1.2-gtk-4.0 python3-nautilus \
  gir1.2-ayatanaappindicator3-0.1
```

From the repository root:

```bash
cargo build --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 -m unittest discover -s packaging/nautilus -p 'test_*.py'
```

Environment-dependent mounted tests are ignored by default; a normal test run does not establish that they passed. Inspect their requirements and use isolated FUSE mounts. Mock tests are not evidence of live Graph behavior.

Build the `.deb` with [scripts/build-deb.sh](scripts/build-deb.sh). For source installation, run `cargo build --workspace --release`, follow the [sign-in guide](doc/getting-started.md) using `target/release/twodrive` to configure/sign in, then inspect and run [scripts/install-desktop-integration.sh](scripts/install-desktop-integration.sh). **The installer starts the user service**; do not run it just to test. Check service and `PATH` precedence before mixing package/source installs.

## Test without a real OneDrive

Run from the repository root in a dedicated terminal. The subshell isolates environment overrides:

```bash
(
  workdir="$(mktemp -d -t twodrive-dev.XXXXXX)"
  export XDG_CONFIG_HOME="$workdir/config"
  export XDG_DATA_HOME="$workdir/data"
  export TWODRIVE_MOUNT_DIR="$workdir/mount"
  export TWODRIVE_BACKEND=mock
  printf 'Isolated test directory: %s\n' "$workdir"
  cargo run -p twodrive-cli -- init-mock
  cargo run -p twodrive-cli -- mount-mock
)
```

Use the printed temporary mount from another terminal. Close its files, unmount that exact path with `fusermount3 -u`, then stop remaining test processes. Test data is intentionally left for inspection, not automatically deleted. The mock remote is in-memory, not persistent cloud storage. Do not run the desktop installer against this environment; Nautilus assumes normal paths, so this setup primarily exercises Rust filesystem/CLI behavior.

## Workspace map

| Component | Responsibility |
| --- | --- |
| [twodrive-core](crates/twodrive-core/src/lib.rs) | Configuration, SQLite, pin policy, cache state, and recovery records. |
| [twodrive-backend](crates/twodrive-backend/src/lib.rs) | Graph, OAuth PKCE, delta, downloads, upload sessions, retries, and mock backend. |
| [twodrive-fs](crates/twodrive-fs/src/lib.rs) | FUSE, hydration, local writes, upload workers, recovery, and conflicts. |
| [twodrive-cli](crates/twodrive-cli/src/main.rs) | Commands and desktop helper entry points. |
| [twodrive-daemon](crates/twodrive-daemon/src/main.rs) | Mount process and optional upload-only known-folder watcher. |
| [Nautilus](packaging/nautilus/twodrive_nautilus.py), [tray](scripts/twodrive-tray), [Settings](scripts/twodrive-settings) | Desktop presentation and commands, not a second sync engine. |

Local identities remain stable; cloud IDs are bound separately. Mutations and pending work are recorded before cloud completion. Recovery must not upload stale generations, run child work ahead of pending parent moves, or discard unuploaded content during release. Read the [engineering notes](doc/README.md#engineering-notes) for earlier investigations.

## Propose a change

Use a focused branch and pull request. Explain behavior, data risks, and tests actually run; add regressions for reproducible bugs. Never report skipped/unrun tests as passing.

Keep English/Chinese homepages and guides aligned. Distinguish implemented behavior from plans, reuse shipped emblems, and publish only authentic sanitized screenshots with accurate captions. Check image paths, language links, and anchors.

The current package documentation allowlist does not include every web guide. The README's [online documentation link](https://github.com/LumiaBlack51/twodrive/tree/main/doc) remains available; do not assume a complete offline copy of this documentation tree is installed with the `.deb`.

[MIT license](LICENSE).
