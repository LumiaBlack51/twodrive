#!/usr/bin/env python3
"""Regression for the acceptance runner's false-success and timeout boundaries."""
import http.server
import importlib.util
import os
from pathlib import Path
import socket
import threading
import time
import unittest
from unittest import mock
import tempfile
from types import SimpleNamespace

SPEC = importlib.util.spec_from_file_location('reliability', os.environ.get(
    'TWODRIVE_TEST_RUNNER', str(Path(__file__).with_name('reliability.py'))))
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


class DelayedPutHandler(http.server.BaseHTTPRequestHandler):
    def do_PUT(self):
        remaining = int(self.headers['Content-Length'])
        while remaining:
            data = self.rfile.read(min(remaining, 65536))
            if not data:
                break
            remaining -= len(data)
        # Wait until the first GET has observed absence, then emulate a bug.
        self.server.first_get.wait(3)
        time.sleep(.25)
        if self.server.commit_partial:
            self.server.committed.set()

    def do_GET(self):
        visible = self.server.committed.is_set()
        self.send_response(200 if visible else 404)
        self.send_header('Content-Length', '0')
        self.end_headers()
        self.server.first_get.set()

    def log_message(self, *args):
        pass


class ReviewRegressions(unittest.TestCase):
    def test_absent_partial_uploads_have_bounded_observation(self):
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), DelayedPutHandler)
        server.first_get, server.committed = threading.Event(), threading.Event()
        server.commit_partial = False
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        results = []
        try:
            gateway = RUNNER.Gateway(server.server_port, {'username': 'test', 'password': 'test'}, 2)
            RUNNER.interrupted_uploads(gateway, '/generated', results)
            self.assertEqual(len(results), 3)
            for result in results:
                self.assertEqual(result['status'], 'passed')
                self.assertGreater(result['checks'], 1)
                self.assertGreaterEqual(result['observed_seconds'], 1)
                self.assertFalse(result['server_completion_confirmed'])
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    def test_confirmed_collection_is_cleaned_after_body_failure(self):
        calls, results = [], []
        def request(method, path, **kwargs):
            calls.append(method)
            return {'status': 201 if method == 'MKCOL' else 404}
        with tempfile.TemporaryDirectory() as tmp, \
                mock.patch.object(RUNNER, 'corpus', side_effect=ValueError('body failure')):
            with self.assertRaises(ValueError):
                RUNNER.exercise(SimpleNamespace(request=request), Path(tmp),
                                SimpleNamespace(faults=False, full=False, small_count=0), results)
        self.assertEqual(calls, ['MKCOL', 'DELETE', 'GET'])
        self.assertEqual(results[-1]['status'], 'passed')

    def test_cleanup_failure_preserves_primary_error_and_is_recorded(self):
        results = []
        def request(method, path, **kwargs):
            if method == 'DELETE': raise OSError('synthetic cleanup failure')
            return {'status': 201}
        with tempfile.TemporaryDirectory() as tmp, \
                mock.patch.object(RUNNER, 'corpus', side_effect=ValueError('body failure')):
            with self.assertRaises(ValueError):
                RUNNER.exercise(SimpleNamespace(request=request), Path(tmp),
                                SimpleNamespace(faults=False, full=False, small_count=0), results)
        self.assertEqual(results[-1]['status'], 'failed')
        self.assertEqual(results[-1]['error_type'], 'OSError')

    def test_cleanup_failure_fails_otherwise_successful_exercise(self):
        results = []
        def request(method, path, **kwargs):
            if method == 'DELETE': raise OSError('synthetic cleanup failure')
            return {'status': 201, 'preview': b'multistatus'}
        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(RUNNER, 'corpus', return_value=[]):
            with self.assertRaises(OSError):
                RUNNER.exercise(SimpleNamespace(request=request), Path(tmp),
                                SimpleNamespace(faults=False, full=False, small_count=0), results)
        self.assertEqual(results[-1]['status'], 'failed')

    def test_delayed_partial_commit_is_rejected(self):
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), DelayedPutHandler)
        server.first_get, server.committed = threading.Event(), threading.Event()
        server.commit_partial = True
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            gateway = RUNNER.Gateway(server.server_port, {'username': 'test', 'password': 'test'}, 2)
            with self.assertRaises(AssertionError):
                RUNNER.interrupted_uploads(gateway, '/generated', [])
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    def test_residual_shared_file_fails_attempt(self):
        class Process:
            pid = 0
            def poll(self): return None
            def terminate(self): pass
            def wait(self, timeout): pass
        def leave_file(gateway, directory, args, results):
            (directory.parent/'share'/'unexpected.tmp').write_bytes(b'residual')
        args = SimpleNamespace(gateway_port=None, binary=Path('unused.exe'), network='lan',
                               timeout=1, faults=False)
        with tempfile.TemporaryDirectory() as tmp, \
                mock.patch.object(RUNNER.subprocess, 'Popen', return_value=Process()), \
                mock.patch.object(RUNNER, 'wait', return_value={}), \
                mock.patch.object(RUNNER, 'Resources'), \
                mock.patch.object(RUNNER, 'Gateway'), \
                mock.patch.object(RUNNER, 'exercise', side_effect=leave_file):
            result = RUNNER.run_attempt(args, Path(tmp), [])
            self.assertEqual(result['shared_files_remaining'], 1)
            self.assertEqual(result['status'], 'failed')
            self.assertTrue((Path(tmp)/'share'/'unexpected.tmp').exists())

    def test_lost_mkcol_response_records_pending_without_delete(self):
        calls, results = [], []
        def request(method, path, **kwargs):
            calls.append((method, path))
            if method == 'MKCOL':
                raise TimeoutError('synthetic lost reply after create')
            raise AssertionError('must not probe or delete unconfirmed ownership')
        gateway = SimpleNamespace(request=request)
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(TimeoutError):
                RUNNER.exercise(gateway, Path(tmp), SimpleNamespace(), results)
        self.assertEqual(len(calls), 1)
        self.assertEqual(len(results), 1)
        self.assertEqual(results[0]['status'], 'pending')
        self.assertEqual(results[0]['collection'], calls[0][1])
        self.assertEqual(results[0]['ownership'], 'unconfirmed')


if __name__ == '__main__':
    unittest.main()
