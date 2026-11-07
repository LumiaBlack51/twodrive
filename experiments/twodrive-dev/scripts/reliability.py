#!/usr/bin/env python3
"""Disposable CLI/gateway regression. Local pairs never establish cross-NAT evidence.

Remote mode consumes an already paired loopback gateway, never exports credentials,
and confines mutations to a freshly created random collection. Run on both devices
with their roles reversed for bidirectional evidence. No automatic mutation retry.
"""
import argparse
import base64
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import threading
import time
from urllib.parse import quote
import uuid

CHUNK = bytes(range(256)) * 4096


class Resources:
    """One-second Windows backend samples; absence is explicit on other OSes."""
    def __init__(self, processes):
        self.samples = []
        self.stopped = threading.Event()
        self.processes = processes
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        if os.name != 'nt':
            return
        import ctypes as c
        from ctypes import wintypes as w

        class Memory(c.Structure):
            _fields_ = [('cb', w.DWORD), ('faults', w.DWORD)] + [
                (name, c.c_size_t) for name in ('peak_rss', 'rss', 'peak_pool_paged',
                'pool_paged', 'peak_pool_nonpaged', 'pool_nonpaged', 'pagefile', 'peak_pagefile')]

        kernel = c.WinDLL('kernel32', use_last_error=True)
        psapi = c.WinDLL('psapi', use_last_error=True)
        kernel.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]
        kernel.OpenProcess.restype = w.HANDLE
        kernel.CloseHandle.argtypes = [w.HANDLE]
        kernel.GetProcessTimes.argtypes = [w.HANDLE] + [c.POINTER(w.FILETIME)] * 4
        psapi.GetProcessMemoryInfo.argtypes = [w.HANDLE, c.POINTER(Memory), w.DWORD]
        handles = [kernel.OpenProcess(0x410, False, p.pid) for p in self.processes]
        started = time.monotonic()
        try:
            while not self.stopped.is_set():
                for index, handle in enumerate(handles):
                    memory = Memory()
                    memory.cb = c.sizeof(memory)
                    times = [w.FILETIME() for _ in range(4)]
                    if handle and psapi.GetProcessMemoryInfo(handle, c.byref(memory), memory.cb) and kernel.GetProcessTimes(handle, *[c.byref(t) for t in times]):
                        cpu = sum((t.dwHighDateTime << 32) + t.dwLowDateTime for t in times[2:]) / 1e7
                        self.samples.append({'seconds': round(time.monotonic()-started, 3),
                                             'role': ('server', 'client')[index],
                                             'cpu_seconds': cpu, 'rss_bytes': memory.rss,
                                             'peak_rss_bytes': memory.peak_rss})
                self.stopped.wait(1)
        finally:
            for handle in handles:
                if handle:
                    kernel.CloseHandle(handle)

    def close(self):
        self.stopped.set()
        self.thread.join(timeout=5)
        return self.samples


def digest_file(path):
    digest = hashlib.sha256()
    size = 0
    with path.open('rb') as stream:
        while data := stream.read(len(CHUNK)):
            digest.update(data)
            size += len(data)
    return size, digest.hexdigest()


class Gateway:
    def __init__(self, port, credentials, timeout):
        self.port, self.timeout = port, timeout
        self.auth = 'Basic ' + base64.b64encode(
            f"{credentials['username']}:{credentials['password']}".encode()).decode()

    def request(self, method, path, source=None, headers=None, expected=(200,), output=None):
        started = time.monotonic()
        deadline = started + self.timeout
        connection = http.client.HTTPConnection('127.0.0.1', self.port, timeout=self.timeout)
        digest, count, preview = hashlib.sha256(), 0, bytearray()
        timer = None
        expired = threading.Event()

        def budget():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError('request deadline')
            if connection.sock:
                connection.sock.settimeout(remaining)

        try:
            connection.connect()
            transport_socket = connection.sock

            def expire():
                expired.set()
                try:
                    transport_socket.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

            timer = threading.Timer(max(0, deadline-time.monotonic()), expire)
            timer.daemon = True
            timer.start()
            connection.putrequest(method, quote(path, safe='/'))
            connection.putheader('Authorization', self.auth)
            connection.putheader('Content-Length', str(source.stat().st_size if source else 0))
            for key, value in (headers or {}).items():
                connection.putheader(key, value)
            connection.endheaders()
            if source:
                with source.open('rb') as stream:
                    while data := stream.read(len(CHUNK)):
                        budget()
                        connection.send(data)
            budget()
            response = connection.getresponse()
            status = response.status
            while True:
                budget()
                data = response.read1(len(CHUNK))
                if not data:
                    break
                digest.update(data)
                count += len(data)
                if len(preview) < 65536:
                    preview.extend(data[:65536-len(preview)])
                if output:
                    output.write(data)
            # http.client.read1 may reach EOF without enforcing Content-Length.
            if expired.is_set():
                raise TimeoutError('request deadline')
            length = response.getheader('Content-Length')
            if method != 'HEAD' and length is not None and count != int(length):
                raise AssertionError('truncated response')
            if status not in expected:
                raise AssertionError(f'HTTP {status}; expected {expected}')
            if expired.is_set():
                raise TimeoutError('request deadline')
            return {'status': status, 'bytes': count, 'sha256': digest.hexdigest(),
                    'seconds': round(time.monotonic()-started, 6), 'preview': bytes(preview)}
        except (OSError, http.client.HTTPException):
            if expired.is_set():
                raise TimeoutError('request deadline') from None
            raise
        finally:
            if timer:
                timer.cancel()
            connection.close()


def wait(check, processes, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if any(p.poll() is not None for p in processes):
            raise RuntimeError('CLI exited before readiness')
        try:
            result = check()
            if result:
                return result
        except (OSError, ValueError, http.client.HTTPException):
            pass
        time.sleep(.1)
    raise TimeoutError('CLI readiness deadline')


def corpus(directory, full, count):
    entries = [('empty.bin', 0), ('中文 空格/' + 'long-' * 25 + '.bin', 4099),
               ('nested/a/b/small.bin', 65537)]
    entries += [(f'many/{i:04}.bin', 257 + i % 31) for i in range(count)]
    if full:
        entries += [('100MiB.bin', 100*1024**2), ('1GiB.bin', 1024**3)]
    for name, size in entries:
        path = directory / name
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open('wb') as stream:
            left = size
            while left:
                data = CHUNK[:min(left, len(CHUNK))]
                stream.write(data)
                left -= len(data)
        yield name, path


def interrupted_uploads(gateway, prefix, results):
    """Close an incomplete HTTP PUT at 0%, 50%, 99%; never truncate an existing object."""
    size = 4 * 1024**2
    for percent in (0, 50, 99):
        path = prefix + f'/interrupted-{percent}.bin'
        connection = http.client.HTTPConnection('127.0.0.1', gateway.port, timeout=gateway.timeout)
        try:
            connection.putrequest('PUT', path)
            connection.putheader('Authorization', gateway.auth)
            connection.putheader('Content-Length', str(size))
            connection.putheader('If-None-Match', '*')
            connection.endheaders()
            left = size * percent // 100
            while left:
                data = CHUNK[:min(left, len(CHUNK))]
                connection.send(data)
                left -= len(data)
        finally:
            connection.close()
        # A partial upload must never become a committed visible file.
        response = gateway.request('GET', path, expected=(404,))
        results.append({'case': f'interrupted-put-{percent}-percent',
                        'status': 'passed', 'visible_after_abort': response['status']})


def exercise(gateway, directory, args, results):
    prefix = '/td-reliability-' + uuid.uuid4().hex
    gateway.request('MKCOL', prefix, expected=(201,))
    created = {prefix}
    cleanup_ok = False
    try:
        if args.faults:
            interrupted_uploads(gateway, prefix, results)
        for name, source in corpus(directory, args.full, args.small_count):
            result = {'file': name, 'status': 'failed', 'stage': 'prepare'}
            results.append(result)
            path = prefix + '/' + name
            parent = path.rsplit('/', 1)[0]
            missing = []
            while parent not in created:
                missing.append(parent)
                parent = parent.rsplit('/', 1)[0]
            for parent in reversed(missing):
                gateway.request('MKCOL', parent, expected=(201,))
                created.add(parent)
            size, sha = digest_file(source)
            result.update(size=size, sha256=sha, stage='upload')
            if size >= 100*1024**2:
                print(f'file={name} stage=upload bytes={size}', flush=True)
            put = gateway.request('PUT', path, source, {'If-None-Match': '*'}, (201, 204))
            result.update(upload_seconds=put['seconds'], stage='download')
            if size >= 100*1024**2:
                print(f'file={name} stage=download', flush=True)
            get = gateway.request('GET', path)
            assert (get['bytes'], get['sha256']) == (size, sha), 'size/SHA-256 mismatch'
            result.update({'download_sha256': get['sha256'],
                      'upload_seconds': put['seconds'], 'download_seconds': get['seconds'],
                      'download_MiB_s': round(size / max(get['seconds'], 1e-9) / 1024**2, 3), 'stage': 'range'})
            if size:
                with source.open('rb') as stream:
                    expected = stream.read(min(size, 1024))
                ranged = gateway.request('GET', path, headers={'Range': f'bytes=0-{len(expected)-1}'}, expected=(206,))
                assert (ranged['bytes'], ranged['sha256']) == (len(expected), hashlib.sha256(expected).hexdigest())
                result['range'] = 'passed'
            if name == 'empty.bin':
                gateway.request('PUT', path, source, {'If-None-Match': '*'}, (412,))
                result['create_collision'] = 'passed'
            moved = path + '.moved'
            result['stage'] = 'move'
            gateway.request('MOVE', path, headers={'Destination': f'http://127.0.0.1:{gateway.port}' + quote(moved, safe='/'), 'Overwrite': 'F'}, expected=(201, 204))
            moved_get = gateway.request('GET', moved)
            assert (moved_get['bytes'], moved_get['sha256']) == (size, sha)
            gateway.request('GET', path, expected=(404,))
            gateway.request('DELETE', moved, expected=(200, 204))
            gateway.request('GET', moved, expected=(404,))
            result.update(status='passed', stage='complete')
            source.unlink()
        listing = gateway.request('PROPFIND', prefix, headers={'Depth': '1'}, expected=(207,))
        assert b'multistatus' in listing['preview']
    finally:
        try:
            gateway.request('DELETE', prefix, expected=(200, 204))
            gateway.request('GET', prefix, expected=(404,))
            cleanup_ok = True
        finally:
            results.append({'case': 'owned_remote_collection_cleanup', 'status': 'passed' if cleanup_ok else 'failed'})


def run_attempt(args, directory, results):
    processes, logs = [], []
    resources = None
    env = {key: value for key, value in os.environ.items()
           if key.lower() not in ('http_proxy', 'https_proxy', 'all_proxy', 'no_proxy')}
    entry = {'status': 'failed', 'files': results, 'path_coverage': 'initial-only',
             'whole_transfer_transport': 'unverified'}
    started = time.monotonic()
    try:
        if args.gateway_port:
            gateway = Gateway(args.gateway_port, json.loads(args.credentials.read_text()), args.timeout)
            entry['path_coverage'] = 'not-observed'
        else:
            root = directory / 'share'
            root.mkdir()
            ticket = directory / 'invitation.json'
            with socket.socket() as sock:
                sock.bind(('127.0.0.1', 0))
                port = sock.getsockname()[1]
            network = {'lan': ['--no-relay'], 'public': [], 'relay-only': ['--relay-only']}[args.network]
            for name in ('server', 'client'):
                log = (directory / (name + '.log')).open('w+')
                logs.append(log)
                command = [str(args.binary), '--state', str(directory/name)]
                if name == 'server':
                    command += ['serve', '--root', str(root), '--write', '--invite-file', str(ticket)]
                else:
                    command += ['connect', '--ticket-file', str(ticket), '--listen', f'127.0.0.1:{port}']
                processes.append(subprocess.Popen(command + network, env=env, stdout=log, stderr=log))
                if name == 'server':
                    wait(lambda: ticket.exists() and json.loads(ticket.read_text()), processes, args.timeout)
            resources = Resources(processes)
            credentials = wait(lambda: json.loads((directory/'client/credentials.json').read_text()), processes, args.timeout)
            gateway = Gateway(port, credentials, args.timeout)
            # Credentials can be written before the listener is ready.
            wait(lambda: gateway.request('PROPFIND', '/', headers={'Depth': '0'}, expected=(207,)), processes, args.timeout)
            entry['connect_seconds'] = round(time.monotonic()-started, 6)
        exercise(gateway, directory/'corpus', args, results)
        if args.faults and processes:
            processes[0].terminate()
            processes[0].wait(timeout=10)
            failure_started = time.monotonic()
            # A stopped peer must not turn into an HTTP success from the gateway.
            try:
                response = gateway.request('GET', '/', expected=(502, 503, 504))
                observed = f"HTTP-{response['status']}"
            except (OSError, http.client.HTTPException) as error:
                observed = type(error).__name__
            results.append({'case': 'server-stopped-request', 'status': 'passed',
                            'observed': observed, 'seconds': round(time.monotonic()-failure_started, 3),
                            'scope': 'bounded-client-observation; not backend-internal-deadline-proof'})
        entry['status'] = 'passed'
    except Exception as error:
        # Do not serialize exceptions containing URLs, secrets or personal paths.
        entry['error_type'] = type(error).__name__
        if isinstance(error, AssertionError):
            entry['assertion'] = str(error)
    finally:
        if resources:
            entry['resource_samples'] = resources.close()
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
        for process in processes:
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)
        for log in logs:
            log.close()
        if processes:
            client_log = directory/'client.log'
            text = client_log.read_text(errors='replace') if client_log.exists() else ''
            entry['initial_transports'] = re.findall(r'\btransport=(direct-udp|relay|negotiating)\b', text)
            entry['owned_processes_stopped'] = all(p.poll() is not None for p in processes)
            entry['shared_files_remaining'] = sum(p.is_file() for p in (directory/'share').rglob('*'))
            if args.network == 'relay-only' and entry['initial_transports'] != ['relay']:
                entry['status'] = 'failed'
                entry['assertion'] = 'forced relay initial path not confirmed'
        entry['seconds'] = round(time.monotonic()-started, 6)
    return entry


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument('--binary', type=Path)
    source.add_argument('--gateway-port', type=int)
    parser.add_argument('--credentials', type=Path)
    parser.add_argument('--network', choices=('lan', 'public', 'relay-only'), default='lan')
    parser.add_argument('--attempts', type=int, default=5)
    parser.add_argument('--small-count', type=int, default=1000)
    parser.add_argument('--full', action='store_true', help='add 100 MiB and 1 GiB per attempt')
    parser.add_argument('--faults', action='store_true', help='incomplete PUT at 0/50/99 percent; local peer stop')
    parser.add_argument('--timeout', type=float, default=120, help='whole HTTP operation deadline in seconds')
    parser.add_argument('--soak-seconds', type=float, default=0, help='repeat complete rounds for at least this duration')
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    if args.attempts < 1 or args.small_count < 0 or args.timeout <= 0 or args.soak_seconds < 0:
        parser.error('invalid count or deadline')
    if args.gateway_port is not None and (not args.credentials or not 1 <= args.gateway_port <= 65535):
        parser.error('gateway requires a valid loopback port and private credentials file')
    if args.binary:
        args.binary = args.binary.resolve(strict=True)
    report = {'schema': 1, 'scope': 'same-host-two-processes' if args.binary else 'existing-gateway',
              'independent_nat_established': False, 'network_mode': args.network if args.binary else 'externally-managed',
              'continuous_path_evidence': False, 'attempts': [], 'resume_supported': False,
              'cpu_memory_sampling': '1s-Windows-owned-backends' if args.binary and os.name == 'nt' else 'not-collected',
              'started_utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}
    if args.binary:
        report['binary_sha256'] = digest_file(args.binary)[1]
    args.report.parent.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    while len(report['attempts']) < args.attempts or time.monotonic()-started < args.soak_seconds:
        with tempfile.TemporaryDirectory(prefix='td-reliability-') as directory:
            report['attempts'].append(run_attempt(args, Path(directory), []))
        report['seconds'] = round(time.monotonic()-started, 6)
        report['passed'] = sum(a['status'] == 'passed' for a in report['attempts'])
        report['failed'] = len(report['attempts']) - report['passed']
        report['soak_duration_met'] = args.soak_seconds > 0 and report['seconds'] >= args.soak_seconds
        temporary = args.report.with_suffix('.tmp')
        temporary.write_text(json.dumps(report, ensure_ascii=False, indent=2)+'\n', encoding='utf-8')
        temporary.replace(args.report)
        print(f"round={len(report['attempts'])} status={report['attempts'][-1]['status']}", flush=True)
        if report['failed']:
            break
    return 1 if report['failed'] else 0


if __name__ == '__main__':
    raise SystemExit(main())
