#!/usr/bin/env python3
"""Exercise the installed Flutter tray on a private X11 desktop."""
import argparse
import errno
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time

from PIL import ImageGrab
from tray_x11_geometry import panel_geometry


def output(*args):
    return subprocess.run(args, text=True, capture_output=True, check=True).stdout


def until(probe, label):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        try:
            value = probe()
        except subprocess.CalledProcessError:
            value = None
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError(f"Timed out waiting for {label}")


def mock_anchor():
    # Real Shell can supply an anchor in different units from GTK. On X11
    # it must never be asked to resize the same window as the application.
    from gi.repository import Gio, GLib
    xml = '<node><interface name="app.twodrive.TrayAnchor1">'
    for name, result in [('GetAnchor', 'biiiiiiii'), ('PositionTray', 'b')]:
        xml += f'<method name="{name}"><arg direction="in" type="u"/>'
        xml += ''.join(f'<arg direction="out" type="{kind}"/>' for kind in result) + '</method>'
    info = Gio.DBusNodeInfo.new_for_xml(xml + '</interface></node>')
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    calls = []
    def reply(conn, sender, path, iface, name, params, invocation):
        calls.append(name)
        value = (GLib.Variant('(biiiiiiii)', (True, 1210, 970, 24, 24, 0, 0, 1280, 960))
                 if name == 'GetAnchor' else GLib.Variant('(b)', (True,)))
        invocation.return_value(value)
    bus.register_object('/app/twodrive/TrayAnchor', info.interfaces[0], reply, None, None)
    result = bus.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
                           'RequestName', GLib.Variant('(su)', ('org.gnome.Shell', 4)),
                           GLib.VariantType('(u)'), Gio.DBusCallFlags.NONE, 1000, None)
    assert result.unpack() == (1,), 'fixture must own its private Shell name'
    loop = GLib.MainLoop()
    threading.Thread(target=loop.run, daemon=True).start()
    return calls, loop, bus

def exercise(bundle, engines, home, gnome, mutter, quit_from_tray):
    wm_command = ["gnome-shell" if gnome else "mutter", "--x11", "--sm-disable"]
    wm = subprocess.Popen(wm_command if gnome or mutter else ["openbox"])
    app = None
    calls, loop, anchor_bus = (None, None, None) if gnome else mock_anchor()
    try:
        wm_name = "GNOME Shell" if gnome else "Mutter" if mutter else "Openbox"
        until(lambda: wm_name in output("wmctrl", "-m"), "window manager")
        if gnome:
            time.sleep(1)
            output("xdotool", "key", "Escape")
        app_log = home / "client.stderr"
        with app_log.open("w") as log:
            app = subprocess.Popen([
                bundle / "twodrive_dav", "--window-title", "Dashboard fixture",
                "--engine", engines / "twodrive-dav-engine", "--peer-bin", engines / "twodrive-dev",
                "--state", home / "state",
            ], stderr=log)

        def assert_clean_exit():
            code = app.wait(timeout=15)
            assert code == 0, f"Quit returned {code}"
            errors = app_log.read_text(errors="replace")
            for diagnostic in ("invalid unclassed pointer in cast to 'FlView'",
                               "gtk_widget_queue_draw: assertion", "g_mutex_clear() called",
                               "Failed to shutdown Flutter engine"):
                assert diagnostic not in errors, f"Flutter teardown emitted {diagnostic}"

        def named(title):
            for row in output("wmctrl", "-lp").splitlines():
                fields = row.split(None, 4)
                if len(fields) == 5 and fields[2] == str(app.pid) and fields[4] == title:
                    return fields[0]
            return None

        def click_tray():
            output("gdbus", "call", "--session", "--dest", f"org.kde.StatusNotifierItem-{app.pid}-1",
                   "--object-path", "/StatusNotifierItem", "--method", "org.kde.StatusNotifierItem.Activate",
                   "1250", "950")

        primary = until(lambda: named("Dashboard fixture"), "dashboard")
        until(lambda: (home / "state/run.lock").exists(), "fixture engine")
        time.sleep(3 if gnome else 1)
        # Compare the Flutter content. GTK repaints its title bar on focus changes.
        region = (240, 260, 680, 590)
        before = ImageGrab.grab(bbox=region).tobytes()
        click_tray()
        panel = until(lambda: named("TwoDrive Tray"), "separate tray surface")
        assert panel != primary and "twodrive-tray" in output("xprop", "-id", panel, "WM_WINDOW_ROLE")
        time.sleep(0.4)
        assert ImageGrab.grab(bbox=region).tobytes() == before, "dashboard dimmed"
        assert "Normal" in output("xprop", "-id", primary, "WM_STATE")
        click_tray()
        until(lambda: "IsUnMapped" in output("xwininfo", "-id", panel), "tray dismissal")
        output("xdotool", "windowminimize", primary)
        minimized = lambda: "Iconic" in output("xprop", "-id", primary, "WM_STATE")
        until(minimized, "minimized dashboard")
        time.sleep(0.35)
        x, y, width, height = panel_geometry(panel)
        # Capture a strip across the left edge with the panel closed. Once the
        # Flutter body appears, the same frame must also darken the desktop.
        strip = (x - 4, y + height // 2, x + 4, y + height // 2 + 8)
        desktop = ImageGrab.grab(bbox=strip).convert("RGB")

        def check_shadow():
            if not (gnome or mutter):
                return
            assert (width, height) == (390, 570), "decoration changed the content size"
            visible = 0
            deadline = time.monotonic() + 2.5
            while time.monotonic() < deadline:
                frame = ImageGrab.grab(bbox=strip).convert("RGB")
                if frame.getpixel((6, 4)) == (250, 251, 254):
                    visible += 1
                    inner = sum(sum(frame.getpixel((3, i))) for i in range(8))
                    background = sum(sum(desktop.getpixel((3, i))) for i in range(8))
                    assert inner < background, "visible tray lost its edge shadow"
                time.sleep(0.016)
            assert visible >= 12, "did not observe enough visible tray frames"
            prop = output("xprop", "-id", panel, "_GTK_FRAME_EXTENTS")
            assert "=" in prop and all(int(v) > 0 for v in prop.split("=")[1].split(",")), prop

        click_tray()
        check_shadow()
        until(lambda: "IsViewable" in output("xwininfo", "-id", panel), "tray above minimized dashboard")
        time.sleep(0.4)
        assert minimized(), "opening the tray restored the dashboard"
        # The footer stays painted while the hidden primary view and the visible
        # secondary view process periodic status updates. Sample the actual GTK
        # presentation, including the text, rather than Flutter layout alone.
        def footer():
            left, top, width, height = panel_geometry(panel)
            x, bottom = left + width // 2, top + height
            bounds = (x - 52, bottom - 26, x + 52, bottom - 4)
            image = ImageGrab.grab(bbox=bounds).convert("RGB")
            return bounds, image
        opacity = output("xprop", "-id", panel, "_NET_WM_WINDOW_OPACITY")
        assert "=" not in opacity or int(opacity.split("=")[1].strip(), 0) == 0xffffffff, opacity
        bounds, first = footer()
        if calls is not None:
            assert 'GetAnchor' in calls and 'PositionTray' not in calls, f'competing X11 positioning: {calls}'
        assert len(first.getcolors(4096) or []) > 8, "tray footer text did not render"
        for _ in range(32):
            time.sleep(0.05)
            next_bounds, frame = footer()
            assert next_bounds == bounds and frame.tobytes() == first.tobytes(), "tray footer flickered"

        output("xdotool", "key", "Escape")
        until(lambda: "IsUnMapped" in output("xwininfo", "-id", panel), "Escape dismissal")
        assert minimized(), "dismissing the tray restored the dashboard"
        time.sleep(0.35)
        click_tray()
        check_shadow()
        until(lambda: "IsViewable" in output("xwininfo", "-id", panel), "tray reopened")
        time.sleep(0.6)
        # Open the settings menu and choose its first item, Dashboard.
        output("xdotool", "mousemove", str(x + 360), str(y + 38), "click", "1")
        time.sleep(0.25)
        output("xdotool", "key", "Down", "Return")
        until(lambda: "Normal" in output("xprop", "-id", primary, "WM_STATE"), "explicit dashboard action")
        assert "IsUnMapped" in output("xwininfo", "-id", panel)
        if mutter and not quit_from_tray:
            # The compositor fixture covers both shadow reveals and dismissal.
            # Quit from the restored dashboard. The workspace regression below
            # separately exercises Quit with a live secondary Flutter view.
            output("xdotool", "key", "ctrl+q")
            assert_clean_exit()
            print("Tray shadow stayed visible across opens on composited X11.")
            return
        time.sleep(0.35)
        click_tray()
        until(lambda: "IsViewable" in output("xwininfo", "-id", panel), "tray before Quit")
        # The disconnected fixture offers Dashboard and Quit in the tray menu.
        time.sleep(0.4)
        output("xdotool", "mousemove", str(x + 360), str(y + 38), "click", "1")
        time.sleep(0.25)
        output("xdotool", "key", "Down", "Down", "Return")
        assert_clean_exit()
        print("Tray preserved dashboard pixels and minimization; Dashboard explicitly restored it.")
    finally:
        if app and sys.exc_info()[0]:
            print(app_log.read_text(errors="replace")[-6000:], file=sys.stderr)
        if loop:
            loop.quit()
            anchor_bus.close_sync(None)
        if app:
            app.terminate()
            app.wait(timeout=10)
        wm.terminate()
        try:
            wm.wait(timeout=10)
        except subprocess.TimeoutExpired:
            wm.kill()
            wm.wait()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle", type=Path)
    parser.add_argument("--engines", type=Path)
    parser.add_argument("--quit-from-tray", action="store_true", help="quit with the secondary view mapped")
    manager = parser.add_mutually_exclusive_group()
    manager.add_argument("--mutter", action="store_true", help="exercise GTK shadows under composited X11")
    manager.add_argument("--gnome", action="store_true", help="use GNOME Shell X11 in the private display")
    parser.add_argument("--private-session", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    bundle = args.bundle.resolve()
    engines = (args.engines or bundle).resolve()
    if args.private_session:
        # Refuse synthetic input unless the caller supplied the private X server and bus.
        home = Path.home()
        if home.parent != Path(tempfile.gettempdir()) or not home.name.startswith("twodrive-tray-test-") or not (home / "fixture").is_file():
            raise SystemExit("Use the outer test entry point to create an isolated desktop")
        exercise(bundle, engines, home, args.gnome, args.mutter, args.quit_from_tray)
    else:
        fixture = tempfile.TemporaryDirectory(prefix="twodrive-tray-test-")
        directory = fixture.name
        try:
            home = Path(directory)
            (home / "fixture").touch()
            runtime = home / "run"
            runtime.mkdir(mode=0o700)
            env = dict(os.environ, HOME=directory, XDG_RUNTIME_DIR=str(runtime), XDG_SESSION_TYPE="x11",
                       XDG_CONFIG_HOME=str(home / "config"), XDG_DATA_HOME=str(home / "data"),
                       XDG_CACHE_HOME=str(home / "cache"), GDK_BACKEND="x11", GDK_SCALE="1",
                       TWODRIVE_TRAY_BACKEND="sni")
            env.pop("WAYLAND_DISPLAY", None)
            if args.gnome:
                env.update(GSETTINGS_BACKEND="memory", LP_NUM_THREADS="2")
                (home / "Desktop").mkdir()
            subprocess.run(["xvfb-run", "-a", "-s", "-screen 0 1280x1024x24", "dbus-run-session",
                            "--", "python3", __file__, str(bundle), "--engines", str(engines),
                            "--private-session", *(["--quit-from-tray"] if args.quit_from_tray else []),
                            *(["--gnome"] if args.gnome else ["--mutter"] if args.mutter else [])], env=env, check=True)

        finally:
            # Private GNOME services may finish disposing their XDG files just
            # after the session bus exits. Retry only this test-owned directory.
            for attempt in range(10):
                try:
                    fixture.cleanup()
                    break
                except OSError as error:
                    if error.errno != errno.ENOTEMPTY or attempt == 9:
                        raise
                    time.sleep(0.2)
