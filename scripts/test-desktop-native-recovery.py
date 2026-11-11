"""Profile edits with a retained native link; no account login/registration."""
import argparse
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


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--engine', required=True)
    parser.add_argument('--peer', required=True)
    args = parser.parse_args()
    run(args.engine, args.peer)
