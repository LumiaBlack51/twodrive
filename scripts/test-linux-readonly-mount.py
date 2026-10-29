#!/usr/bin/env python3
"""Actual Linux FUSE with two owner-granted read-only peers; no cloud/user state."""
import argparse,concurrent.futures,hashlib,importlib.util,json,os,re,shutil,signal,subprocess,sys,tempfile,time
from pathlib import Path
spec=importlib.util.spec_from_file_location('desktop',Path(__file__).with_name('test-dav-desktop.py'))
desktop=importlib.util.module_from_spec(spec);spec.loader.exec_module(desktop)

def mounted(path):
    # stat/ismount can return false on an ENOTCONN FUSE that remains attached.
    for line in Path('/proc/self/mountinfo').read_text().splitlines():
        field=line.split()[4]
        target=re.sub(r'\\([0-7]{3})',lambda match:chr(int(match.group(1),8)),field)
        if target==str(path):return True
    return False

def wait_unmounted(path,timeout=15):
    start=time.monotonic()
    while mounted(path):
        assert time.monotonic()-start<timeout,'Owned FUSE mount survived shutdown'
        time.sleep(.1)

def children(pid):
    result=set()
    try:
        for task in Path(f'/proc/{pid}/task').iterdir():
            for value in (task/'children').read_text().split():
                child=int(value);result.add(child);result.update(children(child))
    except OSError:pass
    return result

def alive(pid):
    try:return Path(f'/proc/{pid}/stat').read_text().split(') ',1)[1][0]!='Z'
    except OSError:return False

def run(args):
    assert sys.platform=='linux' and Path('/dev/fuse').exists(),'Requires actual Linux /dev/fuse'
    checks={};servers=[];engine=None;mounts=[]
    name='你好 #%.bin'
    payloads={'a':bytes(range(251))*8500,'b':b'account-b\x00\xff'*719}
    with tempfile.TemporaryDirectory(prefix='td-linux-ro-mount-') as directory:
        root=Path(directory)
        try:
            engine=desktop.Engine(args.engine,args.peer,root)
            owners={}
            for account,payload in payloads.items():
                share=root/('share-'+account);share.mkdir();(share/name).write_bytes(payload)
                invitation=root/('invite-'+account+'.json')
                server=subprocess.Popen([args.peer,'--state',str(root/('server-'+account)),'serve',
                    '--root',str(share),'--no-relay','--invite-file',str(invitation)],
                    stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
                servers.append(server)
                deadline=time.monotonic()+30
                while not invitation.exists():
                    assert server.poll() is None, 'disposable read-only peer exited'
                    assert time.monotonic()<deadline, 'invitation not created'
                    time.sleep(.1)
                ticket=json.loads(invitation.read_text())
                target=root/'views'/account
                if account=='a':target.mkdir(parents=True)
                mounts.append(target)
                engine.call('save_profile',{'id':account,'name':'Disposable '+account,'kind':'peer',
                    'invitation_file':str(invitation),'expected_device':ticket['address']['id'],
                    'network_route':'system','writable':False,'mount_path':str(target)})
                public=next(p for p in engine.call('profiles')['profiles'] if p['id']==account)
                owners[account]={'profile':account,'revision':public['revision']}
            engine.call('connect',{'id':'a'})
            engine.call('mount',owners['a'],timeout=90)
            assert mounted(mounts[0])
            preflight=engine.call('local_preflight',{'schema_version':1,'source_path':str(mounts[0]/name),'source_kind':'ordinary_directory'})
            assert preflight['decision']=='unsupported_virtual_mount' and preflight['content_read'] is False
            assert (mounts[0]/name).read_bytes()==payloads['a']
            with (mounts[0]/name).open('rb') as file:
                file.seek(777);assert file.read(4097)==payloads['a'][777:777+4097]
            copied=root/'copied-from-native-mount.bin'
            shutil.copyfile(mounts[0]/name,copied)
            assert copied.read_bytes()==payloads['a']
            checks['native_copy_to_local_preserves_complete_verified_bytes']=True
            # A short visit elsewhere must not turn a cached listing into a
            # blocking remote operation. Pause only our disposable server.
            os.kill(servers[0].pid,signal.SIGSTOP)
            probe=None;stalled=False
            try:
                time.sleep(6)
                probe=subprocess.Popen([sys.executable,'-c',
                    'import os,sys; assert len(os.listdir(sys.argv[1])) == 1',str(mounts[0])],
                    stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
                try:probe.communicate(timeout=2)
                except subprocess.TimeoutExpired:stalled=True
            finally:
                os.kill(servers[0].pid,signal.SIGCONT)
                if probe is not None:
                    try:probe.communicate(timeout=20)
                    except subprocess.TimeoutExpired:
                        probe.kill();probe.communicate(timeout=10)
            assert probe is not None and probe.returncode==0 and not stalled, 'Cached navigation waited for a slow remote after only six seconds'
            checks['native_return_to_cached_directory_avoids_slow_remote']=True
            try:(mounts[0]/'forbidden-write').write_bytes(b'must not upload')
            except OSError:pass
            else:raise AssertionError('Read-only FUSE accepted a write')
            assert not (root/'share-a/forbidden-write').exists()
            checks['actual_fuse_unicode_listing_exact_bytes_seek_and_write_denial']=True
            engine.call('connect',{'id':'b'})
            assert (mounts[0]/name).read_bytes()==payloads['a']
            engine.call('connect',{'id':'a'})
            engine.call('mount',dict(owners['b'],revision='stale'),failure=True)
            # Historical engines ignore the target and fail here because A is
            # already mounted. Keep this step before new status-schema checks.
            engine.call('mount',owners['b'],timeout=90)
            status=engine.call('status')
            assert status['profile']=='a'
            assert {mount['profile'] for mount in status['mounts']}=={'a','b'}
            assert all(mount['readonly'] is True for mount in status['mounts'])
            assert status['mount_support']['platform']=='linux'
            with concurrent.futures.ThreadPoolExecutor() as pool:
                futures=[pool.submit((mount/name).read_bytes) for mount in mounts]
                assert [future.result() for future in futures]==list(payloads.values())
            checks['two_native_mounts_targeted_without_switch_keep_account_bytes_separate']=True
            engine.call('unmount',dict(owners['b'],revision='stale'),failure=True)
            engine.call('unmount',owners['b']);wait_unmounted(mounts[1])
            assert not mounts[1].exists()
            assert engine.call('status')['profile']=='a' and (mounts[0]/name).read_bytes()==payloads['a']
            owned=children(engine.process.pid)
            engine.call('mount',owners['b'],timeout=90)
            checks['independent_unmount_revision_guards_and_remount']=True
            engine.call('quit');engine.process.stdin.close();engine.process.wait(timeout=20)
            for mount in mounts:wait_unmounted(mount)
            assert mounts[0].is_dir(),'Pre-existing target was removed'
            time.sleep(.3);assert not any(alive(pid) for pid in owned),'Owned child survived normal quit'
            checks['quit_unmounts_both_reaps_children_and_preserves_existing_empty_target']=True
            engine.close();engine=None
            engine=desktop.Engine(args.engine,args.peer,root)
            engine.call('connect',{'id':'a'});engine.call('mount',owners['a'],timeout=90)
            assert (mounts[0]/name).read_bytes()==payloads['a']
            owned=children(engine.process.pid)
            engine.process.kill();engine.process.wait(timeout=10)
            wait_unmounted(mounts[0])
            deadline=time.monotonic()+10
            while any(alive(pid) for pid in owned):
                assert time.monotonic()<deadline,'Owned mount descendants survived parent death'
                time.sleep(.1)
            checks['engine_crash_unmounts_fuse_and_reaps_owned_descendants']=True
            engine.close();engine=None
            engine=desktop.Engine(args.engine,args.peer,root)
            engine.call('connect',{'id':'a'});engine.call('mount',owners['a'],timeout=90)
            components=[]
            for pid in children(engine.process.pid):
                try:
                    if b'rcd' in Path(f'/proc/{pid}/cmdline').read_bytes().split(b'\0'):components.append(pid)
                except OSError:pass
            assert len(components)==1
            os.kill(components[0],signal.SIGKILL)
            wait_unmounted(mounts[0])
            assert engine.process.poll() is None
            engine.call('quit');engine.process.stdin.close();engine.process.wait(timeout=20)
            checks['unexpected_owned_component_death_detaches_verified_fuse_without_stat']=True
        finally:
            if engine:engine.close()
            for mount in mounts:
                if mounted(mount):subprocess.run(['fusermount3','-u',str(mount)],check=True)
            for server in servers:
                if server.poll() is None:server.terminate();server.wait(timeout=10)
    report={'scope':'actual Linux FUSE over two owner-granted read-only peers; zero cloud requests',
        'native_mount_verified':True,'checks':checks,'sha256':{key:hashlib.sha256(value).hexdigest() for key,value in payloads.items()},'passed':True}
    if args.report:args.report.write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(report,indent=2),flush=True)

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--engine',required=True);parser.add_argument('--peer',required=True);parser.add_argument('--report',type=Path)
    run(parser.parse_args())
