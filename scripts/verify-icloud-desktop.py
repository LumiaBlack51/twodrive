#!/usr/bin/env python3
"""Explicit live iCloud read-only verification; no auth input, cloud writes or private listings in reports."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile

spec = importlib.util.spec_from_file_location('desktop_check', Path(__file__).with_name('test-dav-desktop.py'))
desktop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(desktop)


def require_idle_account(state):
    # The desktop retains a shared auth lock while connected. Refuse before
    # starting any cloud request if an existing client is using this login.
    # This is a preflight, not a replacement for the engine's operation locks.
    with (state / 'auth.lock').open('r+b') as lock:
        try:
            if os.name == 'nt':
                import msvcrt
                msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            raise RuntimeError('Account is in use; disconnect its GUI connection before live validation. No cloud request was made.') from None
        if os.name == 'nt':
            msvcrt.locking(lock.fileno(), msvcrt.LK_UNLCK, 1)
        else:
            fcntl.flock(lock.fileno(), fcntl.LOCK_UN)


def run(args):
    auth = json.loads((args.icloud_state / 'auth.json').read_text(encoding='utf-8'))
    if auth.get('kind') != 'icloud' or auth.get('ready') is not True:
        raise RuntimeError('Human iCloud login has not completed; no authentication was attempted')
    require_idle_account(args.icloud_state)
    report = {'scope': 'live iCloud Drive read-only through packaged desktop engine', 'checks': {},
              'cloud_write_requests': 0,
              'boundaries': ['No cloud create/upload/overwrite/rename/delete, backup, native mount, ADP or expiration validation.',
                             'Only bounded shallow browsing and one small existing file are read; private names/content are not reported.']}
    checks = report['checks']
    with tempfile.TemporaryDirectory(prefix='td-icloud-live-read-') as tmp:
        root = Path(tmp)
        engine = desktop.Engine(str(args.engine.resolve()), str(args.peer.resolve()), root)
        try:
            profile = dict(id='icloud-read-probe', name='Isolated read validation', kind='icloud',
                           icloud_state=str(args.icloud_state.resolve()), network_route='system', writable=False)
            engine.call('save_profile', profile)
            engine.call('connect', {'id': profile['id']}, timeout=180)
            checks['saved_human_login_connects'] = True
            public = next(p for p in engine.call('profiles')['profiles'] if p['id'] == profile['id'])
            assert not any(public['capabilities'].values()), 'Unexpected write capability'
            checks['write_capabilities_disabled'] = True
            listing = engine.call('list', {'path': '/'}, timeout=90)['entries']
            report['root_entry_count'] = len(listing)
            checks['root_shallow_listing'] = True
            directories = [p['path'] for p in listing if p['directory']]
            candidates = [(p, '/') for p in listing if not p['directory'] and 0 < p['size'] <= args.max_bytes]
            scanned = 0
            while not candidates and directories and scanned < 8:
                folder = directories.pop(0)
                rows = engine.call('list', {'path': folder}, timeout=90)['entries']
                scanned += 1
                candidates.extend((p, folder) for p in rows if not p['directory'] and 0 < p['size'] <= args.max_bytes)
                directories.extend(p['path'] for p in rows if p['directory'])
            report['additional_directories_read'] = scanned
            if candidates:
                item, parent = candidates[0]
                first, second = root / 'first.bin', root / 'second.bin'
                for output in (first, second):
                    engine.call('download', {'path': item['path'], 'output': str(output), 'size': item['size']}, timeout=420)
                original = first.read_bytes()
                assert len(original) == item['size'] and original == second.read_bytes(), 'Downloaded contents differ'
                current = engine.call('list', {'path': parent}, timeout=90)['entries']
                matching = [p for p in current if p['path'] == item['path']]
                assert len(matching) == 1 and matching[0]['etag'] == item['etag'], 'Read metadata snapshot changed'
                engine.call('download', {'path': item['path'], 'output': str(first)}, failure=True)
                assert first.read_bytes() == original, 'Existing local download was overwritten'
                checks.update(two_downloads_equal=True, metadata_snapshot_unchanged=True, local_no_clobber=True)
                report.update(download_bytes=len(original), download_sha256=hashlib.sha256(original).hexdigest())
            else:
                report['boundaries'].append('No existing small file was found within the browse budget; downloads remain unverified.')
            engine.call('disconnect')
            engine.call('connect', {'id': profile['id']}, timeout=180)
            engine.call('list', {'path': '/'}, timeout=90)
            checks['saved_login_reconnects_without_prompt'] = True
        finally:
            engine.close()
    report['passed'] = bool(candidates) and all(checks.values())
    args.report.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--engine', type=Path, required=True)
    parser.add_argument('--peer', type=Path, required=True)
    parser.add_argument('--icloud-state', type=Path, required=True)
    parser.add_argument('--report', type=Path, required=True)
    parser.add_argument('--max-bytes', type=int, default=1024 * 1024)
    print(json.dumps(run(parser.parse_args()), indent=2))
