#!/usr/bin/env python3
"""Check local package versions in every tracked workspace lockfile."""
from pathlib import Path
import subprocess
import tomllib


def verify(manifests, locks):
    versions = {}
    for path in manifests:
        package = tomllib.loads(path.read_text()).get('package', {})
        if isinstance(package.get('version'), str):
            versions.setdefault(package['name'], set()).add(package['version'])
    errors = []
    for path in locks:
        for package in tomllib.loads(path.read_text()).get('package', []):
            expected = versions.get(package['name'])
            if 'source' not in package and expected and package['version'] not in expected:
                errors.append(f"{path}: {package['name']} locks {package['version']}; local manifest declares {', '.join(sorted(expected))}")
    if errors:
        raise ValueError('\n'.join(errors))


def main():
    root = Path(__file__).resolve().parent.parent
    tracked = subprocess.check_output(['git', '-C', str(root), 'ls-files', '-z'], text=True).split('\0')
    manifests = [root / name for name in tracked if Path(name).name == 'Cargo.toml']
    locks = [root / name for name in tracked if Path(name).name == 'Cargo.lock']
    verify(manifests, locks)
    print(f'PASS: local package versions match all {len(locks)} tracked workspace lockfiles')


if __name__ == '__main__':
    main()
