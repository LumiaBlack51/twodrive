#!/usr/bin/env python3
"""Desktop hosting/export/import over real IPC and two local QUIC peers."""
import argparse
import importlib.util
import json
from pathlib import Path
import tempfile
import subprocess

spec = importlib.util.spec_from_file_location('desktop_check', Path(__file__).with_name('test-dav-desktop.py'))
desktop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(desktop)


def run(args):
    checks = {}
    with tempfile.TemporaryDirectory(prefix='td-share-gui-') as directory:
        root = Path(directory)
        shared = root / 'shared'
        shared.mkdir()
        (shared / 'sample.txt').write_bytes(b'desktop shared bytes\n')
        engines = []
        try:
            for name in ['host', 'client']:
                home = root / name
                home.mkdir()
                engines.append(desktop.Engine(args.engine, args.peer, home, tdlink=name == 'host'))
            host, client = engines
            # Same default state as a running host, exactly as installed.
            shared_home = root / 'same-state-client'
            shared_home.mkdir()
            same = desktop.Engine(args.engine, args.peer, shared_home,
                                  state_path=root / 'host' / 'client')
            engines.append(same)
            assert 'sharing' not in same.call('status')
            duplicate = subprocess.run([args.engine, '--state', str(root / 'host' / 'client'),
                                        '--peer-bin', args.peer, '--launcher', args.peer, '--tdlink-server'],
                                       input='', capture_output=True, encoding='utf-8', timeout=8)
            assert duplicate.returncode != 0 and 'already running' in duplicate.stderr
            checks['shared_state_with_distinct_roles_and_exclusive_server_lock'] = True
            settings = dict(name='Disposable shared PC', root=str(shared), writable=True,
                            discoverable=False, network='lan')
            initial = host.call('share_start', settings)
            assert initial['running'] and initial['settings']['writable']
            host.call('share_start', settings, failure=True)
            connection = root / 'connection.json'
            exported = host.call('share_export', dict(output=str(connection)))
            assert exported['name'] == settings['name']
            ticket = json.loads(connection.read_text())
            assert set(ticket) == {'version', 'address', 'secret', 'expires', 'connection'}
            assert str(shared) not in connection.read_text()
            assert 'identity' not in ticket
            host.call('share_export', dict(output=str(shared / 'unsafe.json')), failure=True)
            host.call('share_export', dict(output=str(connection)), failure=True)
            assert ticket == json.loads(connection.read_text())
            preview = client.call('provider_action', dict(kind='peer', action='import', file=str(connection)))
            assert ticket['secret'] not in json.dumps(preview)
            options = preview['options']
            assert options['name'] == settings['name'] and options['writable']
            client.call('save_profile', dict(id='shared-pc',kind='peer',network_route='system',**options))
            client.call('connect', dict(id='shared-pc'))
            download = root / 'download.txt'
            client.call('download', dict(path='/sample.txt',output=str(download)))
            assert download.read_bytes() == b'desktop shared bytes\n'
            source = root / 'source.txt'
            source.write_bytes(b'uploaded via exported GUI connection\n')
            client.assert_editable('shared-pc')
            client.call('upload', dict(path='/uploaded.txt',source=str(source)))
            assert (shared/'uploaded.txt').read_bytes() == source.read_bytes()
            checks['start_export_import_pair_download_upload'] = True
            checks['imported_remote_write_grant_works_without_local_password'] = True
            peers = host.call('status')['sharing']['peers']
            assert len(peers) == 1
            host.call('share_revoke', dict(device=peers[0]))
            client.call('list', dict(path='/'), failure=True)
            client.call('disconnect')
            assert host.call('status')['sharing']['running']
            checks['revoke_denies_new_requests_and_client_disconnect_keeps_hosting'] = True
            host.call('share_export', dict(output=str(connection),replace=True))
            client.call('save_profile', dict(id='shared-pc',kind='peer',network_route='system',**options))
            client.call('connect', dict(id='shared-pc'))
            client.call('disconnect')
            client.call('share_start', settings, failure=True)
            host.call('profiles', failure=True)
            checks['client_and_server_roles_are_separate'] = True
            host.call('share_stop')
            assert not host.call('status')['sharing']['running']
            restarted = host.call('share_start', settings)
            assert restarted['device'] == initial['device']
            host.call('share_stop')
            checks['explicit_replace_reauthorizes_and_same_scope_restart_retains_identity'] = True
            settings['writable'] = False
            readonly = host.call('share_start', settings)
            assert readonly['device'] != initial['device'] and readonly['peers'] == []
            readonly_file = root / 'readonly.json'
            host.call('share_export', dict(output=str(readonly_file)))
            options = client.call('provider_action', dict(kind='peer',action='import',file=str(readonly_file)))['options']
            assert not options['writable']
            client.call('save_profile', dict(id='readonly',kind='peer',network_route='system',**options))
            client.call('connect', dict(id='readonly'))
            client.call('upload', dict(path='/denied.txt',source=str(source)), failure=True)
            assert not (shared/'denied.txt').exists()
            checks['changed_permissions_use_new_identity_and_enforce_readonly'] = True
            client.call('list', dict(path='/'))
            host.close()
            client.call('disconnect')
            host = desktop.Engine(args.engine, args.peer, root / 'host', tdlink=True)
            engines[0] = host
            assert not host.call('status')['sharing']['running']
            host.call('share_export', dict(output=str(root/'stopped.json')), failure=True)
            # A new start immediately succeeds only if quit released its child/lock.
            host.call('share_start', settings)
            host.call('share_stop')
            checks['quit_stops_hosting_and_releases_lock'] = True
        finally:
            for engine in reversed(engines):
                engine.close()
    print(json.dumps({'mode':'isolated desktop IPC and local QUIC','checks':checks,'passed':True},indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--engine',required=True)
    parser.add_argument('--peer',required=True)
    run(parser.parse_args())
