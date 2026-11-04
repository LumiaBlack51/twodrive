"""Windows console/IPC boundary, using a deliberately invalid state before authentication."""
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest


@unittest.skipUnless(os.name == 'nt' and os.environ.get('TWODRIVE_TEST_PROVIDER_LOGIN'),
                     'requires a built Windows login helper')
class LoginConsoleTests(unittest.TestCase):
    def test_human_console_survives_without_inheriting_parent_ipc(self):
        with tempfile.TemporaryDirectory(prefix='td-console-check-') as tmp:
            invalid_state = Path(tmp) / 'regular-file'
            invalid_state.write_bytes(b'not-a-directory')
            child = subprocess.Popen([os.environ['TWODRIVE_TEST_PROVIDER_LOGIN'],
                '--interactive-console', '--state', str(invalid_state)],
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                creationflags=subprocess.CREATE_NEW_CONSOLE)
            try:
                time.sleep(1)
                self.assertIsNone(child.poll(), 'The human console should retain the readable failure and wait for Enter')
            finally:
                if child.poll() is None:
                    child.terminate()
                stdout, stderr = child.communicate(timeout=10)
            self.assertEqual(stdout, b'', 'Authentication helper must never write to engine IPC stdout')
            self.assertEqual(stderr, b'', 'Authentication helper must never write to engine IPC stderr')
            self.assertEqual(invalid_state.read_bytes(), b'not-a-directory')


if __name__ == '__main__':
    unittest.main()
