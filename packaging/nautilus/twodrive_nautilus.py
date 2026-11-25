import os
import shutil
import sqlite3
import subprocess
import threading
import time
import json
import sys
import base64
import hashlib
import fcntl
from pathlib import Path
import re
import stat
import urllib.request

import gi
gi.require_version("Nautilus", "4.0")
from gi.repository import Gio, GLib, GObject, Nautilus

STATUS_CACHE = {}
DAV_ONLY = Path(__file__).name == "twodrive_dav_nautilus.py"
DAV_CONTROL = "user.twodrive.dav-controls"
DAV_CONTROL_KEYS = set()
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
PROC_ROOT = Path("/proc")


def twodrive_bin():
    return shutil.which("twodrive") or os.path.expanduser("~/.local/bin/twodrive")


def mount_dir():
    return os.path.expanduser("~/TwoDrive/OneDrive")


def dav_state_dir():
    data = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local/share")
    return data / "twodrive-dav/state"


def local_json(path, limit=1048576):
    with open(path, "rb") as handle:
        data = handle.read(limit + 1)
    if len(data) > limit:
        raise ValueError("local metadata too large")
    return json.loads(data)


def dav_profiles():
    # Helper process only. Never read configuration or probe FUSE on the UI thread.
    try:
        profiles = local_json(dav_state_dir() / "profiles.json")["profiles"]
        return [p for p in profiles if isinstance(p, dict)
                and re.fullmatch(r"[a-zA-Z0-9_-]{1,256}", p.get("id", ""))
                and isinstance(p.get("revision"), str)
                and isinstance(p.get("mount_path"), str)
                and p["mount_path"].startswith("/")
                and os.path.normpath(p["mount_path"]) == p["mount_path"]
                and p["mount_path"] != "/"]
    except (OSError, ValueError, TypeError, KeyError):
        return []


def dav_key(profile, path):
    return "dav:" + json.dumps([profile["id"], profile["revision"],
                               profile["mount_path"], path], separators=(",", ":"))


def dav_path(path, profiles):
    for profile in sorted(profiles, key=lambda p: len(p["mount_path"]), reverse=True):
        root = profile["mount_path"]
        if path == root or path.startswith(root + "/"):
            return dav_key(profile, path[len(root):] or "/")
    return None


def cloud_path_from_local_path(path, profiles=None):
    root = None if DAV_ONLY else os.path.abspath(mount_dir())
    resolved = os.path.abspath(path)
    profiles = dav_profiles() if profiles is None else profiles
    # Called by the helper process only: even readlink outside our mount can
    # block on another filesystem. Stop as soon as a shortcut enters our mount.
    for _ in range(40):
        key = dav_path(resolved, profiles)
        if key:
            return key
        if root and (resolved == root or resolved.startswith(root + os.sep)):
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
    if root is None:
        return None
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
    root = None if DAV_ONLY else os.path.abspath(mount_dir())
    if root and path == root:
        return "/"
    if root and path.startswith(root + os.sep):
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


def aggregate_directory_flags(directory_state, has_error, has_syncing, has_local, has_pending_release=False, has_pending_pin=False):
    if directory_state in {"conflict", "error"} or has_error:
        return "error"
    if has_syncing:
        return "uploading_release_pending" if has_pending_release else "uploading"
    if has_pending_pin:
        return "pin_pending"
    if directory_state == "pinned":
        return "pinned"
    if has_local:
        return "cached"
    return "online_only"


def directory_state_for(conn, path, state):
    prefix = path.rstrip("/") + "/"
    has_error, has_syncing, has_local, has_pending_release, has_pending_pin = conn.execute(
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
                  AND (state IN ('writing', 'dirty', 'uploading')
                       OR (state = 'hydrating' AND pin_explicit = 0 AND pin_origin_remote_id IS NULL))
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
            ),
            EXISTS(
                SELECT 1 FROM files
                WHERE is_dir = 0 AND path >= ? AND path < ?
                  AND (pin_explicit = 1 OR pin_origin_remote_id IS NOT NULL OR state = 'pinned')
                  AND coalesce(cache_path, '') = ''
                  AND state IN ('online_only', 'hydrating', 'pinned')
            )
        """,
        (prefix, path.rstrip("/") + "0") * 5,
    ).fetchone()
    return aggregate_directory_flags(state, has_error, has_syncing, has_local, has_pending_release, has_pending_pin)


def file_display_state(state, release_pending, effective_pinned=False, cache_path=None):
    if release_pending and state in {"writing", "dirty", "uploading"}:
        return "uploading_release_pending"
    # begin_hydration can already set state=pinned before it publishes cache.
    # Policy alone is not proof that the content is available offline.
    if (effective_pinned or state == "pinned") and not cache_path and state in {"online_only", "hydrating", "pinned"}:
        return "pin_pending"
    return state


def emblems_for_state(state):
    prefix = "emblem-twodrive-dav-" if DAV_ONLY else "emblem-twodrive-"
    if state == "pin_pending":
        return [prefix + "cloud", prefix + "pinned"]
    if state == "uploading_release_pending":
        return [prefix + "cloud", prefix + "syncing"]
    emblem = {
        "online_only": "emblem-twodrive-cloud",
        "hydrating": "emblem-twodrive-downloading",
        "writing": "emblem-twodrive-syncing",
        "dirty": "emblem-twodrive-syncing",
        "uploading": "emblem-twodrive-syncing",
        "cached": "emblem-twodrive-synced",
        "synced": "emblem-twodrive-synced",
        "pinned": "emblem-twodrive-pinned",
        "conflict": "emblem-twodrive-error",
        "error": "emblem-twodrive-error",
    }.get(state)
    return [prefix + emblem.removeprefix("emblem-twodrive-")] if emblem else []


def run_local_async(args, callback):
    command = [twodrive_bin(), *args]
    try:
        process = Gio.Subprocess.new(
            command,
            Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_MERGE,
        )
    except Exception as exc:
        GLib.idle_add(callback, subprocess.CompletedProcess(command, 1, f"{type(exc).__name__}: {exc}"))
        return

    timed_out = False

    def timeout():
        nonlocal timer, timed_out
        timer = 0
        timed_out = True
        process.force_exit()
        return False

    timer = GLib.timeout_add_seconds(3600, timeout)

    def finished(source, result):
        nonlocal timer
        if timer:
            GLib.source_remove(timer)
            timer = 0
        try:
            _ok, output, _error = source.communicate_utf8_finish(result)
            if timed_out:
                output += "\nCommand timed out after 3600 seconds"
            rc = source.get_exit_status() if source.get_if_exited() and not timed_out else 1
            completed = subprocess.CompletedProcess(command, rc, output)
        except Exception as exc:
            completed = subprocess.CompletedProcess(command, 1, f"{type(exc).__name__}: {exc}")
        callback(completed)

    process.communicate_utf8_async(None, None, finished)


def selected_paths(files):
    selected = []
    seen = set()
    for file_info in files:
        path = cloud_path(file_info)
        # DAV profiles have their own engine. Do not invoke the legacy CLI on them.
        if path and not path.startswith("dav:") and path not in seen:
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
            "select state, coalesce(cache_path, ''), is_dir, size, release_pending, pin_explicit, pin_origin_remote_id from files where path = ?",
            (path,),
        ).fetchone()
        if row:
            data = {
                "state": file_display_state(row[0], row[4], bool(row[5]) or row[6] is not None, row[1]),
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


def db_states_for(paths, controls=None):
    if not paths:
        return {}

    states = dav_states_for([p for p in paths if p.startswith("dav:")], controls=controls)
    paths = [p for p in paths if not p.startswith("dav:")]
    if not paths:
        return states
    db_path = os.path.expanduser("~/.local/share/twodrive/twodrive.sqlite3")
    try:
        conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True, timeout=0.05)
        for index in range(0, len(paths), 200):
            chunk = paths[index : index + 200]
            placeholders = ",".join("?" for _path in chunk)
            rows = conn.execute(
                f"select path, state, is_dir, release_pending, pin_explicit, pin_origin_remote_id, cache_path from files where path in ({placeholders})",
                chunk,
            ).fetchall()
            for path, state, is_dir, release_pending, pin_explicit, pin_origin, cache_path in rows:
                states[path] = (directory_state_for(conn, path, state) if is_dir
                                else file_display_state(state, release_pending, bool(pin_explicit) or pin_origin is not None, cache_path))
        conn.close()
    except Exception as exc:
        log(f"state poll failed: {exc}")
    return states


def mounted_filesystems():
    def unescape(value):
        return re.sub(r"\\([0-7]{3})", lambda m: chr(int(m[1], 8)), value)
    result = {}
    for line in Path("/proc/self/mountinfo").read_text().splitlines():
        left, right = line.split(" - ", 1)
        fields = right.split()
        result[unescape(left.split()[4])] = (fields[0], unescape(fields[1]))
    return result


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        return None


def rc_read(env, operation):
    # Only authenticated read-only local IPC. Never use proxies, redirects,
    # cloud operations or credentials in a URL/log message.
    if operation not in {"core/pid", "core/stats", "mount/listmounts"}:
        raise ValueError("unsupported local status operation")
    addr = env.get("RCLONE_RC_ADDR", "")
    if not re.fullmatch(r"127\.0\.0\.1:[0-9]{1,5}", addr):
        raise ValueError("invalid local status address")
    user, password = env.get("RCLONE_RC_USER"), env.get("RCLONE_RC_PASS")
    if not user or not password:
        raise ValueError("missing local status authentication")
    auth = base64.b64encode(f"{user}:{password}".encode()).decode()
    request = urllib.request.Request("http://" + addr + "/" + operation, data=b"{}",
                                     headers={"Authorization": "Basic " + auth,
                                              "Content-Type": "application/json"})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    with opener.open(request, timeout=0.35) as response:
        data = response.read(65537)
    if len(data) > 65536:
        raise ValueError("local status too large")
    return json.loads(data)


def running_dav_engine_directory(state):
    # An update can move the runtime directory before the old engine exits.
    # The process holding this state's exclusive lock identifies that runtime.
    try:
        info = (state / "run.lock").lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid():
            return None
        lock_id = (os.major(info.st_dev), os.minor(info.st_dev), info.st_ino)
        for line in (PROC_ROOT / "locks").read_text().splitlines():
            fields = line.split()
            if len(fields) < 6 or fields[1:4] != ["FLOCK", "ADVISORY", "WRITE"]:
                continue
            major, minor, inode = fields[5].split(":")
            if (int(major, 16), int(minor, 16), int(inode)) != lock_id:
                continue
            if not fields[4].isdigit():
                return None
            owner = PROC_ROOT / fields[4]
            if owner.stat().st_uid != os.getuid():
                return None
            executable = Path(os.readlink(owner / "exe"))
            if not executable.is_absolute() or executable.name != "twodrive-dav-engine":
                return None
            args = (owner / "cmdline").read_bytes().split(b"\0")
            if any(flag == b"--state" and value == os.fsencode(state)
                   for flag, value in zip(args, args[1:])):
                return executable.parent
    except (OSError, ValueError):
        pass
    return None


def bridge_candidates(cache_root):
    state = dav_state_dir()
    directories = (state.parent / "runtime", running_dav_engine_directory(state))
    executables = {str(directory / name) for directory in directories if directory is not None
                   for name in ("rclone", "rclone-mount")}
    executables.update({"/usr/lib/twodrive-dav/rclone", "/usr/lib/twodrive-dav/rclone-mount"})
    for process in PROC_ROOT.iterdir():
        if not process.name.isdigit():
            continue
        try:
            if process.stat().st_uid != os.getuid() or os.readlink(process / "exe") not in executables:
                continue
            env = dict(item.decode().split("=", 1) for item in
                       (process / "environ").read_bytes().split(b"\0") if b"=" in item)
            if env.get("RCLONE_CACHE_DIR") != str(cache_root):
                continue
            yield int(process.name), env
        except (OSError, ValueError, TypeError):
            continue


def bridge_status(cache_root, mount_path):
    for pid, env in bridge_candidates(cache_root):
        try:
            if rc_read(env, "core/pid").get("pid") != pid:
                continue
            live_mounts = rc_read(env, "mount/listmounts").get("mountPoints", [])
            active = [m.get("Fs") for m in live_mounts if m.get("MountPoint") == mount_path]
            if len(active) != 1 or not isinstance(active[0], str):
                continue
            # The pinned component names VFS cache directories after this
            # active remote. vfs/stats would walk and lock the directory tree.
            if not re.fullmatch(r"TWODRIVE(?:\{[A-Za-z0-9_+\-=]+\})?:", active[0]):
                continue
            namespace = active[0][:-1]
            data, meta = cache_root / "vfs" / namespace, cache_root / "vfsMeta" / namespace
            # Transfers identify active downloads; Dirty metadata takes priority
            # for pending uploads. Partial, idle caches remain cloud-only.
            try:
                downloads = {t["name"] for t in rc_read(env, "core/stats").get("transferring", [])
                             if isinstance(t.get("name"), str)}
            except (OSError, ValueError, TypeError, KeyError):
                downloads = set()
            return data, meta, downloads
        except (OSError, ValueError, TypeError, KeyError):
            continue
    return None


def regular_local_file(path, root):
    # All paths are private local cache paths, never mounted cloud paths.
    relative = path.relative_to(root)
    current = root
    for part in relative.parts:
        current = current / part
        if stat.S_ISLNK(current.lstat().st_mode):
            raise ValueError("cache symlink")
    info = path.stat()
    if not stat.S_ISREG(info.st_mode):
        raise ValueError("not a cache file")
    return info


def bridge_file_state(path, cache):
    data_root, meta_root, downloads = cache
    relative = path.lstrip("/")
    try:
        metadata_path = meta_root / relative
        regular_local_file(metadata_path, meta_root.parent.parent)
        metadata = local_json(metadata_path, 65536)
        if metadata.get("Dirty") is True:
            return "uploading"
        if relative in downloads:
            return "hydrating"
        if metadata.get("Dirty") is not False:
            return "unknown"
        size = metadata["Size"]
        if type(size) is not int or size < 0:
            return "unknown"
        info = regular_local_file(data_root / relative, data_root.parent.parent)
        if info.st_size != size or not metadata.get("Fingerprint"):
            return "online_only"
        end = 0
        ranges = metadata.get("Rs") or []
        if not isinstance(ranges, list):
            return "unknown"
        for item in sorted(ranges, key=lambda r: r["Pos"]):
            start, length = item["Pos"], item["Size"]
            if type(start) is not int or type(length) is not int or start < 0 or length < 0:
                return "unknown"
            if start > end:
                break
            end = max(end, start + length)
        return "cached" if end >= size else "online_only"
    except (OSError, ValueError, TypeError, KeyError):
        return "hydrating" if relative in downloads else "online_only"


def native_states(mount, paths, *, offline=False):
    states = {}
    with sqlite3.connect((mount / "mount.sqlite3").as_uri() + "?mode=ro", uri=True, timeout=0.05) as conn:
        for key, path in paths:
            row = conn.execute("""SELECT state,is_dir,release_pending,pin_explicit,
                                  pin_origin_remote_id,cache_path,size FROM files WHERE path=?""",
                               (path,)).fetchone()
            if not row:
                continue
            state, is_dir, release, pin, origin, cache, size = row
            if offline and (is_dir or state not in {"cached", "pinned"}):
                states[key] = "online_only"
                continue
            if is_dir:
                state = directory_state_for(conn, path, state)
                # A cached child does not mean the entire folder is offline.
                states[key] = state if state in {"error", "uploading", "uploading_release_pending", "pin_pending"} else "online_only"
                continue
            if state in {"cached", "synced", "pinned"}:
                try:
                    root = mount / "cache"
                    if (not cache or (offline and Path(cache).parent != root)
                            or regular_local_file(Path(cache), root).st_size != size):
                        cache = None
                except (OSError, ValueError):
                    cache = None
                if not cache:
                    state = "online_only"
            if offline:
                states[key] = state
            else:
                states[key] = file_display_state(state, release, bool(pin) or origin is not None, cache)
    return states


def dav_states_for(keys, *, controls=None):
    if not keys:
        return {}
    # Explicit unknown removes obsolete badges on logout, rebind or unmount.
    states = dict.fromkeys(keys, "unknown")
    profiles = {p["id"]: p for p in dav_profiles()}
    grouped = {}
    for key in keys:
        try:
            ident, revision, root, path = json.loads(key[4:])
            p = profiles[ident]
            if (revision != p["revision"] or root != p["mount_path"] or not isinstance(path, str)
                    or not path.startswith("/") or path.startswith("//") or "\0" in path
                    or os.path.normpath(path) != path):
                continue
            grouped.setdefault(ident, []).append((key, path))
        except (ValueError, TypeError, KeyError):
            continue
    mounts = mounted_filesystems()
    for ident, paths in grouped.items():
        p = profiles[ident]
        try:
            session = local_json(dav_state_dir() / f"session-{ident}.json")
            if session["revision"] != p["revision"] or session["kind"] != p["kind"]:
                continue
            mount = dav_state_dir() / f"mount-{ident}"
            kind, source = mounts.get(p["mount_path"], (None, None))
            if kind == "fuse" and source in {"twodrive", "twodrive-dev", "twodrive-offline"}:
                if local_json(mount / "binding.json")["provider"] == session["binding"]:
                    states.update(native_states(mount, paths, offline=source == "twodrive-offline"))
                    if controls is not None and source == "twodrive":
                        try:
                            supported = os.getxattr(p["mount_path"], DAV_CONTROL, follow_symlinks=False) == b"1"
                        except OSError:
                            supported = False
                        if supported:
                            controls.update(key for key, path in paths if path != "/" and states[key] != "unknown")
            elif kind == "fuse" and source.startswith("twodrive-native-"):
                owner = local_json(mount / "linux-readonly" / "native-owner.json")
                if owner == {"source": source, "binding": session["binding"], "path": p["mount_path"]}:
                    # The worker's verified content cache lasts only for this mount.
                    states.update({key: "online_only" for key, _path in paths})
            elif kind == "fuse.rclone" and source.startswith("twodrive-ro-"):
                digest = hashlib.sha256(session["binding"].encode()).hexdigest()
                cache = bridge_status(mount / "linux-accounts" / digest / "vfs", p["mount_path"])
                if cache:
                    states.update({key: bridge_file_state(path, cache) for key, path in paths})
        except (OSError, ValueError, TypeError, KeyError, sqlite3.Error):
            # Missing or mismatched local state never means safely downloaded.
            pass
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

    timer = GLib.timeout_add_seconds(3600 if operation == "--dav-action" else 5, timeout)

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
            if operation == "--read-states":
                DAV_CONTROL_KEYS.intersection_update(TRACKED_FILES)
                DAV_CONTROL_KEYS.difference_update(paths)
                DAV_CONTROL_KEYS.update(states["controls"])
                states = states["states"]
        except Exception as exc:
            states = {}
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
            # Once persisted work changes state, it takes over from the immediate
            # menu feedback, even if a multi-item command is still running.
            if transient and transient[1] == "pin_pending" and db_states.get(path) not in {None, "online_only", "unknown"}:
                TRANSIENT_STATES.pop(path, None)
                transient = None
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
                log(f"state changed path={'dav-item' if path.startswith('dav:') else path} state={state}")
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
    log(f"action={action} paths={paths}")
    if action == "pin":
        with STATE_LOCK:
            pending = [path for path in paths if STATUS_CACHE.get(path, (0, {}))[1].get("state", "unknown")
                       in {"unknown", "online_only", "hydrating", "pin_pending"}]
        set_transient(pending, "pin_pending")
        # Apply the accepted request before starting any CLI/network work.
        refresh_files(file_infos)
    elif action in {"release", "unpin"}:
        clear_transient(paths)

    schedule_refreshes(file_infos)

    if action == "sync":
        def sync_finished(result):
            log(f"sync rc={result.returncode} output={result.stdout.strip()[:500]}")
            clear_transient(paths)
            notify("TwoDrive sync", result.stdout.strip() or "sync finished")
            schedule_refreshes(file_infos)

        run_local_async(["sync"], sync_finished)
        return

    outputs = []
    failures = 0
    released_zero = 0

    def finished():
        if action == "status":
            notify("TwoDrive status", "\n\n".join(outputs[:20]))
            schedule_refreshes(file_infos)
            return

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

    def run_next(index):
        if index == len(paths):
            finished()
            return
        path = paths[index]

        def command_finished(result):
            nonlocal failures, released_zero
            output = result.stdout.strip() or (f"{path}: no status output" if action == "status"
                                               else f"{action} finished for {path}")
            log(f"{action} rc={result.returncode} path={path} output={output[:500]}")
            if action != "status":
                if result.returncode != 0:
                    failures += 1
                    output = f"{path}: command failed\n{output}"
                elif action == "release" and "released 0" in output and "queued 0" in output:
                    released_zero += 1
                clear_transient([path])
                schedule_refreshes(file_infos[index:index + 1])
            outputs.append(output)
            run_next(index + 1)

        run_local_async(["status-path" if action == "status" else action, path], command_finished)

    run_next(0)


def apply_dav_action(key, root, path, action):
    # O_PATH takes a reference to the selected inode without opening its contents.
    # Recheck the account binding after obtaining the descriptors. A remount or
    # nested filesystem must never redirect a cache action to another account.
    root_fd = os.open(root, os.O_PATH | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        target_fd = os.open(path.lstrip("/") or ".", os.O_PATH | os.O_NOFOLLOW, dir_fd=root_fd)
        try:
            def mount_id(fd):
                info = Path(f"/proc/self/fdinfo/{fd}").read_text()
                return next(line for line in info.splitlines() if line.startswith("mnt_id:"))
            controls = set()
            dav_states_for([key], controls=controls)
            current_fd = os.open(root, os.O_PATH | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:
                if key not in controls or not mount_id(root_fd) == mount_id(target_fd) == mount_id(current_fd):
                    raise OSError("mount changed")
            finally:
                os.close(current_fd)
            target = f"/proc/self/fd/{target_fd}"
            if stat.S_ISLNK(os.fstat(target_fd).st_mode) or os.getxattr(target, DAV_CONTROL) != b"1":
                raise OSError("unsupported cache action")
            value = action if action == "sync" else f"{action}:{os.fstat(target_fd).st_ino}"
            os.setxattr(f"/proc/self/fd/{root_fd}", DAV_CONTROL, value.encode("ascii"))
        finally:
            os.close(target_fd)
    finally:
        os.close(root_fd)


def dav_action(request):
    action, keys = request["action"], request["keys"]
    if action not in {"status", "release", "pin", "unpin", "sync"} or not 1 <= len(keys) <= 500:
        raise ValueError("Invalid DAV cache action")
    controls = set()
    states = dav_states_for(keys, controls=controls)
    messages = []
    synced = set()
    for key in keys:
        _id, _revision, root, path = json.loads(key[4:])
        name = os.path.basename(path) or "/"
        if action == "status":
            messages.append(f"{name}: {states.get(key, 'unknown').replace('_', ' ')}")
            continue
        if key not in controls:
            messages.append(f"{name}: mount no longer supports this action")
            continue
        if action == "sync" and root in synced:
            continue
        try:
            apply_dav_action(key, root, "/" if action == "sync" else path, action)
            if action == "sync":
                synced.add(root)
            messages.append(f"{name}: request accepted")
        except OSError as error:
            messages.append(f"{name}: {error.strerror or 'mount operation failed'}")
    return {"messages": messages}


class TwoDriveExtension(GObject.GObject, Nautilus.MenuProvider, Nautilus.InfoProvider):
    if DAV_ONLY:
        __gtype_name__ = "TwoDriveDavExtension"
    # Use the synchronous provider entry point. nautilus-python 4.0 passes
    # *handle to update_file_info_full but never assigns a unique operation
    # handle back to Nautilus. IN_PROGRESS can therefore leave a NULL handle
    # and late completions can be rejected or applied to another request.
    # Database work still runs asynchronously; only cache application is sync.

    def get_file_items(self, files):
        if not files:
            return []
        dav_files = [(key, info) for info in files if (key := cloud_path(info)) and key.startswith("dav:")]
        if DAV_ONLY and dav_files and len(dav_files) == len(files):
            return self.dav_items(dav_files)
        if DAV_ONLY:
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

    def dav_items(self, selected):
        keys, files = zip(*selected)
        if any(STATUS_CACHE.get(key, (0, {}))[1].get("state", "unknown") == "unknown" for key in keys):
            return []
        actions = ["status"]
        if all(key in DAV_CONTROL_KEYS for key in keys):
            actions = ["release", "pin", "unpin", "sync", "status"]
        items = []
        for action in actions:
            title = action_title(action)
            item = Nautilus.MenuItem(name="TwoDriveDav" + action.title(), label=title,
                                     tip="TwoDrive " + title)
            item.connect("activate", self.activate_dav, action, keys, files)
            items.append(item)
        return items

    def activate_dav(self, _item, action, keys, files):
        def finished(result):
            messages = result.get("messages") or ["The mount did not confirm the request."]
            notify("TwoDrive " + action_title(action), "\n".join(messages[:20]))
            schedule_refreshes(files)
        read_helper_async({"action": action, "keys": keys}, finished, "--dav-action")

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


if DAV_ONLY:
    # nautilus-python registers its wrapper type from the Python class name.
    TwoDriveExtension.__name__ = "TwoDriveDavExtension"


if __name__ == "__main__" and sys.argv[1:] == ["--read-states"]:
    controls = set()
    states = db_states_for(json.load(sys.stdin), controls=controls)
    print(json.dumps({"states": states, "controls": sorted(controls)}))
elif __name__ == "__main__" and sys.argv[1:] == ["--resolve-paths"]:
    profiles = dav_profiles()
    print(json.dumps({path: cloud_path_from_local_path(path, profiles) for path in json.load(sys.stdin)}))
elif __name__ == "__main__" and sys.argv[1:] == ["--dav-action"]:
    print(json.dumps(dav_action(json.load(sys.stdin))))
