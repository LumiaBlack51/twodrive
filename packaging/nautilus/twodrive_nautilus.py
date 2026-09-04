import os
import shutil
import sqlite3
import subprocess
import threading
import time

from gi.repository import GLib, GObject, Nautilus

STATUS_CACHE = {}
STATUS_CACHE_TTL = 2.0
TRANSIENT_STATES = {}
TRANSIENT_TTL = 60.0
TRACKED_FILES = {}
TRACKED_TTL = 600.0
MAX_TRACKED_FILES = 500
REFRESH_INTERVAL_MS = 1500
REFRESH_TIMER_STARTED = False
STATE_LOCK = threading.Lock()
LOG_PATH = os.path.expanduser("~/.local/state/twodrive/nautilus.log")


def twodrive_bin():
    return shutil.which("twodrive") or os.path.expanduser("~/.local/bin/twodrive")


def mount_dir():
    return os.path.expanduser("~/TwoDrive/OneDrive")


def cloud_path_from_local_path(path):
    root = os.path.realpath(mount_dir())
    resolved = os.path.realpath(path)
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
    return cloud_path_from_local_path(path)


def aggregate_directory_flags(directory_state, has_error, has_syncing, has_local):
    if directory_state in {"conflict", "error"} or has_error:
        return "error"
    syncing_states = {"hydrating", "writing", "dirty", "uploading"}
    if directory_state in syncing_states or has_syncing:
        return "uploading"
    if directory_state == "pinned":
        return "pinned"
    if has_local:
        return "cached"
    return "online_only"


def directory_state_for(conn, path, state):
    prefix = path.rstrip("/") + "/"
    has_error, has_syncing, has_local = conn.execute(
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
            )
        """,
        (prefix, path.rstrip("/") + "0") * 3,
    ).fetchone()
    return aggregate_directory_flags(state, has_error, has_syncing, has_local)


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
            "select state, coalesce(cache_path, ''), is_dir, size from files where path = ?",
            (path,),
        ).fetchone()
        if row:
            data = {
                "state": row[0],
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
                f"select path, state, is_dir from files where path in ({placeholders})",
                chunk,
            ).fetchall()
            for path, state, is_dir in rows:
                states[path] = directory_state_for(conn, path, state) if is_dir else state
        conn.close()
    except Exception as exc:
        log(f"state poll failed: {exc}")
    return states


def poll_tracked_files():
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

    db_states = db_states_for([path for path, _file_info, _state in tracked])
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
                state = db_states.get(path, "unknown")

            if state != entry.get("state"):
                entry["state"] = state
                STATUS_CACHE.pop(path, None)
                refresh_targets.append(file_info)
                log(f"state changed path={path} state={state}")

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

        set_transient(paths, "hydrating")
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
            elif action == "release" and "released 0" in output:
                released_zero += 1
            outputs.append(output)

        clear_transient(paths)
        title = f"TwoDrive {action_title(action)}"
        summary = f"{action_title(action)} finished for {len(paths)} item(s)"
        if failures:
            summary = f"{summary}; {failures} failed"
        if action == "release" and released_zero == len(paths):
            summary += "\nNo local cache was removed. Items may already be online-only, pinned, or busy."
        body = summary
        if outputs:
            body += "\n\n" + "\n".join(outputs[:12])
            if len(outputs) > 12:
                body += f"\n... {len(outputs) - 12} more item(s)"
        notify(title, body)
        schedule_refreshes(file_infos)

    threading.Thread(target=worker, daemon=True).start()


class TwoDriveExtension(GObject.GObject, Nautilus.MenuProvider, Nautilus.InfoProvider):
    def get_file_items(self, files):
        if not files:
            return []
        selected = selected_paths(files)
        if not selected:
            return []
        paths = [path for path, _file_info in selected]
        file_infos = [file_info for _path, file_info in selected]
        count = len(paths)

        items = []
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

    def activate(self, _item, action, paths, file_infos):
        run_action(action, paths, file_infos)

    def update_file_info(self, file_info):
        path = cloud_path(file_info)
        if not path:
            return
        state = status_for(path).get("state", "unknown")
        register_file_info(path, file_info, state)
        emblem = {
            "online_only": "emblem-twodrive-cloud",
            "hydrating": "emblem-twodrive-syncing",
            "uploading": "emblem-twodrive-syncing",
            "cached": "emblem-twodrive-synced",
            "synced": "emblem-twodrive-synced",
            "pinned": "emblem-twodrive-pinned",
            "conflict": "emblem-twodrive-error",
            "error": "emblem-twodrive-error",
        }.get(state)
        if emblem:
            file_info.add_emblem(emblem)
