"""A helper window on another thread must never receive the GUI quit message."""
import ctypes as c
from ctypes import wintypes as w
import json
import os
from pathlib import Path
import queue
import re
import subprocess
import sys
import tempfile
import threading
import time
import unittest


def close_fixture(code, pid, directory):
    # PS7 -> Python preserves PSModulePath when launching Windows PowerShell.
    # Let that host resolve its own compatible modules, as the packaging tests do.
    environment = {k: v for k, v in os.environ.items() if k.upper() != 'PSMODULEPATH'}
    script = ("$ErrorActionPreference='Stop'\nAdd-Type -TypeDefinition @'\n" + code +
              "\n'@\n[Console]::WriteLine('TwoDrive fixture compiled')\n" +
              "[Console]::Out.Flush()\n" +
              "if ([Console]::ReadLine() -ne 'close') { throw 'Missing fixture command' }\n" +
              f"[TwoDriveMsixSmoke.Native]::Close({pid})\n")
    (directory/'close.ps1').write_text(script, encoding='utf-8')
    # Command text keeps this generated test independent of script-file policy;
    # no execution policy, installed package or machine setting is changed.
    process = subprocess.Popen(
        ['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', script],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, env=environment)
    lines = queue.Queue()
    captured = []
    def read_output():
        for line in process.stdout:
            lines.put(line)
        lines.put(None)
    reader = threading.Thread(target=read_output, daemon=True)
    reader.start()
    phase = 'C# compilation'
    try:
        deadline = time.monotonic()+60
        while True:
            try:
                line = lines.get(timeout=max(.01, deadline-time.monotonic()))
            except queue.Empty:
                raise AssertionError(f'{phase} timed out: '+''.join(captured)) from None
            if line is None:
                raise AssertionError(f'{phase} exited before ready: '+''.join(captured))
            captured.append(line)
            if line.strip() == 'TwoDrive fixture compiled':
                break
            if time.monotonic() >= deadline:
                raise AssertionError(f'{phase} timed out: '+''.join(captured))
        phase = 'runner close'
        process.stdin.write('close\n')
        process.stdin.flush()
        process.stdin.close()
        try:
            result = process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            raise AssertionError(f'{phase} timed out after compilation') from None
        reader.join(timeout=2)
        while not lines.empty():
            line = lines.get_nowait()
            if line is not None: captured.append(line)
        if result != 0:
            raise AssertionError(f'{phase} failed ({result}): '+''.join(captured))
        return ''.join(captured)
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
        if not process.stdin.closed: process.stdin.close()
        reader.join(timeout=2)
        process.stdout.close()


def fixture(directory):
    u=c.WinDLL('user32',use_last_error=True)
    k=c.WinDLL('kernel32',use_last_error=True)
    callback=c.WINFUNCTYPE(c.c_ssize_t,w.HWND,w.UINT,w.WPARAM,w.LPARAM)
    u.DefWindowProcW.argtypes=[w.HWND,w.UINT,w.WPARAM,w.LPARAM]
    u.DefWindowProcW.restype=c.c_ssize_t
    proc=callback(lambda *args:u.DefWindowProcW(*args))
    class WindowClass(c.Structure):
        _fields_=[('style',w.UINT),('proc',callback),('class_extra',c.c_int),('window_extra',c.c_int),('instance',w.HINSTANCE),('icon',w.HICON),('cursor',w.HANDLE),('background',w.HBRUSH),('menu',w.LPCWSTR),('name',w.LPCWSTR)]
    k.GetModuleHandleW.argtypes=[w.LPCWSTR];k.GetModuleHandleW.restype=w.HMODULE
    u.RegisterClassW.argtypes=[c.POINTER(WindowClass)];u.RegisterClassW.restype=w.ATOM
    u.CreateWindowExW.argtypes=[w.DWORD,w.LPCWSTR,w.LPCWSTR,w.DWORD,c.c_int,c.c_int,c.c_int,c.c_int,w.HWND,w.HMENU,w.HINSTANCE,w.LPVOID];u.CreateWindowExW.restype=w.HWND
    u.SetWindowPos.argtypes=[w.HWND,w.HWND,c.c_int,c.c_int,c.c_int,c.c_int,w.UINT]
    u.GetWindowThreadProcessId.argtypes=[w.HWND,c.POINTER(w.DWORD)]
    u.GetMessageW.argtypes=[c.POINTER(w.MSG),w.HWND,w.UINT,w.UINT]
    instance=k.GetModuleHandleW(None)
    cls=WindowClass();cls.proc=proc;cls.instance=instance;cls.name='FLUTTER_RUNNER_WIN32_WINDOW'
    assert u.RegisterClassW(c.byref(cls))
    runner=u.CreateWindowExW(0,cls.name,'isolated runner',0,0,0,1,1,None,None,instance,None)
    assert runner
    ready=threading.Event()
    def decoy():
        window=u.CreateWindowExW(0,'STATIC','isolated COM-like helper',0,0,0,1,1,None,None,instance,None)
        assert window
        assert u.SetWindowPos(window,w.HWND(-1),0,0,0,0,0x1|0x2|0x10)
        ready.set()
        msg=w.MSG()
        while u.GetMessageW(c.byref(msg),None,0,0)>0: pass
    threading.Thread(target=decoy,daemon=True).start()
    assert ready.wait(5)
    windows=[]
    enum=c.WINFUNCTYPE(w.BOOL,w.HWND,w.LPARAM)
    def collect(window,_):
        pid=w.DWORD();u.GetWindowThreadProcessId(window,c.byref(pid))
        if pid.value==os.getpid(): windows.append(window)
        return True
    u.EnumWindows.argtypes=[enum,w.LPARAM]
    assert u.EnumWindows(enum(collect),0)
    assert windows and windows[0]!=runner,'fixture requires the helper to precede the runner'
    (directory/'ready.json').write_text(json.dumps({'pid':os.getpid(),'helper_precedes_runner':True}),encoding='utf-8')
    msg=w.MSG()
    while u.GetMessageW(c.byref(msg),None,0,0)>0: pass
    (directory/'runner-stopped').write_text('runner received WM_QUIT',encoding='utf-8')


@unittest.skipUnless(os.name=='nt','native Win32 fixture')
class WindowTargetTests(unittest.TestCase):
    def test_quit_targets_runner_after_another_thread_helper(self):
        source=Path(os.environ.get('TWODRIVE_TEST_MSIX_HELPER_SOURCE',Path(__file__).with_name('test-msix-runtime.ps1'))).read_text(encoding='utf-8-sig')
        code=re.search(r"Add-Type -TypeDefinition @'\n(.*?)\n'@",source.replace('\r\n','\n'),re.S).group(1)
        with tempfile.TemporaryDirectory(prefix='td-window-target-') as name:
            directory=Path(name)
            log=(directory/'fixture.log').open('w',encoding='utf-8')
            child=subprocess.Popen([sys.executable,__file__,'--fixture',name],stdout=log,stderr=log)
            try:
                deadline=time.monotonic()+8
                while not (directory/'ready.json').exists():
                    self.assertIsNone(child.poll(), 'window fixture exited')
                    self.assertLess(time.monotonic(),deadline)
                    time.sleep(.02)
                output=close_fixture(code,child.pid,directory)
                deadline=time.monotonic()+3
                while not (directory/'runner-stopped').exists() and time.monotonic()<deadline:time.sleep(.02)
                self.assertTrue((directory/'runner-stopped').exists(),'quit reached another thread instead of the runner: '+output)
                child.wait(timeout=5)
            finally:
                if child.poll() is None:child.kill();child.wait(timeout=5)
                log.close()


if __name__=='__main__':
    if len(sys.argv)>1 and sys.argv[1]=='--fixture':fixture(Path(sys.argv[2]))
    else:unittest.main()
