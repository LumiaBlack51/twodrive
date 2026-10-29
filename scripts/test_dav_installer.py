import importlib.util
from pathlib import Path
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('dav_installer', Path(__file__).with_name('install-dav-desktop.py'))
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class InstallerTests(unittest.TestCase):
    def test_preserves_running_runtime_paths_and_account_state(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            app = root/'data/twodrive-dav'
            old = app/'runtime'
            old.mkdir(parents=True)
            (old/'twodrive-dav-engine').write_bytes(b'old-executable')
            (app/'state').mkdir()
            (app/'state/profiles.json').write_bytes(b'private-account-fixture')
            bundle = root/'bundle'
            (bundle/'data/flutter_assets/assets').mkdir(parents=True)
            (bundle/'twodrive_dav').write_bytes(b'new-gui')
            (bundle/'data/flutter_assets/assets/cloud-connected.svg').write_bytes(b'status-svg')
            (bundle/'data/flutter_assets/assets/app-icon.svg').write_bytes(b'plain-app-svg')
            engines = root/'engines'
            engines.mkdir()
            for name in ['twodrive-dev', 'twodrive-dav-engine', 'twodrive-provider-login']:
                (engines/name).write_bytes(b'new-executable')
            installer.install(bundle, engines, root/'data', root/'bin', preserve_runtime=True)
            retired = list(app.glob('runtime-before-*'))
            self.assertEqual(len(retired), 1)
            self.assertEqual((retired[0]/'twodrive-dav-engine').read_bytes(), b'old-executable')
            self.assertEqual((app/'state/profiles.json').read_bytes(), b'private-account-fixture')
            self.assertEqual((app/'runtime/twodrive_dav').read_bytes(), b'new-gui')
            launcher = (root/'bin/twodrive-dav').read_text()
            compile(launcher, 'launcher', 'exec')
            self.assertIn('os.environ.setdefault("TWODRIVE_TRAY_BACKEND", "sni")', launcher)
            self.assertEqual((app/'runtime/twodrive-provider-login').read_bytes(), b'new-executable')
            desktop = (root/'data/applications/app.twodrive.twodrive_dav.desktop').read_text()
            self.assertIn('Name=TwoDrive\n', desktop)
            self.assertIn('Icon=twodrive-dav-minimal\n', desktop)
            self.assertEqual((root/'data/icons/hicolor/scalable/apps/twodrive-dav-minimal.svg').read_bytes(), b'plain-app-svg')

    def test_unknown_gnome_component_queues_only_its_uuid_for_next_session(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root/'source'
            source.mkdir()
            for name in ['metadata.json', 'extension.js', 'geometry.js']:
                (source/name).write_text('fixture')
            responses = [SimpleNamespace(returncode=1),
                         SimpleNamespace(stdout="['existing-user-extension']", returncode=0),
                         SimpleNamespace(returncode=0)]
            with patch.object(installer.shutil, 'which', return_value='/fixture/tool'), \
                 patch.object(installer.subprocess, 'run', side_effect=responses) as run:
                installer.install_gnome_anchor(root/'data', source)
            self.assertEqual(run.call_args_list[-1].args[0], ['gsettings', 'set', 'org.gnome.shell',
                'enabled-extensions', "['existing-user-extension', 'twodrive-tray-anchor@twodrive']"])


if __name__ == '__main__':
    unittest.main()
