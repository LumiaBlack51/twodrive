#!/usr/bin/env python3
"""Desktop hosting/export/import over real IPC and two local QUIC peers."""
import argparse
import importlib.util
import json
from pathlib import Path
import tempfile

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
                engines.append(desktop.Engine(args.engine, args.peer, home))
            host, client = engines
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
            # The same desktop engine can host while using a storage connection.
            local_file = root / 'local-client.json'
            host.call('share_export', dict(output=str(local_file)))
            local_options = host.call('provider_action', dict(kind='peer', action='import', file=str(local_file)))['options']
            host.call('save_profile', dict(id='local-client', kind='peer', network_route='system', **local_options))
            host.call('connect', dict(id='local-client'))
            simultaneous = host.call('status')
            assert simultaneous['connected'] and simultaneous['sharing']['running']
            host.call('list', dict(path='/'))
            host.call('disconnect')
            assert host.call('status')['sharing']['running']
            checks['same_engine_disconnect_preserves_hosting'] = True
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
            client.call('disconnect')
            host.call('share_stop')
            host.call('share_export', dict(output=str(root/'stopped.json')), failure=True)
            checks['changed_permissions_use_new_identity_and_enforce_readonly'] = True
            host.call('share_start', settings)
            host.close()
            host = desktop.Engine(args.engine, args.peer, root / 'host')
            engines[0] = host
            assert not host.call('status')['sharing']['running']
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
