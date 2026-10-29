#!/usr/bin/env python3
"""Actual Linux FUSE default editing and restart recovery, using a disposable loopback account."""
import argparse
import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time

spec = importlib.util.spec_from_file_location('desktop', Path(__file__).with_name('test-dav-desktop.py'))
desktop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(desktop)


def wait_for(test, seconds=45):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            if test():
                return
        except OSError:
            pass
        time.sleep(.2)
    raise AssertionError('disposable native mount operation did not settle')


def mounted(path):
    # stat/mountpoint can fail on a disconnected FUSE endpoint. Inspect the
    # kernel table instead so cleanup never follows a retained mounted tree.
    target = str(path.absolute())
    for line in Path('/proc/self/mountinfo').read_text(encoding='utf-8').splitlines():
        point = re.sub(r'\\([0-7]{3})', lambda m: chr(int(m[1], 8)), line.split()[4])
        if point == target:
            return True
    return False


def run(args):
    root = Path(tempfile.mkdtemp(prefix='td-linux-locked-')).resolve()
    target = root / 'mount'
    engine = server = None
    checks = {}
    try:
        share = root / 'share'
        share.mkdir()
        (share / 'existing.txt').write_bytes(b'original remote bytes')
        server = subprocess.Popen([args.peer, '--state', str(root / 'server'), 'dav-serve',
            '--root', str(share), '--listen', '127.0.0.1:0', '--write'],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, encoding='utf-8')
        url = server.stdout.readline().strip().removeprefix('WebDAV: ')
        credentials = json.loads((root / 'server/credentials.json').read_text(encoding='utf-8'))
        engine = desktop.Engine(args.engine, args.peer, root)
        engine.call('save_profile', dict(id='locked', kind='dav', name='Disposable Linux account',
            url=url, **credentials, network_route='system', writable=True, mount_path=str(target)))
        # Simulate a legacy on-disk readonly profile and mount binding.
        config_path = root / 'client/profiles.json'
        engine.close(); engine = None
        config = json.loads(config_path.read_text(encoding='utf-8'))
        config['profiles'][0]['writable'] = False
        config_path.write_text(json.dumps(config), encoding='utf-8')
        engine = desktop.Engine(args.engine, args.peer, root)
        engine.call('connect', {'id': 'locked'})
        owner = lambda: {'profile': 'locked', 'revision': engine.active_revision}
        engine.call('mount', owner())
        assert (target / 'existing.txt').read_bytes() == b'original remote bytes'
        binding_file = root / 'client/mount-locked/binding.json'
        assert json.loads(binding_file.read_text(encoding='utf-8'))['writable']
        engine.call('pause', {'paused': True})
        (target / 'pending.txt').write_bytes(b'local edit retained across restart')
        wait_for(lambda: engine.call('status')['cache']['pending'] > 0)
        assert not (share / 'pending.txt').exists()
        engine.call('unmount', owner(), failure=True)
        engine.call('quit', failure=True)
        checks['default_editing_pending_writes_still_block_unmount_and_quit'] = True
        engine.process.terminate()
        engine.process.wait(timeout=10)
        engine.close()
        engine = None
        if mounted(target):
            subprocess.run(['fusermount3', '-u', str(target)], check=True)
        assert not mounted(target)
        saved = json.loads((root / 'client/profiles.json').read_text(encoding='utf-8'))['profiles'][0]
        assert not saved['writable'] and json.loads(binding_file.read_text(encoding='utf-8'))['writable']
        engine = desktop.Engine(args.engine, args.peer, root)
        engine.call('resume', {'id': 'locked', 'revision': saved['revision']})
        engine.active_profile = 'locked'
        engine.active_revision = saved['revision']
        engine.call('pause', {'paused': False})
        status = engine.call('status')
        assert status['capabilities']['mount'] and status['capabilities']['upload']
        engine.call('mount', owner())
        assert json.loads(binding_file.read_text(encoding='utf-8'))['writable']
        assert (target / 'pending.txt').read_bytes() == b'local edit retained across restart'
        checks['legacy_readonly_profile_restores_editable_without_changing_revision'] = True
        wait_for(lambda: (share / 'pending.txt').exists() and
            (share / 'pending.txt').read_bytes() == b'local edit retained across restart')
        wait_for(lambda: engine.call('status')['cache']['pending'] == 0)
        assert json.loads(binding_file.read_text(encoding='utf-8'))['writable']
        (target / 'unlocked.txt').write_bytes(b'default editing after restart')
        wait_for(lambda: (share / 'unlocked.txt').exists() and
            (share / 'unlocked.txt').read_bytes() == b'default editing after restart')
        wait_for(lambda: engine.call('status')['cache']['pending'] == 0)
        checks['restart_recovers_original_pending_bytes_and_allows_new_writes'] = True
        engine.close()
        engine = None
        assert not mounted(target)
        checks['normal_teardown_releases_owned_mount'] = True
        report = {'scope': 'actual Linux FUSE and one disposable loopback account',
            'real_cloud_requests': 0, 'checks': checks, 'passed': True}
        Path(args.report).write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
        print(json.dumps(report, indent=2))
    finally:
        cleanup_error = None
        if engine:
            try:
                engine.close()
            except Exception as error:
                cleanup_error = error
        if mounted(target):
            subprocess.run(['fusermount3', '-u', str(target)], check=False)
        if server:
            server.terminate()
            server.wait(timeout=10)
        # Never recursively remove a live or disconnected mounted filesystem.
        if mounted(target):
            raise RuntimeError('disposable mount retained; temporary directory preserved')
        shutil.rmtree(root)
        if cleanup_error:
            raise cleanup_error


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--engine', required=True)
    parser.add_argument('--peer', required=True)
    parser.add_argument('--report', required=True)
    run(parser.parse_args())
