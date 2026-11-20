"""Local fixtures only: no mounted file reads, cloud requests or credentials."""
import hashlib
import json
from pathlib import Path
import sqlite3
import tempfile
import types
import unittest
from unittest.mock import patch

from test_twodrive_nautilus import EXTENSION as ext


class DavEmblemTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.state = self.root / "state"
        self.state.mkdir()
        self.profiles = []
        self.mounts = {}
        self.addCleanup(patch.stopall)
        patch.object(ext, "dav_state_dir", return_value=self.state).start()
        patch.object(ext, "mounted_filesystems", side_effect=lambda: self.mounts).start()

    def profile(self, ident, kind="onedrive"):
        p = {"id": ident, "kind": kind, "revision": "revision-1",
             "mount_path": str(self.root / ident)}
        self.profiles.append(p)
        (self.state / "profiles.json").write_text(json.dumps({"profiles": self.profiles}))
        (self.state / f"session-{ident}.json").write_text(json.dumps(
            {"kind": kind, "revision": p["revision"], "binding": f"binding-{ident}"}))
        return p

    def native(self, p):
        mount = self.state / f"mount-{p['id']}"
        (mount / "cache").mkdir(parents=True)
        (mount / "binding.json").write_text(json.dumps({"provider": f"binding-{p['id']}"}))
        conn = sqlite3.connect(mount / "mount.sqlite3")
        self.addCleanup(conn.close)
        conn.execute("""CREATE TABLE files (path TEXT UNIQUE, state TEXT,
                        is_dir INTEGER DEFAULT 0, release_pending INTEGER DEFAULT 0,
                        pin_explicit INTEGER DEFAULT 0, pin_origin_remote_id TEXT,
                        cache_path TEXT, size INTEGER DEFAULT 4)""")
        self.mounts[p["mount_path"]] = ("fuse", "twodrive")
        return conn, mount

    def bridge_cache(self, p):
        digest = hashlib.sha256(f"binding-{p['id']}".encode()).hexdigest()
        root = self.state / f"mount-{p['id']}" / "linux-accounts" / digest / "vfs"
        data, meta = root / "vfs/TWODRIVE{ACTIVE}", root / "vfsMeta/TWODRIVE{ACTIVE}"
        data.mkdir(parents=True)
        meta.mkdir(parents=True)
        self.mounts[p["mount_path"]] = ("fuse.rclone", "twodrive-ro-test")
        return data, meta, set()

    def cache_file(self, cache, path="song.mp3", **changes):
        data, meta, _downloads = cache
        (data / path).parent.mkdir(parents=True, exist_ok=True)
        (meta / path).parent.mkdir(parents=True, exist_ok=True)
        (data / path).write_bytes(b"1234")
        item = {"Dirty": False, "Size": 4, "Fingerprint": "fixture",
                "Rs": [{"Pos": 0, "Size": 4}]}
        item.update(changes)
        (meta / path).write_text(json.dumps(item))

    def test_all_four_mounts_route_without_fuse_probes_or_account_collisions(self):
        keys = []
        for kind in ("onedrive", "google", "baidu", "icloud"):
            p = self.profile(kind, kind)
            with patch.object(ext.os, "readlink", side_effect=AssertionError("FUSE probe")):
                actual = ext.cloud_path_from_local_path(p["mount_path"] + "/same.mp3")
            expected = "dav:" + json.dumps([p["id"], p["revision"], p["mount_path"],
                                            "/same.mp3"], separators=(",", ":"))
            self.assertEqual(actual, expected)
            keys.append(actual)
        self.assertEqual(len(set(keys)), 4)
        self.assertIsNone(ext.dav_path(self.profiles[0]["mount_path"] + "-other/a", self.profiles))

    def test_shortcut_into_dav_mount(self):
        p = self.profile("baidu", "baidu")
        shortcut = self.root / "shortcut"
        shortcut.symlink_to(p["mount_path"] + "/music", target_is_directory=True)
        self.assertEqual(ext.cloud_path_from_local_path(shortcut / "song.mp3"),
                         ext.dav_key(p, "/music/song.mp3"))

    def test_dav_callback_reads_no_files_on_ui_thread(self):
        p = self.profile("baidu", "baidu")
        path, events = p["mount_path"] + "/song.mp3", []
        file_info = types.SimpleNamespace(
            get_location=lambda: types.SimpleNamespace(get_path=lambda: path),
            add_emblem=events.append)
        with patch.dict(ext.PATH_CACHE, {}, clear=True), \
             patch.object(ext, "local_json", side_effect=AssertionError("UI metadata read")), \
             patch.object(ext, "queue_path_read") as queued:
            ext.TwoDriveExtension().update_file_info(file_info)
            queued.assert_called_once()
        self.assertEqual(events, [])

    def test_native_states_and_missing_cache_are_honest(self):
        p = self.profile("onedrive")
        db, mount = self.native(p)
        cache = mount / "cache/complete"
        cache.write_bytes(b"1234")
        for name, state, cached in [("cloud", "online_only", None),
                                    ("download", "hydrating", None),
                                    ("local", "cached", str(cache)),
                                    ("missing", "cached", str(mount / "cache/missing")),
                                    ("upload", "dirty", str(cache)), ("error", "error", None)]:
            db.execute("INSERT INTO files(path,state,cache_path) VALUES(?,?,?)",
                       ("/" + name, state, cached))
        db.commit()
        expected = {"cloud": "online_only", "download": "hydrating", "local": "cached",
                    "missing": "online_only", "upload": "dirty", "error": "error"}
        keys = {name: ext.dav_key(p, "/" + name) for name in expected}
        states = ext.db_states_for(list(keys.values()))
        self.assertEqual({name: states[key] for name, key in keys.items()}, expected)
        self.assertEqual(ext.emblems_for_state(states[keys["download"]]),
                         ["emblem-twodrive-downloading"])

    def test_revision_binding_unmount_and_relocated_root_drop_stale_badges(self):
        p = self.profile("onedrive")
        db, mount = self.native(p)
        key = ext.dav_key(p, "/song.mp3")
        db.execute("INSERT INTO files(path,state) VALUES('/song.mp3','online_only')")
        db.commit()
        self.assertEqual(ext.db_states_for([key])[key], "online_only")
        for field, replacement in [("revision", "new"), ("mount_path", "/relocated")]:
            altered = dict(p, **{field: replacement})
            stale = ext.dav_key(altered, "/song.mp3")
            self.assertEqual(ext.db_states_for([stale])[stale], "unknown")
        (mount / "binding.json").write_text(json.dumps({"provider": "another-account"}))
        self.assertEqual(ext.db_states_for([key])[key], "unknown")
        self.mounts.clear()
        self.assertEqual(ext.db_states_for([key])[key], "unknown")

    def test_two_accounts_same_name_read_separate_databases(self):
        results = []
        for ident, state in [("first", "online_only"), ("second", "error")]:
            p = self.profile(ident)
            db, _mount = self.native(p)
            db.execute("INSERT INTO files(path,state) VALUES('/same.mp3',?)", (state,))
            db.commit()
            results.append((ext.dav_key(p, "/same.mp3"), state))
        self.assertEqual(ext.db_states_for([key for key, _ in results]), dict(results))

    def test_gnome_webdav_exports_badges_without_touching_the_remote_file(self):
        p = self.profile("icloud", "icloud")
        export = self.root / "gvfs/dav:fixture"
        Path(p["mount_path"]).symlink_to(export, target_is_directory=True)
        owner = self.state / "mount-icloud/linux-readonly/gnome-webdav.json"
        owner.parent.mkdir(parents=True)
        owner.write_text(json.dumps({
            "revision": p["revision"], "binding": "binding-icloud",
            "path": p["mount_path"], "export": str(export),
        }))
        with patch.dict(ext.os.environ, {"XDG_RUNTIME_DIR": str(self.root)}):
            key = ext.dav_key(p, "/document.txt")
            self.assertEqual(ext.dav_path(str(export / "document.txt"), [p]), key)
            self.assertEqual(ext.dav_states_for([key])[key], "online_only")
            cache = owner.parent / "gnome-cache.json"
            with (owner.parent / "run.lock").open("wb") as lock:
                ext.fcntl.flock(lock, ext.fcntl.LOCK_EX)
                cache.write_text(json.dumps({"export": str(export), "paths": ["/document.txt"]}))
                self.assertEqual(ext.emblems_for_state(ext.dav_states_for([key])[key]),
                                 ["emblem-twodrive-synced"])
                cache.write_text(json.dumps({"export": str(export) + "-old", "paths": ["/document.txt"]}))
                self.assertEqual(ext.dav_states_for([key])[key], "online_only")
                cache.write_text(json.dumps({"export": str(export), "paths": ["/document.txt"]}))
            self.assertEqual(ext.dav_states_for([key])[key], "online_only")
            with patch.object(ext, "gvfs_export", side_effect=AssertionError("unexpected iCloud probe")):
                self.assertIsNone(ext.dav_path("/unrelated/document.txt", [p]))
            Path(p["mount_path"]).unlink()
            self.assertIsNone(ext.dav_path(str(export / "document.txt"), [p]))
            self.assertEqual(ext.dav_states_for([key])[key], "unknown")

    def test_bridge_complete_sparse_partial_empty_dirty_and_active_download(self):
        p = self.profile("baidu", "baidu")
        cache = self.bridge_cache(p)
        self.cache_file(cache)
        self.assertEqual(ext.bridge_file_state("/song.mp3", cache), "cached")
        for ranges in [[{"Pos": 0, "Size": 2}], [{"Pos": 2, "Size": 2}],
                       [{"Pos": 0, "Size": 1}, {"Pos": 2, "Size": 2}]]:
            self.cache_file(cache, Rs=ranges)
            self.assertEqual(ext.bridge_file_state("/song.mp3", cache), "online_only")
        self.cache_file(cache, Rs=[{"Pos": 2, "Size": 2}, {"Pos": 0, "Size": 2}])
        self.assertEqual(ext.bridge_file_state("/song.mp3", cache), "cached")
        cache[2].add("song.mp3")
        self.assertEqual(ext.bridge_file_state("/song.mp3", cache), "hydrating")
        self.cache_file(cache, Dirty=True)
        self.assertEqual(ext.bridge_file_state("/song.mp3", cache), "uploading")
        cache[2].clear()
        self.cache_file(cache, Size=0, Rs=[])
        (cache[0] / "song.mp3").write_bytes(b"")
        self.assertEqual(ext.bridge_file_state("/song.mp3", cache), "cached")

    def test_missing_malformed_and_symlinked_cache_never_shows_downloaded(self):
        p = self.profile("icloud", "icloud")
        cache = self.bridge_cache(p)
        self.cache_file(cache)
        (cache[0] / "song.mp3").unlink()
        self.assertEqual(ext.bridge_file_state("/song.mp3", cache), "online_only")
        (cache[0] / "song.mp3").symlink_to("/etc/hosts")
        self.assertEqual(ext.bridge_file_state("/song.mp3", cache), "online_only")
        (cache[1] / "song.mp3").write_text("{")
        self.assertEqual(ext.bridge_file_state("/song.mp3", cache), "online_only")

    def test_inactive_cache_cannot_turn_current_file_green(self):
        p = self.profile("baidu", "baidu")
        cache = self.bridge_cache(p)
        self.cache_file(cache)
        key = ext.dav_key(p, "/song.mp3")
        with patch.object(ext, "bridge_status", return_value=None):
            self.assertEqual(ext.db_states_for([key])[key], "unknown")
        with patch.object(ext, "bridge_status", return_value=cache):
            self.assertEqual(ext.db_states_for([key])[key], "cached")

    def test_bridge_selects_current_process_mount_and_cache_paths(self):
        p = self.profile("baidu", "baidu")
        data, meta, _downloads = self.bridge_cache(p)
        root = data.parent.parent
        responses = {"core/pid": {"pid": 123},
                     "mount/listmounts": {"mountPoints": [{"MountPoint": p["mount_path"], "Fs": "TWODRIVE{ACTIVE}:"}]},
                     "core/stats": {"transferring": [{"name": "song.mp3"}]}}
        with patch.object(ext, "bridge_candidates", return_value=[(123, {})]), \
             patch.object(ext, "rc_read", side_effect=lambda _env, op: responses[op]):
            self.assertEqual(ext.bridge_status(root, p["mount_path"]), (data, meta, {"song.mp3"}))
            self.assertIsNone(ext.bridge_status(root, "/another-mount"))
            responses["core/pid"] = {"pid": 999}
            self.assertIsNone(ext.bridge_status(root, p["mount_path"]))
            responses["core/pid"] = {"pid": 123}
            responses["mount/listmounts"]["mountPoints"][0]["Fs"] = "../../escape:"
            self.assertIsNone(ext.bridge_status(root, p["mount_path"]))

    def test_cache_namespace_changes_without_querying_vfs_directory_statistics(self):
        p = self.profile("baidu", "baidu")
        cache = self.bridge_cache(p)
        self.cache_file(cache)
        root = cache[0].parent.parent
        replies = {"core/pid": {"pid": 123},
                   "mount/listmounts": {"mountPoints": [{"MountPoint": p["mount_path"], "Fs": "TWODRIVE{NEW}:"}]},
                   "core/stats": {}}
        with patch.object(ext, "bridge_candidates", return_value=[(123, {})]), \
             patch.object(ext, "rc_read", side_effect=lambda _env, op: replies[op]):
            current = ext.bridge_status(root, p["mount_path"])
        self.assertEqual(current[:2], (root / "vfs/TWODRIVE{NEW}", root / "vfsMeta/TWODRIVE{NEW}"))
        self.assertEqual(ext.bridge_file_state("/song.mp3", current), "online_only")

    def test_malformed_or_traversing_keys_do_not_access_cache(self):
        p = self.profile("baidu", "baidu")
        bad = ["dav:invalid", ext.dav_key(p, "/../outside"), ext.dav_key(p, "//outside")]
        with patch.object(ext, "bridge_status", side_effect=AssertionError("invalid routing")):
            self.assertEqual(ext.db_states_for(bad), dict.fromkeys(bad, "unknown"))

    def test_legacy_cli_actions_excluded_for_dav_items(self):
        p = self.profile("baidu", "baidu")
        key = ext.dav_key(p, "/song.mp3")
        with patch.object(ext, "cloud_path", return_value=key):
            self.assertEqual(ext.selected_paths([object()]), [])

    def test_unmount_invalidates_existing_green_badge(self):
        key = ext.dav_key(self.profile("baidu", "baidu"), "/song.mp3")
        with patch.dict(ext.TRACKED_FILES, {key: {"file_info": object(), "state": "cached",
                                                "seen": ext.time.monotonic()}}, clear=True), \
             patch.dict(ext.TRANSIENT_STATES, {}, clear=True), \
             patch.object(ext, "log"), patch.object(ext, "refresh_file") as refresh:
            ext.refresh_tracked_states({key: "unknown"})
            refresh.assert_called_once()
            self.assertEqual(ext.STATUS_CACHE[key][1]["state"], "unknown")

    def test_local_rc_rejects_external_redirect_and_mutation_operations(self):
        env = {"RCLONE_RC_ADDR": "example.org:80", "RCLONE_RC_USER": "u", "RCLONE_RC_PASS": "p"}
        with self.assertRaises(ValueError):
            ext.rc_read(env, "core/stats")
        with self.assertRaises(ValueError):
            ext.rc_read(env, "operations/delete")
        with self.assertRaises(ValueError):
            ext.rc_read(env, "vfs/stats")
        self.assertIsNone(ext.NoRedirect().redirect_request(None, None, None, None, None, None))


if __name__ == "__main__":
    unittest.main()
