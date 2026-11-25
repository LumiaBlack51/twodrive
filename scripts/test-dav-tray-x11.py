#!/usr/bin/env python3
"""Send GNOME-style tray events only to the selected TwoDrive GTK process.
Requires an already running client and X11 tools. Does not inject global input.
"""
import argparse
import ctypes as c,subprocess,time,re,json,ast,os
from tray_x11_geometry import panel_geometry

x=c.CDLL('libX11.so.6');x.XOpenDisplay.restype=c.c_void_p;d=x.XOpenDisplay(None)
class Button(c.Structure):
 _fields_=[('type',c.c_int),('serial',c.c_ulong),('send_event',c.c_int),('display',c.c_void_p),('window',c.c_ulong),('root',c.c_ulong),('subwindow',c.c_ulong),('time',c.c_ulong),('x',c.c_int),('y',c.c_int),('x_root',c.c_int),('y_root',c.c_int),('state',c.c_uint),('button',c.c_uint),('same_screen',c.c_int)]
class Crossing(c.Structure):
 _fields_=Button._fields_[:12]+[('mode',c.c_int),('detail',c.c_int),('same_screen',c.c_int),('focus',c.c_int),('state',c.c_uint)]
class Event(c.Union):
 _fields_=[('button',Button),('crossing',Crossing),('pad',c.c_long*24)]
x.XSendEvent.argtypes=[c.c_void_p,c.c_ulong,c.c_int,c.c_long,c.POINTER(Event)];x.XFlush.argtypes=[c.c_void_p]
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--pid',type=int,required=True)
parser.add_argument('--virtual-input', action='store_true',
 help='Use input only on a private test display, never the user desktop.')
args=parser.parse_args()
if args.virtual_input:
 assert os.environ.get('XDG_RUNTIME_DIR','').startswith('/tmp/td-panel-isolated-')
def owned_windows():
 tree=subprocess.check_output(['xwininfo','-root','-tree'],text=True)
 for line in tree.splitlines():
  if 'twodrive' not in line.lower() or not line.strip().startswith('0x'):continue
  ident=line.strip().split()[0]
  prop=subprocess.run(['xprop','-id',ident,'_NET_WM_PID','WM_WINDOW_ROLE'],capture_output=True,text=True).stdout
  if re.search(r'_NET_WM_PID\(CARDINAL\) = '+str(args.pid)+r'\b',prop):yield ident,line,prop
icon=next((ident for ident,line,_ in owned_windows() if '("twodrive-development" "twodrive-development")' in line),None)
assert icon, 'selected process does not own the expected XEmbed icon'
tray=int(icon,16)
window=None
info=subprocess.check_output(['xwininfo','-id',icon],text=True)
tray_x=int(re.search(r'Absolute upper-left X:\s+(-?\d+)',info).group(1))
tray_y=int(re.search(r'Absolute upper-left Y:\s+(-?\d+)',info).group(1))
x.XDefaultRootWindow.argtypes=[c.c_void_p];x.XDefaultRootWindow.restype=c.c_ulong
root_window=x.XDefaultRootWindow(d)
assert d, 'X11 display is unavailable'
x.XQueryPointer.argtypes=[c.c_void_p,c.c_ulong,c.POINTER(c.c_ulong),c.POINTER(c.c_ulong),c.POINTER(c.c_int),c.POINTER(c.c_int),c.POINTER(c.c_int),c.POINTER(c.c_int),c.POINTER(c.c_uint)]
workarea_text=subprocess.check_output(['xprop','-root','_NET_WORKAREA'],text=True)
workarea=[int(v) for v in workarea_text.split('=',1)[1].strip().split(',')[:4]]

def pointer():
 root=c.c_ulong();child=c.c_ulong();rx=c.c_int();ry=c.c_int();wx=c.c_int();wy=c.c_int();mask=c.c_uint()
 assert x.XQueryPointer(d,root_window,c.byref(root),c.byref(child),c.byref(rx),c.byref(ry),c.byref(wx),c.byref(wy),c.byref(mask)), 'pointer is on another X screen'
 return [rx.value,ry.value]
def host_anchor():
 result=subprocess.run(['gdbus','call','--session','--dest','org.gnome.Shell',
  '--object-path','/app/twodrive/TrayAnchor','--method','app.twodrive.TrayAnchor1.GetAnchor',str(args.pid)],capture_output=True,text=True)
 if result.returncode:return None
 value=ast.literal_eval(result.stdout.replace('true','True').replace('false','False'))
 return value if value[0] else None
anchor=host_anchor()
def expected_geometry(point,geometry):
 scale=int(geometry['Width'])//390
 assert scale>=1 and int(geometry['Width'])==390*scale, 'fixture requires a workarea wider than the panel'
 if anchor:
  _,ix,iy,iw,ih,ax,ay,aw,ah=anchor
  px=ix+iw//2;py=iy+ih//2
 else:
  ax,ay,aw,ah=[v//scale for v in workarea]
  px,py=[int(v/scale+.5) for v in point]
 left=max(ax+8,min(px-390//2,ax+aw-390-8))
 top=ay+8 if py<ay else ay+ah-570-8
 return [left*scale,top*scale,390*scale,570*scale]

def state():
 global window
 if window is None:
  window=next((ident for ident,_,props in owned_windows() if '"twodrive-tray"' in props),None)
  if window is None:return {'Map State':'IsUnMapped'}
 data=subprocess.check_output(['xwininfo','-id',window],text=True)
 result = dict(zip(('Absolute upper-left X', 'Absolute upper-left Y', 'Width', 'Height'), panel_geometry(window)))
 result['Map State'] = re.search(r'Map State:\s+(.+)', data).group(1)
 return result
def click():
 if args.virtual_input:
  assert anchor, 'virtual input must use the actual TwoDrive tray actor'
  t=c.CDLL('libXtst.so.6')
  t.XTestFakeMotionEvent.argtypes=[c.c_void_p,c.c_int,c.c_int,c.c_int,c.c_ulong]
  t.XTestFakeButtonEvent.argtypes=[c.c_void_p,c.c_uint,c.c_int,c.c_ulong]
  _,ix,iy,iw,ih,*_=anchor
  t.XTestFakeMotionEvent(d,-1,ix+iw//2,iy+ih//2,0);x.XFlush(d)
  time.sleep(.05)
  t.XTestFakeButtonEvent(d,1,1,0);t.XTestFakeButtonEvent(d,1,0,30);x.XFlush(d)
  return
 stamp=int(time.monotonic()*1000)&0xffffffff
 e=Event();e.crossing=Crossing(7,0,1,d,tray,root_window,0,stamp,16,16,16,16,0,3,1,0,0)
 x.XSendEvent(d,tray,0,0,c.byref(e));x.XFlush(d)
 for typ,mask in [(4,4),(5,8)]:
  e=Event();e.button=Button(typ,0,1,d,tray,root_window,0,stamp,16,16,16,16,0,1,1)
  x.XSendEvent(d,tray,0,0,c.byref(e));x.XFlush(d)
results=[]
anchor_point=None
initial=state()['Map State']
for wanted in (['IsUnMapped','IsViewable'] if initial=='IsViewable' else ['IsViewable','IsUnMapped'])*3:
 time.sleep(.35);point=pointer();t=time.monotonic();click()
 while True:
  s=state()
  if s['Map State']==wanted:break
  assert time.monotonic()-t<3,(wanted,s)
  time.sleep(.01)
 latency=round((time.monotonic()-t)*1000)
 time.sleep(.4)
 settled=state()
 assert settled['Map State']==wanted, 'visibility changed after settling'
 if wanted=='IsViewable':
  assert anchor or all(abs(a-b)<=4 for a,b in zip(point,pointer())), 'pointer moved during this read-only fixture; rerun when stationary'
  if anchor_point is None:anchor_point=point
  assert anchor or all(abs(a-b)<=4 for a,b in zip(anchor_point,point)), 'pointer moved between activations; rerun when stationary'
  actual=[int(settled[k]) for k in ['Absolute upper-left X','Absolute upper-left Y','Width','Height']]
  expected=expected_geometry(anchor_point,settled)
  assert actual==expected, ('anchor mismatch: GNOME icon-local coordinates must not position the panel',actual,expected)
 results.append(dict(expected=wanted,ms=latency,geometry=settled,pointer=point))
positions=[r['geometry'] for r in results if r['expected']=='IsViewable']
assert positions[0]==positions[1]==positions[2], 'window manager moved the panel between activations'
print(json.dumps({'single_click_roundtrips':results,'settling_ms':400,'stable':True,'icon_geometry_verified':anchor is not None,'pointer_fallback':anchor is None,'host_event_root':'virtual display input' if args.virtual_input else 'icon-local center'},indent=2))
