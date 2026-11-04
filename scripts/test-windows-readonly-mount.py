#!/usr/bin/env python3
"""Remote write-denial and owned read mounts of disposable loopback data.

Filename retained for workflow compatibility; there is no local readonly mode.
WebDAV capabilities express protocol support, while the server enforces grants
on requests. Do not stage a VFS write and mistake local acceptance for upload.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

spec = importlib.util.spec_from_file_location('desktop', Path(__file__).with_name('test-dav-desktop.py'))
desktop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(desktop)


def run(args):
    if os.name != 'nt':
        raise RuntimeError('This check requires Windows; it is not a native Linux mount test')
    report = {'scope': 'disposable local WebDAV Provider through actual Windows engine', 'checks': {},
              'cloud_requests': 0, 'native_mount_verified': False}
    with tempfile.TemporaryDirectory(prefix='td-win-readonly-mount-') as directory:
        root = Path(directory)
        share = root/'share'
        share.mkdir()
        payload = b'disposable Windows mount ownership and bytes\0\xff'
        (share/'generated.txt').write_bytes(payload)
        server = subprocess.Popen([args.peer, '--state', str(root/'server'), 'dav-serve', '--root', str(share),
            '--listen', '127.0.0.1:0'], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            text=True, encoding='utf-8', creationflags=subprocess.CREATE_NO_WINDOW)
        engine = None
        try:
            ready = server.stdout.readline().strip()
            assert ready.startswith('WebDAV: ')
            credentials = json.loads((root/'server/credentials.json').read_text())
            engine = desktop.Engine(args.engine, args.peer, root)
            profile = {'id':'native-ro','kind':'dav','name':'Disposable read-only mount',
                'url':ready.removeprefix('WebDAV: '), **credentials, 'network_route':'system',
                'writable':True,'mount_path':str(root/'view/native')}
            engine.call('save_profile',profile)
            engine.call('connect',{'id':profile['id']})
            public = engine.call('profiles')['profiles'][0]
            target = {'profile':public['id'],'revision':public['revision']}
            status = engine.call('status')
            assert status['mount_support']['editable'] is True
            engine.call('mkdir', dict(target,path='/server-denied'),failure=True)
            assert not (share/'server-denied').exists()
            report['checks']['remote_server_denies_write_without_local_mode'] = True
            engine.call('mount',dict(target,revision='stale'),failure=True)
            engine.call('unmount',dict(target,revision='stale'),failure=True)
            report['checks']['profile_revision_guard'] = True
            mount = Path(public['mount_path'])
            if not status['mount_support']['driver_installed']:
                assert status['capabilities']['mount'] is False
                error = engine.call('mount',target,failure=True)
                assert 'WinFsp' in error and not mount.exists()
                report['checks']['missing_driver_is_actionable_without_mounting'] = True
                report['blocked_by'] = 'WinFsp installation required for actual native mount checks'
            else:
                assert engine.call('mount',target,timeout=90)['mounted']
                assert (mount/'generated.txt').read_bytes() == payload
                report['checks']['native_read_bytes_under_remote_read_grant'] = True
                # New connection / selection must retain the existing native mount.
                other = dict(profile,id='other',name='Other view',mount_path=str(root/'view/other'))
                engine.call('save_profile',other)
                engine.call('connect',{'id':'other'})
                assert (mount/'generated.txt').read_bytes() == payload
                report['checks']['mount_survives_account_selection'] = True
                engine.call('connect',{'id':profile['id']})
                other_public = next(p for p in engine.call('profiles')['profiles'] if p['id']=='other')
                other_owner = {'profile':'other','revision':other_public['revision']}
                other_mount = Path(other_public['mount_path'])
                engine.call('mount',other_owner,timeout=90)
                both = engine.call('status')
                assert both['profile']==profile['id'], 'Mounting another account changed the selected view'
                assert {m['profile'] for m in both['mounts']} == {profile['id'],'other'}
                assert (mount/'generated.txt').read_bytes() == payload
                assert (other_mount/'generated.txt').read_bytes() == payload
                engine.call('unmount',dict(other_owner,revision='stale'),failure=True)
                engine.call('unmount',other_owner)
                assert engine.call('status')['profile']==profile['id']
                assert (mount/'generated.txt').read_bytes() == payload
                assert not other_mount.exists()
                engine.call('mount',other_owner,timeout=90)
                report['checks']['two_mounts_targeted_without_switch_and_independent_unmount'] = True
                engine.call('unmount',target)
                assert not engine.call('status')['mounted']
                engine.call('mount',target,timeout=90)
                engine.call('quit')
                engine.process.stdin.close()
                engine.process.wait(timeout=20)
                for _ in range(20):
                    if not (mount/'generated.txt').exists(): break
                    time.sleep(.1)
                assert not (mount/'generated.txt').exists(), 'owned mount survived engine quit'
                assert not (other_mount/'generated.txt').exists(), 'parked owned mount survived engine quit'
                report['checks']['unmount_remount_and_quit_cleanup'] = True
                report['native_mount_verified'] = True
        finally:
            if engine: engine.close()
            server.terminate()
            server.wait(timeout=10)
    report['passed'] = all(report['checks'].values())
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--engine',required=True)
    parser.add_argument('--peer',required=True)
    parser.add_argument('--report',type=Path)
    args = parser.parse_args()
    result = run(args)
    if args.report: args.report.write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(result,indent=2))
