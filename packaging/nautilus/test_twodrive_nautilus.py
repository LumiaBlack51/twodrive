import importlib.util
import os
from pathlib import Path
import sqlite3
import sys
import tempfile
import types
import unittest


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
            "create table files (path text unique, is_dir integer, state text, cache_path text)"
        )

    def tearDown(self):
        self.conn.close()

    def add_file(self, path, state="online_only", cache_path=None):
        self.conn.execute(
            "insert into files values (?, 0, ?, ?)",
            (path, state, cache_path),
        )

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
