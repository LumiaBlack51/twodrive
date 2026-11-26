"""Profile edits with a retained native link; no account login/registration."""
import argparse
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile

spec = importlib.util.spec_from_file_location('desktop_fixture', Path(__file__).with_name('test-dav-desktop.py'))
desktop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(desktop)


def run(engine_path, peer_path):
    with tempfile.TemporaryDirectory(prefix='td-native-edit-') as private:
        root = Path(private)
        state = root / 'client'
        state.mkdir()
        account = root / 'empty-account'
        account.mkdir()
        profile = {'id': 'one', 'revision': 'retained-revision', 'name': 'Original',
                   'kind': 'onedrive', 'writable': True, 'network_route': 'system',
                   'mount_path': str(root / 'mount'), 'onedrive_state': str(account)}
        (state / 'profiles.json').write_text(json.dumps({'profiles': [profile], 'selected': 'one'}))
        link = {'profile': 'one', 'revision': 'retained-revision',
                'root': str(root / 'unregistered-native'),
                'state': str(state / 'unsealed-attempt'), 'auto_start': False}
        link_file = state / 'native-desktop.json'
        link_file.write_text(json.dumps(link))
        original_link = link_file.read_bytes()
        engine = desktop.Engine(engine_path, peer_path, root)
        try:
            assert engine.call('status')['cfapi']['supported'] is True
            engine.call('save_profile', dict(profile, name='Renamed'))
            saved = json.loads((state / 'profiles.json').read_text())['profiles'][0]
            assert saved['name'] == 'Renamed'
            assert saved['revision'] == link['revision'], 'Name edit stranded the retained root'
            assert saved['onedrive_state'] == profile['onedrive_state']
            assert link_file.read_bytes() == original_link
            unchanged = (state / 'profiles.json').read_bytes()
            error = engine.call('save_profile', dict(saved, onedrive_state=str(root / 'different-account')), failure=True)
            assert '仅支持修改显示名称' in error
            assert (state / 'profiles.json').read_bytes() == unchanged
            assert link_file.read_bytes() == original_link
            assert not Path(link['root']).exists()
        finally:
            engine.close()
    print(json.dumps({'passed': True, 'name_edit_keeps_revision': True,
                      'binding_change_rejected': True, 'root_registered': False,
                      'cloud_requests': False}))

def check_sign_out(engine_path, peer_path):
    import nt

    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.CreateEventW.argtypes = [wintypes.LPVOID, wintypes.BOOL, wintypes.BOOL, wintypes.LPCWSTR]
    kernel.CreateEventW.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.CloseHandle.restype = wintypes.BOOL

    with tempfile.TemporaryDirectory(prefix='td-native-signout-') as private:
        root = Path(private)
        state = root / 'client'
        native_state = state / 'native-fixture'
        native_state.mkdir(parents=True)
        profiles = []
        tokens = {}
        for profile_id in ('owner', 'other'):
            account = root / profile_id
            account.mkdir()
            tokens[profile_id] = account / 'tokens.json'
            tokens[profile_id].write_bytes(b'disposable sign-out test data')
            profiles.append({'id': profile_id, 'revision': f'{profile_id}-revision',
                             'name': profile_id, 'kind': 'onedrive', 'network_route': 'system',
                             'mount_path': str(root / f'mount-{profile_id}'),
                             'onedrive_state': str(account)})
        profile_file = state / 'profiles.json'
        profile_file.write_text(json.dumps({'profiles': profiles, 'selected': 'other'}))
        profile_before = profile_file.read_bytes()
        owner_token = tokens['owner'].read_bytes()
        link_file = state / 'native-desktop.json'
        link_file.write_text(json.dumps({'profile': 'owner', 'revision': 'owner-revision',
                                        'root': str(root / 'unregistered-native'),
                                        'state': str(native_state), 'auto_start': False}))
        link_before = link_file.read_bytes()

        # Exercise the same OS signal a separate native worker holds. No root
        # registration, credentials, or cloud connection is needed.
        canonical = nt._getfinalpathname(str(native_state))
        digest = hashlib.sha256(canonical.encode('utf-8')).hexdigest()
        name = 'Local\\TwoDriveDevNativeStop-' + digest
        engine = desktop.Engine(engine_path, peer_path, root)
        try:
            event = kernel.CreateEventW(None, True, False, name)
            if not event:
                raise ctypes.WinError(ctypes.get_last_error())
            try:
                cfapi = engine.call('status')['cfapi']
                assert cfapi['running'] and cfapi['profile'] == 'owner', cfapi
                assert engine.call('profiles')['selected'] == 'other'
                error = engine.call('sign_out', {'id': 'owner'}, failure=True)
                assert '请先停止此账户的 CFAPI 原生同步' in error, error
                assert profile_file.read_bytes() == profile_before
                assert tokens['owner'].read_bytes() == owner_token
                assert link_file.read_bytes() == link_before

                engine.call('sign_out', {'id': 'other'})
                assert not tokens['other'].exists()
                remaining = engine.call('profiles')['profiles']
                assert [p['id'] for p in remaining] == ['owner']
                assert engine.call('status')['cfapi']['running']
                assert tokens['owner'].read_bytes() == owner_token
            finally:
                assert kernel.CloseHandle(event), 'Failed to close fixture event'

            assert engine.call('status')['cfapi']['running'] is False
            engine.call('sign_out', {'id': 'owner'})
            assert not tokens['owner'].exists()
            assert not engine.call('profiles')['profiles']
            assert not (root / 'unregistered-native').exists()
        finally:
            engine.close()

    print(json.dumps({'native_owner_sign_out_blocked': True,
                      'other_account_sign_out_allowed': True,
                      'owner_sign_out_after_stop_allowed': True}))



if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--engine', required=True)
    parser.add_argument('--peer', required=True)
    args = parser.parse_args()
    run(args.engine, args.peer)
    check_sign_out(args.engine, args.peer)
