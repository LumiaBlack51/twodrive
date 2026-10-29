#!/usr/bin/env python3
"""Reintroduce the historical path-as-ID expressions in an isolated source copy.

These are mutation regressions, not live OneDrive verification. The user's tree
is never edited. Both candidates must compile and fail the behavioral assertion.
"""
import os
import re
from pathlib import Path
import shutil
import subprocess
import tempfile

experiment = Path(__file__).resolve().parents[1]
repository = experiment.parents[1]
checks = [
    ('desktop_connections.rs', r'backend\s*\.\s*download_path_to\(path,', 'backend.download_to(path,',
     'desktop::tests::ipc_file_service_works_with_non_dav_opaque_ids'),
    ('desktop_backup.rs', r'backend\s*\.\s*download_path_to\(dest,', 'backend.download_to(dest,',
     'desktop::backup::tests::backup_recovery_uses_opaque_ids_and_preserves_conflicts'),
]
with tempfile.TemporaryDirectory(prefix='td-provider-old-') as directory:
    root = Path(directory)
    shutil.copy2(repository / 'Cargo.toml', root / 'Cargo.toml')
    shutil.copytree(repository / 'crates', root / 'crates')
    project = root / 'experiments/twodrive-dev'
    shutil.copytree(experiment, project, ignore=shutil.ignore_patterns('target', '__pycache__'))
    # Preserve the new local adapter dependency in the disposable source graph.
    shutil.copytree(repository / 'experiments/windows-cfapi', root / 'experiments/windows-cfapi',
                    ignore=shutil.ignore_patterns('target', '__pycache__'))
    env = dict(os.environ, CARGO_TARGET_DIR=str(Path(os.environ.get('CARGO_TARGET_DIR', experiment / 'target')).resolve()))
    for filename, fixed, old, test in checks:
        path = project / 'src' / filename
        original = path.read_text(encoding='utf-8')
        assert len(re.findall(fixed, original)) == 1, f'Expected exactly one production expression in {filename}'
        try:
            path.write_text(re.sub(fixed, old, original), encoding='utf-8', newline='\n')
            result = subprocess.run(['cargo', 'test', '--locked', '--bin', 'twodrive-dav-engine',
                test, '--', '--exact'], cwd=project, env=env, text=True,
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            assert result.returncode != 0 and 'test result: FAILED' in result.stdout and 'a path was incorrectly used as a remote ID' in result.stdout, result.stdout[-6000:]
            print(f'Confirmed historical {filename} path-as-ID behavior fails {test}')
        finally:
            path.write_text(original, encoding='utf-8', newline='\n')
