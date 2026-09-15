import os
import shutil
import sqlite3
import subprocess
import threading
import time
import json
import sys

import gi
gi.require_version("Nautilus", "4.0")
from gi.repository import Gio, GLib, GObject, Nautilus

STATUS_CACHE = {}
STATUS_CACHE_TTL = 2.0
TRANSIENT_STATES = {}
TRANSIENT_TTL = 60.0
TRACKED_FILES = {}
TRACKED_TTL = 600.0
MAX_TRACKED_FILES = 500
REFRESH_INTERVAL_MS = 1500
REFRESH_TIMER_STARTED = False
POLL_IN_PROGRESS = False
POLL_QUEUED = False
PATH_CACHE = {}
PATH_CACHE_TTL = 10.0
PENDING_PATHS = {}
PATH_READ_IN_PROGRESS = False
PATH_READ_QUEUED = False
STATE_LOCK = threading.Lock()
LOG_PATH = os.path.expanduser("~/.local/state/twodrive/nautilus.log")


def twodrive_bin():
    return shutil.which("twodrive") or os.path.expanduser("~/.local/bin/twodrive")


def mount_dir():
    return os.path.expanduser("~/TwoDrive/OneDrive")


def cloud_path_from_local_path(path):
    root = os.path.abspath(mount_dir())
    resolved = os.path.abspath(path)
    # Called by the helper process only: even readlink outside our mount can
    # block on another filesystem. Stop as soon as a shortcut enters our mount.
    for _ in range(40):
        if resolved == root or resolved.startswith(root + os.sep):
            break
        parts = resolved.strip(os.sep).split(os.sep)
        prefix = os.sep
        for index, part in enumerate(parts):
            prefix = os.path.join(prefix, part)
            try:
                target = os.readlink(prefix)
            except OSError:
                continue
            resolved = os.path.abspath(os.path.join(
                os.path.dirname(prefix), target, *parts[index + 1:]
            ))
            break
        else:
            break
    try:
        if os.path.commonpath((resolved, root)) != root:
            return None
    except ValueError:
        return None
    if resolved == root:
        return "/"
    return "/" + os.path.relpath(resolved, root)


def cloud_path(file_info):
    location = file_info.get_location()
    path = location.get_path() if location else None
    if not path:
        return None
    path = os.path.abspath(path)
    root = os.path.abspath(mount_dir())
    if path == root:
        return "/"
    if path.startswith(root + os.sep):
        return "/" + os.path.relpath(path, root)
    cached = PATH_CACHE.get(path)
    if cached and time.monotonic() - cached[0] < PATH_CACHE_TTL:
        return cached[1]
    PENDING_PATHS[path] = file_info
    if len(PENDING_PATHS) > MAX_TRACKED_FILES:
        PENDING_PATHS.pop(next(iter(PENDING_PATHS)))
    queue_path_read()
    return None


def queue_path_read():
    global PATH_READ_QUEUED
    if PATH_READ_QUEUED or PATH_READ_IN_PROGRESS:
        return
    PATH_READ_QUEUED = True
    GLib.idle_add(resolve_pending_paths)


def resolve_pending_paths():
    global PATH_READ_QUEUED, PATH_READ_IN_PROGRESS
    PATH_READ_QUEUED = False
    pending = dict(PENDING_PATHS)
    PENDING_PATHS.clear()
    if not pending:
        return False
    PATH_READ_IN_PROGRESS = True

    def finished(resolved):
        global PATH_READ_IN_PROGRESS
        # Cache failures briefly too; an unavailable mount must not cause a
        # subprocess retry loop. A later Nautilus refresh can retry it.
        now = time.monotonic()
        for path in pending:
            PATH_CACHE.pop(path, None)
            PATH_CACHE[path] = (now, resolved.get(path))
        while len(PATH_CACHE) > MAX_TRACKED_FILES * 4:
            PATH_CACHE.pop(next(iter(PATH_CACHE)))
        try:
            for path, file_info in pending.items():
                if resolved.get(path):
                    refresh_file(file_info)
        finally:
            PATH_READ_IN_PROGRESS = False
            if PENDING_PATHS:
                queue_path_read()

    read_helper_async(list(pending), finished, "--resolve-paths")
    return False


def aggregate_directory_flags(directory_state, has_error, has_syncing, has_local, has_pending_release=False):
    if directory_state in {"conflict", "error"} or has_error:
        return "error"
    if has_syncing:
        return "uploading_release_pending" if has_pending_release else "uploading"
    if directory_state == "pinned":
        return "pinned"
    if has_local:
        return "cached"
    return "online_only"


def directory_state_for(conn, path, state):
    prefix = path.rstrip("/") + "/"
    has_error, has_syncing, has_local, has_pending_release = conn.execute(
        """
        SELECT
            EXISTS(
                SELECT 1 FROM files
                WHERE is_dir = 0 AND path >= ? AND path < ?
                  AND state IN ('conflict', 'error')
            ),
            EXISTS(
                SELECT 1 FROM files
                WHERE is_dir = 0 AND path >= ? AND path < ?
                  AND state IN ('hydrating', 'writing', 'dirty', 'uploading')
            ),
            EXISTS(
                SELECT 1 FROM files
                WHERE is_dir = 0 AND path >= ? AND path < ?
                  AND (
                      state IN ('cached', 'synced', 'pinned')
                      OR coalesce(cache_path, '') != ''
                  )
            ),
            EXISTS(
                SELECT 1 FROM files
                WHERE is_dir = 0 AND path >= ? AND path < ?
                  AND release_pending = 1 AND state IN ('writing', 'dirty', 'uploading')
            )
        """,
        (prefix, path.rstrip("/") + "0") * 4,
    ).fetchone()
    return aggregate_directory_flags(state, has_error, has_syncing, has_local, has_pending_release)


def file_display_state(state, release_pending):
    if release_pending and state in {"writing", "dirty", "uploading"}:
        return "uploading_release_pending"
    return state


def emblems_for_state(state):
    if state == "uploading_release_pending":
        return ["emblem-twodrive-cloud", "emblem-twodrive-syncing"]
    emblem = {
        "online_only": "emblem-twodrive-cloud",
        "hydrating": "emblem-twodrive-syncing",
        "writing": "emblem-twodrive-syncing",
        "dirty": "emblem-twodrive-syncing",
        "uploading": "emblem-twodrive-syncing",
        "cached": "emblem-twodrive-synced",
        "synced": "emblem-twodrive-synced",
        "pinned": "emblem-twodrive-pinned",
        "conflict": "emblem-twodrive-error",
        "error": "emblem-twodrive-error",
    }.get(state)
    return [emblem] if emblem else []


def run_local(*args):
    command = [twodrive_bin(), *args]
    try:
        return subprocess.run(
            command,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=3600,
            check=False,
        )
    except Exception as exc:
        return subprocess.CompletedProcess(command, 1, f"{type(exc).__name__}: {exc}")


def selected_paths(files):
    selected = []
    seen = set()
    for file_info in files:
        path = cloud_path(file_info)
        if path and path not in seen:
            selected.append((path, file_info))
            seen.add(path)
    return selected


def set_transient(paths, state):
    now = time.monotonic()
    with STATE_LOCK:
        for path in paths:
            TRANSIENT_STATES[path] = (now, state)
            STATUS_CACHE.pop(path, None)


def clear_transient(paths):
    with STATE_LOCK:
        for path in paths:
            TRANSIENT_STATES.pop(path, None)
            STATUS_CACHE.pop(path, None)


def start_refresh_timer():
    global REFRESH_TIMER_STARTED
    with STATE_LOCK:
        if REFRESH_TIMER_STARTED:
            return
        REFRESH_TIMER_STARTED = True
    GLib.timeout_add(REFRESH_INTERVAL_MS, poll_tracked_files)


def register_file_info(path, file_info, state):
    now = time.monotonic()
    with STATE_LOCK:
        TRACKED_FILES[path] = {
            "file_info": file_info,
            "state": state,
            "seen": now,
        }
        if len(TRACKED_FILES) > MAX_TRACKED_FILES:
            oldest = sorted(
                TRACKED_FILES.items(),
                key=lambda item: item[1].get("seen", 0),
            )
            for old_path, _entry in oldest[: len(TRACKED_FILES) - MAX_TRACKED_FILES]:
                TRACKED_FILES.pop(old_path, None)
    start_refresh_timer()


def status_for(path):
    now = time.monotonic()
    with STATE_LOCK:
        transient = TRANSIENT_STATES.get(path)
        if transient and now - transient[0] < TRANSIENT_TTL:
            return {"state": transient[1]}
        if transient:
            TRANSIENT_STATES.pop(path, None)

        cached = STATUS_CACHE.get(path)
        if cached and now - cached[0] < STATUS_CACHE_TTL:
            return cached[1]

    db_path = os.path.expanduser("~/.local/share/twodrive/twodrive.sqlite3")
    data = {"state": "unknown"}
    try:
        conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True, timeout=0.2)
        row = conn.execute(
            "select state, coalesce(cache_path, ''), is_dir, size, release_pending from files where path = ?",
            (path,),
        ).fetchone()
        if row:
            data = {
                "state": file_display_state(row[0], row[4]),
                "cache_path": row[1],
                "is_dir": str(bool(row[2])).lower(),
                "size": str(row[3]),
            }
            if row[2]:
                data["state"] = directory_state_for(conn, path, row[0])
        conn.close()
    except Exception:
        pass
    with STATE_LOCK:
        STATUS_CACHE[path] = (now, data)
    return data


def db_states_for(paths):
    if not paths:
        return {}

    db_path = os.path.expanduser("~/.local/share/twodrive/twodrive.sqlite3")
    states = {}
    try:
        conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True, timeout=0.05)
        for index in range(0, len(paths), 200):
            chunk = paths[index : index + 200]
            placeholders = ",".join("?" for _path in chunk)
            rows = conn.execute(
                f"select path, state, is_dir, release_pending from files where path in ({placeholders})",
                chunk,
            ).fetchall()
            for path, state, is_dir, release_pending in rows:
                states[path] = (directory_state_for(conn, path, state) if is_dir
                                else file_display_state(state, release_pending))
        conn.close()
    except Exception as exc:
        log(f"state poll failed: {exc}")
    return states


def read_states_async(paths, callback):
    read_helper_async(paths, callback, "--read-states")


def read_helper_async(paths, callback, operation):
    # nautilus-python 4.0 can retain the GIL while its native main loop is idle.
    # Use a child process and Gio callbacks, not Python worker threads, so the
    # first lookup completes even when the user does not click or refresh.
    try:
        process = Gio.Subprocess.new(
            ["/usr/bin/python3", os.path.abspath(__file__), operation],
            Gio.SubprocessFlags.STDIN_PIPE | Gio.SubprocessFlags.STDOUT_PIPE
            | Gio.SubprocessFlags.STDERR_PIPE,
        )
    except Exception as exc:
        log(f"state reader start failed: {exc}")
        GLib.idle_add(callback, {})
        return

    def timeout():
        nonlocal timer
        timer = 0
        process.force_exit()
        return False

    timer = GLib.timeout_add_seconds(5, timeout)

    def finished(source, result):
        nonlocal timer
        if timer:
            GLib.source_remove(timer)
            timer = 0
        states = {}
        try:
            _ok, output, error = source.communicate_utf8_finish(result)
            if not source.get_successful():
                raise RuntimeError(error.strip() or "state reader exited unsuccessfully")
            states = json.loads(output)
        except Exception as exc:
            log(f"state reader failed: {exc}")
        callback(states)

    process.communicate_utf8_async(json.dumps(paths), None, finished)


def queue_state_poll():
    global POLL_QUEUED
    with STATE_LOCK:
        if POLL_QUEUED:
            return
        POLL_QUEUED = True

    def run():
        global POLL_QUEUED
        with STATE_LOCK:
            POLL_QUEUED = False
        poll_tracked_files()
        return False

    # Coalesce all file callbacks in this main-loop turn into one read.
    GLib.idle_add(run)


def poll_tracked_files():
    global POLL_IN_PROGRESS
    with STATE_LOCK:
        if POLL_IN_PROGRESS:
            return True
        POLL_IN_PROGRESS = True
        paths = list(TRACKED_FILES)

    def finished(states):
        global POLL_IN_PROGRESS
        try:
            refresh_tracked_states(states)
        finally:
            POLL_IN_PROGRESS = False

    if paths:
        read_states_async(paths, finished)
    else:
        POLL_IN_PROGRESS = False
    return True


def refresh_tracked_states(db_states):
    now = time.monotonic()
    with STATE_LOCK:
        stale = [
            path
            for path, entry in TRACKED_FILES.items()
            if now - entry.get("seen", 0) > TRACKED_TTL
        ]
        for path in stale:
            TRACKED_FILES.pop(path, None)
            STATUS_CACHE.pop(path, None)
            TRANSIENT_STATES.pop(path, None)
        tracked = [
            (path, entry["file_info"], entry.get("state", "unknown"))
            for path, entry in TRACKED_FILES.items()
        ]

    refresh_targets = []
    now = time.monotonic()
    with STATE_LOCK:
        for path, file_info, _old_state in tracked:
            entry = TRACKED_FILES.get(path)
            if not entry:
                continue

            transient = TRANSIENT_STATES.get(path)
            if transient and now - transient[0] < TRANSIENT_TTL:
                state = transient[1]
            else:
                if transient:
                    TRANSIENT_STATES.pop(path, None)
                # A temporary database/read failure must not erase a known badge.
                state = db_states.get(path, entry.get("state", "unknown"))

            if state != entry.get("state"):
                entry["state"] = state
                refresh_targets.append(file_info)
                log(f"state changed path={path} state={state}")
            STATUS_CACHE[path] = (now, {"state": state})

    for file_info in refresh_targets:
        refresh_file(file_info)
    return True


def notify(title, body):
    try:
        subprocess.Popen(["notify-send", title, body[:4000]])
    except Exception:
        pass


def log(message):
    try:
        os.makedirs(os.path.dirname(LOG_PATH), exist_ok=True)
        with open(LOG_PATH, "a", encoding="utf-8") as handle:
            handle.write(f"{time.strftime('%Y-%m-%d %H:%M:%S')} {message}\n")
    except Exception:
        pass


def refresh_file(file_info):
    try:
        file_info.invalidate_extension_info()
    except Exception as exc:
        log(f"refresh failed: {exc}")
    return False


def refresh_files(file_infos):
    for file_info in file_infos:
        refresh_file(file_info)
    return False


def schedule_refreshes(file_infos):
    GLib.idle_add(refresh_files, file_infos)
    for delay_ms in (800, 2000, 5000):
        GLib.timeout_add(delay_ms, refresh_files, file_infos)


def action_title(action):
    return {
        "release": "Release space",
        "pin": "Always keep on this device",
        "unpin": "Cancel always keep on this device",
        "sync": "Sync now",
        "status": "View status",
    }.get(action, action)


def run_action(action, paths, file_infos):
    def worker():
        log(f"action={action} paths={paths}")
        if action == "status":
            outputs = []
            for path in paths:
                result = run_local("status-path", path)
                log(f"status rc={result.returncode} path={path} output={result.stdout.strip()[:500]}")
                outputs.append(result.stdout.strip() or f"{path}: no status output")
            notify("TwoDrive status", "\n\n".join(outputs[:20]))
            schedule_refreshes(file_infos)
            return

        schedule_refreshes(file_infos)

        if action == "sync":
            result = run_local("sync")
            log(f"sync rc={result.returncode} output={result.stdout.strip()[:500]}")
            clear_transient(paths)
            notify("TwoDrive sync", result.stdout.strip() or "sync finished")
            schedule_refreshes(file_infos)
            return

        outputs = []
        failures = 0
        released_zero = 0
        for path in paths:
            result = run_local(action, path)
            output = result.stdout.strip() or f"{action} finished for {path}"
            log(f"{action} rc={result.returncode} path={path} output={output[:500]}")
            if result.returncode != 0:
                failures += 1
                output = f"{path}: command failed\n{output}"
            elif action == "release" and "released 0" in output and "queued 0" in output:
                released_zero += 1
            outputs.append(output)

        clear_transient(paths)
        title = f"TwoDrive {action_title(action)}"
        summary = f"{action_title(action)} finished for {len(paths)} item(s)"
        if failures:
            summary = f"{summary}; {failures} failed"
        if action == "release" and released_zero == len(paths):
            summary += "\nNo local cache was removed. Items may already be online-only."
        body = summary
        if outputs:
            body += "\n\n" + "\n".join(outputs[:12])
            if len(outputs) > 12:
                body += f"\n... {len(outputs) - 12} more item(s)"
        notify(title, body)
        schedule_refreshes(file_infos)

    threading.Thread(target=worker, daemon=True).start()


class TwoDriveExtension(GObject.GObject, Nautilus.MenuProvider, Nautilus.InfoProvider):
    # Use the synchronous provider entry point. nautilus-python 4.0 passes
    # *handle to update_file_info_full but never assigns a unique operation
    # handle back to Nautilus. IN_PROGRESS can therefore leave a NULL handle
    # and late completions can be rejected or applied to another request.
    # Database work still runs asynchronously; only cache application is sync.

    def get_file_items(self, files):
        if not files:
            return []
        local_paths = []
        for file_info in files:
            location = file_info.get_location()
            path = location.get_path() if location else None
            if path and path not in local_paths:
                local_paths.append(path)
        items = []
        if local_paths:
            item = Nautilus.MenuItem(
                name="TwoDriveCopyPath", label="Copy path",
                tip="Copy the full local path (one per line for multiple selections)",
            )
            item.connect("activate", self.copy_paths, local_paths)
            items.append(item)
        selected = selected_paths(files)
        if not selected:
            return items
        paths = [path for path, _file_info in selected]
        file_infos = [file_info for _path, file_info in selected]
        count = len(paths)

        actions = [
            ("TwoDriveRelease", "Release space", "release"),
            ("TwoDrivePin", "Always keep on this device", "pin"),
            ("TwoDriveUnpin", "Cancel always keep on this device", "unpin"),
            ("TwoDriveSync", "Sync now", "sync"),
            ("TwoDriveStatus", "View status", "status"),
        ]
        for name, label, action in actions:
            menu_label = f"{label} ({count})" if count > 1 else label
            item = Nautilus.MenuItem(
                name=name,
                label=menu_label,
                tip=f"TwoDrive {menu_label}",
            )
            item.connect("activate", self.activate, action, paths, file_infos)
            items.append(item)
        return items

    def copy_paths(self, _item, paths):
        try:
            import gi
            gi.require_version("Gdk", "4.0")
            from gi.repository import Gdk
            display = Gdk.Display.get_default()
            if display is None:
                raise RuntimeError("No graphical display is available")
            display.get_clipboard().set("\n".join(paths))
        except Exception as exc:
            log(f"copy path failed: {exc}")
            notify("Copy path failed", str(exc))

    def activate(self, _item, action, paths, file_infos):
        run_action(action, paths, file_infos)

    def update_file_info(self, file_info):
        path = cloud_path(file_info)
        if not path:
            return Nautilus.OperationResult.COMPLETE
        with STATE_LOCK:
            transient = TRANSIENT_STATES.get(path)
            cached = STATUS_CACHE.get(path)
            state = (transient[1] if transient and time.monotonic() - transient[0] < TRANSIENT_TTL
                     else cached[1].get("state", "unknown") if cached else "unknown")
        register_file_info(path, file_info, state)
        for emblem in emblems_for_state(state):
            file_info.add_emblem(emblem)
        if not cached or time.monotonic() - cached[0] >= STATUS_CACHE_TTL:
            queue_state_poll()
        return Nautilus.OperationResult.COMPLETE


if __name__ == "__main__" and sys.argv[1:] == ["--read-states"]:
    print(json.dumps(db_states_for(json.load(sys.stdin))))
elif __name__ == "__main__" and sys.argv[1:] == ["--resolve-paths"]:
    print(json.dumps({path: cloud_path_from_local_path(path) for path in json.load(sys.stdin)}))
