#!/usr/bin/env python3
"""Real WinFsp writes against disposable loopback servers; no user's cloud/state."""
import argparse,importlib.util,json,os,subprocess,tempfile,time
from pathlib import Path
spec=importlib.util.spec_from_file_location('desktop',Path(__file__).with_name('test-dav-desktop.py'))
desktop=importlib.util.module_from_spec(spec);spec.loader.exec_module(desktop)
def wait_for(test,seconds=25):
    until=time.monotonic()+seconds
    while time.monotonic()<until:
        try:
            if test(): return
        except OSError:
            pass
        time.sleep(.1)
    raise AssertionError('synthetic filesystem operation did not settle')
def run(args):
    report={'scope':'disposable accounts and real WinFsp only','cloud_requests':0,'native_mount_verified':False,'checks':{}}
    servers=[];engine=None
    with tempfile.TemporaryDirectory(prefix='td-editable-') as scratch:
        root=Path(scratch)
        try:
            for name in ('a','b'):
                share=root/('share-'+name);share.mkdir();(share/'file.txt').write_bytes(('original '+name).encode())
                server=subprocess.Popen([args.peer,'--state',str(root/('server-'+name)),'dav-serve','--root',str(share),'--listen','127.0.0.1:0','--write'],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True,encoding='utf-8',creationflags=subprocess.CREATE_NO_WINDOW)
                servers.append(server);url=server.stdout.readline().strip().removeprefix('WebDAV: ')
                credentials=json.loads((root/('server-'+name)/'credentials.json').read_text(encoding='utf-8'))
                if engine is None:engine=desktop.Engine(args.engine,args.peer,root)
                engine.call('save_profile',{'id':name,'kind':'dav','name':'Generated '+name,'url':url,**credentials,'network_route':'system','writable':False,'mount_path':str(root/'mounts'/name)})
                engine.call('connect',{'id':name})
            profiles={p['id']:p for p in engine.call('profiles')['profiles']}
            owner=lambda name:{'profile':name,'revision':profiles[name]['revision']}
            assert all(p['writable'] and 'write_locked' not in p for p in profiles.values())
            assert 'write_lock' not in engine.call('status')
            report['checks']['legacy_readonly_profiles_edit_without_password']=True
            if not engine.call('status')['mount_support']['driver_installed'] and args.allow_missing_driver:
                profiles={p['id']:p for p in engine.call('profiles')['profiles']}
                source=root/'gui-fixture.txt';source.write_bytes(b'GUI permission fixture')
                engine.call('upload',dict(owner('a'),path='/gui.txt',source=str(source)))
                assert (root/'share-a'/'gui.txt').read_bytes()==source.read_bytes()
                engine.call('mount',owner('a'),failure=True)
                report['checks']['gui_write_and_missing_driver_rejection']=True
                report['native_skip_reason']='WinFsp not installed on this runner'
                engine.close();engine=None
                Path(args.report).write_text(json.dumps(report,indent=2),encoding='utf-8');print(json.dumps(report));return
            for name in ('a','b'):engine.call('mount',owner(name))
            engine.active_profile='a';engine.active_revision=profiles['a']['revision']
            assert engine.call('status')['profile']=='b','mounting a parked account must preserve browsing selection'
            assert {m['profile'] for m in engine.call('status')['mounts']}=={'a','b'}
            mount=root/'mounts'/'a'
            with (mount/'created.txt').open('w+b') as file:
                file.write(b'0123456789');file.seek(3);file.write(b'EDIT');file.flush()
            wait_for(lambda:(root/'share-a'/'created.txt').exists() and (root/'share-a'/'created.txt').read_bytes()==b'012EDIT789')
            assert (root/'share-b'/'file.txt').read_bytes()==b'original b'
            with (mount/'file.txt').open('r+b') as file:
                file.seek(0);file.write(b'changed A!');file.truncate();file.flush()
            wait_for(lambda:(root/'share-a'/'file.txt').read_bytes()==b'changed A!')
            (mount/'folder').mkdir();(mount/'created.txt').rename(mount/'folder'/'renamed.txt')
            wait_for(lambda:(root/'share-a'/'folder'/'renamed.txt').exists())
            (mount/'folder'/'renamed.txt').unlink();(mount/'folder').rmdir()
            wait_for(lambda:not (root/'share-a'/'folder').exists())
            # Directories need genuine identity metadata, including empty and
            # nested cases; a file-only MOVE regression cannot cover this.
            (mount/'empty-dir').mkdir()
            wait_for(lambda:(root/'share-a'/'empty-dir').is_dir())
            assert (mount/'empty-dir').stat().st_mtime>1700000000, 'new directory has a placeholder date'
            (mount/'empty-dir').rename(mount/'empty-renamed')
            wait_for(lambda:(root/'share-a'/'empty-renamed').is_dir() and not (root/'share-a'/'empty-dir').exists())
            (mount/'empty-renamed').rmdir()
            (mount/'tree-dir').mkdir()
            (mount/'tree-dir'/'child.txt').write_bytes(b'child survives directory rename')
            wait_for(lambda:(root/'share-a'/'tree-dir'/'child.txt').exists())
            wait_for(lambda:engine.call('status')['overall']['pending']==0)
            (mount/'tree-dir').rename(mount/'tree-renamed')
            wait_for(lambda:(root/'share-a'/'tree-renamed'/'child.txt').exists() and not (root/'share-a'/'tree-dir').exists())
            assert (mount/'tree-renamed'/'child.txt').read_bytes()==b'child survives directory rename'
            (mount/'tree-renamed'/'child.txt').unlink();(mount/'tree-renamed').rmdir()
            wait_for(lambda:not (root/'share-a'/'tree-renamed').exists())
            report['checks']['native_empty_and_populated_directory_rename_preserves_children']=True
            report['checks']['native_new_directory_exposes_recent_time']=True
            source=root/'gui-upload.txt';source.write_bytes(b'GUI writes work without a local permission step')
            engine.call('upload',dict(owner('a'),path='/gui.txt',source=str(source)))
            assert (root/'share-a'/'gui.txt').read_bytes()==source.read_bytes()
            report['native_mount_verified']=True
            report['checks']['native_seek_edit_create_rename_delete_and_gui_upload']=True
            report['checks']['other_account_mount_and_content_preserved']=True
            wait_for(lambda:engine.call('status')['overall']['pending']==0)
            engine.close();engine=desktop.Engine(args.engine,args.peer,root)
            for name in ('a','b'):
                engine.call('resume',{'id':name,'revision':profiles[name]['revision']})
                engine.call('mount',owner(name))
                assert engine.call('status')['capabilities']['upload']
                (root/'mounts'/name/'restart-created.txt').write_bytes(b'editable after restart')
                wait_for(lambda name=name:(root/('share-'+name)/'restart-created.txt').exists())
            assert (root/'mounts'/'a'/'file.txt').read_bytes()==b'changed A!'
            report['checks']['both_accounts_restart_editable_without_password']=True
            # Crash recovery must retain the editor's original version claim.
            profiles={p['id']:p for p in engine.call('profiles')['profiles']}
            assert (root/'mounts'/'a'/'file.txt').read_bytes()==b'changed A!'
            (root/'share-a'/'file.txt').write_bytes(b'external change')
            try:(root/'mounts'/'a'/'file.txt').write_bytes(b'my conflicting edit')
            except OSError:pass
            wait_for(lambda:engine.call('status')['overall']['pending']>0)
            assert (root/'share-a'/'file.txt').read_bytes()==b'external change'
            engine.call('unmount',owner('a'),failure=True)
            engine.call('quit',failure=True)
            assert len(engine.call('status')['connections'])==2
            recovery=engine.call('write_recovery',{'profile':'a'})['directories']
            assert any(p.read_bytes()==b'my conflicting edit' for d in recovery for p in Path(d).rglob('*.content'))
            engine.process.terminate();engine.process.wait(timeout=10);engine.close();engine=None
            wait_for(lambda:all(not (root/'mounts'/name).exists() for name in ('a','b')))
            engine=desktop.Engine(args.engine,args.peer,root)
            for name in ('a','b'):
                engine.call('resume',{'id':name,'revision':profiles[name]['revision']});engine.call('mount',owner(name))
            assert (root/'mounts'/'a'/'file.txt').read_bytes()==b'external change'
            assert any(p.read_bytes()==b'my conflicting edit' for d in recovery for p in Path(d).rglob('*.content'))
            assert engine.call('status')['overall']['pending']>0
            report['checks']['conflict_blocks_teardown_and_crash_restart_retains_local_edit_without_overwriting']=True
            engine.process.terminate();engine.process.wait(timeout=10);engine.close();engine=None
            wait_for(lambda:all(not (root/'mounts'/name).exists() for name in ('a','b')))
            engine=desktop.Engine(args.engine,args.peer,root)
            # An explicit account change must not replay the former account's
            # dirty cache, even when the virtual filename is identical.
            account_b=json.loads((root/'server-b'/'credentials.json').read_text(encoding='utf-8'))
            old_profile=profiles['a']
            engine.call('save_profile',{'id':'a','kind':'dav','name':'Changed generated account','url':profiles['b']['url'],**account_b,'network_route':'system','writable':True,'mount_path':old_profile['mount_path']})
            engine.call('connect',{'id':'a'})
            profiles={p['id']:p for p in engine.call('profiles')['profiles']}
            engine.call('mount',owner('a'))
            assert (root/'mounts'/'a'/'file.txt').read_bytes()==b'original b'
            assert (root/'share-b'/'file.txt').read_bytes()==b'original b'
            assert engine.call('status')['overall']['pending']==0
            assert any(p.read_bytes()==b'my conflicting edit' for d in recovery for p in Path(d).rglob('*.content'))
            report['checks']['changed_account_binding_uses_new_cache_and_never_replays_old_edit']=True
            engine.close();engine=None
            wait_for(lambda:all(not (root/'mounts'/name).exists() for name in ('a','b')))
            report['checks']['clean_quit_unmounts_both_accounts']=True
        finally:
            if engine:engine.close()
            for server in servers:
                server.terminate();server.wait(timeout=10)
    Path(args.report).write_text(json.dumps(report,indent=2),encoding='utf-8')
    print(json.dumps(report))
if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--engine',required=True);p.add_argument('--peer',required=True);p.add_argument('--report',required=True);p.add_argument('--allow-missing-driver',action='store_true');run(p.parse_args())
