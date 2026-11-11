# Windows Shell validation and registration ownership

This work began at dev `4fad4d1`. Its independent implementation and native
evidence are retained at `00ff180`. During delivery, upstream merged PR #41 into
dev `0fc9e12`, including a more complete generated Explorer host, retention gate,
intent retries and custom properties. The final PR preserves that upstream
implementation. It does not add a parallel Shell backend or new feature names.

The remaining source change stores the expected generated context and checks it
for both the ID and folder lookups in `ShellRoot::verify`, which gates teardown.
Matching an ID and path alone no longer establishes ownership of a registration
with a different context. The WinRT apartment owner is also non-Send/non-Sync so
its destructor cannot uninitialize a different thread's apartment.

## Integrated validation

The ignored `changed_context_is_not_owned_even_when_id_and_folder_match` regression
creates a new guarded generated root and a non-owning view with the same ID and
folder but a different expected context. On the old verification logic, it failed
with `changed context must veto ownership` (exit 101). After the fix it passed on
a different new root. The original context still verifies. Both runs cleaned the
empty generated root before asserting the result; the test does not modify an
existing OS context or enumerate other registrations.

```powershell
cargo fmt --manifest-path experiments/windows-cfapi/Cargo.toml -- --check
cargo clippy --manifest-path experiments/windows-cfapi/Cargo.toml --all-features --all-targets --locked -- -D warnings
cargo test --manifest-path experiments/windows-cfapi/Cargo.toml --all-features --locked
# Explicit new local generated leaf, with an existing unmanaged parent:
$env:TWODRIVE_APPROVED_SHELL_FIXTURE_ROOT = 'C:\YOUR_TEST_PARENT\twodrive-cfapi-fixture-NEW'
cargo test --manifest-path experiments/windows-cfapi/Cargo.toml --features native-local-fixture --locked changed_context_is_not_owned_even_when_id_and_folder_match -- --ignored --nocapture
```

On Windows 11 10.0.26340.9750 x64 / Rust 1.99.1, strict Clippy and 76 nonignored
Rust test executions passed; five ignored executions were not counted as passes.
Flutter 3.41.8 / Dart 3.11.5 analyzer and all 43 current widget/model tests passed.
The explicit context regression passed separately. See the
[sanitized validation record](windows-shell-validation-2026-10-22.json) for the
recompiled actual-engine checks and read-only Explorer observations.

The integrated engine passed real OS reads of all three generated files,
including independent PNG CRC/zlib/pixel checks, dehydration/cache preservation,
reread, dirty-file veto/byte retention, and clean unregister retaining the files
and directory. Explorer browsing left FETCH_DATA at 3. Its new-window capture
did not expose usable status glyphs or accessibility text, so integrated visual
acceptance remains **unverified**, despite the earlier independent visual pass.
The cause of this capture result is not established. An initial harness attempt
used the old flat names/IDs against the new nested visual_v1 dataset and failed;
its cleanup succeeded, and the corrected run used another fresh generated root.

## Initial independent evidence and limits

Before integrating PR #41, the independent implementation at `00ff180` was
tested against a bare CFAPI control. Shell status values were 8 (online), 14
(resident) and 15 (pin intent); metadata caused no FETCH_DATA, pinned release was
refused, clean release restored online status, and cleanup retained complete
generated files. Actual Explorer displayed a blue cloud ("联机时可用") and two
outlined green checks ("在此设备上可用"). Browsing left FETCH_DATA at 3. These
results belong to that exact initial implementation, not automatically to a
later source revision. The initial probe package is likewise frozen at that
implementation and is not the final integrated build.

Failed initial attempts are retained: WinRT rejected verbatim paths with
0x800700A1 and Shell parsing with 0x80070057; equivalent DOS paths preserved UTF-16.
An intermediate test incorrectly expected unhydrated placeholders to survive
unregister. The final integrated code uses upstream's stronger retention and
ordinary-file conversion gates rather than that initial fixture-only workaround.

Pinned icon appearance, syncing/error/conflict icons, full manual GUI acceptance,
thumbnails/indexer, native root restart, Linux GUI and live-provider native
synchronization remain unverified or disabled. No mouse or keyboard input was
sent, and no live cloud data was changed. Full screenshots remain local because
Explorer's sidebar contains personal shortcuts.

References: [CloudMirror](https://github.com/microsoft/Windows-classic-samples/blob/main/Samples/CloudMirror/CloudMirror/CloudProviderRegistrar.cpp),
[Shell placeholder flags](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/ne-shobjidl_core-placeholder_states),
[cloud sync architecture](https://learn.microsoft.com/en-us/windows/win32/cfapi/build-a-cloud-file-sync-engine).
