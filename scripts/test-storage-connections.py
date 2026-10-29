#!/usr/bin/env python3
"""Isolated real IPC, loopback WebDAV and LAN UDP/QUIC connection checks.

Does not use cloud credentials or report invitations, identities, or file lists.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

spec = importlib.util.spec_from_file_location('desktop_check', Path(__file__).with_name('test-dav-desktop.py'))
desktop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(desktop)


def run(args):
    checks = {}
    with tempfile.TemporaryDirectory(prefix='td-connections-') as tmp:
        root = Path(tmp)
        share = root / 'share'
        share.mkdir()
        children = []
        engine = None
        try:
            dav = subprocess.Popen([args.peer, '--state', str(root/'dav'), 'dav-serve',
                                    '--root', str(share), '--write', '--listen', '127.0.0.1:0'],
                                   stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
            children.append(dav)
            url = dav.stdout.readline().strip().removeprefix('WebDAV: ')
            credentials = json.loads((root/'dav/credentials.json').read_text())
            engine = desktop.Engine(args.engine, args.peer, root)
            providers = engine.call('profiles')['providers']
            assert {'dav', 'peer', 'google', 'onedrive'} <= {p['id'] for p in providers}
            active = dict(id='dav-test', name='Disposable DAV', kind='dav', url=url,
                          writable=True, **credentials)
            engine.call('save_profile', active)
            engine.call('connect', {'id': active['id']})
            engine.assert_editable(active['id'])
            revision = engine.active_revision
            engine.call('save_profile', dict(id='od-test', name='OneDrive test', kind='onedrive'))
            public = engine.call('profiles')
            od = next(p for p in public['profiles'] if p['id'] == 'od-test')
            assert od['network_route'] == 'system'
            account_path = Path(od['onedrive_state'])
            # Rust canonicalizes Windows state paths to the extended namespace.
            # Compare the existing state directory's identity, and the exact
            # uncreated account suffix, rather than two path spellings.
            assert account_path.parts[-3:] == ('accounts', 'od-test', 'onedrive')
            assert account_path.parent.parent.parent.samefile(root/'client')
            assert Path(od['mount_path']).is_absolute()
            assert public['selected'] == active['id']
            engine.call('save_profile', dict(od, name='Edited OneDrive test'))
            engine.call('save_profile', active, failure=True)
            engine.call('remove_profile', {'id': active['id']}, failure=True)
            engine.call('connect', {'id': 'nonexistent'}, failure=True)
            assert engine.call('status')['profile'] == active['id']
            assert engine.active_revision == revision
            engine.call('remove_profile', {'id': 'od-test'})
            checks['online_add_edit_remove_preserves_active_revision'] = True
            checks['onedrive_schema_and_independent_defaults_without_login'] = True

            invitation = root/'invite.json'
            quic = subprocess.Popen([args.peer, '--state', str(root/'quic'), 'serve',
                                     '--root', str(share), '--write', '--no-relay',
                                     '--invite-file', str(invitation), '--discoverable',
                                     '--device-name', 'Disposable LAN device'],
                                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            children.append(quic)
            deadline = time.monotonic()+30
            while not invitation.exists():
                assert quic.poll() is None, 'test QUIC server exited'
                assert time.monotonic() < deadline, 'invitation not created'
                time.sleep(.1)
            ticket = json.loads(invitation.read_text())
            devices = engine.call('provider_action', {'kind': 'peer', 'action': 'discover'})['items']
            selected = next(d for d in devices if d['label'] == 'Disposable LAN device')
            # A discovery result must not grant access or contain pairing secrets.
            assert set(selected['options']) == {'expected_device', 'writable'}
            peer_id = selected['options']['expected_device']
            assert peer_id == ticket['address']['id']
            checks['actual_signed_udp_discovery_without_pairing_secret'] = True
            paired = dict(id='pc-test', kind='peer', name='Disposable PC', writable=True,
                          network_route='system', invitation_file=str(invitation), **{'expected_device': peer_id})
            engine.call('save_profile', paired)
            assert engine.call('status')['profile'] == active['id']
            public = next(p for p in engine.call('profiles')['profiles'] if p['id'] == 'pc-test')
            assert 'invitation_file' not in public
            private = Path(public['peer_state'])/'pair-invitation.json'
            assert private.exists()
            if os.name != 'nt':
                assert private.stat().st_mode & 0o077 == 0
            engine.call('connect', {'id': 'pc-test'})
            engine.assert_editable('pc-test')
            assert not private.exists()
            source = root/'source.txt'
            source.write_bytes(b'disposable connection write\n')
            engine.call('mkdir', {'path': '/connection-test'})
            uploaded = engine.call('upload', {'path': '/connection-test/file.txt', 'source': str(source)})
            engine.call('upload', {'path': uploaded['path'], 'source': str(source)}, failure=True)
            changed = b'conditional update\n'
            source.write_bytes(changed)
            engine.call('upload', {'path': uploaded['path'], 'source': str(source), 'etag': uploaded['etag']})
            engine.call('rename', {'path': uploaded['path'], 'to': '/connection-test/moved.txt'})
            output = root/'download.txt'
            engine.call('download', {'path': '/connection-test/moved.txt', 'output': str(output)})
            assert output.read_bytes() == changed
            engine.call('delete', {'path': '/connection-test'})
            assert not list(share.iterdir())
            checks['actual_invitation_pairing_and_quic_read_write_modify_delete'] = True
            engine.call('disconnect')
            engine.call('connect', {'id': 'pc-test'})
            engine.call('list', {'path': '/'})
            checks['paired_reconnect_without_reusing_invitation'] = True
            engine.call('connect', {'id': active['id']})
            assert engine.call('status')['profile'] == active['id']
            checks['switch_back_to_existing_connection'] = True
        finally:
            if engine:
                engine.close()
            for child in reversed(children):
                if child.poll() is None:
                    child.terminate()
                    child.wait(timeout=10)
    return {'mode': 'isolated-local-IPC-UDP-QUIC', 'checks': checks,
            'not_verified': ['real-OneDrive-OAuth-and-cloud', 'physical-NAS', 'cross-network-NAT-hole-punching', 'native-Windows-Shell-and-CFAPI']}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--engine', required=True)
    parser.add_argument('--peer', required=True)
    args = parser.parse_args()
    print(json.dumps(run(args), indent=2))
