#!/usr/bin/env python3
"""Exercise the installed Flutter GUI, Openbox and SNI in a private X11 session."""
import argparse
import errno
import os
from pathlib import Path
import subprocess
import tempfile
import time

from PIL import ImageGrab


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


def exercise(bundle, engines, home, gnome):
    wm = subprocess.Popen(["gnome-shell", "--x11", "--sm-disable"] if gnome else ["openbox"])
    app = None
    try:
        wm_name = "GNOME Shell" if gnome else "Openbox"
        until(lambda: wm_name in output("wmctrl", "-m"), "window manager")
        if gnome:
            time.sleep(1)
            output("xdotool", "key", "Escape")
        app = subprocess.Popen([
            bundle / "twodrive_dav", "--window-title", "Dashboard fixture",
            "--engine", engines / "twodrive-dav-engine", "--peer-bin", engines / "twodrive-dev",
            "--state", home / "state",
        ])

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
        click_tray()
        until(lambda: "IsViewable" in output("xwininfo", "-id", panel), "tray above minimized dashboard")
        time.sleep(0.4)
        assert minimized(), "opening the tray restored the dashboard"
        output("xdotool", "key", "Escape")
        until(lambda: "IsUnMapped" in output("xwininfo", "-id", panel), "Escape dismissal")
        assert minimized(), "dismissing the tray restored the dashboard"
        time.sleep(0.35)
        click_tray()
        until(lambda: "IsViewable" in output("xwininfo", "-id", panel), "tray reopened")
        time.sleep(0.6)
        # Open the settings menu and choose its first item, Dashboard.
        output("xdotool", "mousemove", "--window", panel, "360", "38", "click", "1")
        time.sleep(0.25)
        output("xdotool", "key", "Down", "Return")
        until(lambda: "Normal" in output("xprop", "-id", primary, "WM_STATE"), "explicit dashboard action")
        assert "IsUnMapped" in output("xwininfo", "-id", panel)
        time.sleep(0.35)
        click_tray()
        until(lambda: "IsViewable" in output("xwininfo", "-id", panel), "tray before Quit")
        # The disconnected fixture offers Dashboard and Quit in the tray menu.
        time.sleep(0.4)
        output("xdotool", "mousemove", "--window", panel, "360", "38", "click", "1")
        time.sleep(0.25)
        output("xdotool", "key", "Down", "Down", "Return")
        assert app.wait(timeout=15) == 0, "Quit must cleanly exit with the tray mapped"
        print("Tray preserved dashboard pixels and minimization; Dashboard explicitly restored it.")
    finally:
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
    parser.add_argument("--gnome", action="store_true", help="use GNOME Shell X11 in the private display")
    parser.add_argument("--private-session", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    bundle = args.bundle.resolve()
    engines = (args.engines or bundle).resolve()
    if args.private_session:
        # Refuse synthetic input unless the caller supplied the private X server and bus.
        home = Path.home()
        if home.parent != Path(tempfile.gettempdir()) or not home.name.startswith("twodrive-tray-test-") or not (home / "fixture").is_file():
            raise SystemExit("Use the outer test entry point to create an isolated desktop")
        exercise(bundle, engines, home, args.gnome)
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
                            "--private-session", *(["--gnome"] if args.gnome else [])], env=env, check=True)

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
