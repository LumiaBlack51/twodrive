#!/usr/bin/env python3
"""Enforce the shared release packaging entry point on main and dev."""
from pathlib import Path
import re
import shlex

root = Path(__file__).resolve().parent.parent
script = (root / 'scripts/build-deb.sh').read_text()
assert 'dpkg-buildpackage' in script, 'release packaging must invoke dpkg-buildpackage'
assert not re.search(r'dpkg-deb\s+(?:--build|-b)\b', script), 'manual release packaging is prohibited'
assert not (root / 'packaging/deb/control').exists(), 'duplicate package control metadata'
assert 'dh $@' in (root / 'debian/rules').read_text(), 'debhelper must drive package assembly'
assert (root / 'debian/source/format').read_text().strip() == '3.0 (quilt)'
for name in ('AGENTS.md', 'CONTRIBUTING.md'):
    assert 'doc/debian-packaging.md' in (root / name).read_text(), f'missing policy link in {name}'

# DAV packaging uses dpkg's normal dependency checks, including its implicit
# build-essential requirement, even though the portable bundle is prebuilt.
desktop = (root / '.github/workflows/linux-dav-desktop.yml').read_text()
linux = desktop.split('\n  linux:\n', 1)[1].split('\n  windows:\n', 1)[0]
if 'scripts/build-dav-deb.sh' in linux:
    packages = set()
    for command in re.findall(r'apt-get install[^\n]*', linux):
        packages.update(shlex.split(command)[2:])
    assert {'build-essential', 'debhelper'} <= packages, 'DAV CI must install Debian build prerequisites'
    assert 'dpkg-checkbuilddeps -Ppkg.twodrive.dav' in linux, 'check DAV build dependencies before compilation'
print('PASS: standard Debian release workflow and shared policy')
