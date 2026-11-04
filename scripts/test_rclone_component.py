import hashlib
import importlib.util
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location('rclone_component', Path(__file__).with_name('install-rclone-component.py'))
component = importlib.util.module_from_spec(spec)
spec.loader.exec_module(component)


class ComponentTests(unittest.TestCase):
    def fixture(self, root, symlink=False):
        archive = root / 'fixture.zip'
        prefix = f'rclone-v{component.VERSION}-linux-amd64/'
        with zipfile.ZipFile(archive, 'w') as bundle:
            member = zipfile.ZipInfo(prefix + 'rclone')
            member.external_attr = ((stat.S_IFLNK if symlink else stat.S_IFREG) | 0o755) << 16
            bundle.writestr(member, b'fixture-executable')
            bundle.writestr(prefix + 'README.txt', b'fixture-license-and-docs')
            bundle.writestr('../outside', b'must-not-extract')
        return archive, hashlib.sha256(archive.read_bytes()).hexdigest()

    def test_tampered_archive_does_not_publish_a_component(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            archive, _ = self.fixture(root)
            with self.assertRaisesRegex(ValueError, 'SHA-256'):
                component.stage(archive, root / 'destination', 'linux-amd64')
            self.assertFalse((root / 'destination').exists())

    def test_verified_named_files_only_and_no_existing_component_overwrite(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            archive, digest = self.fixture(root)
            with patch.dict(component.SHA256, {'linux-amd64': digest}):
                manifest = component.stage(archive, root / 'destination', 'linux-amd64')
                self.assertEqual(manifest['executable_sha256'], hashlib.sha256(b'fixture-executable').hexdigest())
                self.assertEqual({p.name for p in (root / 'destination').iterdir()},
                                 {'rclone', 'RCLONE-README.txt', 'rclone-component.json'})
                self.assertFalse((root / 'outside').exists())
                with self.assertRaises(FileExistsError):
                    component.stage(archive, root / 'destination', 'linux-amd64')

    def test_symlink_component_is_rejected_before_publish(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            archive, digest = self.fixture(root, symlink=True)
            with patch.dict(component.SHA256, {'linux-amd64': digest}), self.assertRaisesRegex(ValueError, 'member'):
                component.stage(archive, root / 'destination', 'linux-amd64')
            self.assertFalse((root / 'destination').exists())


if __name__ == '__main__':
    unittest.main()
