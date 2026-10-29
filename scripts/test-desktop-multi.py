#!/usr/bin/env python3
"""Two disposable loopback accounts, a gated transfer and real engine IPC.

No cloud account, GUI input, installed service or user state is accessed.
"""
import argparse
import importlib.util
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import queue
import tempfile
import threading
import time

spec = importlib.util.spec_from_file_location('desktop', Path(__file__).with_name('test-dav-desktop.py'))
desktop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(desktop)


def run(args):
    started, release = threading.Event(), threading.Event()
    list_hold, list_started, list_release = threading.Event(), threading.Event(), threading.Event()
    servers = []
    payloads = {'a': b'account-a-content', 'b': b'account-b-content'}

    def handler(account):
        data = payloads[account]

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_PROPFIND(self):
                if account == 'b' and list_hold.is_set():
                    list_started.set()
                    if not list_release.wait(20):
                        return
                root = self.path.rstrip('/') == ''
                collection = '<d:response><d:href>/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>' if root else ''
                body = (f'<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">{collection}'
                        f'<d:response><d:href>/same.txt</d:href><d:propstat><d:prop>'
                        f'<d:resourcetype/><d:getcontentlength>{len(data)}</d:getcontentlength>'
                        f'<d:getetag>"{account}-version"</d:getetag></d:prop>'
                        '<d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>').encode()
                self.send_response(207)
                self.send_header('Content-Length', str(len(body)))
                self.send_header('Content-Type', 'application/xml')
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                self.send_response(200)
                self.send_header('Content-Length', str(len(data)))
                self.send_header('ETag', f'"{account}-version"')
                self.end_headers()
                if account == 'a':
                    started.set()
                    if not release.wait(20):
                        return
                self.wfile.write(data)

        return Handler

    report = {'scope': 'packaged engine and two synthetic loopback WebDAV accounts; no real cloud', 'checks': {}}
    try:
        with tempfile.TemporaryDirectory(prefix='td-multi-') as directory:
            root = Path(directory)
            engine = desktop.Engine(args.engine, args.peer, root)
            try:
                revisions = {}
                for account in payloads:
                    server = ThreadingHTTPServer(('127.0.0.1', 0), handler(account))
                    servers.append(server)
                    threading.Thread(target=server.serve_forever, daemon=True).start()
                    engine.call('save_profile', {'id': account, 'kind': 'dav', 'name': account,
                                'url': f'http://127.0.0.1:{server.server_port}/', 'network_route': 'system'})
                    revisions[account] = engine.call('connect', {'id': account})['revision']
                assert {p['id'] for p in engine.call('status')['connections']} == {'a', 'b'}
                assert all(p['connected'] for p in engine.call('profiles')['profiles'])
                report['checks']['both_accounts_remain_connected'] = True

                replies = {}

                def send(identifier, op, values=None):
                    engine.process.stdin.write(json.dumps({'id': identifier, 'op': op, 'args': values or {}}) + '\n')
                    engine.process.stdin.flush()

                def receive(ids):
                    deadline = time.monotonic() + 12
                    while not ids.issubset(replies):
                        try:
                            value = engine.events.get(timeout=max(.01, deadline-time.monotonic()))
                        except queue.Empty:
                            raise AssertionError('control or transfer reply timed out') from None
                        if not value.get('event'):
                            replies[value['id']] = value

                def file_args(account, **values):
                    return {'profile': account, 'revision': revisions[account], 'path': '/same.txt', **values}

                send(500, 'download', file_args('a', output=str(root/'a.txt')))
                assert started.wait(8), 'download did not reach gate'
                send(501, 'connect', {'id': 'b'})
                send(502, 'status')
                send(503, 'disconnect', {'id': 'a'})
                send(504, 'quota', file_args('a'))
                send(505, 'list', file_args('b', path='/'))
                receive({501, 502, 503, 504, 505})
                assert 500 not in replies, 'gated transfer unexpectedly completed'
                assert replies[501]['ok'] and replies[501]['result']['retained']
                assert replies[502]['result']['profile'] == 'b'
                assert not replies[503]['ok'], 'disconnect must retain running account'
                assert replies[504]['result']['state'] == 'unsupported'
                assert replies[505]['ok']
                send(506, 'download', file_args('b', output=str(root/'b.txt')))
                receive({506})
                assert replies[506]['ok'] and (root/'b.txt').read_bytes() == payloads['b']
                report['checks']['switch_list_status_quota_and_other_download_while_first_is_gated'] = True
                report['checks']['disconnect_refuses_running_transfer'] = True
                release.set()
                receive({500})
                assert replies[500]['ok'] and (root/'a.txt').read_bytes() == payloads['a']
                report['checks']['same_path_downloads_preserve_original_account_bytes'] = True
                send(507, 'download', file_args('a', output=str(root/'a.txt')))
                send(508, 'list', file_args('b', revision='obsolete'))
                receive({507, 508})
                assert not replies[507]['ok'] and not replies[508]['ok']
                assert (root/'a.txt').read_bytes() == payloads['a']
                report['checks']['no_clobber_and_revision_guards_retained'] = True
                send(509, 'disconnect', {'id': 'a'})
                send(510, 'status')
                receive({509, 510})
                assert replies[509]['ok']
                assert replies[510]['result']['profile'] == 'b'
                assert [p['id'] for p in replies[510]['result']['connections']] == ['b']
                report['checks']['disconnect_one_keeps_other_online'] = True
                list_hold.set()
                send(511, 'list', file_args('b', path='/'))
                assert list_started.wait(8), 'directory read did not reach gate'
                send(512, 'quit')
                # This immediate reader reply proves quit has reached the input
                # loop, while the independent control worker is still gated.
                send(513, 'cancel', {'id': 999999})
                receive({513})
                list_release.set()
                receive({511, 512})
                engine.process.stdin.close()
                engine.process.wait(timeout=10)
                assert not replies[511]['ok'], 'quit must cancel the queued display read'
                assert replies[512]['ok'], 'clean idle quit must remain accepted'
                report['checks']['quit_cancels_display_read_before_ack_and_process_exits'] = True
                engine.counter = 600
            finally:
                release.set()
                list_release.set()
                engine.counter = max(engine.counter, 600)
                engine.close()
    finally:
        release.set()
        list_release.set()
        for server in servers:
            server.shutdown()
            server.server_close()
    report['passed'] = True
    if args.report:
        Path(args.report).write_text(json.dumps(report, indent=2)+'\n', encoding='utf-8')
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--engine', required=True)
    parser.add_argument('--peer', required=True)
    parser.add_argument('--report')
    run(parser.parse_args())
