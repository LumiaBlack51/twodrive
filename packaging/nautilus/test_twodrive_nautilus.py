import importlib.util
import os
from pathlib import Path
import sqlite3
import sys
import tempfile
import types
import unittest
from unittest.mock import patch


def load_extension():
    gi = types.ModuleType("gi")
    repository = types.ModuleType("gi.repository")

    class MenuProvider:
        pass

    class InfoProvider:
        pass

    class GObjectBase:
        pass

    class DummyGObject:
        GObject = GObjectBase

    class DummyNautilus:
        pass

    class MenuItem:
        def __init__(self, **kwargs):
            self.label = kwargs["label"]
            self.callback = None

        def connect(self, _signal, callback, *args):
            self.callback = lambda: callback(self, *args)

    DummyNautilus.MenuItem = MenuItem
    DummyNautilus.MenuProvider = MenuProvider
    DummyNautilus.InfoProvider = InfoProvider
    DummyNautilus.OperationResult = types.SimpleNamespace(COMPLETE=0, IN_PROGRESS=1)
    DummyNautilus.info_provider_update_complete_invoke = lambda *_args: None

    class DummyGLib:
        @staticmethod
        def timeout_add(*_args):
            return 1

        @staticmethod
        def idle_add(*_args):
            return 1

    repository.GLib = DummyGLib
    repository.GObject = DummyGObject
    repository.Nautilus = DummyNautilus
    gi.require_version = lambda *_args: None
    repository.Gio = types.SimpleNamespace()
    gi.repository = repository
    sys.modules["gi"] = gi
    sys.modules["gi.repository"] = repository

    path = Path(__file__).with_name("twodrive_nautilus.py")
    spec = importlib.util.spec_from_file_location("twodrive_nautilus", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


EXTENSION = load_extension()


class CloudPathTests(unittest.TestCase):
    def test_direct_mount_path_never_probes_fuse(self):
        with patch.object(EXTENSION, "mount_dir", return_value="/mount/OneDrive"), \
             patch.object(os, "readlink", side_effect=AssertionError("FUSE probe")), \
             patch.object(os.path, "realpath", side_effect=AssertionError("FUSE probe")):
            self.assertEqual(EXTENSION.cloud_path_from_local_path("/mount/OneDrive/Pictures/large.jpg"),
                             "/Pictures/large.jpg")

    def test_resolves_shortcut_into_mount(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "OneDrive"
            child = root / "must" / "CV"
            child.mkdir(parents=True)
            shortcut = Path(directory) / "must-shortcut"
            shortcut.symlink_to(root / "must", target_is_directory=True)
            original_mount_dir = EXTENSION.mount_dir
            EXTENSION.mount_dir = lambda: os.fspath(root)
            try:
                self.assertEqual(
                    EXTENSION.cloud_path_from_local_path(shortcut / "CV"),
                    "/must/CV",
                )
            finally:
                EXTENSION.mount_dir = original_mount_dir

    def test_rejects_path_outside_mount(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "OneDrive"
            root.mkdir()
            original_mount_dir = EXTENSION.mount_dir
            EXTENSION.mount_dir = lambda: os.fspath(root)
            try:
                self.assertIsNone(
                    EXTENSION.cloud_path_from_local_path(Path(directory) / "elsewhere")
                )
            finally:
                EXTENSION.mount_dir = original_mount_dir


class MainThreadTests(unittest.TestCase):
    def test_shortcut_resolution_is_batched_outside_ui_thread(self):
        callbacks, idle, refreshed = [], [], []
        extension = EXTENSION.TwoDriveExtension()
        def make_file(path):
            return types.SimpleNamespace(
                get_location=lambda: types.SimpleNamespace(get_path=lambda: path),
                invalidate_extension_info=lambda: refreshed.append(path),
            )
        files = [make_file("/desktop/link"), make_file("/ordinary")]
        with patch.dict(EXTENSION.PATH_CACHE, {}, clear=True), \
             patch.dict(EXTENSION.PENDING_PATHS, {}, clear=True), \
             patch.object(EXTENSION, "PATH_READ_QUEUED", False), \
             patch.object(EXTENSION, "PATH_READ_IN_PROGRESS", False), \
             patch.object(os, "readlink", side_effect=AssertionError("UI filesystem probe")), \
             patch.object(EXTENSION.GLib, "idle_add", side_effect=idle.append), \
             patch.object(EXTENSION, "read_helper_async", side_effect=lambda paths, cb, op: callbacks.append((paths, cb, op))):
            for file_info in files:
                self.assertEqual(extension.update_file_info(file_info), EXTENSION.Nautilus.OperationResult.COMPLETE)
            self.assertEqual(len(idle), 1)
            self.assertFalse(idle.pop()())
            paths, callback, operation = callbacks.pop()
            self.assertEqual(paths, ["/desktop/link", "/ordinary"])
            self.assertEqual(operation, "--resolve-paths")
            # Requests arriving while a helper is active get a later batch.
            EXTENSION.cloud_path(make_file("/later"))
            self.assertEqual(idle, [])
            callback({"/desktop/link": "/must", "/ordinary": None})
            self.assertEqual(refreshed, ["/desktop/link"])
            self.assertEqual(EXTENSION.cloud_path(files[0]), "/must")
            self.assertIsNone(EXTENSION.cloud_path(files[1]))
            self.assertEqual(len(idle), 1)
            idle.pop()()
            callbacks.pop()[1]({})
            self.assertEqual(idle, [])
            self.assertFalse(EXTENSION.PATH_READ_IN_PROGRESS)

    def test_direct_mount_and_nonlocal_callbacks_do_not_start_path_reader(self):
        with patch.object(EXTENSION, "mount_dir", return_value="/mount/OneDrive"), \
             patch.object(EXTENSION, "queue_path_read", side_effect=AssertionError("unnecessary helper")), \
             patch.object(os, "readlink", side_effect=AssertionError("UI filesystem probe")):
            for local_path, expected in [("/mount/OneDrive", "/"),
                                         ("/mount/OneDrive/a", "/a"), (None, None)]:
                file_info = types.SimpleNamespace(get_location=lambda: types.SimpleNamespace(get_path=lambda: local_path))
                self.assertEqual(EXTENSION.cloud_path(file_info), expected)

    def test_poll_uses_native_async_reader_without_python_threads(self):
        callbacks = []
        with patch.dict(EXTENSION.TRACKED_FILES, {"/poll": {}}, clear=True), \
             patch.object(EXTENSION, "POLL_IN_PROGRESS", False), \
             patch.object(EXTENSION, "read_states_async", side_effect=lambda paths, cb: callbacks.append(cb)), \
             patch.object(EXTENSION.threading, "Thread", side_effect=AssertionError("embedded Python worker")), \
             patch.object(EXTENSION, "refresh_tracked_states") as refresh:
            self.assertTrue(EXTENSION.poll_tracked_files())
            self.assertTrue(EXTENSION.POLL_IN_PROGRESS)
            callbacks[0]({"/poll": "cached"})
            refresh.assert_called_once_with({"/poll": "cached"})
            self.assertFalse(EXTENSION.POLL_IN_PROGRESS)

    def test_failed_poll_preserves_existing_emblem_state(self):
        tracked = {"/retained": {"file_info": None, "state": "cached", "seen": EXTENSION.time.monotonic()}}
        with patch.dict(EXTENSION.TRACKED_FILES, tracked, clear=True), \
             patch.dict(EXTENSION.TRANSIENT_STATES, {}, clear=True), \
             patch.dict(EXTENSION.STATUS_CACHE, {}, clear=True), \
             patch.object(EXTENSION, "refresh_file") as refresh:
            EXTENSION.refresh_tracked_states({})
            self.assertEqual(EXTENSION.STATUS_CACHE["/retained"][1]["state"], "cached")
            refresh.assert_not_called()

    def test_provider_does_not_expose_broken_async_handle_entry_point(self):
        self.assertFalse(hasattr(EXTENSION.TwoDriveExtension(), "update_file_info_full"))

    def test_first_visit_batches_read_and_refreshes_cloud_and_cached_icons(self):
        callbacks, idle, events = [], [], []
        extension = EXTENSION.TwoDriveExtension()
        def make_file(path):
            file_info = types.SimpleNamespace(path=path)
            file_info.add_emblem = lambda name: events.append((path, name))
            file_info.invalidate_extension_info = lambda: extension.update_file_info(file_info)
            return file_info
        files = [make_file("/cloud"), make_file("/local")]
        with patch.dict(EXTENSION.TRACKED_FILES, {}, clear=True), \
             patch.dict(EXTENSION.STATUS_CACHE, {}, clear=True), \
             patch.dict(EXTENSION.TRANSIENT_STATES, {}, clear=True), \
             patch.object(EXTENSION, "POLL_QUEUED", False), \
             patch.object(EXTENSION, "POLL_IN_PROGRESS", False), \
             patch.object(EXTENSION, "start_refresh_timer"), \
             patch.object(EXTENSION, "log"), \
             patch.object(EXTENSION, "cloud_path", side_effect=lambda f: f.path), \
             patch.object(EXTENSION.GLib, "idle_add", side_effect=idle.append), \
             patch.object(EXTENSION, "read_states_async", side_effect=lambda paths, cb: callbacks.append((paths, cb))), \
             patch.object(EXTENSION.Nautilus, "info_provider_update_complete_invoke", side_effect=AssertionError("NULL handle completion")):
            for f in files:
                self.assertEqual(extension.update_file_info(f), EXTENSION.Nautilus.OperationResult.COMPLETE)
            self.assertEqual(events, [])
            self.assertEqual(len(idle), 1)
            self.assertFalse(idle.pop()())
            self.assertEqual(callbacks[0][0], ["/cloud", "/local"])
            callbacks[0][1]({"/cloud": "online_only", "/local": "cached"})
            self.assertEqual(events, [("/cloud", "emblem-twodrive-cloud"),
                                      ("/local", "emblem-twodrive-synced")])
            self.assertEqual(idle, [])
            # An ordinary Nautilus refresh reapplies an unchanged known badge.
            events.clear()
            extension.update_file_info(files[1])
            self.assertEqual(events, [("/local", "emblem-twodrive-synced")])
            # State changes invalidate the provider, which consumes the new cache.
            EXTENSION.refresh_tracked_states({"/cloud": "cached", "/local": "cached"})
            self.assertEqual(events[-1], ("/cloud", "emblem-twodrive-synced"))

    def test_file_info_callback_does_not_query_database(self):
        file_info = types.SimpleNamespace(add_emblem=lambda _emblem: None)
        with patch.object(EXTENSION, "cloud_path", return_value="/ui-test"), \
             patch.object(EXTENSION, "register_file_info"), \
             patch.object(EXTENSION, "status_for", side_effect=AssertionError("UI database query")):
            EXTENSION.TwoDriveExtension().update_file_info(file_info)


class ReleaseEmblemTests(unittest.TestCase):
    def test_upload_then_release_has_two_emblems_until_cloud_only(self):
        for state in ("writing", "dirty", "uploading"):
            display = EXTENSION.file_display_state(state, True)
            self.assertEqual(EXTENSION.emblems_for_state(display),
                             ["emblem-twodrive-cloud", "emblem-twodrive-syncing"])
        self.assertEqual(EXTENSION.emblems_for_state(EXTENSION.file_display_state("online_only", False)),
                         ["emblem-twodrive-cloud"])
        self.assertEqual(EXTENSION.emblems_for_state(EXTENSION.file_display_state("uploading", False)),
                         ["emblem-twodrive-syncing"])
        self.assertEqual(EXTENSION.emblems_for_state(EXTENSION.file_display_state("error", True)),
                         ["emblem-twodrive-error"])

    def test_callback_adds_both_emblems(self):
        emblems = []
        file_info = types.SimpleNamespace(add_emblem=emblems.append)
        with patch.object(EXTENSION, "cloud_path", return_value="/dual-icon-test"), \
             patch.object(EXTENSION, "register_file_info"), \
             patch.dict(EXTENSION.STATUS_CACHE, {"/dual-icon-test": (0, {"state": "uploading_release_pending"})}):
            EXTENSION.TwoDriveExtension().update_file_info(file_info)
        self.assertEqual(emblems, ["emblem-twodrive-cloud", "emblem-twodrive-syncing"])


class PinEmblemTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.db_path = Path(self.directory.name) / "status.sqlite3"
        self.conn = sqlite3.connect(self.db_path)
        self.addCleanup(self.conn.close)
        self.conn.execute("""create table files (
            path text unique, is_dir integer, state text, cache_path text,
            size integer default 1, release_pending integer default 0,
            pin_explicit integer default 0, pin_origin_remote_id text
        )""")
        self.conn.executemany(
            "insert into files (path, is_dir, state) values (?, ?, ?)",
            [("/project", 1, "online_only"), ("/project/file", 0, "online_only"),
             ("/project/plain", 0, "hydrating"), ("/project2/sibling", 0, "online_only")],
        )
        self.conn.commit()
        self.addCleanup(patch.stopall)
        patch.object(EXTENSION.os.path, "expanduser", return_value=os.fspath(self.db_path)).start()
        for mapping in (EXTENSION.STATUS_CACHE, EXTENSION.TRANSIENT_STATES, EXTENSION.TRACKED_FILES):
            patch.dict(mapping, {}, clear=True).start()
        patch.object(EXTENSION, "log").start()

    def test_persisted_pin_shows_cloud_and_pin_until_cache_publication(self):
        path = "/project/file"
        for state, explicit, origin in [("hydrating", 1, None), ("pinned", 1, None),
                                         ("online_only", 1, None), ("pinned", 0, "parent-id")]:
            with self.subTest(state=state, explicit=explicit):
                self.conn.execute("""update files set state=?, pin_explicit=?,
                    pin_origin_remote_id=?, cache_path=NULL where path=?""",
                    (state, explicit, origin, path))
                self.conn.commit()
                EXTENSION.STATUS_CACHE.clear()
                for display in (EXTENSION.db_states_for([path])[path], EXTENSION.status_for(path)["state"]):
                    self.assertEqual(EXTENSION.emblems_for_state(display),
                                     ["emblem-twodrive-cloud", "emblem-twodrive-pinned"])
        self.conn.execute("update files set state='pinned', cache_path='/cache/file' where path=?", (path,))
        self.conn.commit()
        self.assertEqual(EXTENSION.emblems_for_state(EXTENSION.db_states_for([path])[path]),
                         ["emblem-twodrive-pinned"])
        self.assertEqual(EXTENSION.emblems_for_state(EXTENSION.db_states_for(["/project/plain"])["/project/plain"]),
                         ["emblem-twodrive-syncing"])
        for state, release_pending, expected in [("error", 0, ["emblem-twodrive-error"]),
                                                  ("conflict", 0, ["emblem-twodrive-error"]),
                                                  ("uploading", 0, ["emblem-twodrive-syncing"]),
                                                  ("dirty", 1, ["emblem-twodrive-cloud", "emblem-twodrive-syncing"])]:
            self.conn.execute("update files set state=?, release_pending=? where path=?", (state, release_pending, path))
            self.conn.commit()
            self.assertEqual(EXTENSION.emblems_for_state(EXTENSION.db_states_for([path])[path]), expected)

    def test_folder_waits_for_pinned_children_and_keeps_upload_error_priority(self):
        self.conn.execute("delete from files where path='/project/plain'")
        self.conn.execute("update files set state='pinned', pin_explicit=1 where path in ('/project', '/project/file')")
        self.conn.commit()
        self.assertEqual(EXTENSION.emblems_for_state(EXTENSION.db_states_for(["/project"])["/project"]),
                         ["emblem-twodrive-cloud", "emblem-twodrive-pinned"])
        self.assertEqual(EXTENSION.db_states_for(["/project2"]), {})
        self.assertEqual(EXTENSION.directory_state_for(self.conn, "/project2", "online_only"), "online_only")
        self.conn.execute("update files set cache_path='/cache/file' where path='/project/file'")
        self.conn.commit()
        self.assertEqual(EXTENSION.db_states_for(["/project"])["/project"], "pinned")
        for state, expected in [("uploading", "uploading"), ("error", "error")]:
            self.conn.execute("update files set state=? where path='/project/file'", (state,))
            self.conn.commit()
            self.assertEqual(EXTENSION.db_states_for(["/project"])["/project"], expected)

    def test_menu_pin_feedback_is_immediate_and_completion_comes_from_database(self):
        extension = EXTENSION.TwoDriveExtension()
        emblems, callbacks = [], []
        file_info = types.SimpleNamespace(path="/project/file", add_emblem=emblems.append)
        file_info.invalidate_extension_info = lambda: extension.update_file_info(file_info)
        patch.object(EXTENSION, "cloud_path", side_effect=lambda file: file.path).start()
        patch.object(EXTENSION, "start_refresh_timer").start()
        patch.object(EXTENSION, "queue_state_poll").start()
        patch.object(EXTENSION, "schedule_refreshes").start()
        patch.object(EXTENSION, "notify").start()
        patch.object(EXTENSION.threading, "Thread", side_effect=AssertionError("embedded Python worker")).start()
        patch.object(EXTENSION, "run_local_async", create=True,
                     side_effect=lambda args, cb: callbacks.append((args, cb))).start()
        EXTENSION.STATUS_CACHE[file_info.path] = (EXTENSION.time.monotonic(), {"state": "online_only"})
        extension.activate(None, "pin", [file_info.path], [file_info])
        self.assertEqual(emblems, ["emblem-twodrive-cloud", "emblem-twodrive-pinned"])
        self.assertEqual(callbacks[0][0], ["pin", file_info.path])
        # The command is still running; authoritative publication must remove
        # the temporary waiting badge before its completion callback arrives.
        self.conn.execute("""update files set state='pinned', pin_explicit=1,
            cache_path='/cache/file' where path=?""", (file_info.path,))
        self.conn.commit()
        emblems.clear()
        EXTENSION.refresh_tracked_states(EXTENSION.db_states_for([file_info.path]))
        self.assertEqual(emblems, ["emblem-twodrive-pinned"])
        callbacks.pop()[1](EXTENSION.subprocess.CompletedProcess([], 0, "pinned"))
        self.assertNotIn(file_info.path, EXTENSION.TRANSIENT_STATES)

    def test_failed_pin_clears_feedback_and_reloads_cloud_state(self):
        callbacks = []
        patch.object(EXTENSION, "run_local_async", create=True,
                     side_effect=lambda args, cb: callbacks.append(cb)).start()
        patch.object(EXTENSION.threading, "Thread", side_effect=AssertionError("embedded Python worker")).start()
        patch.object(EXTENSION, "notify").start()
        patch.object(EXTENSION, "schedule_refreshes").start()
        file_info = types.SimpleNamespace(invalidate_extension_info=lambda: None)
        EXTENSION.run_action("pin", ["/project/file"], [file_info])
        self.assertIn("/project/file", EXTENSION.TRANSIENT_STATES)
        callbacks.pop()(EXTENSION.subprocess.CompletedProcess([], 1, "failed"))
        self.assertNotIn("/project/file", EXTENSION.TRANSIENT_STATES)
        self.assertEqual(EXTENSION.db_states_for(["/project/file"])["/project/file"], "online_only")


class ActionSequenceTests(unittest.TestCase):
    def test_multi_selection_runs_in_order_and_clears_each_completed_request(self):
        callbacks, notifications = [], []
        paths = ["/first", "/second"]
        files = [types.SimpleNamespace(invalidate_extension_info=lambda: None) for _path in paths]
        with patch.dict(EXTENSION.TRANSIENT_STATES, {}, clear=True), \
             patch.dict(EXTENSION.STATUS_CACHE, {}, clear=True), \
             patch.object(EXTENSION, "log"), \
             patch.object(EXTENSION, "schedule_refreshes"), \
             patch.object(EXTENSION, "notify", side_effect=lambda *args: notifications.append(args)), \
             patch.object(EXTENSION, "run_local_async", side_effect=lambda args, cb: callbacks.append((args, cb))):
            EXTENSION.run_action("pin", paths, files)
            self.assertEqual([args for args, _cb in callbacks], [["pin", "/first"]])
            callbacks.pop()[1](EXTENSION.subprocess.CompletedProcess([], 1, "download failed"))
            self.assertNotIn("/first", EXTENSION.TRANSIENT_STATES)
            self.assertIn("/second", EXTENSION.TRANSIENT_STATES)
            self.assertEqual(callbacks[0][0], ["pin", "/second"])
            callbacks.pop()[1](EXTENSION.subprocess.CompletedProcess([], 0, "pinned"))
            self.assertEqual(EXTENSION.TRANSIENT_STATES, {})
            self.assertEqual(len(notifications), 1)
            self.assertIn("2 item(s); 1 failed", notifications[0][1])

    def test_other_menu_commands_preserve_arguments_and_notifications(self):
        for action, args, output, title in [
            ("sync", ["sync"], "synced", "TwoDrive sync"),
            ("status", ["status-path", "/file"], "state=online_only", "TwoDrive status"),
            ("unpin", ["unpin", "/file"], "unpinned", "TwoDrive Cancel always keep on this device"),
            ("release", ["release", "/file"], "released 0; queued 0", "TwoDrive Release space"),
        ]:
            with self.subTest(action=action), \
                 patch.dict(EXTENSION.TRANSIENT_STATES, {}, clear=True), \
                 patch.dict(EXTENSION.STATUS_CACHE, {}, clear=True), \
                 patch.object(EXTENSION, "log"), \
                 patch.object(EXTENSION, "schedule_refreshes"), \
                 patch.object(EXTENSION, "notify") as notify, \
                 patch.object(EXTENSION, "run_local_async") as run:
                EXTENSION.run_action(action, ["/file"], [])
                self.assertEqual(run.call_args.args[0], args)
                run.call_args.args[1](EXTENSION.subprocess.CompletedProcess([], 0, output))
                self.assertEqual(notify.call_args.args[0], title)
                self.assertIn(output, notify.call_args.args[1])
                if action == "release":
                    self.assertIn("No local cache was removed", notify.call_args.args[1])


class CopyPathTests(unittest.TestCase):
    def test_copy_path_outside_mount_preserves_exact_names(self):
        paths = ["/tmp/a folder/report.txt", '/tmp/a"b$`c']
        files = []
        for path in paths:
            location = types.SimpleNamespace(get_path=lambda path=path: path)
            files.append(types.SimpleNamespace(get_location=lambda location=location: location))
        clipboard = []
        repository = sys.modules["gi.repository"]
        sys.modules["gi"].require_version = lambda *_args: None
        repository.Gdk = types.SimpleNamespace(
            Display=types.SimpleNamespace(get_default=lambda: types.SimpleNamespace(
                get_clipboard=lambda: types.SimpleNamespace(set=clipboard.append)
            ))
        )
        items = EXTENSION.TwoDriveExtension().get_file_items(files)
        self.assertEqual([item.label for item in items], ["Copy path"])
        items[0].callback()
        self.assertEqual(clipboard, ["\n".join(paths)])


class DirectoryStateTests(unittest.TestCase):
    def setUp(self):
        self.conn = sqlite3.connect(":memory:")
        self.conn.execute(
            "create table files (path text unique, is_dir integer, state text, cache_path text, release_pending integer default 0, pin_explicit integer default 0, pin_origin_remote_id text)"
        )

    def tearDown(self):
        self.conn.close()

    def add_file(self, path, state="online_only", cache_path=None):
        self.conn.execute(
            "insert into files (path, is_dir, state, cache_path) values (?, 0, ?, ?)",
            (path, state, cache_path),
        )

    def test_pending_release_descendant_adds_cloud_to_syncing_folder(self):
        self.add_file("/project/big.zip", "uploading", "/cache/big")
        self.conn.execute("update files set release_pending=1 where path='/project/big.zip'")
        self.assertEqual(EXTENSION.directory_state_for(self.conn, "/project", "online_only"),
                         "uploading_release_pending")
        self.assertEqual(EXTENSION.directory_state_for(self.conn, "/project2", "online_only"),
                         "online_only")

    def test_any_local_descendant_marks_folder_cached(self):
        self.add_file("/project/cloud.txt")
        self.add_file("/project/src/local.txt", "cached", "/cache/local")
        self.assertEqual(
            EXTENSION.directory_state_for(self.conn, "/project", "online_only"),
            "cached",
        )

    def test_all_online_only_descendants_leave_folder_online_only(self):
        self.add_file("/project/cloud.txt")
        self.assertEqual(
            EXTENSION.directory_state_for(self.conn, "/project", "online_only"),
            "online_only",
        )

    def test_stale_folder_state_does_not_imply_active_files(self):
        self.add_file("/project/local.txt", "cached", "/cache/local")
        self.add_file("/project/cloud.txt")
        for state in ("hydrating", "writing", "dirty", "uploading"):
            self.assertEqual(
                EXTENSION.directory_state_for(self.conn, "/project", state), "cached"
            )

    def test_sibling_prefix_does_not_affect_folder(self):
        self.add_file("/project2/busy.txt", "uploading")
        self.assertEqual(
            EXTENSION.directory_state_for(self.conn, "/project", "online_only"),
            "online_only",
        )

    def test_sync_and_error_states_take_priority(self):
        self.add_file("/project/local.txt", "cached", "/cache/local")
        self.add_file("/project/upload.txt", "uploading", "/cache/upload")
        self.assertEqual(
            EXTENSION.directory_state_for(self.conn, "/project", "pinned"),
            "uploading",
        )
        self.add_file("/project/conflict.txt", "conflict", "/cache/conflict")
        self.assertEqual(
            EXTENSION.directory_state_for(self.conn, "/project", "pinned"),
            "error",
        )


if __name__ == "__main__":
    unittest.main()
