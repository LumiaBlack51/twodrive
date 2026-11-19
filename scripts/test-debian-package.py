#!/usr/bin/env python3
"""Inspect the produced archive; fail for missing dependencies or payload."""
import argparse
from pathlib import Path
import subprocess
import tarfile
import io
import hashlib
import gzip
import json
import re


def verify_manifests(directory):
    manifests = list(directory.glob('*.changes')) + list(directory.glob('*.dsc'))
    assert manifests, 'missing Debian upload manifests'
    for manifest in manifests:
        active = False
        checked = 0
        for line in manifest.read_text().splitlines():
            if line == 'Checksums-Sha256:':
                active = True
                continue
            if not line.startswith(' '):
                active = False
            if active:
                digest, size, name = line.split()
                assert Path(name).name == name, 'unexpected manifest path'
                path = directory / name
                assert path.is_file(), f'missing manifest member: {name}'
                assert path.stat().st_size == int(size), f'wrong size: {name}'
                with path.open('rb') as stream:
                    assert hashlib.file_digest(stream, 'sha256').hexdigest() == digest, f'wrong SHA-256: {name}'
                checked += 1
        assert checked, f'missing SHA-256 entries: {manifest.name}'
    print(f'Verified all members of {len(manifests)} Debian upload manifests')


def verify(package):
    fields = subprocess.check_output(['dpkg-deb', '-f', str(package)], text=True)
    for dependency in ['libc6', 'libsqlite3-0', 'python3', 'fuse3']:
        assert dependency in fields, f'missing generated/runtime dependency: {dependency}'
    assert 'noreply@localhost' not in fields
    archive = subprocess.check_output(['dpkg-deb', '--fsys-tarfile', str(package)])
    with tarfile.open(fileobj=io.BytesIO(archive)) as stream:
        members = stream.getmembers()
        assert all(m.uid == 0 and m.gid == 0 for m in members), 'non-root package ownership'
        names = {m.name.removeprefix('./') for m in members}
        for name in ['usr/bin/twodrive', 'usr/bin/twodrive-daemon',
                     'usr/share/doc/twodrive/copyright',
                     'usr/share/doc/twodrive/changelog.Debian.gz',
                     'usr/share/man/man1/twodrive.1.gz',
                     'usr/bin/twodrive-tray', 'usr/bin/twodrive-settings',
                     'usr/share/nautilus-python/extensions/twodrive_nautilus.py',
                     'etc/xdg/autostart/twodrive-tray.desktop',
                     'usr/share/doc/twodrive/third-party-notices.txt.gz',
                     'usr/lib/systemd/user/twodrive-daemon.service']:
            assert name in names, f'missing payload: {name}'
        assert not any(n.startswith(('home/', 'root/', 'usr/local/')) for n in names), 'user/local payload'
        assert not any(n.endswith(('tokens.json', 'config.toml', '.sqlite3')) for n in names), 'runtime state in package'
        notes = gzip.decompress(stream.extractfile('./usr/share/doc/twodrive/third-party-notices.txt.gz').read())
        assert b'=== rusqlite ' in notes and b'Permission is hereby granted' in notes
    control = subprocess.check_output(['dpkg-deb', '--ctrl-tarfile', str(package)])
    with tarfile.open(fileobj=io.BytesIO(control)) as stream:
        conffiles = stream.extractfile('./conffiles').read().decode()
        assert '/etc/xdg/autostart/twodrive-tray.desktop' in conffiles
        for item in stream.getmembers():
            if Path(item.name).name in ('preinst', 'postinst', 'prerm', 'postrm'):
                script = stream.extractfile(item).read().decode()
                assert not re.search(r'(?:/home/|/root/|\$HOME|\$\{HOME\})', script), 'maintainer script accesses user data'
    print(f'Verified Debian dependencies, ownership and payload: {package.name}')
    verify_manifests(package.parent)


def verify_dav(package, bundle):
    def field(name):
        return subprocess.check_output(['dpkg-deb', '-f', str(package), name], text=True).strip()

    assert field('Package') == 'twodrive-dav'
    assert field('Architecture') == 'amd64'
    dependencies = field('Depends')
    for dependency in ('libc6', 'libgtk-3-0', 'fuse3'):
        assert dependency in dependencies, f'missing DAV dependency: {dependency}'

    runtime = 'usr/lib/twodrive-dav/'
    bundle_files = ('twodrive_dav', 'twodrive-dav-engine', 'twodrive-dev',
                    'twodrive-provider-login', 'rclone', 'rclone-component.json',
                    'lib/libapp.so', 'lib/libflutter_linux_gtk.so', 'data/icudtl.dat')
    remaining = {runtime + name: bundle / name for name in bundle_files}
    required = {'usr/bin/twodrive-dav', 'usr/share/icons/hicolor/scalable/apps/twodrive-dav.svg',
                'usr/share/applications/app.twodrive.twodrive_dav_packaged.desktop',
                'usr/share/gnome-shell/extensions/twodrive-tray-anchor@twodrive/extension.js'}
    with subprocess.Popen(['dpkg-deb', '--fsys-tarfile', str(package)], stdout=subprocess.PIPE) as process:
        with tarfile.open(fileobj=process.stdout, mode='r|') as stream:
            for member in stream:
                name = member.name.removeprefix('./')
                assert (member.uid, member.gid) == (0, 0), f'non-root package ownership: {name}'
                assert not name.startswith(('home/', 'root/', 'usr/local/', 'etc/xdg/autostart/',
                                            'usr/lib/systemd/user/')), f'unexpected package entry: {name}'
                assert not name.endswith(('tokens.json', 'profiles.json', '.sqlite3')), f'runtime state: {name}'
                required.discard(name)
                if name == 'usr/bin/twodrive-dav':
                    assert member.mode & 0o111, 'launcher is not executable'
                if source := remaining.pop(name, None):
                    assert member.isfile(), f'not a regular file: {name}'
                    with source.open('rb') as original, stream.extractfile(member) as installed:
                        assert hashlib.file_digest(original, 'sha256').digest() == hashlib.file_digest(
                            installed, 'sha256').digest(), f'payload differs from bundle: {name}'
        assert process.wait() == 0, 'cannot read DAV package payload'
    assert not (required or remaining), f'missing DAV payload: {sorted(required | remaining.keys())}'

    component = json.loads((bundle / 'rclone-component.json').read_text())
    with (bundle / 'rclone').open('rb') as stream:
        assert component['platform'] == 'linux-amd64'
        assert component['executable_sha256'] == hashlib.file_digest(stream, 'sha256').hexdigest()
    control = subprocess.check_output(['dpkg-deb', '--ctrl-tarfile', str(package)])
    with tarfile.open(fileobj=io.BytesIO(control)) as stream:
        assert not {'preinst', 'postinst', 'prerm', 'postrm', 'conffiles'} & {
            Path(member.name).name for member in stream}, 'unexpected DAV installation hooks'
    verify_manifests(package.parent)
    print(f'Verified DAV dependencies and original portable payload: {package.name}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('package', type=Path)
    parser.add_argument('--dav-bundle', type=Path)
    args = parser.parse_args()
    if args.dav_bundle:
        verify_dav(args.package, args.dav_bundle)
    else:
        verify(args.package)
