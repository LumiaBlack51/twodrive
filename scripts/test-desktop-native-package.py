"""Exercise the assembled Windows payload, never the user's state or root."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile

spec = importlib.util.spec_from_file_location('desktop_fixture', Path(__file__).with_name('test-dav-desktop.py'))
desktop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(desktop)


def verify(bundle):
    bundle = Path(bundle).resolve(strict=True)
    for name in ('twodrive_dav.exe', 'twodrive-dev.exe',
                 'twodrive-dav-engine.exe', 'twodrive-native-dev.exe'):
        if not (bundle / name).is_file():
            raise AssertionError(f'Missing packaged component: {name}')
    with tempfile.TemporaryDirectory(prefix='td-native-package-') as private:
        options = {'creationflags': subprocess.CREATE_NO_WINDOW} if os.name == 'nt' else {}
        engine = desktop.Engine(str(bundle / 'twodrive-dav-engine.exe'), str(bundle / 'twodrive-dev.exe'), Path(private))
        try:
            status = engine.call('status', timeout=45)
            assert status['cfapi']['supported'] is True, 'Package lacks native-sync'
            assert status['cfapi']['configured'] is False, 'Smoke state must be fresh'
        finally:
            engine.close()
        assert engine.process.returncode == 0, 'Packaged engine did not exit cleanly'
        helper = subprocess.run([str(bundle / 'twodrive-native-dev.exe'), '--help'],
                                capture_output=True, timeout=20, **options)
        assert helper.returncode == 0, 'Packaged native helper cannot run'
    return {'passed': True, 'native_sync': True, 'helper_runs': True,
            'root_registered': False, 'cloud_requests': False}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--bundle', required=True)
    print(json.dumps(verify(parser.parse_args().bundle)))
