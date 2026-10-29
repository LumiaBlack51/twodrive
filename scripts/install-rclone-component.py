#!/usr/bin/env python3
"""Stage the pinned official Rclone component; never read a user's configuration."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import stat
import tempfile
import urllib.request
import zipfile

VERSION = '1.75.2'
SHA256 = {
    'linux-amd64': '349ac8fba6ff65d6247043f1750cdcb518ec5d500ef91463a10d37c0ccdf3702',
    'windows-amd64': '8a9db2bad1dc79a65b3348fd857069598e2aab7cf4d118b77b1df23055aff818',
}
MAX_ARCHIVE = 150 * 1024 * 1024


def stage(archive, destination, platform):
    with archive.open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    if digest != SHA256[platform]:
        raise ValueError('Official Rclone archive SHA-256 mismatch')
    prefix = f'rclone-v{VERSION}-{platform}/'
    executable = 'rclone.exe' if platform.startswith('windows-') else 'rclone'
    destination = Path(destination)
    if destination.exists():
        raise FileExistsError('Use a new component staging directory')
    destination.parent.mkdir(parents=True, exist_ok=True)
    # Extract only named regular files after verification, never arbitrary ZIP paths.
    with tempfile.TemporaryDirectory(prefix='rclone-stage-', dir=destination.parent) as tmp:
        staged = Path(tmp) / 'component'
        staged.mkdir()
        with zipfile.ZipFile(archive) as bundle:
            for source, target in [(executable, executable), ('README.txt', 'RCLONE-README.txt')]:
                member = bundle.getinfo(prefix + source)
                if stat.S_ISLNK(member.external_attr >> 16) or member.file_size > MAX_ARCHIVE:
                    raise ValueError('Invalid Rclone component member')
                with bundle.open(member) as stream, (staged / target).open('wb') as output:
                    shutil.copyfileobj(stream, output)
            (staged / executable).chmod(0o755)
        manifest = {'component': 'rclone', 'version': VERSION, 'platform': platform,
                    'official_archive_sha256': digest,
                    'executable_sha256': hashlib.sha256((staged / executable).read_bytes()).hexdigest(),
                    'source': f'https://github.com/rclone/rclone/releases/tag/v{VERSION}'}
        (staged / 'rclone-component.json').write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')
        staged.rename(destination)
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--platform', choices=SHA256, required=True)
    parser.add_argument('--destination', type=Path, required=True)
    parser.add_argument('--archive', type=Path, help='Previously downloaded official ZIP; SHA-256 is still verified')
    args = parser.parse_args()
    if args.archive:
        print(json.dumps(stage(args.archive, args.destination, args.platform)))
        return
    name = f'rclone-v{VERSION}-{args.platform}.zip'
    url = f'https://github.com/rclone/rclone/releases/download/v{VERSION}/{name}'
    with tempfile.TemporaryDirectory(prefix='twodrive-component-') as tmp:
        archive = Path(tmp) / name
        request = urllib.request.Request(url, headers={'User-Agent': 'TwoDrive-component-build'})
        with urllib.request.urlopen(request, timeout=120) as response, archive.open('wb') as output:
            count = 0
            while chunk := response.read(1024 * 1024):
                count += len(chunk)
                if count > MAX_ARCHIVE:
                    raise ValueError('Component archive exceeds size budget')
                output.write(chunk)
        print(json.dumps(stage(archive, args.destination, args.platform)))


if __name__ == '__main__':
    main()
