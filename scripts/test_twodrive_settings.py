"""Settings diagnostics/selection tests without a desktop or live configuration."""
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import subprocess
import types
import unittest
from unittest.mock import patch, Mock

loader = importlib.machinery.SourceFileLoader("twodrive_settings", str(Path(__file__).with_name("twodrive-settings")))
spec = importlib.util.spec_from_loader(loader.name, loader)
settings = importlib.util.module_from_spec(spec)
loader.exec_module(settings)


class Widget:
    def __init__(self, **kwargs):
        self.label = kwargs.get("label", "")
        self.children = []
        self.callback = None

    def append(self, child):
        self.children.append(child)

    def remove(self, child):
        self.children.remove(child)

    def get_first_child(self):
        return self.children[0] if self.children else None

    def connect(self, signal, callback):
        self.callback = callback

    def set_hexpand(self, value):
        pass

    def set_xalign(self, value):
        pass

    def set_wrap(self, value):
        pass

    def set_selectable(self, value):
        pass


class SettingsTests(unittest.TestCase):
    def test_status_errors_are_displayable_instead_of_claiming_no_errors(self):
        for result in [subprocess.CompletedProcess([], 1, "", "cannot read config"),
                       subprocess.CompletedProcess([], 0, "invalid JSON", "")]:
            with patch.object(settings.subprocess, "run", return_value=result):
                self.assertIn("error", settings.run_known_folder_status())
        with patch.object(settings.subprocess, "run", side_effect=subprocess.TimeoutExpired([], 5)):
            self.assertIn("error", settings.run_known_folder_status())

    def test_status_keeps_missing_sources_and_system_warnings(self):
        folder = dict(index=0, configured_local="~/下载", local="/home/test/下载",
                      remote="/Downloads", state="missing", message="Source does not exist.",
                      warnings=["The system directory differs."], system_directory="/home/test/Downloads")
        result = subprocess.CompletedProcess([], 0, json.dumps(dict(enabled=True, folders=[folder])), "")
        with patch.object(settings.subprocess, "run", return_value=result):
            self.assertEqual(settings.run_known_folder_status()["folders"][0], folder)

    def test_save_uses_argument_array_and_expected_source_without_a_shell(self):
        folder = dict(index=1, configured_local="~/下载")
        path = "/tmp/a folder; literal $value"
        result = subprocess.CompletedProcess([], 0, "Restart TwoDrive to apply.", "")
        with patch.object(settings, "twodrive_bin", return_value="/test/twodrive"), \
             patch.object(settings.subprocess, "run", return_value=result) as run:
            success, message = settings.save_known_folder_source(folder, path)
        self.assertTrue(success)
        self.assertIn("Restart", message)
        self.assertEqual(run.call_args.args[0], ["/test/twodrive", "known-folders", "set-source", "1", path,
                                               "--expected-local", "~/下载"])
        self.assertNotIn("shell", run.call_args.kwargs)
        with patch.object(settings.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, "", "unsafe source")):
            self.assertEqual(settings.save_known_folder_source(folder, path), (False, "unsafe source"))

    def test_refresh_shows_errors_and_each_choose_button_targets_its_own_mapping(self):
        folders = [dict(index=i, local=f"/local/{i}", remote=f"/cloud/{i}",
                        message="Source does not exist.", warnings=["System directory differs."])
                   for i in range(2)]
        app = types.SimpleNamespace(source_label=settings.SettingsApp.source_label, choose_source=Mock())
        box, window = Widget(), object()
        with patch.object(settings.Gtk, "Box", Widget), patch.object(settings.Gtk, "Label", Widget), \
             patch.object(settings.Gtk, "Button", Widget), \
             patch.object(settings, "run_known_folder_status", return_value=dict(enabled=True, mode="upload_only", folders=folders)):
            settings.SettingsApp.refresh_sources(app, box, window)
            rows = box.children[-2:]
            self.assertEqual(rows[0].children[0].children[1].label, "Source does not exist.")
            for index, row in enumerate(rows):
                row.children[1].callback(None)
                self.assertEqual(app.choose_source.call_args.args, (folders[index], box, window))
        with patch.object(settings.Gtk, "Label", Widget), \
             patch.object(settings, "run_known_folder_status", return_value=dict(error="unavailable", folders=[])):
            settings.SettingsApp.refresh_sources(app, box, window)
        self.assertEqual(len(box.children), 1)
        self.assertIn("Cannot check", box.children[0].label)


if __name__ == "__main__":
    unittest.main()
