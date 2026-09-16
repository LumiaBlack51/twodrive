# Windows 0.2.10 preview

This is an unsigned engineering preview, not an accepted Windows sync release.
Full and Lite contain byte-identical Rust engines and native tray hosts.
Full additionally contains Flutter Fluent UI. Lite contains no Flutter/Dart runtime.

Run Start.ps1 with an explicit absolute disposable -StateDirectory. Add -Mock only
for synthetic tests. Omit -Mock for the empty, unconfigured production shell:
there are no sample accounts/files in that mode. Modes cannot share a state root.
Do not point at an existing Linux data directory, real sync root or account.

Full: left tray click opens the activity window; right click opens the Win32 menu.
Lite: either click opens the simple native menu. Closing the window/tray leaves
the engine running. A second tray or engine for the same state root is rejected.
To switch editions, close the first tray, start the other package against the same
state directory and same mode. The running engine is reused. Do not delete state.

The mock remote is in memory, not persistent cloud storage. Mock uploaded remote
content does not survive engine restart; retained local cache must not be mistaken
for a persistent cloud copy. Use new disposable roots for new test scenarios.

Supported: framed IPC v1, live engine state, scheduler pause/resume persisted by
the engine, explicit mock downloads/uploads, cache release with existing dirty/
pinned/open-file protections, extension-only file glyphs, recent confirmations,
disconnected state. A pause drains already-started work and starts no new work.
The management list is bounded to 200 rows; transfer history to 32 entries.

Not implemented/enabled: CFAPI registration and callbacks, browser sign-in,
account switching, production Graph worker binding, peer integration, native
sync-root file operations, pin controls, bandwidth limits, persistent operation
journal, autostart, installer/updater/signing, aggregate batch throughput/ETA.
No FUSE or WSL process is used by the Windows binaries.
Flutter and native tray UI require interactive Windows. The Full UI may require
the Microsoft Visual C++ runtime used by the installed Flutter toolchain.

CLI: twodrive-engine ipc --state ABSOLUTE_DIRECTORY reads a UTF-8 JSON request
on stdin and prints exactly one structured JSON response. Example:

    {"version":1,"id":"inspect-1","command":{"type":"snapshot"}}

There is no human-text parsing by the UI. The bridge talks to the owner-only
local named pipe. IPC has a 1 MiB frame bound, timeouts, a 16-connection limit,
version/request ID checks and a 256-entry in-memory mutation replay cache.
Request IDs are not a durable exactly-once journal across engine restarts.

Never use this preview as the sole copy of any file. No installer or service
registration is performed by these scripts.
