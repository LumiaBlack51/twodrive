#!/usr/bin/env python3
"""Native GTK transition regression in private Xvfb and private D-Bus.

Build apps/dav/test/window_transition_fixture.dart for old and fixed sources.
Compile fixtures/gtk_transition_probe.c with `cc -shared -fPIC ...
$(pkg-config --cflags gtk+-3.0) -ldl`. The probe only observes the fixture's
GTK calls; no input or window operations are sent to the user's display.
"""
from pathlib import Path
import argparse,json,os,signal,subprocess,sys,tempfile,time

def stop(child):
 if child and child.poll() is None:
  os.killpg(child.pid,signal.SIGTERM)
  try:child.wait(timeout=5)
  except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait(timeout=5)

def inside(args):
 root=Path(args.root);shell=None
 assert os.environ['XDG_RUNTIME_DIR']==str(root/'runtime')
 assert os.environ.get('DISPLAY')==args.display
 try:
  if args.gnome:
   with (root/'shell.log').open('w') as log:
    shell=subprocess.Popen(['gnome-shell','--x11','--display',args.display,'--mode','user','--sm-disable'],stdout=log,stderr=log,start_new_session=True)
   deadline=time.monotonic()+35
   while True:
    assert shell.poll() is None,'Private compositor failed to start'
    r=subprocess.run(['gdbus','call','--session','--dest','org.freedesktop.DBus','--object-path','/org/freedesktop/DBus','--method','org.freedesktop.DBus.NameHasOwner','org.gnome.Shell'],capture_output=True,text=True)
    if 'true' in r.stdout:break
    assert time.monotonic()<deadline,'Private compositor not ready'
    time.sleep(.2)
  env=dict(os.environ,LD_PRELOAD=str(Path(args.probe).resolve()),TWODRIVE_TRANSITION_TEST_MARKER=str(root/'changing'))
  run=subprocess.run([str(Path(args.program).resolve()),'--fixture-root',str(root)],env=env,capture_output=True,text=True,timeout=100)
  (root/'client.log').write_text(run.stdout+'\n'+run.stderr)
  assert run.returncode==0,run.stderr[-2000:]
  line=next(line for line in run.stdout.splitlines() if line.startswith('TRANSITION_RESULT '))
  result=json.loads(line.removeprefix('TRANSITION_RESULT '))
  result['exposed_operations']=[line.removeprefix('TRANSITION_EXPOSED ') for line in run.stderr.splitlines() if line.startswith('TRANSITION_EXPOSED ')]
  result['environment']='private GNOME / Xvfb / GTK X11' if args.gnome else 'private Xvfb / GTK X11'
  print('REPORT '+json.dumps(result),flush=True)
 finally:stop(shell)

def main():
 p=argparse.ArgumentParser(description=__doc__)
 for name in ['old','new','xvfb','probe','output','program','root','display']:p.add_argument('--'+name)
 p.add_argument('--inside',action='store_true');p.add_argument('--gnome',action='store_true');args=p.parse_args()
 if args.inside:return inside(args)
 assert all([args.old,args.new,args.xvfb,args.probe,args.output])
 reports={}
 with tempfile.TemporaryDirectory(prefix='td-transition-') as tmp:
  root=Path(tmp)
  for label,program in [('old',args.old),('fixed',args.new)]:
   case=root/label;case.mkdir();(case/'runtime').mkdir(mode=0o700)
   with (case/'display.log').open('w') as log:
    display=subprocess.Popen([str(Path(args.xvfb).resolve()),'-displayfd','1','-screen','0','1920x1080x24','-nolisten','tcp','-ac'],stdout=subprocess.PIPE,stderr=log,text=True,start_new_session=True)
   try:
    number=display.stdout.readline().strip();assert number.isdigit()
    env=dict(os.environ,DISPLAY=':'+number,GDK_BACKEND='x11',XDG_RUNTIME_DIR=str(case/'runtime'),XDG_CONFIG_HOME=str(case/'config'),XDG_DATA_HOME=str(case/'data'),XDG_CACHE_HOME=str(case/'cache'),XDG_CURRENT_DESKTOP='GNOME' if args.gnome else 'Test',XDG_SESSION_TYPE='x11',LIBGL_ALWAYS_SOFTWARE='1',TWODRIVE_TRAY_BACKEND='sni')
    for key in ['DBUS_SESSION_BUS_ADDRESS','WAYLAND_DISPLAY','LD_PRELOAD']:env.pop(key,None)
    command=['dbus-run-session','--',sys.executable,str(Path(__file__).resolve()),'--inside','--root',str(case),'--display',':'+number,'--program',str(Path(program).resolve()),'--probe',str(Path(args.probe).resolve())]
    if args.gnome:command.append('--gnome')
    run=subprocess.run(command,env=env,capture_output=True,text=True,timeout=145)
    if run.returncode:
     details=(case/'client.log').read_text()[-2500:] if (case/'client.log').exists() else ''
     raise AssertionError((label,run.stderr[-2000:],details))
    line=next(line for line in run.stdout.splitlines() if line.startswith('REPORT '))
    reports[label]=json.loads(line.removeprefix('REPORT '));print(label+': '+json.dumps(reports[label]),flush=True)
   finally:stop(display)
 Path(args.output).write_text(json.dumps(reports,indent=2)+'\n')
 assert reports['old']['exposed_operations'],'Old runner did not reproduce visible reconfiguration'
 assert not reports['fixed']['exposed_operations'],'Fixed runner still exposes reconfiguration'
 assert reports['old']['pages']==[4,4,4] and reports['fixed']['pages']==[5,5,5]
 assert reports['fixed']['final_visible'] and reports['fixed']['final_opacity']==1

if __name__=='__main__':main()
