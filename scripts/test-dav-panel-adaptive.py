#!/usr/bin/env python3
"""Adaptive GTK panel test with a synthetic non-reserving dock on a private desktop.

Never activates the user's desktop, moves its pointer, or uses cloud credentials.
Requires GNOME 45+, the Zorin taskbar GSettings schema, AppIndicators, and Xvfb.
HiDPI host-anchor regression: add --host-anchor --gdk-scale 2 --screen 2880x1800.
The Shell stays at scale 1; only the GTK client is scaled, exercising the protocol
boundary rather than scaling the whole test desktop and hiding a double scale.
The synthetic dock reserves no workarea; all settings/input stay on this private desktop.
"""
import argparse
import ast
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

REPO = Path(__file__).resolve().parent.parent
UUID = 'twodrive-tray-anchor@twodrive'


def call(method, *args, dest='org.gnome.Shell', path='/app/twodrive/TrayAnchor'):
    r = subprocess.run(['gdbus', 'call', '--session', '--dest', dest, '--object-path',
                        path, '--method', method, *map(str, args)],
                       capture_output=True, text=True, timeout=4, check=True)
    return ast.literal_eval(r.stdout.replace('true', 'True').replace('false', 'False'))


def stop(child):
    if child and child.poll() is None:
        os.killpg(child.pid, signal.SIGTERM)
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait(timeout=5)


def inside(args):
    root = args.root
    assert str(root).startswith('/tmp/td-panel-isolated-')
    assert os.environ['XDG_RUNTIME_DIR'] == str(root/'runtime')
    indicators = ['zorin-appindicator@zorinos.com', 'ubuntu-appindicators@ubuntu.com',
                  'appindicatorsupport@rgcjonas.gmail.com']
    host = next(i for i in indicators if (Path('/usr/share/gnome-shell/extensions')/i).exists())
    subprocess.run(['gsettings', 'set', 'org.gnome.shell', 'enabled-extensions', repr([host, UUID])], check=True)
    for key,value in [('panel-sizes',json.dumps({'0':args.dock_size})),('panel-positions',json.dumps({'0':'BOTTOM'})),('panel-margin',str(args.dock_margin))]:
        subprocess.run(['gsettings','set','org.gnome.shell.extensions.zorin-taskbar',key,value],check=True)
    shell = display = app = None
    try:
        shell_env = dict(os.environ, GDK_SCALE='1', GDK_DPI_SCALE='1')
        with (root/'display.log').open('w') as log:
            display = subprocess.Popen([args.xvfb, '-displayfd', '1', '-screen', '0',
                args.screen+'x24', '-nolisten', 'tcp'], stdout=subprocess.PIPE, stderr=log,
                text=True, start_new_session=True)
        number = display.stdout.readline().strip()
        assert number.isdigit()
        shell_env['DISPLAY'] = ':'+number
        command = ['gnome-shell', '--x11', '--display', ':'+number, '--mode', 'user', '--sm-disable']
        with (root/'shell.log').open('w') as log:
            shell = subprocess.Popen(command, env=shell_env, stdout=log, stderr=log,
                                     start_new_session=True)
        deadline = time.monotonic()+40
        while True:
            assert shell.poll() is None, 'isolated compositor exited; inspect shell.log'
            try:
                call('app.twodrive.TrayAnchor1.GetAnchor', 0)
                break
            except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
                assert time.monotonic()<deadline, 'isolated helper did not load'
                time.sleep(.2)
        call('app.twodrive.TrayAnchor1.LeaveOverview')
        time.sleep(.8)
        env = dict(shell_env, GDK_BACKEND='x11', GDK_SCALE=str(args.gdk_scale), TWODRIVE_TRAY_BACKEND='sni', GTK_USE_PORTAL='0', NO_AT_BRIDGE='1')
        with (root/'client.log').open('w') as log:
            app = subprocess.Popen([str(args.bundle/'twodrive_dav'), '--background', '--state',
                str(root/'state'), '--engine', str(args.engine), '--peer-bin', str(args.peer)],
                env=env, stdout=log, stderr=log, start_new_session=True)
        tray_name=f'org.kde.StatusNotifierItem-{app.pid}-1'
        while True:
            assert app.poll() is None, 'isolated client exited'
            if call('org.freedesktop.DBus.NameHasOwner',tray_name,
                    dest='org.freedesktop.DBus',path='/org/freedesktop/DBus')[0]: break
            assert time.monotonic()<deadline, 'tray did not register'
            time.sleep(.2)
        sw,sh=map(int,args.screen.split('x'))
        if args.host_anchor:
            ready=time.monotonic()+6
            while not call('app.twodrive.TrayAnchor1.GetAnchor',app.pid)[0]:
                assert time.monotonic()<ready, 'host-anchor scenario has no mapped indicator'
                time.sleep(.1)
        point=(sw//2,sh-args.dock_size//2-args.dock_margin)
        dock_top=sh-args.dock_size-2*args.dock_margin
        panel_bounds=[]
        observations = []
        assert not call('app.twodrive.TrayAnchor1.GetPanel', app.pid)[0]
        for _ in range(3):
            time.sleep(.4)
            call('org.kde.StatusNotifierItem.Activate', *point,
                 dest=f'org.kde.StatusNotifierItem-{app.pid}-1', path='/StatusNotifierItem')
            time.sleep(.8)
            rect=call('app.twodrive.TrayAnchor1.GetPanel',app.pid)
            panel_bounds.append(rect[1:])
            args.output.write_text(json.dumps({'observed_panel':rect,'dock_top':dock_top})+'\n')
            assert rect[0] and rect[2]+rect[4] <= dock_top-8, ('Panel overlaps autohide dock',rect,dock_top)
            assert rect[1] >= 0 and rect[2] >= 0 and rect[1]+rect[3] <= sw, (
                'Panel escaped the screen', rect, args.screen)
            if args.host_anchor:
                host_rect = call('app.twodrive.TrayAnchor1.GetAnchor', app.pid)
                assert host_rect[0], 'host anchor disappeared'
                scale = args.gdk_scale
                ix, iy, iw, ih, ax, ay, aw, ah = (v//scale for v in host_rect[1:])
                # Independent expected client rectangle in GDK units. The private
                # Shell and GetPanel still report X11 root pixels.
                ah = min(ay+ah, dock_top//scale)-ay
                width, height = min(390, aw-16), min(570, ah-16)
                cx, cy = ix+iw//2, iy+ih//2
                left = max(ax+8, min(cx-width//2, ax+aw-width-8))
                top = ay+8 if cy < ay+ah//2 else ay+ah-height-8
                expected = tuple(v*scale for v in (left, top, width, height))
                assert tuple(rect[1:]) == expected, ('Host/GDK scale mismatch', rect, expected)

            if args.screenshot:
                from PIL import ImageGrab
                ImageGrab.grab(xdisplay=env['DISPLAY']).save(args.screenshot)
            state = call('app.twodrive.TrayAnchor1.GetPanelState', app.pid)
            observations.append(state)
            assert state[0] and not state[1], state
            # Input stays on this disposable compositor's display.
            import ctypes
            x = ctypes.CDLL('libX11.so.6')
            x.XOpenDisplay.argtypes=[ctypes.c_char_p]; x.XOpenDisplay.restype=ctypes.c_void_p
            x.XFlush.argtypes=[ctypes.c_void_p]; x.XCloseDisplay.argtypes=[ctypes.c_void_p]
            d=x.XOpenDisplay(env['DISPLAY'].encode()); assert d
            t=ctypes.CDLL('libXtst.so.6')
            t.XTestFakeMotionEvent.argtypes=[ctypes.c_void_p,ctypes.c_int,ctypes.c_int,ctypes.c_int,ctypes.c_ulong]
            t.XTestFakeButtonEvent.argtypes=[ctypes.c_void_p,ctypes.c_uint,ctypes.c_int,ctypes.c_ulong]
            _,px,py,pw,ph=call('app.twodrive.TrayAnchor1.GetPanel',app.pid)
            def click(cx,cy):
                t.XTestFakeMotionEvent(d,0,cx,cy,0)
                t.XTestFakeButtonEvent(d,1,1,0); t.XTestFakeButtonEvent(d,1,0,0); x.XFlush(d)
            click(px+pw-32*args.gdk_scale,py+40*args.gdk_scale); time.sleep(.5)
            click(px+pw-95*args.gdk_scale,py+52*args.gdk_scale); time.sleep(.8)
            main_state=call('app.twodrive.TrayAnchor1.GetPanelState',app.pid)
            assert main_state[0] and not main_state[1] and not main_state[2], ('Dashboard did not open', main_state)
            observations.append(main_state)
            call('org.kde.StatusNotifierItem.Activate',*point,
                 dest=f'org.kde.StatusNotifierItem-{app.pid}-1',path='/StatusNotifierItem')
            time.sleep(.8)
            return_state=call('app.twodrive.TrayAnchor1.GetPanelState',app.pid)
            observations.append(return_state)
            x.XCloseDisplay(d)

            call('org.kde.StatusNotifierItem.Activate', *point,
                 dest=f'org.kde.StatusNotifierItem-{app.pid}-1', path='/StatusNotifierItem')
            time.sleep(.4)
            assert not call('app.twodrive.TrayAnchor1.GetPanel', app.pid)[0]
        report = dict(gdk_scale=args.gdk_scale, used_host_anchor=args.host_anchor, panel_bounds=panel_bounds, dock_top=dock_top, panel_window_states=observations,
                      all_panels_skip_taskbar=all(s[2] for i,s in enumerate(observations) if i%3 != 1),
                      repeat_open_close=3, background_start_hidden=True)
        shell_version = subprocess.check_output(['gnome-shell', '--version'], text=True).strip()
        report.update(environment=f'isolated {shell_version} / native GTK {args.backend}',
                      foreground_desktop_input=False)
        args.output.write_text(json.dumps(report, indent=2)+'\n')
        print(json.dumps(report, indent=2))
    finally:
        if sys.exc_info()[0]:
            for name in ('client.log','shell.log'):
                if (root/name).exists(): print(name+': '+(root/name).read_text()[-3500:],file=sys.stderr)
        stop(app)
        stop(shell)
        stop(display)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--backend', choices=['x11'], required=True)
    parser.add_argument('--bundle', type=Path, required=True)
    parser.add_argument('--engine', type=Path, required=True)
    parser.add_argument('--peer', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--xvfb', default='Xvfb')
    parser.add_argument('--screen',default='1440x900')
    parser.add_argument('--dock-size',type=int,default=64)
    parser.add_argument('--dock-margin',type=int,default=4)
    parser.add_argument('--screenshot',type=Path)
    parser.add_argument('--host-anchor',action='store_true')
    parser.add_argument('--gdk-scale',type=int,choices=[1,2],default=1)
    parser.add_argument('--root', type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.gdk_scale != 1 and not args.host_anchor:
        parser.error('--gdk-scale 2 requires --host-anchor')
    for name in ['bundle', 'engine', 'peer', 'output']:
        setattr(args, name, getattr(args, name).resolve())
    if args.root:
        inside(args)
    else:
        with tempfile.TemporaryDirectory(prefix='td-panel-isolated-') as tmp:
            root = Path(tmp)
            (root/'runtime').mkdir(mode=0o700)
            shutil.copytree(REPO/'packaging/gnome-tray-anchor', root/'data/gnome-shell/extensions'/UUID)
            extension_file=root/'data/gnome-shell/extensions'/UUID/'extension.js'
            extension=extension_file.read_text()
            extension=extension.replace('const XML = `<node><interface name="app.twodrive.TrayAnchor1">', 'const XML = `<node><interface name="app.twodrive.TrayAnchor1"><method name="GetPanelState"><arg type="u" direction="in"/><arg type="b" direction="out"/><arg type="b" direction="out"/><arg type="b" direction="out"/><arg type="i" direction="out"/></method>')
            extension=extension.replace('<method name="GetPanelState">', '<method name="LeaveOverview"/><method name="GetX11Environment"><arg type="s" direction="out"/><arg type="s" direction="out"/></method><method name="GetPanelState">')
            extension=extension.replace('    GetPanel(pid) {', '    LeaveOverview() { Main.overview.hide(); }\n    GetX11Environment() { return [GLib.getenv("DISPLAY") ?? "", GLib.getenv("XAUTHORITY") ?? ""]; }\n    GetPanel(pid) {')
            extension=extension.replace('    GetPanel(pid) {', '    GetPanelState(pid) { const w = this._ownWindow(pid); return w ? [true, w.is_hidden(), w.skip_taskbar, w.get_window_type()] : [false, true, false, -1]; }\n\n    GetPanel(pid) {')
            extension=extension.replace("import Clutter from 'gi://Clutter';","import Clutter from 'gi://Clutter';\nimport St from 'gi://St';")
            if not args.host_anchor:
                extension=extension.replace('    GetAnchor(pid) {','    GetAnchor(pid) { if(pid) return [false,0,0,0,0,0,0,0,0];')
            extension=extension.replace('Main.overview.hide();', 'Main.overview.hide(); if(!this._testDock) { const m=Main.layoutManager.primaryMonitor; this._testDock=new St.Widget({style: \'background-color: #323842;\'}); Main.layoutManager.addChrome(this._testDock,{affectsStruts:false}); this._testDock.set_position(m.x,m.y+m.height-'+str(args.dock_size+2*args.dock_margin)+'); this._testDock.set_size(m.width,'+str(args.dock_size+2*args.dock_margin)+'); }')
            extension=extension.replace('    disable() {','    disable() { this._testDock?.destroy(); this._testDock=null;')
            extension_file.write_text(extension)
            (root/'test-bus.conf').write_text('''<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd"><busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>''')
            env = dict(os.environ, XDG_CONFIG_HOME=str(root/'config'), XDG_DATA_HOME=str(root/'data'),
                       XDG_RUNTIME_DIR=str(root/'runtime'), XDG_CACHE_HOME=str(root/'cache'),
                       XDG_CURRENT_DESKTOP='Zorin', XDG_SESSION_TYPE=args.backend,
                       LIBGL_ALWAYS_SOFTWARE='1', GSETTINGS_BACKEND='keyfile')
            for key in ['DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'GDK_BACKEND']:
                env.pop(key, None)
            command = ['dbus-run-session', '--config-file='+str(root/'test-bus.conf'), '--', sys.executable, str(Path(__file__).resolve()),
                '--backend', args.backend, '--bundle', str(args.bundle), '--engine', str(args.engine),
                '--peer', str(args.peer), '--output', str(args.output), '--xvfb', args.xvfb, '--root', str(root), '--screen',args.screen,'--dock-size',str(args.dock_size),'--dock-margin',str(args.dock_margin),'--gdk-scale',str(args.gdk_scale)]
            if args.screenshot: command += ['--screenshot',str(args.screenshot.resolve())]
            if args.host_anchor: command += ['--host-anchor']
            subprocess.run(command, env=env, check=True, timeout=100)
