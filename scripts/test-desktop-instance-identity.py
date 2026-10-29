"""Compile the production identity header and verify real isolated Windows mutexes; no GUI/account access."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument('--cmake',default='cmake')
parser.add_argument('--legacy-ref')
parser.add_argument('--report',type=Path)
args = parser.parse_args()
assert os.name == 'nt', 'Windows mutex check requires Windows'
repo = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='td-instance-identity-') as temp:
    root = Path(temp)
    build = root/'build'
    build.mkdir()
    if args.legacy_ref:
        old = subprocess.check_output(['git','show',f'{args.legacy_ref}:apps/dav/windows/runner/main.cpp'],cwd=repo,text=True,encoding='utf-8')
        start = old.index('std::wstring InstanceName(')
        end = old.index('struct InstanceWindow',start)
        (build/'legacy_identity.h').write_text(old[start:end],encoding='utf-8')
    subprocess.run([args.cmake,'-S',str(repo/'scripts/windows-instance-identity'),'-B',str(build),'-A','x64'],check=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
    subprocess.run([args.cmake,'--build',str(build),'--config','Release'],check=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
    report = json.loads(subprocess.check_output([str(build/'Release/instance_identity_test.exe'),str(root/'current')],text=True))
    if args.legacy_ref:
        old = subprocess.run([str(build/'Release/instance_identity_legacy.exe'),str(root/'legacy')],text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
        assert old.returncode != 0 and 'implicit/explicit default state must share a mutex' in old.stdout, old.stdout
        report['old_identity_reproduced_mismatch'] = True
    report['scope'] = 'production C++ identity and real Windows mutexes in a private test namespace; no GUI/account access'
if args.report: args.report.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
print(json.dumps(report))
