#!/usr/bin/env python3
"""Compare stable package runtime integration and application crate versions."""
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('old')
    parser.add_argument('new')
    args = parser.parse_args()
    old, new = payload(args.old), payload(args.new)
    compared = 0
    for name, content in old.items():
        if name.startswith(('usr/share/doc/',)) or name in ('usr/bin/twodrive', 'usr/bin/twodrive-daemon'):
            continue
        assert name in new, f'lost runtime integration: {name}'
        expected, actual = content, new[name]
        if name.endswith('.py') or name in ('usr/bin/twodrive-tray', 'usr/bin/twodrive-settings'):
            # dh_python3 selects the packaged interpreter; all application lines
            # following the shebang must remain byte-for-byte identical.
            def normalized(value):
                mode, data = value
                if data.startswith(b'#!/usr/bin/env python3\n'):
                    data = b'#!/usr/bin/python3\n' + data.split(b'\n', 1)[1]
                if data.startswith(b'#! /usr/bin/python3\n'):
                    data = b'#!/usr/bin/python3\n' + data.split(b'\n', 1)[1]
                return mode, data
            if name == 'usr/bin/twodrive-settings':
                # The released settings controls intentionally changed. Check the
                # exact repository script while still requiring the old executable mode.
                expected = (expected[0], Path(__file__).with_name('twodrive-settings').read_bytes())
            expected, actual = normalized(expected), normalized(actual)
        assert actual == expected, f'changed runtime integration: {name}'
        compared += 1
    assert compared >= 10, 'runtime comparison was unexpectedly incomplete'
    print(f'PASS: {compared} stable runtime integration files preserve content and permissions (Debian Python shebang normalization allowed)')


if __name__ == '__main__':
    main()
