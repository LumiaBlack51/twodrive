#!/usr/bin/env python3
"""Install the Linux DAV app for this user, without enabling autostart."""
import argparse
import os
from pathlib import Path
import shutil
import tempfile
import subprocess
import ast
import uuid


def install_gnome_anchor(data, source):
    uuid = 'twodrive-tray-anchor@twodrive'
    destination = data / 'gnome-shell/extensions' / uuid
    destination.mkdir(parents=True, exist_ok=True)
    for name in ['metadata.json', 'extension.js', 'geometry.js']:
        shutil.copy2(source / name, destination / name)
    # Enabling affects only our component; no Shell restart or other extensions.
    result = subprocess.run(['gnome-extensions', 'enable', uuid],
        capture_output=True, text=True) if shutil.which('gnome-extensions') else None
    if result is None or result.returncode:
        if shutil.which('gsettings'):
            current = subprocess.run(['gsettings', 'get', 'org.gnome.shell', 'enabled-extensions'],
                capture_output=True, text=True, check=True).stdout.strip()
            enabled = ast.literal_eval(current.removeprefix('@as '))
            if uuid not in enabled:
                subprocess.run(['gsettings', 'set', 'org.gnome.shell', 'enabled-extensions',
                    repr([*enabled, uuid])], check=True)
        print('TwoDrive GNOME positioning component installed; a new desktop session may be needed to load it.')
    else:
        print('TwoDrive GNOME positioning component enabled (other extensions unchanged).')


def quote_exec(value):
    if any(c in value for c in '\n\r%'):
        raise ValueError('unsupported desktop launcher path')
    return '"' + value.replace('\\', '\\\\').replace('"', '\\"').replace('`', '\\`').replace('$', '\\$') + '"'


def install(bundle, engine_dir, data, bin_dir, preserve_runtime=False):
    app = data / 'twodrive-dav'
    app.mkdir(parents=True, exist_ok=True)
    stage = Path(tempfile.mkdtemp(prefix='install-', dir=app))
    previous = app / ('runtime-before-' + uuid.uuid4().hex[:12] if preserve_runtime else 'previous')
    runtime = app / 'runtime'
    try:
        shutil.copytree(bundle, stage, dirs_exist_ok=True)
        for name in ['twodrive-dev', 'twodrive-dav-engine', 'twodrive-provider-login']:
            shutil.copy2(engine_dir / name, stage / name)
            (stage / name).chmod(0o755)
        if not (stage / 'twodrive_dav').is_file():
            raise ValueError('Flutter Linux bundle is missing')
        if previous.exists():
            shutil.rmtree(previous)
        if runtime.exists():
            runtime.rename(previous)
        stage.rename(runtime)
    finally:
        if stage.exists():
            shutil.rmtree(stage)
    bin_dir.mkdir(parents=True, exist_ok=True)
    launcher = bin_dir / 'twodrive-dav'
    # exec argv is constructed in Python, so install paths need no shell expansion.
    launcher.write_text('#!/usr/bin/env python3\nimport os, sys, subprocess\n'
        + 'if "GDK_BACKEND" not in os.environ and os.environ.get("DISPLAY"):\n'
        + '    backend = "x11"\n'
        + '    if os.environ.get("WAYLAND_DISPLAY"):\n'
        + '        try:\n'
        + '            probe = subprocess.run(["gdbus", "call", "--session", "--dest", "org.gnome.Shell", "--object-path", "/app/twodrive/TrayAnchor", "--method", "app.twodrive.TrayAnchor1.GetAnchor", "0"], capture_output=True, timeout=1)\n'
        + '            if probe.returncode == 0: backend = "wayland"\n'
        + '        except (OSError, subprocess.TimeoutExpired): pass\n'
        + '    os.environ["GDK_BACKEND"] = backend\n'
        + 'os.environ.setdefault("TWODRIVE_TRAY_BACKEND", "sni")\n'
        + 'program = ' + repr(str(runtime / 'twodrive_dav')) + '\n'
        + 'os.execv(program, [program, "--state", ' + repr(str(app / 'state'))
        + ', "--launcher", ' + repr(str(launcher)) + '] + sys.argv[1:])\n')
    launcher.chmod(0o755)
    icons = data / 'icons/hicolor/scalable/apps'
    icons.mkdir(parents=True, exist_ok=True)
    icon = runtime / 'data/flutter_assets/assets/app-icon.svg'
    if not icon.is_file():
        icon = runtime / 'data/flutter_assets/assets/cloud-connected.svg'
    for name in ['twodrive-dav.svg', 'twodrive-dav-minimal.svg']:
        shutil.copy2(icon, icons / name)
    applications = data / 'applications'
    applications.mkdir(parents=True, exist_ok=True)
    (applications / 'app.twodrive.twodrive_dav.desktop').write_text(
        '[Desktop Entry]\nType=Application\nName=TwoDrive\nComment=WebDAV files and TwoDrive paired shares\n'
        + 'Exec=' + quote_exec(str(launcher)) + '\nIcon=twodrive-dav-minimal\nTerminal=false\n'
        + 'Categories=Network;FileTransfer;\nStartupWMClass=app.twodrive.twodrive_dav\n')
    # Deliberately do not create or modify XDG_CONFIG_HOME/autostart.
    if previous.exists() and not preserve_runtime:
        shutil.rmtree(previous)
    if preserve_runtime:
        print('Running runtime preserved: ' + str(previous))
    print('Installed: ' + str(launcher))
    print('Autostart: unchanged (a new installation defaults to off)')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bundle', type=Path, required=True)
    parser.add_argument('--engine-dir', type=Path, required=True)
    parser.add_argument('--data-dir', type=Path, default=Path(os.environ.get('XDG_DATA_HOME', Path.home() / '.local/share')))
    parser.add_argument('--bin-dir', type=Path, default=Path.home() / '.local/bin')
    parser.add_argument('--gnome-anchor', type=Path,
        help='Install TwoDrive-only GNOME icon positioning component (X11/Wayland).')
    parser.add_argument('--preserve-running-runtime', action='store_true',
        help='Keep the prior runtime paths valid without restarting the running client.')
    args = parser.parse_args()
    install(args.bundle.resolve(), args.engine_dir.resolve(), args.data_dir.resolve(), args.bin_dir.resolve(), args.preserve_running_runtime)
    if args.gnome_anchor:
        install_gnome_anchor(args.data_dir.resolve(), args.gnome_anchor.resolve())
