"""Local lock tests only; no Apple account, credentials, engine or network."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('icloud_validation', Path(__file__).with_name('verify-icloud-desktop.py'))
validation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(validation)


class IdleAccountGuard(unittest.TestCase):
    def test_idle_lock_is_released_and_active_lock_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            path = state / 'auth.lock'
            path.write_bytes(b'x')
            validation.require_idle_account(state)
            with path.open('r+b') as held:
                if os.name == 'nt':
                    import msvcrt
                    msvcrt.locking(held.fileno(), msvcrt.LK_NBLCK, 1)
                else:
                    import fcntl
                    fcntl.flock(held.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                with self.assertRaisesRegex(RuntimeError, 'Account is in use'):
                    validation.require_idle_account(state)
                if os.name == 'nt':
                    msvcrt.locking(held.fileno(), msvcrt.LK_UNLCK, 1)
                else:
                    fcntl.flock(held.fileno(), fcntl.LOCK_UN)
            validation.require_idle_account(state)
            self.assertEqual(path.read_bytes(), b'x')


if __name__ == '__main__':
    unittest.main()
