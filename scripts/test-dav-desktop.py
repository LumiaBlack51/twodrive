#!/usr/bin/env python3
"""Disposable desktop IPC/WebDAV/FUSE check. Never prints credentials or private listings."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time
import uuid


class Engine:
    def __init__(self, engine, peer, root, *, state_path=None):
        self.counter = 0
        self.events = queue.Queue()
        self.progress = 0
        self.operation_events = 0
        self.active_profile = None
        self.active_revision = None
        env = dict(os.environ, XDG_CONFIG_HOME=str(root / 'config'), APPDATA=str(root / 'config'))
        self.log = (root / 'engine.stderr').open('w')
        try:
            self.process = subprocess.Popen([engine, '--state', str(state_path or root / 'client'),
                '--peer-bin', peer, '--launcher', peer], stdin=subprocess.PIPE,
                stdout=subprocess.PIPE, stderr=self.log, text=True, encoding="utf-8", env=env)
        except BaseException:
            self.log.close()
            raise
        def read():
            for line in self.process.stdout:
                self.events.put(json.loads(line))
        threading.Thread(target=read, daemon=True).start()

    def call(self, op, args=None, failure=False, timeout=90):
        args = dict(args or {})
        if op in ('list', 'mkdir', 'rename', 'delete', 'upload', 'download'):
            args.setdefault('profile', self.active_profile)
            args.setdefault('revision', self.active_revision)
        self.counter += 1
        request_id = self.counter
        self.process.stdin.write(json.dumps({'id':request_id, 'op':op, 'args':args or {}}) + '\n')
        self.process.stdin.flush()
        deadline = time.monotonic() + timeout
        while True:
            message = self.events.get(timeout=max(.01, deadline-time.monotonic()))
            if message.get('event') == 'progress':
                self.progress += 1
                continue
            if message.get('event'):
                self.operation_events += 1
                continue
            assert message['id'] == request_id, 'unexpected IPC response'
            if failure:
                assert not message['ok'], 'unsafe operation unexpectedly succeeded'
                return message['error']
            assert message['ok'], message.get('error', 'IPC operation failed')
            if op == 'connect':
                self.active_profile = args['id']
                self.active_revision = message['result']['revision']
            return message['result']

    def assert_editable(self, profile_id):
        """No local mode or password step for an authorized fixture account."""
        profile = next(p for p in self.call('profiles')['profiles'] if p['id'] == profile_id)
        assert profile['writable'] and 'write_locked' not in profile
        if self.active_profile == profile_id:
            self.active_revision = profile['revision']

    def close(self):
        try:
            if self.process.poll() is None:
                self.call('quit')
                self.process.stdin.close()
                self.process.wait(timeout=10)
        finally:
            if self.process.poll() is None:
                self.process.terminate()
                self.process.wait(timeout=10)
            self.log.close()


def run(args):
    report = {'mode':'actual-paired-Windows' if args.paired_state else 'local-WebDAV', 'checks':{}}
    with tempfile.TemporaryDirectory(prefix='td-dav-check-') as directory:
        root = Path(directory)
        server = None
        engine = None
        remote = '/td-desktop-' + uuid.uuid4().hex
        created = False
        try:
            profile = {'id':'verification', 'name':'Disposable validation', 'kind':'peer' if args.paired_state else 'dav',
                'writable':True, 'mount_path':str(root / 'mount')}
            if args.paired_state:
                profile['peer_state'] = str(Path(args.paired_state).resolve())
            else:
                share = root / 'share'
                share.mkdir()
                server = subprocess.Popen([args.peer, '--state', str(root / 'server'), 'dav-serve',
                    '--root', str(share), '--listen', '127.0.0.1:0', '--write'], stdout=subprocess.PIPE,
                    stderr=subprocess.DEVNULL, text=True, encoding="utf-8")
                line = server.stdout.readline().strip()
                assert line.startswith('WebDAV: '), 'local server not ready'
                profile['url'] = line.removeprefix('WebDAV: ')
                credentials = json.loads((root / 'server/credentials.json').read_text())
                profile.update(credentials)
            engine = Engine(args.engine, args.peer, root)
            assert engine.call('status')['autostart'] is False
            profile['network_route'] = 'auto'
            engine.call('save_profile', profile)
            public = engine.call('profiles')
            assert public['profiles'][0]['network_route'] == 'auto'
            assert 'password' not in public['profiles'][0]
            if os.name != 'nt':
                assert (root / 'client/profiles.json').stat().st_mode & 0o077 == 0
            report['checks']['private_profiles'] = True
            engine.call('connect', {'id':profile['id']})
            engine.assert_editable(profile['id'])
            report['checks']['connect'] = True
            active_revision = engine.active_revision
            other_profile = dict(profile, id='other-profile', name='Other connection')
            engine.call('save_profile', other_profile)
            engine.call('save_profile', dict(other_profile, name='Edited inactive connection'))
            assert engine.call('status')['profile'] == profile['id']
            assert engine.call('profiles')['selected'] == profile['id']
            engine.call('save_profile', profile, failure=True)
            engine.call('remove_profile', {'id':'other-profile'})
            assert engine.active_revision == active_revision
            report['checks']['connected_profile_management_preserves_active_account'] = True
            engine.call('mkdir', {'path':'/wrong-connection', 'profile':'not-connected'}, failure=True)
            engine.call('mkdir', {'path':'/outdated-configuration', 'revision':'old'}, failure=True)
            report['checks']['connection_affinity_enforced'] = True
            engine.call('mkdir', {'path':remote})
            created = True
            engine.call('mkdir', {'path':remote+'/folder'})
            source = root / 'source.bin'
            data = bytes(range(251)) * (2 * 1024 * 1024 // 251 + 1)
            source.write_bytes(data)
            uploaded = engine.call('upload', {'path':remote+'/folder/样本 #%.bin', 'source':str(source)}, timeout=120)
            engine.call('upload', {'path':uploaded['path'], 'source':str(source)}, failure=True)
            listing = engine.call('list', {'path':remote})
            assert len(listing['entries']) == 1 and listing['entries'][0]['directory']
            engine.call('download', {'path':uploaded['path'], 'output':str(root/'download.bin'), 'size':len(data)}, timeout=120)
            assert hashlib.sha256((root/'download.bin').read_bytes()).digest() == hashlib.sha256(data).digest()
            engine.call('download', {'path':uploaded['path'], 'output':str(source)}, failure=True)
            assert source.read_bytes() == data
            engine.call('mkdir', {'path':'/../escape'}, failure=True)
            engine.call('delete', {'path':'/'}, failure=True)
            engine.call('rename', {'path':uploaded['path'], 'to':remote+'/folder/moved.bin'})
            report['checks'].update(roundtrip_bytes=len(data), roundtrip_sha256=hashlib.sha256(data).hexdigest(),
                shallow_directory=True, no_clobber=True, traversal_rejected=True, rename=True,
                progress_events=engine.progress)
            if args.mount:
                engine.call('mount', timeout=90)
                mounted = root / 'mount' / remote.lstrip('/') / 'folder' / 'moved.bin'
                assert mounted.read_bytes() == data
                report['checks']['fuse_read'] = True
                if not args.paired_state:
                    other_root = root / 'other'
                    other_root.mkdir()
                    other = Engine(args.engine, args.peer, other_root)
                    try:
                        other.call('save_profile', profile)
                        other.call('connect', {'id':profile['id']})
                        other.call('mount', failure=True)
                    finally:
                        other.close()
                    assert mounted.read_bytes() == data, 'another client unmounted our mount'
                    report['checks']['existing_mount_not_adopted'] = True
                local_write = mounted.parent / 'fuse-write.txt'
                local_write.write_bytes(b'TwoDrive DAV FUSE validation\n')
                deadline = time.monotonic() + 150
                while True:
                    result = engine.call('list', {'path':remote+'/folder'})
                    if any(e['name'] == local_write.name for e in result['entries']):
                        break
                    assert time.monotonic() < deadline, 'FUSE write not uploaded in budget'
                    time.sleep(1)
                engine.call('download', {'path':remote+'/folder/fuse-write.txt', 'output':str(root/'fuse-download.txt')})
                assert (root/'fuse-download.txt').read_bytes() == local_write.read_bytes()
                deadline = time.monotonic() + 30
                while engine.call('status')['cache']['pending']:
                    assert time.monotonic() < deadline, 'pending writes not drained'
                    time.sleep(.5)
                report['checks']['fuse_write'] = True
                engine.call('pause', {'paused':True})
                paused_write = mounted.parent / 'paused-fuse.txt'
                paused_write.write_bytes(b'queued while paused')
                time.sleep(11)
                assert not any(e['name']=='paused-fuse.txt' for e in engine.call('list', {'path':remote+'/folder'})['entries'])
                engine.call('unmount', failure=True)
                engine.call('pause', {'paused':False})
                deadline=time.monotonic()+150
                while not any(e['name']=='paused-fuse.txt' for e in engine.call('list', {'path':remote+'/folder'})['entries']):
                    assert time.monotonic()<deadline, 'paused FUSE write did not resume'
                    time.sleep(1)
                deadline=time.monotonic()+30
                while engine.call('status')['cache']['pending']:
                    assert time.monotonic()<deadline
                    time.sleep(.5)
                report['checks']['fuse_pause_preserves_and_resumes_pending_write'] = True
                engine.call('unmount')
                cleaned = engine.call('clear_cache', {'id':profile['id']})
                assert cleaned['pending'] == 0 and cleaned['bytes'] == 0
                report['checks']['cache_cleared'] = True
            backup_source = root / 'backup-source'
            backup_source.mkdir()
            (backup_source/'sample.txt').write_bytes(b'first backup version')
            (backup_source/'skip-link').symlink_to(root/'source.bin')
            backup_map = [{'local':str(backup_source), 'remote':remote+'/backup', 'enabled':True}]
            engine.call('pause', {'paused':True})
            engine.call('save_backups', {'id':profile['id'], 'folders':backup_map})
            time.sleep(2)
            assert not any(e['name']=='backup' for e in engine.call('list', {'path':remote})['entries'])
            assert engine.call('status')['paused']
            engine.call('upload', {'source':str(backup_source/'sample.txt'), 'path':remote+'/paused.txt'}, failure=True)
            engine.call('pause', {'paused':False})
            def wait_backup(content):
                deadline = time.monotonic()+100
                output = root/'backup-downloaded.txt'
                while True:
                    status = engine.call('status')
                    assert not status.get('backup_failed'), 'backup child exited'
                    if status.get('backup', {}).get('state') == 'error':
                        raise AssertionError(status['backup'].get('error'))
                    listing = engine.call('list', {'path':remote})
                    if any(e['name']=='backup' for e in listing['entries']):
                        listing = engine.call('list', {'path':remote+'/backup'})
                        if any(e['name']=='sample.txt' for e in listing['entries']):
                            output.unlink(missing_ok=True)
                            engine.call('download', {'path':remote+'/backup/sample.txt','output':str(output)})
                            if output.read_bytes()==content:
                                assert not any(e['name']=='skip-link' for e in listing['entries'])
                                break
                    assert time.monotonic()<deadline, 'backup deadline exceeded'
                    time.sleep(1)
            wait_backup(b'first backup version')
            engine.call('disconnect')
            manifest_file=root/'client/backup-verification/manifest.json'
            manifest=json.loads(manifest_file.read_text())
            record=manifest['files'][remote+'/backup/sample.txt']
            record['intent']=hashlib.sha256(b'first backup version').hexdigest()
            record['hash']='';record['etag']=None;record['fingerprint']=''
            manifest_file.write_text(json.dumps(manifest))
            engine.call('connect',{'id':profile['id']})
            deadline=time.monotonic()+45
            while True:
                manifest=json.loads(manifest_file.read_text())
                record=manifest['files'][remote+'/backup/sample.txt']
                if record.get('intent') is None and record['hash']==hashlib.sha256(b'first backup version').hexdigest(): break
                assert time.monotonic()<deadline, 'ambiguous upload was not reconciled'
                time.sleep(.5)
            report['checks']['backup_committed_upload_recovery_by_hash'] = True
            (backup_source/'sample.txt').write_bytes(b'updated backup version')
            wait_backup(b'updated backup version')
            etag_before_pause = next(e['etag'] for e in engine.call('list', {'path':remote+'/backup'})['entries'] if e['name']=='sample.txt')
            engine.call('pause', {'paused':True})
            (backup_source/'sample.txt').write_bytes(b'paused next version')
            time.sleep(11)
            assert next(e['etag'] for e in engine.call('list', {'path':remote+'/backup'})['entries'] if e['name']=='sample.txt') == etag_before_pause
            engine.call('pause', {'paused':False})
            wait_backup(b'paused next version')
            current = next(e for e in engine.call('list', {'path':remote+'/backup'})['entries'] if e['name']=='sample.txt')
            foreign = root/'external.txt'
            foreign.write_bytes(b'external remote edit')
            engine.call('upload', {'path':remote+'/backup/sample.txt','source':str(foreign),'etag':current['etag']})
            (backup_source/'sample.txt').write_bytes(b'conflicting local edit')
            deadline=time.monotonic()+45
            while engine.call('status')['backup']['state'] != 'error':
                assert time.monotonic()<deadline, 'conflict was not reported'
                time.sleep(1)
            check=root/'conflict-check.txt'
            engine.call('download', {'path':remote+'/backup/sample.txt','output':str(check)})
            assert check.read_bytes()==b'external remote edit'
            engine.call('save_backups', {'id':profile['id'], 'folders':[]})
            report['checks']['backup_conflict_preserves_remote'] = True
            engine.call('save_backups', {'id':profile['id'], 'folders':[{'local':str(Path.home()),'remote':remote+'/bad','enabled':True}]}, failure=True)
            report['checks']['folder_backup_create_update_pause_resume_symlink_guard'] = True
            engine.call('delete', {'path':remote})
            created = False
            assert not any(e['path'] == remote for e in engine.call('list', {'path':'/'})['entries'])
            report['checks']['remote_test_removed'] = True
            engine.call('disconnect')
            if not args.paired_state:
                profile['writable'] = False
                engine.call('save_profile', profile)
                engine.call('connect', {'id':profile['id']})
                engine.assert_editable(profile['id'])
                engine.call('mkdir', {'path':remote})
                engine.call('delete', {'path':remote})
                report['checks']['legacy_readonly_preference_normalized_to_default_editing'] = True
                engine.call('disconnect')
            engine.call('autostart', {'enabled':True})
            autostart = root/('config/Microsoft/Windows/Start Menu/Programs/Startup/TwoDrive Development.vbs' if os.name == 'nt' else 'config/autostart/twodrive-dav.desktop')
            assert autostart.exists()
            engine.call('autostart', {'enabled':False})
            assert not autostart.exists()
            report['checks']['autostart_toggle_isolated_default_off'] = True
        except Exception:
            if engine:
                try:
                    diagnostic=engine.call('status')
                    print(json.dumps({'failure_network':diagnostic.get('network',[]), 'backup_failed':diagnostic.get('backup_failed')}),flush=True)
                except Exception:
                    pass
            raise
        finally:
            if engine:
                if created:
                    try:
                        engine.call('pause', {'paused':False})
                        engine.call('save_backups', {'id':'verification', 'folders':[]})
                        if engine.call('status').get('mounted'):
                            engine.call('unmount')
                        engine.call('delete', {'path':remote})
                    except Exception:
                        # Keep the exact owned path available for targeted cleanup.
                        print('OWNED REMOTE CLEANUP REQUIRED: ' + remote, flush=True)
                engine.close()
            if server:
                server.terminate()
                server.wait(timeout=10)
    report['passed'] = True
    if args.report:
        Path(args.report).write_text(json.dumps(report, indent=2)+'\n')
    print(json.dumps(report, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--engine', required=True)
    parser.add_argument('--peer', required=True)
    parser.add_argument('--paired-state')
    parser.add_argument('--mount', action='store_true')
    parser.add_argument('--report')
    run(parser.parse_args())
