# Unexpected background downloads, 2026-09-08

The live activity file showed downloads of unpinned IELTS PDFs and images while the
user was doing unrelated work. By 19:23:31 the activity had stopped. Existing logs
had no caller attribution, so the original requesting process remains unknown.
The inspected database contained no explicit or inherited pins. Its 53 recently
accessed cached files totalled 32,403,825 bytes; access timestamps alone are not a
complete transfer history.

The deferred read-handle path checked background callers at open but did not check
the reader before hydration. It now checks the reader as well, covering descriptors
passed to background extractors. Process detection also includes the executable
path, rather than relying solely on worker names and command lines. This closes a
verified gap but does not establish it as the cause of the observed incident.

Uncached read requests now log the file path, requesting PID, executable, thread
name, offset and size. Command-line arguments are not logged. Logs can be inspected
with `journalctl --user -u twodrive-daemon.service` and filtered for `on-demand read`.
These describe requests that can initiate hydration, not necessarily completed
network transfers. Kernel-generated reads may lack a usable requesting PID.

Validation: the isolated FUSE regression opens a descriptor as a normal process,
changes its identity to a Tracker extractor, verifies the read is rejected, then
restores its identity and verifies normal content reading with one backend download.
The GIO regression verifies directory metadata queries cause zero downloads and an
explicit copy succeeds. Both passed; formatting and workspace clippy passed.
The workspace test run initially hit the existing atomic-replacement concurrency
test failure; that test passed when rerun in isolation, and a subsequent full workspace run passed
(72 tests, 4 explicit integration tests skipped by default).

Installed the rebuilt daemon locally with a backup of the previous executable,
then restarted after confirming no active transfers or unsynced file states.
The mount recovered and no further unsolicited read requests were observed during
the immediate post-restart check. Do not claim the original trigger is resolved
without caller evidence if the downloads recur.
