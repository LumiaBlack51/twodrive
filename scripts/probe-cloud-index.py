"""Authorized read-only index/cache probe. Output only aggregate, non-identifying facts."""
import json
import subprocess
import sys
import time

exe, root = sys.argv[1:3]

def ipc(command):
    p = subprocess.run([exe, "ipc", "--state", root], input=json.dumps(dict(
        version=1, id=f"index-probe-{time.time_ns()}", command=command)),
        capture_output=True, text=True, encoding="utf-8", timeout=15, check=True)
    reply = json.loads(p.stdout)
    if not reply["ok"]:
        raise RuntimeError(reply.get("error", "IPC failed"))
    return reply["snapshot"]

def refresh():
    ipc(dict(type="refresh_index"))
    deadline = time.monotonic() + 600
    while time.monotonic() < deadline:
        view = ipc(dict(type="snapshot"))["cloud"]
        if view["status"] != "refreshing":
            if view["status"] != "ready":
                raise RuntimeError(view["error"])
            return view
        time.sleep(.25)
    raise TimeoutError("refresh timeout")

if __name__ == "__main__":
    first = refresh()
    print(json.dumps(dict(initial_delta="passed", indexed_count=first["count"])), flush=True)
    second = refresh()
    print(json.dumps(dict(incremental_delta="passed", indexed_count=second["count"])), flush=True)
    items = []
    for offset in range(0, second["count"], 100):
        items.extend(ipc(dict(type="index_page", offset=offset))["cloud"]["items"])
    files = [i for i in items if i["kind"] == "file"]
    small = next((i for i in files if 0 < i["size"] <= 2 * 1024 * 1024), None)
    print(json.dumps(dict(file_count=len(files), small_candidate=small is not None,
        larger_candidate=any(8 * 1024 * 1024 <= i["size"] <= 256 * 1024 * 1024 for i in files))), flush=True)
    if small:
        ipc(dict(type="download_cloud", id=small["id"]))
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            found = None
            for offset in range(0, second["count"], 100):
                view = ipc(dict(type="index_page", offset=offset))["cloud"]
                found = next((i for i in view["items"] if i["id"] == small["id"]), found)
            if found["state"] == "cached":
                print(json.dumps(dict(small_download="cached", bytes=found["size"])), flush=True)
                break
            if found["state"] == "error":
                raise RuntimeError("small_download_failed")
            time.sleep(.2)
        else:
            raise TimeoutError("small download timeout")
    ipc(dict(type="index_page", offset=0))
