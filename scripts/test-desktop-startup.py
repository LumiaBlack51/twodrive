#!/usr/bin/env python3
"""Restart saved synthetic accounts through resume; optionally verify native mounts.

Never opens a GUI/browser or uses a user's state or cloud account.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import re
import tempfile

spec = importlib.util.spec_from_file_location('desktop', Path(__file__).with_name('test-dav-desktop.py'))
desktop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(desktop)


def run(args):
    report = {'scope': 'new engine process restores two saved loopback accounts; disposable state only',
              'checks': {}, 'cloud_requests': 0, 'native_mount_verified': False}
    servers = []
    engine = None
    with tempfile.TemporaryDirectory(prefix='td-startup-') as scratch:
        root = Path(scratch)
        profiles = []
        payloads = {}
        try:
            for name in ['a', 'b']:
                share = root / ('share-' + name)
                share.mkdir()
                payloads[name] = ('correct account ' + name).encode()
                (share / 'generated.txt').write_bytes(payloads[name])
                options = {'creationflags': subprocess.CREATE_NO_WINDOW} if os.name == 'nt' else {}
                server = subprocess.Popen([args.peer, '--state', str(root / ('server-' + name)),
                    'dav-serve', '--root', str(share), '--listen', '127.0.0.1:0', '--write'],
                    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, encoding='utf-8', **options)
                servers.append(server)
                ready = server.stdout.readline().strip()
                assert ready.startswith('WebDAV: ')
                credentials = json.loads((root / ('server-' + name) / 'credentials.json').read_text())
                profiles.append({'id': name, 'kind': 'dav', 'name': 'Generated ' + name,
                    'url': ready.removeprefix('WebDAV: '), **credentials, 'network_route': 'system',
                    'writable': False, 'mount_path': str(root / 'mounts' / name)})
            engine = desktop.Engine(args.engine, args.peer, root)
            for p in profiles:
                engine.call('save_profile', p)
                engine.call('connect', {'id': p['id']})
            engine.call('save_profile', dict(profiles[0], id='new', name='Not authorized', mount_path=str(root / 'mounts/new')))
            engine.close()
            engine = None
            profile_file = root / 'client/profiles.json'
            before = json.loads(profile_file.read_text(encoding='utf-8'))
            session_file = root / 'client/session-a.json'
            session_bytes = session_file.read_bytes()
            session = json.loads(session_bytes)
            session['binding'] = 'different-account-binding'
            session_file.write_text(json.dumps(session), encoding='utf-8')
            engine = desktop.Engine(args.engine, args.peer, root)
            public = engine.call('profiles')
            by_id = {p['id']: p for p in public['profiles']}
            assert {p['id'] for p in public['profiles'] if p.get('startup_ready')} == {'a', 'b'}, 'saved accounts must be eligible, fresh profiles must not auto-login'
            report['checks']['saved_accounts_eligible_without_new_login'] = True
            engine.call('resume', {'id': 'new', 'revision': by_id['new']['revision']}, failure=True)
            engine.call('resume', {'id': 'a', 'revision': 'stale'}, failure=True)
            engine.call('resume', {'id': 'a', 'revision': by_id['a']['revision']}, failure=True)
            assert not engine.call('status')['connected']
            session_file.write_bytes(session_bytes)
            report['checks']['missing_session_revision_and_changed_binding_refused'] = True
            for name in ['a', 'b']:
                target = {'profile': name, 'revision': by_id[name]['revision']}
                engine.call('resume', {'id': name, 'revision': target['revision']})
                if args.native:
                    status = engine.call('status')
                    assert status['mount_support']['supported'] and status['mount_support'].get('driver_installed', True)
                    engine.call('mount', dict(target, startup=True))
            status = engine.call('status')
            assert {p['id'] for p in status['connections']} == {'a', 'b'}
            assert status['profile'] == 'b'
            report['checks']['restart_restores_both_accounts_and_preferred_view'] = True
            after = json.loads(profile_file.read_text(encoding='utf-8'))
            assert before['profiles'] == after['profiles'], 'startup must not revise or rewrite account options'
            report['checks']['profile_revisions_and_options_preserved'] = True
            if args.native:
                assert {m['profile'] for m in status['mounts']} == {'a', 'b'}
                for name in ['a', 'b']:
                    assert (Path(by_id[name]['mount_path']) / 'generated.txt').read_bytes() == payloads[name]
                report['checks']['both_native_mounts_recreated_with_correct_account_bytes'] = True
                report['native_mount_verified'] = True
            engine.close()
            engine = None
            if args.native:
                for name in ['a', 'b']:
                    target = Path(by_id[name]['mount_path'])
                    if sys.platform == 'linux':
                        points = {re.sub(r'\\([0-7]{3})', lambda m: chr(int(m[1], 8)), line.split()[4])
                                  for line in Path('/proc/self/mountinfo').read_text().splitlines()}
                        assert str(target) not in points, 'restored FUSE survived normal quit'
                        # The conditional-upload filesystem retains its empty
                        # target; bridge-owned targets can be removed on exit.
                        assert not target.exists() or (target.is_dir() and not any(target.iterdir()))
                    else:
                        assert not target.exists()
                report['checks']['quit_cleans_both_restored_mounts'] = True
        finally:
            if engine:
                engine.close()
            for server in servers:
                server.terminate()
                server.wait(timeout=10)
    report['passed'] = True
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--engine', required=True)
    parser.add_argument('--peer', required=True)
    parser.add_argument('--native', action='store_true')
    parser.add_argument('--report', type=Path)
    args = parser.parse_args()
    result = run(args)
    if args.report:
        args.report.write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
    print(json.dumps(result, indent=2))
