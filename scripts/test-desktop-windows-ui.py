#!/usr/bin/env python3
"""Exercise the native Windows tray callback in an isolated desktop process.
Sends Shell notification messages, not hardware input. Does not use real profiles.
"""
import ctypes as c
from ctypes import wintypes as w
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

assert os.name == 'nt'
u = c.WinDLL('user32', use_last_error=True)
u.FindWindowW.argtypes = [w.LPCWSTR, w.LPCWSTR]
u.FindWindowW.restype = w.HWND
u.IsWindowVisible.argtypes = [w.HWND]
u.GetPropW.argtypes = [w.HWND, w.LPCWSTR]
u.GetPropW.restype = w.HANDLE
u.PostMessageW.argtypes = [w.HWND, w.UINT, w.WPARAM, w.LPARAM]
u.GetWindowRect.argtypes = [w.HWND, c.POINTER(w.RECT)]
u.GetWindowThreadProcessId.argtypes = [w.HWND, c.POINTER(w.DWORD)]
u.GetForegroundWindow.restype = w.HWND
u.SetForegroundWindow.argtypes = [w.HWND]
u.AllowSetForegroundWindow.argtypes = [w.DWORD]
u.AttachThreadInput.argtypes = [w.DWORD, w.DWORD, w.BOOL]
u.CreateWindowExW.argtypes = [w.DWORD, w.LPCWSTR, w.LPCWSTR, w.DWORD,
    c.c_int, c.c_int, c.c_int, c.c_int, w.HWND, w.HMENU, w.HINSTANCE, w.LPVOID]
u.CreateWindowExW.restype = w.HWND
u.DestroyWindow.argtypes = [w.HWND]
k = c.WinDLL('kernel32', use_last_error=True)
k.GetCurrentThreadId.restype = w.DWORD


def foreground(window, source):
    # Attach only for the focus transfer between this fixture's two windows.
    # This avoids foreground-lock timing in hosted desktops; no hardware input.
    current = k.GetCurrentThreadId()
    assert u.GetForegroundWindow() == source, 'panel was not foreground before focus transfer'
    active = u.GetWindowThreadProcessId(source, None)
    attached = active != 0 and active != current
    if attached:
        assert u.AttachThreadInput(current, active, True), 'cannot attach fixture focus queues'
    try:
        assert u.SetForegroundWindow(window), 'cannot focus fixture window'
    finally:
        if attached:
            u.AttachThreadInput(current, active, False)


def wait_visible(window, visible, stage):
    started = time.monotonic()
    while bool(u.IsWindowVisible(window)) != visible:
        assert time.monotonic()-started < 3, f'{stage}: expected_visible={visible}'
        time.sleep(.01)

with tempfile.TemporaryDirectory(prefix='td-ui-') as directory:
    root = Path(directory)
    exe = Path(sys.argv[1]).resolve()
    title = f'TwoDrive tray smoke {root.name}'
    state_lock = root/'state/run.lock'
    with (root/'stderr.log').open('w', encoding='utf-8') as log:
        proc = subprocess.Popen(
            [str(exe), '--background', '--state', str(root/'state'), '--window-title', title],
            cwd=exe.parent, stdout=log, stderr=log,
            env={**os.environ, 'TWODRIVE_TRAY_TRACE_FILE': str(root/'native.log')})
        helper = None
        try:
            helper = u.CreateWindowExW(0, 'STATIC', 'TwoDrive isolated focus fixture',
                0x80000000 | 0x10000000, 0, 0, 32, 32, None, None, None, None)
            assert helper, 'cannot create isolated focus fixture'
            deadline = time.monotonic()+30
            while True:
                hwnd = u.FindWindowW(None, title)
                owner = w.DWORD()
                if hwnd:
                    u.GetWindowThreadProcessId(hwnd, c.byref(owner))
                found_window = owner.value == proc.pid
                engine_ready = state_lock.exists()
                tray_ready = found_window and bool(u.GetPropW(hwnd, 'TwoDrive.TrayReady'))
                if engine_ready and tray_ready:
                    break
                assert proc.poll() is None, 'desktop process exited during startup'
                assert time.monotonic() < deadline, (
                    f'desktop did not initialize: window={found_window}, '
                    f'engine={engine_ready}, tray={tray_ready}'
                )
                time.sleep(.1)
            time.sleep(1)
            assert not u.IsWindowVisible(hwnd), 'background startup showed a window'
            positions=[]
            for cycle, visible in enumerate([True, False]*3, 1):
                time.sleep(.3)
                started=time.monotonic()
                if u.GetForegroundWindow() == helper:
                    assert u.AllowSetForegroundWindow(proc.pid)
                assert u.PostMessageW(hwnd, 0x8000+41, 0, 0x400), 'tray notification delivery failed'
                while bool(u.IsWindowVisible(hwnd)) != visible:
                    assert time.monotonic()-started<3, f'tray did not toggle: event={cycle}, expected_visible={visible}, process_exit={proc.poll()}'
                    time.sleep(.01)
                time.sleep(.4)
                assert bool(u.IsWindowVisible(hwnd)) == visible, f'visibility changed after settling: event={cycle}'
                if visible:
                    rect=w.RECT();u.GetWindowRect(hwnd,c.byref(rect))
                    positions.append([rect.left,rect.top,rect.right-rect.left,rect.bottom-rect.top])
                    if cycle == 1:
                        foreground(helper, hwnd)
                        wait_visible(hwnd, False, 'outside focus did not dismiss panel')
                        time.sleep(.3)
                        assert u.AllowSetForegroundWindow(proc.pid)
                        assert u.PostMessageW(hwnd, 0x8000+41, 0, 0x400)
                        wait_visible(hwnd, True, 'panel did not reopen after focus dismissal')
            assert positions[0]==positions[1]==positions[2], 'tray panel drifted'
            time.sleep(.3)
            if u.GetForegroundWindow() == helper:
                assert u.AllowSetForegroundWindow(proc.pid)
            assert u.PostMessageW(hwnd, 0x8000+41, 0, 0x400)
            wait_visible(hwnd, True, 'panel did not open for Escape check')
            time.sleep(.4)
            assert u.PostMessageW(hwnd, 0x100, 0x1b, 0)
            wait_visible(hwnd, False, 'Escape notification did not dismiss panel')
            print(json.dumps({'native_tray_toggle':True,'background_hidden':True,'stable_position':True,
                'outside_focus_dismiss':True,'escape_notification_dismiss':True,
                'boundary':'Shell notification message simulation; no hardware input or cloud profile'}))
        except BaseException:
            log.flush()
            for name in ['native.log', 'stderr.log']:
                path = root/name
                if path.exists():
                    print(f'{name}:\n{path.read_text(encoding="utf-8", errors="replace")[-12000:]}', flush=True)
            raise
        finally:
            # This verifies window interaction. Graceful menu quit is a separate manual check.
            proc.terminate()
            proc.wait(timeout=10)
            if helper:
                u.DestroyWindow(helper)
    # The child sees IPC EOF and releases its private state/job; never stop another app.
    deadline=time.monotonic()+10
    while True:
        try:
            import shutil
            shutil.rmtree(root/'state')
            break
        except FileNotFoundError:
            break
        except PermissionError:
            assert time.monotonic()<deadline, 'desktop engine retained private state after GUI exit'
            time.sleep(.1)
