#!/usr/bin/env python3
"""Default path names and collision avoidance via isolated IPC; no cloud calls."""
import argparse,importlib.util,json,os,tempfile
from pathlib import Path
spec=importlib.util.spec_from_file_location('desktop',Path(__file__).with_name('test-dav-desktop.py'))
desktop=importlib.util.module_from_spec(spec);spec.loader.exec_module(desktop)
def run(args):
    with tempfile.TemporaryDirectory(prefix='td-readable-path-') as directory:
        root=Path(directory);home=root/'home';home.mkdir()
        previous=os.environ.get('HOME');os.environ['HOME']=str(home)
        try:engine=desktop.Engine(args.engine,args.peer,root)
        finally:
            if previous is None:os.environ.pop('HOME',None)
            else:os.environ['HOME']=previous
        try:
            def save(id,kind,**extra):
                engine.call('save_profile',{'id':id,'name':'Generated account','kind':kind,'writable':False,'network_route':'system',**extra})
                return next(p for p in engine.call('profiles')['profiles'] if p['id']==id)
            base=home/'TwoDrive Development'
            first=save('p-random-first','icloud')
            assert Path(first['mount_path'])==base/'iCloud Drive','Random account id leaked into default path'
            second=save('p-random-second','icloud')
            assert Path(second['mount_path'])==base/'iCloud Drive (2)'
            base.mkdir(exist_ok=True);occupied=base/'百度网盘';occupied.mkdir();(occupied/'keep.txt').write_bytes(b'existing user data')
            dangling=base/'百度网盘 (2)';dangling.symlink_to(root/'missing')
            third=save('p-random-third','baidu')
            assert Path(third['mount_path'])==base/'百度网盘 (3)'
            custom=root/'custom-folder';custom.mkdir()
            assert Path(save('custom','icloud',mount_path=str(custom))['mount_path'])==custom
            assert (occupied/'keep.txt').read_bytes()==b'existing user data' and dangling.is_symlink()
        finally:engine.close()
    print(json.dumps({'passed':True,'cloud_requests':False,'checks':['readable_provider_names','multiple_accounts_separate','existing_data_and_dangling_links_not_claimed','explicit_custom_path_preserved']}),flush=True)
if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--engine',required=True);parser.add_argument('--peer',required=True);run(parser.parse_args())
