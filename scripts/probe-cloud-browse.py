"""Authorized GET-only live probe. Logs aggregate evidence, never names/IDs/tokens."""
import json
import subprocess
import sys
import time

exe, root = sys.argv[1:3]
sequence = 0
def ipc(command):
    global sequence
    sequence += 1
    request = dict(version=1, id=f"probe-{time.time_ns()}-{sequence}", command=command)
    p = subprocess.run([exe, "ipc", "--state", root], input=json.dumps(request),
                       capture_output=True, text=True, encoding="utf-8", check=True)
    reply = json.loads(p.stdout)
    assert reply["ok"], reply.get("error")
    return reply["snapshot"]

def directory(item=None, drive=None):
    query = f"probe-{time.time_ns()}"
    ipc(dict(type="browse", query_id=query, drive_id=drive, item_id=item))
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        snap = ipc(dict(type="snapshot"))
        d = snap.get("directory")
        if d and d["query_id"] == query and d["status"] != "loading":
            assert d["status"] in ("partial", "complete"), d.get("error")
            assert not snap["recent"], "metadata must not produce sync activity"
            return d
        time.sleep(.25)
    raise TimeoutError("directory query did not complete")

root_page = directory()
page = root_page["page"]
print(json.dumps(dict(root_status=root_page["status"], entries=len(page["items"]),
    folders=sum(i["kind"] == "folder" for i in page["items"]),
    files=sum(i["kind"] == "file" for i in page["items"]),
    unsupported=sum(i["kind"] == "unsupported" for i in page["items"])), ensure_ascii=True))
folder = next((i for i in page["items"] if i["kind"] == "folder"), None)
if folder:
    child = directory(folder["id"], page["drive_id"])
    print(json.dumps(dict(child_status=child["status"], entries=len(child["page"]["items"]))))
directory()
cli = subprocess.run([exe, "list", "--state", root], capture_output=True,
                     text=True, encoding="utf-8", check=True, timeout=125)
listed = json.loads(cli.stdout)
assert listed["status"] in ("partial", "complete")
assert all(listed["page"][key] for key in ("account_id", "drive_id", "item_id"))
print("PASS live root / available child / return / refresh / Lite list; cloud GET metadata only")
