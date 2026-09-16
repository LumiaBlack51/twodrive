#!/usr/bin/env python3
"""Cross-platform, isolated process test. Never discovers normal application paths."""
import argparse
import json
import subprocess
import tempfile
import time
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument("--engine", required=True)
args = p.parse_args()
exe = str(Path(args.engine).resolve())
root = Path(tempfile.mkdtemp(prefix="twodrive-ipc-"))
sequence = 0
def rpc(command, version=1, request_id=None):
    global sequence
    sequence += 1
    data = {"version": version, "id": request_id or str(sequence), "command": command}
    result = subprocess.run([exe, "ipc", "--state", str(root)], input=json.dumps(data),
                            text=True, encoding="utf-8", capture_output=True, timeout=8)
    if result.returncode:
        raise RuntimeError("IPC unavailable")
    return json.loads(result.stdout)
log = open(root / "test-process.log", "w")
# Root ownership deliberately needs a separate empty state subdirectory.
root = root / "state"
server = subprocess.Popen([exe, "serve", "--state", str(root), "--mock"], stdout=log, stderr=log)
try:
    for attempt in range(80):
        try:
            snap = rpc({"type": "snapshot"})
            break
        except RuntimeError:
            time.sleep(.1)
    else:
        raise AssertionError("server did not start")
    assert snap["ok"] and snap["snapshot"]["mode"] == "isolated_mock"
    assert not list((root / "cache").iterdir()), "listing hydrated files"
    duplicate = subprocess.run([exe, "serve", "--state", str(root), "--mock"], capture_output=True, timeout=8)
    assert duplicate.returncode != 0, "second engine was admitted"
    assert not rpc({"type": "set_paused", "paused": True}, version=42)["ok"]
    paused = rpc({"type": "set_paused", "paused": True})
    assert paused["snapshot"]["paused"]
    imported = rpc({"type": "mock_import", "name": "ipc-roundtrip.txt", "content": "isolated round trip"})
    assert imported["ok"]
    item = next(f for f in imported["snapshot"]["files"] if f["name"] == "ipc-roundtrip.txt")
    assert item["state"] == "dirty"
    assert not rpc({"type": "release", "id": item["id"]})["ok"]
    time.sleep(.3)
    assert rpc({"type": "snapshot"})["snapshot"]["queued"] == 1
    assert rpc({"type": "set_paused", "paused": False})["ok"]
    for _ in range(80):
        snap = rpc({"type": "snapshot"})["snapshot"]
        if snap["recent"]:
            break
        time.sleep(.1)
    assert snap["recent"][0]["outcome"] == "completed"
    assert rpc({"type": "release", "id": item["id"]})["ok"]
    assert rpc({"type": "download", "id": item["id"]})["ok"]
    for _ in range(80):
        snap = rpc({"type": "snapshot"})["snapshot"]
        if len(snap["recent"]) == 2:
            break
        time.sleep(.1)
    assert snap["recent"][0]["outcome"] == "completed"
    assert (root / "cache" / item["id"]).read_text() == "isolated round trip"
    assert rpc({"type": "set_paused", "paused": True})["ok"]
    server.terminate()
    server.wait(timeout=8)
    try:
        rpc({"type": "snapshot"})
        raise AssertionError("dead engine appeared connected")
    except RuntimeError:
        pass
    server = subprocess.Popen([exe, "serve", "--state", str(root), "--mock"], stdout=log, stderr=log)
    for _ in range(80):
        try:
            snap = rpc({"type": "snapshot"})["snapshot"]
            break
        except RuntimeError:
            time.sleep(.1)
    assert snap["paused"], "edition-compatible settings did not survive restart"
    print(json.dumps({"result": "PASS", "transport": "native named pipe on Windows; Unix socket on Linux",
      "root": str(root), "checks": ["no implicit hydration", "single engine", "protocol rejection",
      "scheduler pause", "dirty protection", "upload acknowledgement", "download byte equality",
      "disconnect", "restart retains settings"]}, indent=2))
finally:
    server.terminate()
    server.wait(timeout=8)
    log.close()
