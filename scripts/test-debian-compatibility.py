#!/usr/bin/env python3
"""Check packaged runtime integration against a historical package and current sources."""
import argparse
import io
from pathlib import Path
import subprocess
import tarfile


def payload(path):
    data = subprocess.check_output(['dpkg-deb', '--fsys-tarfile', str(path)])
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        return {m.name.removeprefix('./'): (m.mode, archive.extractfile(m).read())
                for m in archive.getmembers() if m.isfile()}


def normalized(value):
    mode, data = value
    # dh_python3 can change the interpreter line. Compare every application line.
    if data.startswith((b'#!/usr/bin/env python3\n', b'#! /usr/bin/python3\n')):
        data = b'#!/usr/bin/python3\n' + data.split(b'\n', 1)[1]
    return mode, data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('old')
    parser.add_argument('new')
    parser.add_argument('--nautilus-source', type=Path,
                        help='verify the intentionally updated Nautilus extension against this source')
    args = parser.parse_args()
    old, new = payload(args.old), payload(args.new)
    compared = 0
    for name, content in old.items():
        if name.startswith(('usr/share/doc/',)) or name in ('usr/bin/twodrive', 'usr/bin/twodrive-daemon'):
            continue
        assert name in new, f'lost runtime integration: {name}'
        expected, actual = content, new[name]
        if name == 'usr/share/nautilus-python/extensions/twodrive_nautilus.py' and args.nautilus_source:
            expected = (expected[0], args.nautilus_source.read_bytes())
        if name.endswith('.py') or name in ('usr/bin/twodrive-tray', 'usr/bin/twodrive-settings'):
            expected, actual = normalized(expected), normalized(actual)
        assert actual == expected, f'changed runtime integration: {name}'
        compared += 1
    assert compared >= 10, 'runtime comparison was unexpectedly incomplete'
    print(f'PASS: {compared} runtime integration files match expected content and permissions')


if __name__ == '__main__':
    main()
