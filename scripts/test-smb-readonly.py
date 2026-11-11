#!/usr/bin/env python3
"""Actual rootless loopback Samba + Provider/desktop IPC, synthetic files only.

Requires a supplied smbd (can be privately extracted), Python venv containing
components/requirements-smb.txt, and built desktop engine. No sudo/system config,
mounts, discovery, personal credentials, or non-loopback addresses are used.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import pwd
import queue
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time

spec = importlib.util.spec_from_file_location('desktop', Path(__file__).with_name('test-dav-desktop.py'))
desktop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(desktop)


def run(args):
    from spnego._ntlm_raw.crypto import ntowfv1
    os.environ['TWODRIVE_SMB_PYTHON'] = str(Path(sys.executable).absolute())
    checks = []
    with tempfile.TemporaryDirectory(prefix='td-smb-readonly-') as tmp:
        root = Path(tmp)
        for name in ('private', 'lock', 'state', 'cache', 'pid', 'logs', 'share', 'other'):
            (root/name).mkdir(mode=0o700)
        share = root/'share'
        content = 'synthetic 你好 with spaces\n'.encode()
        (share/'你好 space.txt').write_bytes(content)
        (share/'empty').touch()
        (share/'Mixed Case').mkdir()
        (share/'Mixed Case'/'inside').write_bytes(b'root isolated')
        (root/'other'/'only-other').write_bytes(b'other share')
        block = bytes(range(256)) * 4096
        digest = hashlib.sha256()
        with (share/'large').open('wb') as out:
            for _ in range(128):
                out.write(block); digest.update(block)
        user = pwd.getpwuid(os.getuid()).pw_name
        password = 'Disposable-SMB-Only-7!'
        (root/'smbpasswd').write_text(f'{user}:{os.getuid()}:XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX:{ntowfv1(password).hex().upper()}:[U          ]:LCT-{int(time.time()):08X}:\n')
        (root/'smbpasswd').chmod(0o600)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0)); port = sock.getsockname()[1]
        config = f'''[global]
server role = standalone server
security = user
map to guest = Never
interfaces = 127.0.0.1
bind interfaces only = yes
smb ports = {port}
server min protocol = SMB2_02
server max protocol = MAX_PROTOCOL
server signing = mandatory
smb encrypt = desired
REPARSE_TEST_DECLARATION
passdb backend = smbpasswd:{root}/smbpasswd
ncalrpc dir = {root}/private/ncalrpc
private dir = {root}/private
lock directory = {root}/lock
state directory = {root}/state
cache directory = {root}/cache
pid directory = {root}/pid
log file = {root}/logs/smbd.log
load printers = no
printing = bsd
disable spoolss = yes
[synthetic]
path = {share}
read only = no
valid users = {user}
follow symlinks = FOLLOW_TEST_SYMLINKS
wide links = no
[other]
path = {root}/other
read only = no
valid users = {user}
[denied]
path = {share}
valid users = nonexisting-disposable-user
'''
        server = None
        engine = None
        def stop():
            nonlocal server
            if server is not None:
                try: os.killpg(server.pid, signal.SIGTERM)
                except ProcessLookupError: pass
                server.wait(timeout=10)
                server = None
        def start(protocol='SMB3_11'):
            nonlocal server
            fixture_config = config.replace('MAX_PROTOCOL', protocol)
            fixture_config = fixture_config.replace('FOLLOW_TEST_SYMLINKS', 'yes' if args.follow_test_symlinks else 'no')
            fixture_config = fixture_config.replace('REPARSE_TEST_DECLARATION',
                'share:fake_fscaps = 128' if args.advertise_test_reparse_capability else '')
            if protocol == 'SMB2_10':
                fixture_config = fixture_config.replace('smb encrypt = desired', 'smb encrypt = off')
            (root/'smb.conf').write_text(fixture_config)
            log = (root/'smbd.stderr').open('a')
            server = subprocess.Popen([args.smbd, '-F', '--no-process-group', '-l', str(root/'logs'),
                                       '-s', str(root/'smb.conf')], stdout=log, stderr=log, start_new_session=True)
            log.close()
            deadline = time.monotonic() + 10
            while True:
                assert server.poll() is None, 'isolated Samba exited; inspect local logs'
                try:
                    with socket.create_connection(('127.0.0.1', port), .1): break
                except OSError:
                    assert time.monotonic() < deadline, 'isolated Samba did not listen'
                    time.sleep(.05)
        def profile(id='smb', **opts):
            return dict(id=id, kind='smb', name='Disposable SMB', server='127.0.0.1', port=str(port),
                        share='synthetic', root='/', username=user, domain='', password=password,
                        network_route='system', **opts)
        def connect(id='smb'):
            return engine.call('connect', {'id': id})
        def interrupted_download(output, interrupt):
            engine.counter += 1
            request = engine.counter
            payload = dict(path='/large', output=str(output), profile=engine.active_profile, revision=engine.active_revision)
            engine.process.stdin.write(json.dumps(dict(id=request, op='download', args=payload))+'\n')
            engine.process.stdin.flush()
            did_interrupt = False
            deadline = time.monotonic() + 30
            while True:
                msg = engine.events.get(timeout=max(.01, deadline-time.monotonic()))
                if msg.get('event') == 'progress' and msg.get('done', 0) > 0 and not did_interrupt:
                    if interrupt == 'cancel':
                        engine.counter += 1
                        engine.process.stdin.write(json.dumps(dict(id=engine.counter, op='cancel', args={'id':request}))+'\n')
                        engine.process.stdin.flush()
                    elif interrupt == 'disconnect': stop()
                    else: os.truncate(share/'large', msg['done'] + 4096)
                    did_interrupt = True
                if msg.get('event') or msg.get('id') != request: continue
                assert did_interrupt and not msg['ok'], 'interrupted transfer accepted'
                assert not output.exists(), 'partial content published'
                return
        try:
            start()
            engine = desktop.Engine(args.engine, args.peer or args.engine, root)
            catalog = engine.call('profiles')['providers']
            smb = next(d for d in catalog if d['id'] == 'smb')
            assert all(v is False for v in smb['capabilities'].values())
            engine.call('save_profile', profile(writable=True))
            saved = (root/'client/profiles.json').read_text()
            assert password not in saved
            pub = engine.call('profiles')['profiles'][0]
            assert pub['password_saved'] and 'password' not in pub and 'credential_state' not in pub
            assert pub['writable'] is False
            checks.append('schema_private_credentials_and_readonly_permission')
            if args.expect_reparse_rejection:
                error=engine.call('connect',{'id':'smb'},failure=True)
                assert 'reparse/no-follow safety' in error
                print(json.dumps({'passed':['server_without_reparse_advertisement_refused'],'unverified':['positive transfers in this mode']}))
                return
            rows = connect()['entries']
            assert {'/你好 space.txt', '/empty', '/large', '/Mixed Case'} == {e['path'] for e in rows}
            assert all(e['etag'] == '' for e in rows)
            assert engine.call('status')['capabilities']['mount'] is False
            engine.call('list', {'path':'/mixed case'})  # Samba case-insensitive alias
            for path, expected in [('/empty',b''),('/你好 space.txt',content),('/Mixed Case/inside',b'root isolated')]:
                out=root/('download-'+str(len(checks)))
                engine.call('download', {'path':path,'output':str(out)})
                assert out.read_bytes() == expected
                out.unlink()
            engine.call('download', {'path':'/large','output':str(root/'large-output')})
            h=hashlib.sha256()
            with (root/'large-output').open('rb') as inp:
                for chunk in iter(lambda:inp.read(1024*1024),b''):h.update(chunk)
            assert h.digest() == digest.digest()
            checks.append('SMB311_encrypted_browse_metadata_unicode_empty_128MiB_hash')
            (root/'existing').write_bytes(b'preserved')
            engine.call('download', {'path':'/empty','output':str(root/'existing')}, failure=True)
            assert (root/'existing').read_bytes() == b'preserved'
            for op, args_ in [('upload',{'path':'/new','source':str(root/'missing')}),
                              ('mkdir',{'path':'/new'}),('rename',{'path':'/empty','to':'/new'}),
                              ('delete',{'path':'/empty'}),('mount',{})]:
                engine.call(op,args_,failure=True)
            assert (share/'empty').exists() and not (share/'new').exists()
            checks.append('backend_and_UI_IPC_reject_all_writes_mount_and_noclobber')
            for path in ['/../other', '/Mixed Case/../../other', '/empty:ads', '/bad\\name', '/CON.txt']:
                engine.call('list',{'path':path},failure=True)
            (share/'bad:name').touch()
            # Samba may return a legal mangled alias for a Unix-only name.
            # Unsafe raw names are unit-tested at the shared input guard.
            names=engine.call('list',{'path':'/'})['entries']
            assert all(':' not in e['name'] for e in names)
            engine.call('download',{'path':'/bad:name','output':str(root/'unsafe-name')},failure=True)
            (share/'bad:name').unlink()
            (share/'escape').symlink_to(root/'other',target_is_directory=True)
            engine.call('list',{'path':'/escape'},failure=True)
            engine.call('download',{'path':'/escape/only-other','output':str(root/'escaped')},failure=True)
            assert not (root/'escaped').exists()
            (share/'escape').unlink()
            # Supported lab policy forbids server-transparent Unix links.
            # Both final-file and intermediate-directory escapes must fail.
            (share/'box').mkdir()
            (share/'box'/'jump').symlink_to('../Mixed Case',target_is_directory=True)
            (share/'box'/'file-jump').symlink_to('../你好 space.txt')
            try:
                linked=profile(id='linked-root');linked['root']='/box'
                engine.call('save_profile',linked)
                linked_result=engine.call('connect',{'id':'linked-root'})
                assert linked_result['entries'] == [], 'server exposed unsupported links in selected subroot'
                request=dict(op='stat',server='127.0.0.1',port=port,share='synthetic',
                             root='/box',path='/jump/inside',username=user,domain='',
                             password=password,encrypt=True)
                helper=Path(__file__).parents[1]/'experiments/twodrive-dev/components/smb_readonly.py'
                for path in ['/jump/inside','/file-jump']:
                    request['path']=path
                    result=subprocess.run([sys.executable,'-I',str(helper)],input=json.dumps(request)+'\n',
                                          text=True,capture_output=True,timeout=10)
                    frames=[json.loads(line) for line in result.stdout.splitlines()]
                    assert (result.returncode != 0 and frames[-1]['type'] == 'error') or (frames[-1]['type'] == 'end' and frames[-1].get('missing') is True), 'link lookup returned a visible object'
                    assert not any(frame['type'] in ('entry','data','verified') for frame in frames)
            finally:
                (share/'box'/'jump').unlink();(share/'box'/'file-jump').unlink();(share/'box').rmdir()
            connect()
            checks.append('path_traversal_dangerous_server_name_and_symlink_escape_rejected')
            checks.append('file_and_directory_subroot_link_escape_refused_under_supported_server_policy')
            interrupted_download(root/'cancelled', 'cancel')
            assert engine.call('list',{'path':'/'})['entries']
            checks.append('cancel_after_content_keeps_partial_private_and_next_request_works')
            interrupted_download(root/'truncated', 'truncate')
            with (share/'large').open('wb') as out:
                for _ in range(128): out.write(block)
            checks.append('server_local_truncation_cannot_publish_complete_download')
            unreadable=share/'unreadable'
            unreadable.write_bytes(b'synthetic protected bytes');unreadable.chmod(0)
            try:
                engine.call('download',{'path':'/unreadable','output':str(root/'permission-output')},failure=True)
                assert not (root/'permission-output').exists()
            finally:
                unreadable.chmod(0o600);unreadable.unlink()
            checks.append('file_read_permission_error_cannot_publish_content')
            interrupted_download(root/'disconnected','disconnect')
            engine.call('list',{'path':'/'},failure=True)
            start()
            binding1=json.loads((root/'client/session-smb.json').read_text())['binding']
            connect()
            binding2=json.loads((root/'client/session-smb.json').read_text())['binding']
            assert binding1 != binding2
            checks.append('real_midstream_disconnect_no_publish_reconnect_new_binding')
            engine.call('disconnect')
            other=profile(id='other'); other['share']='other'
            engine.call('save_profile',other)
            assert [e['path'] for e in connect('other')['entries']] == ['/only-other']
            engine.call('disconnect')
            nested=profile(id='nested'); nested['root']='/Mixed Case'
            engine.call('save_profile',nested)
            assert [e['path'] for e in connect('nested')['entries']] == ['/inside']
            engine.call('disconnect')
            denied=profile(id='denied');denied['share']='denied'
            engine.call('save_profile',denied)
            error=engine.call('connect',{'id':'denied'},failure=True)
            assert 'c0000022' in error.lower(), 'permission error not classified'
            bad=profile(id='bad');bad['password']='Wrong-Synthetic-Password'
            engine.call('save_profile',bad)
            error=engine.call('connect',{'id':'bad'},failure=True)
            assert password not in error and bad['password'] not in error and user not in error
            checks.append('separate_share_root_binding_and_auth_permission_errors_redacted')
            # A saved configuration can retain its own credential, never another root/account.
            engine.call('save_profile',dict(pub,name='Renamed local connection'))
            connect()
            engine.call('disconnect')
            for field,value in [('username','other-disposable-user'),('share','other'),('root','/Mixed Case'),('server','localhost'),('port','1445'),('domain','OTHER')]:
                changed=dict(pub); changed[field]=value
                engine.call('save_profile',changed,failure=True)
            checks.append('IPC_identity_edits_do_not_inherit_credentials')
            stop(); start('SMB2_10')
            engine.call('connect',{'id':'smb'},failure=True) # encryption remains mandatory
            signed=profile(id='signed',signed_only=True)
            engine.call('save_profile',signed)
            connect('signed')
            engine.call('download',{'path':'/你好 space.txt','output':str(root/'signed-output')})
            assert (root/'signed-output').read_bytes() == content
            checks.append('SMB210_signed_only_explicit_and_encryption_downgrade_refused')
            engine.call('disconnect')
            # A real TCP peer that accepts but never negotiates: cancellation
            # must not wait for the library's socket/SMB timeout.
            accepted, release = threading.Event(), threading.Event()
            with socket.socket() as listener:
                listener.bind(('127.0.0.1',0));listener.listen()
                listener.settimeout(5)
                def blackhole():
                    try:
                        with listener.accept()[0]:
                            accepted.set(); release.wait(5)
                    except OSError: pass
                thread=threading.Thread(target=blackhole)
                thread.start()
                try:
                    stalled=profile(id='stalled');stalled['port']=str(listener.getsockname()[1])
                    engine.call('save_profile',stalled)
                    engine.counter+=1; request=engine.counter
                    engine.process.stdin.write(json.dumps(dict(id=request,op='connect',args={'id':'stalled'}))+'\n')
                    engine.process.stdin.flush()
                    assert accepted.wait(5), 'protocol child did not connect to synthetic peer'
                    started=time.monotonic()
                    engine.counter+=1
                    engine.process.stdin.write(json.dumps(dict(id=engine.counter,op='cancel',args={'id':request}))+'\n')
                    engine.process.stdin.flush()
                    while True:
                        msg=engine.events.get(timeout=3)
                        if not msg.get('event') and msg.get('id') == request:
                            assert not msg['ok'] and time.monotonic()-started < 3
                            break
                finally:
                    release.set(); thread.join(timeout=6)
            connect('signed')
            checks.append('stalled_negotiation_cancelled_within_three_seconds')
            assert not any(password in str(value) for value in engine.call('status').values())
            print(json.dumps({'fixture_reparse_advertisement_override':args.advertise_test_reparse_capability,'fixture_follow_symlinks':args.follow_test_symlinks,'passed':checks,'unverified':['Windows','physical NAS','Kerberos','Windows junction/DFS interoperability','SMB 2.0.2/3.0/3.0.2 dialects']},indent=2))
        finally:
            if engine: engine.close()
            stop()


if __name__ == '__main__':
    p=argparse.ArgumentParser()
    p.add_argument('--smbd',required=True)
    p.add_argument('--engine',required=True)
    p.add_argument('--peer')
    p.add_argument('--advertise-test-reparse-capability',action='store_true',
                   help='Synthetic server only: explicitly declare tested reparse capability using Samba share:fake_fscaps; not a production workaround')
    p.add_argument('--expect-reparse-rejection',action='store_true')
    p.add_argument('--follow-test-symlinks',action='store_true',help='Synthetic negative reproducer: permit server-transparent Unix links; not a supported positive policy')
    run(p.parse_args())
