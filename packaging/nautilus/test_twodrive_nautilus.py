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

    def test_first_visit_completes_only_after_emblems_are_ready(self):
        events, callbacks = [], []
        file_info = types.SimpleNamespace(add_emblem=lambda name: events.append(name))
        extension = EXTENSION.TwoDriveExtension()
        with patch.object(EXTENSION, "cloud_path", return_value="/first-visit"), \
             patch.object(EXTENSION, "read_states_async", side_effect=lambda paths, cb: callbacks.append(cb)), \
             patch.object(EXTENSION, "status_for", side_effect=AssertionError("UI database query")), \
             patch.object(EXTENSION, "register_file_info"), \
             patch.object(EXTENSION.Nautilus, "info_provider_update_complete_invoke", side_effect=lambda *args: events.append("complete")):
            result = extension.update_file_info_full(extension, 1, None, file_info)
            self.assertEqual(result, EXTENSION.Nautilus.OperationResult.IN_PROGRESS)
            self.assertEqual(events, [])
            callbacks[0]({"/first-visit": "online_only"})
            self.assertEqual(events, ["emblem-twodrive-cloud", "complete"])
            self.assertEqual(extension.pending_updates, [])

    def test_cancelled_request_does_not_update_file_info(self):
        callbacks = []
        extension = EXTENSION.TwoDriveExtension()
        with patch.object(EXTENSION, "cloud_path", return_value="/cancelled"), \
             patch.object(EXTENSION, "read_states_async", side_effect=lambda paths, cb: callbacks.append(cb)), \
             patch.object(EXTENSION.Nautilus, "info_provider_update_complete_invoke") as complete:
            extension.update_file_info_full(extension, 1, None, None)
            extension.cancel_update(extension, 1)
            callbacks[0]({"/cancelled": "cached"})
            complete.assert_not_called()
            self.assertEqual(extension.pending_updates, [])

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
            "create table files (path text unique, is_dir integer, state text, cache_path text, release_pending integer default 0)"
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
