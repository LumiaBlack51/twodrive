# Nautilus startup hang investigation

The reported window stopped responding immediately after opening the home folder.
No hung Nautilus process remained when diagnostics started, so its original stack
could not be captured. The earlier `Unexpected plugin response ... handle=(nil)`
messages occurred before the installed fix in commit `2da875a`. That fix and the
release commit `ed236a0` remain intact.

The current mount responded to metadata requests. Five launches with all installed
extensions and three launches with Folder Color temporarily excluded all responded
to D-Bus pings. These observations do not establish either extension as the original
cause.

Two startup risks were removed:

- TwoDrive still called `readlink` on the UI thread for paths outside its mount,
  including home-folder entries and shortcuts. These requests now run in a batched
  helper process with the existing five-second timeout. Direct mount paths require
  no filesystem lookup. Results are cached with bounds and expiry; resolved shortcuts
  invalidate extension information to load their existing status emblems. Non-cloud
  files explicitly return `OperationResult.COMPLETE`.
- The locally installed `folder-color-revival.py` scheduled a full palette scan using
  `GLib.idle_add` during construction. Idle work still executes on the UI thread.
  The startup scheduling was removed; its existing first-menu theme loader retains
  the palette functionality. This local third-party change is outside this repository.

Both installed originals were backed up in
`~/.local/state/twodrive/nautilus-recovery-20260911/` before replacement. The Folder
Color backup preserves the previously installed version, not an upstream download.

Verification:

- All 19 Python extension tests pass, including the previous NULL-handle/emblem
  regressions and new UI-thread path lookup/batching checks.
- A native Gio helper resolved a temporary shortcut into the real mount and rejected
  an unrelated directory.
- The native Folder Color menu starts with an empty palette and loads 11 colors on
  its first menu request.
- After installing both changes, home, OneDrive and desktop windows responded to
  D-Bus pings in 7–10 ms, with no new Nautilus extension errors in the journal.

If the startup hang recurs, capture the still-running process before quitting it;
the tests above establish recovery and specific risk reductions, not a confirmed
root cause for the original intermittent hang.
