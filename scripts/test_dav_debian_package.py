"""Synthetic staging and tar-stream checks; no manual .deb assembly or install."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('package_check', ROOT / 'scripts/test-debian-package.py')
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)
HELPER = 'rclone-mount'
LICENSE = 'RCLONE-MOUNT-LICENSE.txt'
RUNTIME = 'usr/lib/twodrive-dav/'
DOC = 'usr/share/doc/twodrive-dav/'
BASE = ('twodrive_dav', 'twodrive-dav-engine', 'twodrive-dev', 'twodrive-provider-login',
        'rclone', 'rclone-component.json', 'lib/libapp.so', 'lib/libflutter_linux_gtk.so',
        'data/icudtl.dat')


def bundle_fixture(bundle):
    for name in (*BASE, 'data/flutter_assets/assets/app-icon.svg', 'docs/test.txt', 'RCLONE-README.txt'):
        path = bundle / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b'synthetic ' + name.encode())
        path.chmod(0o755 if '/' not in name else 0o644)
    (bundle / 'rclone-component.json').write_text(json.dumps({
        'platform': 'linux-amd64',
        'executable_sha256': hashlib.sha256((bundle / 'rclone').read_bytes()).hexdigest(),
    }))


def add_helper(bundle):
    (bundle / HELPER).write_bytes(b'synthetic executable helper')
    (bundle / HELPER).chmod(0o755)
    (bundle / LICENSE).write_bytes(b'synthetic helper license\n' * 200)


def tar_stream(entries):
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode='w') as archive:
        for name, (content, mode) in entries.items():
            item = tarfile.TarInfo('./' + name)
            item.size = len(content)
            item.mode = mode
            archive.addfile(item, io.BytesIO(content))
    return data.getvalue()


class TarProcess:
    def __init__(self, data):
        self.stdout = io.BytesIO(data)

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.stdout.close()

    def wait(self):
        return 0


class DavPackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bundle = self.root / 'bundle'
        bundle_fixture(self.bundle)
        self.package = self.root / 'synthetic.deb'
        self.package.write_bytes(b'not a real deb; dpkg output mocked')
        digest = hashlib.sha256(self.package.read_bytes()).hexdigest()
        (self.root / 'synthetic.changes').write_text(
            f'Checksums-Sha256:\n {digest} {self.package.stat().st_size} {self.package.name}\n')
        self.entries = {RUNTIME + name: ((self.bundle / name).read_bytes(), 0o755) for name in BASE}
        for name in ('usr/bin/twodrive-dav', 'usr/share/icons/hicolor/scalable/apps/twodrive-dav.svg',
                     'usr/share/applications/app.twodrive.twodrive_dav_packaged.desktop',
                     'usr/share/gnome-shell/extensions/twodrive-tray-anchor@twodrive/extension.js'):
            self.entries[name] = (b'synthetic', 0o755)

    def pair(self):
        add_helper(self.bundle)
        self.entries[RUNTIME + HELPER] = ((self.bundle / HELPER).read_bytes(), 0o755)
        self.entries[DOC + LICENSE] = ((self.bundle / LICENSE).read_bytes(), 0o644)

    def verify(self):
        def output(args, **kwargs):
            if '--ctrl-tarfile' in args:
                return tar_stream({})
            return {'Package': 'twodrive-dav', 'Architecture': 'amd64',
                    'Depends': 'libc6, libgtk-3-0, fuse3'}[args[-1]]
        with patch.object(checker.subprocess, 'check_output', side_effect=output), \
             patch.object(checker.subprocess, 'Popen', return_value=TarProcess(tar_stream(self.entries))):
            checker.verify_dav(self.package, self.bundle)

    def test_legacy_bundle_without_helper(self):
        self.verify()

    def test_complete_pair_and_compressed_license(self):
        self.pair()
        self.verify()
        import gzip
        content, mode = self.entries.pop(DOC + LICENSE)
        self.entries[DOC + LICENSE + '.gz'] = (gzip.compress(content), mode)
        self.verify()

    def test_source_pair_missing_one_member(self):
        self.pair()
        for name in (HELPER, LICENSE):
            with self.subTest(name=name):
                path = self.bundle / name
                content, mode = path.read_bytes(), path.stat().st_mode
                path.unlink()
                with self.assertRaises(AssertionError):
                    self.verify()
                path.write_bytes(content)
                path.chmod(mode)

    def test_source_helper_not_executable(self):
        self.pair()
        (self.bundle / HELPER).chmod(0o644)
        with self.assertRaises(AssertionError):
            self.verify()

    def test_archive_omits_one_or_both_source_members(self):
        self.pair()
        original = self.entries.copy()
        for omitted in ((RUNTIME + HELPER,), (DOC + LICENSE,), (RUNTIME + HELPER, DOC + LICENSE)):
            with self.subTest(omitted=omitted):
                self.entries = {k: v for k, v in original.items() if k not in omitted}
                with self.assertRaises(AssertionError):
                    self.verify()

    def test_archive_helper_not_executable_or_wrong_bytes(self):
        self.pair()
        for name, payload in ((RUNTIME + HELPER, (b'synthetic executable helper', 0o644)),
                              (RUNTIME + HELPER, (b'wrong helper', 0o755)),
                              (DOC + LICENSE, (b'wrong license', 0o644))):
            with self.subTest(name=name, payload=payload):
                original = self.entries[name]
                self.entries[name] = payload
                with self.assertRaises(AssertionError):
                    self.verify()
                self.entries[name] = original

    def test_archive_unexpected_pair_without_source(self):
        self.pair()
        (self.bundle / HELPER).unlink()
        (self.bundle / LICENSE).unlink()
        with self.assertRaises(AssertionError):
            self.verify()

    def stage(self):
        # Stop at the real build boundary and capture the generated manifests.
        # This tests staging only; actual dpkg-buildpackage is validated separately.
        bin_dir = self.root / 'bin'
        bin_dir.mkdir(exist_ok=True)
        stub = bin_dir / 'dpkg-buildpackage'
        stub.write_text('#!/bin/sh\ncp debian/twodrive-dav.install "$CAPTURE/install"\n'
                        'cp debian/twodrive-dav.docs "$CAPTURE/docs"\nexit 86\n')
        stub.chmod(0o755)
        return subprocess.run(['bash', str(ROOT / 'scripts/build-dav-deb.sh'), str(self.bundle)],
                              env={**os.environ, 'PATH': str(bin_dir) + ':' + os.environ['PATH'],
                                   'CAPTURE': str(self.root)}, capture_output=True, text=True)

    def test_staging_legacy_and_complete_pair(self):
        self.assertEqual(self.stage().returncode, 86)
        self.assertNotIn(HELPER, (self.root / 'install').read_text())
        add_helper(self.bundle)
        self.assertEqual(self.stage().returncode, 86)
        self.assertIn('bundle/rclone-mount usr/lib/twodrive-dav', (self.root / 'install').read_text())
        self.assertIn('bundle/RCLONE-MOUNT-LICENSE.txt', (self.root / 'docs').read_text())

    def test_staging_rejects_invalid_source_before_build(self):
        for missing in (HELPER, LICENSE, None):
            with self.subTest(missing=missing):
                add_helper(self.bundle)
                if missing:
                    (self.bundle / missing).unlink()
                else:
                    (self.bundle / HELPER).chmod(0o644)
                self.assertEqual(self.stage().returncode, 1)
                self.assertFalse((self.root / 'install').exists())


if __name__ == '__main__':
    unittest.main()
