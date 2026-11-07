#!/usr/bin/env python3
"""Regression for the acceptance runner's false-success and timeout boundaries."""
import http.server
import importlib.util
from pathlib import Path
import socket
import threading
import time
import unittest

SPEC = importlib.util.spec_from_file_location('reliability', Path(__file__).with_name('reliability.py'))
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/slow-headers':
            try:
                for value in b'HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n':
                    self.wfile.write(bytes([value]))
                    self.wfile.flush()
                    time.sleep(.05)
            except OSError:
                pass
            return
        self.send_response(200)
        self.send_header('Content-Length', '100' if self.path == '/truncated' else '4')
        self.end_headers()
        try:
            for value in b'test':
                self.wfile.write(bytes([value]))
                self.wfile.flush()
                if self.path == '/slow':
                    time.sleep(.15)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass

    def log_message(self, *args):
        pass


class AcceptanceBoundaries(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join()

    def gateway(self, timeout=2):
        return RUNNER.Gateway(self.server.server_port, {'username': 'synthetic', 'password': 'synthetic'}, timeout)

    def test_truncated_success_response_is_failure(self):
        with self.assertRaisesRegex(AssertionError, 'truncated response'):
            self.gateway().request('GET', '/truncated')

    def test_slow_trickle_cannot_reset_whole_request_deadline(self):
        started = time.monotonic()
        with self.assertRaises((TimeoutError, socket.timeout)):
            self.gateway(.2).request('GET', '/slow')
        self.assertLess(time.monotonic()-started, 1)

    def test_status_mismatch_is_failure(self):
        with self.assertRaisesRegex(AssertionError, 'HTTP 200'):
            self.gateway().request('GET', '/', expected=(404,))

    def test_header_trickle_is_bounded(self):
        started = time.monotonic()
        with self.assertRaises((OSError, RUNNER.http.client.HTTPException, AssertionError)):
            self.gateway(.2).request('GET', '/slow-headers')
        self.assertLess(time.monotonic()-started, 1)

    def test_streamed_hash_and_length(self):
        response = self.gateway().request('GET', '/')
        self.assertEqual(response['bytes'], 4)
        self.assertEqual(response['sha256'], RUNNER.hashlib.sha256(b'test').hexdigest())


if __name__ == '__main__':
    unittest.main()
