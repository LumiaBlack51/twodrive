#!/usr/bin/env python3
"""Prepare a clean quilt source tree and locked vendor component before building.

Network access is allowed only in this explicit source-preparation step. The
Debian build itself uses an empty, in-tree Cargo home and the supplied sources.
"""
import argparse
import hashlib
import io
import json
import os
import re
from pathlib import Path
import subprocess
import tarfile
import tempfile
import tomllib


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def archive(source, destination, prefix, epoch, excludes=()):
    with tarfile.open(destination, 'w:xz', preset=6) as output:
        for path in sorted(source.rglob('*')):
            relative = path.relative_to(source)
            if relative.parts[0] in excludes:
                continue
            info = output.gettarinfo(str(path), str(Path(prefix) / relative))
            info.uid = info.gid = 0
            info.uname = info.gname = ''
            info.mtime = epoch
            if info.isfile():
                with path.open('rb') as content:
                    output.addfile(info, content)
            else:
                output.addfile(info)



def debian_license_text(text):
    # Debian copyright files reference the distribution's common Apache text.
    # Retain every attribution before/after the canonical grant; the companion
    # notices document above preserves the exact, complete upstream files too.
    text = re.sub(
        r"(?ms)^[ \t]*Apache License[ \t]*\n[ \t]*Version 2\.0, January 2004[^\n]*\n.*?^[ \t]*END OF TERMS AND CONDITIONS[ \t]*$",
        "On Debian systems, the complete Apache License, Version 2.0 is in\n"
        "/usr/share/common-licenses/Apache-2.0.", text)
    return text.expandtabs(8)


def notices(vendor, destination):
    copyright_path = destination.parent / "copyright"
    dep5 = []
    with destination.open('w') as output:
        output.write('Locked Rust dependency attribution and license texts\n'
                     'Sources are supplied in the orig-vendor source component.\n\n')
        for directory in sorted(vendor.iterdir()):
            manifest = directory / 'Cargo.toml'
            if not manifest.is_file():
                continue
            package = tomllib.loads(manifest.read_text())['package']
            output.write(f"\n=== {package['name']} {package['version']} ===\n")
            output.write('Declared license: ' + package.get('license', 'see license file') + '\n')
            output.write('Upstream authors: ' + ', '.join(package.get('authors', [])) + '\n')
            texts = []
            for path in sorted(directory.rglob('*')):
                if path.is_file() and (path.name.upper().startswith(('LICENSE', 'COPYING', 'NOTICE'))
                                       or path.name.lower() == 'copyright'):
                    output.write('\n--- ' + str(path.relative_to(vendor)) + ' ---\n')
                    content = path.read_text(errors='replace')
                    output.write(content + '\n')
                    texts.append(str(path.relative_to(directory)) + '\n' + content)
            if not texts:
                if package['name'] == 'r-efi':
                    # This source-only UEFI dependency declares MIT as an option
                    # but ships no separate license document, including upstream.
                    # Reproduce that declaration and the standard MIT grant.
                    mit = (Path(__file__).resolve().parent.parent / 'LICENSE').read_text()
                    texts = ['MIT license selected from upstream SPDX alternatives.\n' +
                             (directory / 'README.md').read_text() + '\n' +
                             mit[mit.index('Permission is hereby granted'):]]
                elif package['name'] in ('winapi-i686-pc-windows-gnu', 'winapi-x86_64-pc-windows-gnu'):
                    texts = ['MIT license from the same upstream winapi source distribution.\n' +
                             (vendor / 'winapi-0.3.9/LICENSE-MIT').read_text()]
                else:
                    raise ValueError('Missing upstream license files: ' + directory.name)
                output.write('\n'.join(texts) + '\n')
            label = 'vendor-' + re.sub(r'[^a-zA-Z0-9.+-]', '-', directory.name)
            dep5.append('\nFiles: vendor/' + directory.name + '/*\n'
                        'Copyright: See the upstream copyright notices reproduced below.\n'
                        'License: ' + label + '\n'
                        'Comment: Upstream declared license expression: ' + package.get('license', 'see license file') + '\n\n'
                        'License: ' + label + '\n' +
                        '\n'.join(' ' + (line if line.strip() else '.')
                                  for line in debian_license_text('\n\n'.join(texts)).splitlines()) + '\n')
    with copyright_path.open('a') as copyright_output:
        copyright_output.write(''.join(dep5))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    os.chdir(root)
    run('git', 'diff', '--quiet', 'HEAD', '--', '.')
    run('python3', 'scripts/check-local-package-locks.py', stdout=subprocess.DEVNULL)
    version = subprocess.check_output(['dpkg-parsechangelog', '-S', 'Version'], text=True).strip()
    upstream = version.rsplit('-', 1)[0]
    epoch = int(subprocess.check_output(['git', 'log', '-1', '--format=%ct'], text=True))
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    stage = Path(tempfile.mkdtemp(prefix='source-', dir=args.output))
    tree = stage / f'twodrive-{upstream}'
    tree.mkdir()
    data = subprocess.check_output(['git', 'archive', 'HEAD'])
    with tarfile.open(fileobj=io.BytesIO(data)) as source:
        source.extractall(tree, filter='data')
    run('cargo', 'vendor', '--locked', '--versioned-dirs', str(tree / 'vendor'), cwd=tree,
        stdout=subprocess.DEVNULL)
    notices(tree / 'vendor', tree / 'debian/third-party-notices.txt')
    # Separate the unmodified upstream tree and vendored sources, as supported by
    # dpkg-source 3.0 (quilt). No dependency cache is needed when extracting .dsc.
    archive(tree, stage / f'twodrive_{upstream}.orig.tar.xz', tree.name, epoch,
            excludes=('debian', 'vendor'))
    archive(tree / 'vendor', stage / f'twodrive_{upstream}.orig-vendor.tar.xz', 'vendor', epoch)
    (stage / 'source.json').write_text(json.dumps({'source_commit': subprocess.check_output(
        ['git', 'rev-parse', 'HEAD'], text=True).strip(), 'version': version,
        'vendor_sha256': hashlib.sha256((stage / f'twodrive_{upstream}.orig-vendor.tar.xz').read_bytes()).hexdigest()}, indent=2)+'\n')
    print(tree)


if __name__ == '__main__':
    main()
