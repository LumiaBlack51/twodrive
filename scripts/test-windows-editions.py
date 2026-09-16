#!/usr/bin/env python3
"""Windows-only disposable Full -> Lite handoff with no installer or real account."""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser()
parser.add_argument("--full", required=True)
parser.add_argument("--lite", required=True)
args = parser.parse_args()
full, lite = Path(args.full).resolve(), Path(args.lite).resolve()
root = Path(tempfile.mkdtemp(prefix="twodrive-editions-")) / "state"
engine_pid = None
children = []
def rpc(command):
    result = subprocess.run([str(lite / "twodrive-engine.exe"), "ipc", "--state", str(root)],
        input=json.dumps({"version":1, "id":str(time.time_ns()), "command":command}),
        capture_output=True, text=True, encoding="utf-8", timeout=8)
    if result.returncode: raise RuntimeError("not connected")
    return json.loads(result.stdout)
def tray(package, extra=()):
    process = subprocess.Popen([str(package / "twodrive-tray.exe"), "--state", str(root), "--mock", *extra],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW)
    children.append(process)
    return process
try:
    first = tray(full, ["--full"])
    for _ in range(100):
        try:
            snapshot = rpc({"type":"snapshot"})["snapshot"]
            engine_pid = snapshot["engine_pid"]
            break
        except RuntimeError:
            time.sleep(.1)
    assert engine_pid is not None and first.poll() is None
    assert rpc({"type":"set_paused","paused":True})["ok"]
    second = tray(lite)
    assert second.wait(timeout=8) != 0, "two edition trays were admitted"
    first.terminate()
    first.wait(timeout=8)
    replacement = tray(lite)
    time.sleep(.6)
    assert replacement.poll() is None
    snapshot = rpc({"type":"snapshot"})["snapshot"]
    assert snapshot["engine_pid"] == engine_pid, "edition switch started another engine"
    assert snapshot["paused"], "edition switch lost saved pause"
    replacement.terminate()
    replacement.wait(timeout=8)
    assert rpc({"type":"snapshot"})["snapshot"]["engine_pid"] == engine_pid
    assert not list(lite.rglob("*flutter*")) and not list(lite.rglob("*dart*"))
    assert (full / "twodrive-engine.exe").read_bytes() == (lite / "twodrive-engine.exe").read_bytes()
    assert (full / "twodrive-tray.exe").read_bytes() == (lite / "twodrive-tray.exe").read_bytes()
    print(json.dumps({"result":"PASS","root":str(root),"checks":[
        "same engine and tray bytes","Lite has no Flutter/Dart","Full owns single tray",
        "second Lite tray rejected","close Full tray keeps engine","Lite reuses same engine PID",
        "pause setting retained","close Lite tray keeps engine"]}, indent=2))
finally:
    for child in children:
        if child.poll() is None:
            child.terminate()
            child.wait(timeout=8)
    if engine_pid is not None:
        os.kill(engine_pid, signal.SIGTERM)
