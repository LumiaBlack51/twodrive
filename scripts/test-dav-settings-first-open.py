#!/usr/bin/env python3
"""Measure the real sidebar handler in separate processes/private Xvfb sessions.

The fixture has synthetic iCloud status and never starts an engine. Times cover
the next Flutter framework frame, with engine build/raster timings recorded
separately; this is not physical compositor input-to-photon measurement.
"""
from pathlib import Path
import argparse,json,os,subprocess,tempfile

def sample(args, case):
    with tempfile.TemporaryDirectory(prefix='td-settings-first-open-') as directory:
        root=Path(directory);(root/'runtime').mkdir(mode=0o700)
        with (root/'display.log').open('w') as log:
            display=subprocess.Popen([str(args.xvfb.resolve()),'-displayfd','1','-screen','0',
                '1920x1080x24','-nolisten','tcp','-ac'],stdout=subprocess.PIPE,stderr=log,text=True)
        try:
            number=display.stdout.readline().strip();assert number.isdigit()
            env=dict(os.environ,DISPLAY=':'+number,GDK_BACKEND='x11',XDG_RUNTIME_DIR=str(root/'runtime'),
                XDG_CONFIG_HOME=str(root/'config'),XDG_DATA_HOME=str(root/'data'),XDG_CACHE_HOME=str(root/'cache'),
                XDG_CURRENT_DESKTOP='Test',GTK_THEME='Adwaita',LIBGL_ALWAYS_SOFTWARE='1',TWODRIVE_TRAY_BACKEND='sni')
            for key in ['WAYLAND_DISPLAY','DBUS_SESSION_BUS_ADDRESS','LD_PRELOAD']:env.pop(key,None)
            if args.font_probe:env['LD_PRELOAD']=str(args.font_probe.resolve())
            result=subprocess.run(['dbus-run-session','--',str(args.bundle.resolve()/'twodrive_dav'),
                '--scale',str(args.scale)],env=env,capture_output=True,text=True,timeout=45)
            args.output.with_suffix('.case-'+str(case)+'.log').write_text(result.stdout+'\n'+result.stderr)
            assert result.returncode==0, result.stderr[-1000:]
            assert 'SETTINGS_FIRST_OPEN_DONE' in result.stdout
            rows=[json.loads(line.removeprefix('SETTINGS_FIRST_OPEN ')) for line in result.stdout.splitlines()
                  if line.startswith('SETTINGS_FIRST_OPEN ')]
            assert len(rows)==3
            return {'first_framework_frame_ms':rows[0]['framework_frame_us']/1000,
                'later_framework_frames_ms':[row['framework_frame_us']/1000 for row in rows[1:]],
                'max_build_ms':max([n/1000 for row in rows for n in row['build_us']],default=0),
                'max_raster_ms':max([n/1000 for row in rows for n in row['raster_us']],default=0),
                'fontconfig_calls':sum(line.startswith('FONT_') for line in result.stderr.splitlines()),'samples':rows}
        finally:
            display.terminate();display.wait(timeout=5)

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bundle',type=Path,required=True)
    parser.add_argument('--xvfb',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--font-probe',type=Path)
    parser.add_argument('--scale',type=float,default=1.5625)
    parser.add_argument('--rounds',type=int,default=5)
    parser.add_argument('--budget-ms',type=float)
    args=parser.parse_args()
    results=[]
    for case in range(args.rounds):
        result=sample(args,case);results.append(result)
        print(json.dumps({'case':case,**{key:value for key,value in result.items() if key!='samples'}}),flush=True)
    report={'measurement':'sidebar handler to next Flutter framework frame; private Xvfb and D-Bus',
        'synthetic_provider':'icloud','text_scale':args.scale,'fresh_processes':len(results),'results':results}
    args.output.write_text(json.dumps(report,indent=2)+'\n')
    if args.budget_ms is not None:
        assert all(row['first_framework_frame_ms']<args.budget_ms for row in results), 'First settings frame exceeded budget'

if __name__=='__main__':main()
