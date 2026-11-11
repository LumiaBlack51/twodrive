#!/usr/bin/env python3
"""Windows manual regression against a configured local HTTP proxy.

Build --example google-transport first. Sends credential-free GETs to the public
Google token endpoint, never starts authorization or requests Drive files.
Does not change user proxy settings. Requires an already configured system proxy.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--probe', required=True)
    parser.add_argument('--control-proxy', required=True)
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    if os.name != 'nt':
        parser.error('This manual native regression requires Windows')
    env = {k: v for k, v in os.environ.items()
           if k.lower() not in ('http_proxy', 'https_proxy', 'all_proxy', 'no_proxy')}
    checks = []
    for route in ('loopback-control', 'system', 'direct'):
        command = [args.probe, route]
        if route == 'loopback-control':
            command.append(args.control_proxy)
        result = subprocess.run(command, capture_output=True, text=True,
                                encoding='utf-8', env=env, timeout=35, check=True)
        checks.append(json.loads(result.stdout))
    report = {'scope': 'actual Windows user system proxy; public endpoint without credentials',
              'environment_proxies_removed': True, 'system_settings_modified': False,
              'checks': checks,
              'passed': checks[0]['reachable'] and checks[1]['reachable']}
    Path(args.output).write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    print(json.dumps(report))
    assert report['passed'], 'Control or system-proxy route could not reach the public endpoint'


if __name__ == '__main__':
    main()
