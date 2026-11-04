"""Start isolated, empty GUI states to verify real Windows singleton behavior."""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser()
parser.add_argument('--app', type=Path, required=True)
parser.add_argument('--report', type=Path, required=True)
args = parser.parse_args()
children = []
report = {'scope': 'actual Windows GUI process lifetime with empty private states; no account login', 'checks': {}}
with tempfile.TemporaryDirectory(prefix='td-gui-instances-') as tmp:
    root = Path(tmp)
    try:
        for label in ('a', 'b'):
            state = root / label
            state.mkdir()
            child = subprocess.Popen([str(args.app.resolve()), '--state', str(state),
                '--window-title', f'TwoDrive isolated instance check {label}'], cwd=args.app.resolve().parent)
            children.append(child)
        time.sleep(3)
        assert all(child.poll() is None for child in children), 'Separate states must not be redirected to the existing old client'
        report['checks']['two_independent_states_run_concurrently'] = True
        duplicate = subprocess.Popen([str(args.app.resolve()), '--state', str(root / 'a') + '/.',
            '--window-title', 'TwoDrive duplicate should exit'], cwd=args.app.resolve().parent)
        children.append(duplicate)
        assert duplicate.wait(timeout=15) == 0
        assert children[0].poll() is None and children[1].poll() is None
        report['checks']['same_canonical_state_is_single_instance'] = True
    finally:
        for child in children:
            if child.poll() is None:
                child.terminate()
            child.wait(timeout=10)
        time.sleep(1)
report['passed'] = all(report['checks'].values())
args.report.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
print(json.dumps(report))
