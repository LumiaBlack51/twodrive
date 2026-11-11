"""Native MakeAppx checks; fixture Store identities never leave temporary output.

TWODRIVE_TEST_MSIX_BUNDLE enables real packaging in CI. These checks do not
install, sign or submit an app, and cannot verify ownership in Partner Center.
"""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET
import zipfile


ROOT = Path(__file__).resolve().parent.parent
FOUNDATION = '{http://schemas.microsoft.com/appx/manifest/foundation/windows10}'
OID = '2.25.311729368913984317654407730594956997722'


@unittest.skipUnless(os.name == 'nt', 'native Windows SDK packaging')
class MsixPackagingTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='td-msix-policy-')
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.bundle = self.directory / 'bundle'
        # Invalid-identity cases must stop before these intentionally fake files
        # can reach SDK packing or image generation.
        for name in ('twodrive_dav.exe', 'twodrive-dev.exe',
                     'twodrive-dav-engine.exe', 'twodrive-native-dev.exe', 'flutter_windows.dll', 'data/icudtl.dat'):
            file = self.bundle / name
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_bytes(b'not an executable')
        (self.bundle / 'data/flutter_assets').mkdir()

    def invoke(self, *arguments, bundle=None, output=None):
        environment = os.environ.copy()
        # CI runs pwsh -> Python -> Windows PowerShell 5.1. Unlike a direct
        # pwsh -> powershell.exe launch, that intermediate process preserves
        # PS7 module paths; Utility can then resolve to an incompatible module.
        # Let Windows PowerShell construct its own standard module path.
        # Python exposes Windows environment keys in uppercase; a copied dict
        # no longer has os.environ's case-insensitive lookup behavior.
        environment = {k: v for k, v in environment.items() if k.upper() != 'PSMODULEPATH'}
        return subprocess.run([
            shutil.which('powershell.exe'), '-NoProfile', '-NonInteractive',
            '-File', str(ROOT / 'scripts/build-msix.ps1'),
            '-Bundle', str(bundle or self.bundle),
            '-OutputDirectory', str(output or self.directory / 'output'),
            *arguments,
        ], env=environment, capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=120)

    def test_missing_native_helper_is_rejected_before_packing(self):
        (self.bundle / 'twodrive-native-dev.exe').unlink()
        result = self.invoke('-TestPackage')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Incomplete Windows bundle: twodrive-native-dev.exe', result.stdout + result.stderr)

    def test_store_rejects_test_name_or_reserved_publisher(self):
        for name, publisher in (
            ('TwoDrive.Development.Test', 'CN=Fixture'),
            ('twodrive.development.test', 'CN=Fixture'),
            ('Fixture.Store', f'CN=Fixture, OID.{OID}=1'),
            ('Fixture.Store', f'CN=Fixture, oid.{OID} = 1'),
            ('Fixture.Store', f'CN=Fixture, {OID}=1'),
        ):
            with self.subTest(name=name, publisher=publisher):
                result = self.invoke('-IdentityName', name, '-Publisher', publisher,
                                     '-PublisherDisplayName', 'Fixture')
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('reserved unsigned test identity/Publisher', result.stderr)
                self.assertFalse((self.directory / 'output').exists())

    def test_test_mode_rejects_store_identity_arguments(self):
        result = self.invoke('-TestPackage', '-IdentityName', 'Fixture.Store')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('TestPackage cannot use a Store identity', result.stderr)

    def test_real_sdk_packages_preserve_payload_and_channel(self):
        source = os.environ.get('TWODRIVE_TEST_MSIX_BUNDLE')
        if not source:
            self.skipTest('set TWODRIVE_TEST_MSIX_BUNDLE to the complete Windows bundle')
        source = Path(source)
        # Synthetic account data exercises the production path, not Store
        # acceptance. Nothing from this fixture is uploaded as an artifact.
        for mode, arguments, expected_name, expected_publisher in (
            ('test', ['-TestPackage'], 'TwoDrive.Development.Test',
             f'CN=TwoDrive Development Test, OID.{OID}=1'),
            ('store-fixture', ['-IdentityName', 'Fixture.Store', '-Publisher', 'CN=Fixture',
                               '-PublisherDisplayName', 'Fixture & Contributors'],
             'Fixture.Store', 'CN=Fixture'),
        ):
            with self.subTest(mode=mode):
                output = self.directory / mode
                result = self.invoke(*arguments, bundle=source, output=output)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                packages = list(output.glob('*.msix'))
                self.assertEqual(len(packages), 1)
                package = packages[0]
                checksum = package.with_suffix('.msix.sha256').read_text().split()[0]
                self.assertEqual(hashlib.sha256(package.read_bytes()).hexdigest(), checksum)
                with zipfile.ZipFile(package) as archive:
                    names = set(archive.namelist())
                    manifest = ET.fromstring(archive.read('AppxManifest.xml'))
                    identity = manifest.find(FOUNDATION + 'Identity')
                    self.assertEqual(identity.get('Name'), expected_name)
                    self.assertEqual(identity.get('Publisher'), expected_publisher)
                    self.assertRegex(identity.get('Version'), r'^[1-9]\d*\.\d+\.\d+\.0$')
                    self.assertEqual(identity.get('ProcessorArchitecture'), 'x64')
                    languages = [r.get('Language') for r in manifest.find(FOUNDATION + 'Resources')]
                    self.assertEqual(languages, ['zh-cn'])
                    self.assertIn('distribution-msix', names)
                    self.assertIn('LICENSE', names)
                    self.assertNotIn('AppxSignature.p7x', names)
                    self.assertNotIn('INSTALL.md', names)
                    self.assertNotIn('install-desktop-windows.ps1', names)
                    for file in source.rglob('*'):
                        if file.is_file() and file.relative_to(source).as_posix() not in (
                            'INSTALL.md', 'install-desktop-windows.ps1',
                        ):
                            relative = file.relative_to(source).as_posix()
                            self.assertEqual(archive.read(relative), file.read_bytes(), relative)
                    if mode == 'store-fixture':
                        display = manifest.find(FOUNDATION + 'Properties/' + FOUNDATION + 'PublisherDisplayName')
                        self.assertEqual(display.text, 'Fixture & Contributors')


if __name__ == '__main__':
    unittest.main()
