import importlib.util
from pathlib import Path
import tempfile
import unittest


spec = importlib.util.spec_from_file_location('local_package_locks', Path(__file__).with_name('check-local-package-locks.py'))
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


class LocalPackageLocks(unittest.TestCase):
    def test_independent_stale_lock_is_rejected_even_when_root_lock_matches(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / 'Cargo.toml'
            manifest.write_text('[package]\nname="shared-core"\nversion="2.0.0"\n')
            current = root / 'Cargo.lock'
            current.write_text('[[package]]\nname="shared-core"\nversion="2.0.0"\n')
            independent = root / 'experiment.lock'
            independent.write_text('[[package]]\nname="shared-core"\nversion="1.0.0"\n')
            with self.assertRaisesRegex(ValueError, 'experiment.lock.*shared-core locks 1.0.0'):
                checker.verify([manifest], [current, independent])
            independent.write_text(current.read_text())
            checker.verify([manifest], [current, independent])

    def test_registry_package_versions_are_not_local_path_versions(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / 'Cargo.toml'
            manifest.write_text('[package]\nname="shared-core"\nversion="2.0.0"\n')
            lock = root / 'Cargo.lock'
            lock.write_text('[[package]]\nname="shared-core"\nversion="1.0.0"\nsource="registry+https://github.com/rust-lang/crates.io-index"\n')
            checker.verify([manifest], [lock])


if __name__ == '__main__':
    unittest.main()
