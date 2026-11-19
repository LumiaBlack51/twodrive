#!/usr/bin/env python3
"""Compare isolated cold Linux FUSE mounts against a gated WebDAV source."""

import argparse
import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
from socket import socket
import subprocess
from tempfile import TemporaryDirectory
from threading import Event, Thread
from time import monotonic, sleep
from urllib.parse import unquote
from urllib.request import ProxyHandler, Request, build_opener


UNKNOWN = "unclassified.twodrive-fixture-524"
DOCUMENT = "document.txt"
FILES = {UNKNOWN: b"synthetic unknown format\n" * 1000, DOCUMENT: b"synthetic text\n"}


def check_mount(binary, guarded):
    get_started = Event()
    allow_get = Event()
    files = FILES.copy()
    gets = []

    class DavFixture(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def reply(self, status, body=b"", headers=None):
            self.send_response(status)
            self.send_header("Content-Length", str(len(body)))
            for name, value in (headers or {}).items():
                self.send_header(name, value)
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(body)

        def do_PROPFIND(self):
            def row(path, data):
                collection = "<d:collection/>" if data is None else ""
                size = "" if data is None else f"<d:getcontentlength>{len(data)}</d:getcontentlength>"
                return (
                    f"<d:response><d:href>{path}</d:href><d:propstat><d:prop>"
                    f"<d:resourcetype>{collection}</d:resourcetype>{size}"
                    "<d:getlastmodified>Thu, 19 Nov 2026 02:00:00 GMT</d:getlastmodified>"
                    '<d:getetag>"version-1"</d:getetag></d:prop>'
                    "<d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
                )

            self.rfile.read(int(self.headers.get("Content-Length", "0")))
            path = unquote(self.path)
            if path == "/":
                rows = row("/", None)
                if self.headers.get("Depth") == "1":
                    rows += "".join(row("/" + name, data) for name, data in files.items())
            elif path[1:] in files:
                rows = row(path, files[path[1:]])
            else:
                self.reply(404)
                return
            self.reply(207, f'<d:multistatus xmlns:d="DAV:">{rows}</d:multistatus>'.encode(),
                       {"Content-Type": "application/xml"})

        def do_GET(self):
            name = unquote(self.path)[1:]
            if name not in files:
                self.reply(404)
                return
            gets.append(name)
            get_started.set()
            if not allow_get.wait(10):
                self.reply(503)
                return
            data = files[name]
            headers = {"ETag": '"version-1"', "Content-Type": "application/octet-stream"}
            span = self.headers.get("Range")
            if span:
                start, _, end = span.removeprefix("bytes=").partition("-")
                first = int(start)
                last = min(int(end), len(data) - 1) if end else len(data) - 1
                headers["Content-Range"] = f"bytes {first}-{last}/{len(data)}"
                self.reply(206, data[first:last + 1], headers)
            else:
                self.reply(200, data, headers)

        def do_PUT(self):
            name = unquote(self.path)[1:]
            files[name] = self.rfile.read(int(self.headers["Content-Length"]))
            self.reply(201)

    with ThreadingHTTPServer(("127.0.0.1", 0), DavFixture) as dav, TemporaryDirectory() as work:
        thread = Thread(target=dav.serve_forever, daemon=True)
        thread.start()
        target = Path(work) / "mount"
        target.mkdir()
        with socket() as reserved:
            reserved.bind(("127.0.0.1", 0))
            control_address = f"127.0.0.1:{reserved.getsockname()[1]}"
        env = {k: v for k, v in os.environ.items() if not k.startswith("RCLONE_")}
        env.update(RCLONE_CONFIG_TWODRIVE_TYPE="webdav",
                   RCLONE_CONFIG_TWODRIVE_URL=f"http://127.0.0.1:{dav.server_port}/",
                   RCLONE_RC_ADDR=control_address, RCLONE_RC_USER="fixture",
                   RCLONE_RC_PASS="fixture", RCLONE_CACHE_DIR=str(Path(work) / "cache"),
                   NO_PROXY="127.0.0.1,localhost",
                   TWODRIVE_RCLONE_DEFER_GIO_PROBES="1" if guarded else "0")
        opener = build_opener(ProxyHandler({}))
        auth = "Basic " + base64.b64encode(b"fixture:fixture").decode()

        def rc(action, fields=None):
            req = Request(f"http://{control_address}/{action}", json.dumps(fields or {}).encode(),
                          {"Authorization": auth, "Content-Type": "application/json"})
            with opener.open(req, timeout=5) as response:
                return json.load(response)

        daemon = subprocess.Popen([binary, "rcd", "--config=/dev/null", "--ask-password=false",
                                   "--retries=1", "--low-level-retries=1"], env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        listing = None
        try:
            deadline = monotonic() + 5
            while True:
                try:
                    assert rc("core/pid")["pid"] == daemon.pid
                    break
                except OSError:
                    if daemon.poll() is not None or monotonic() >= deadline:
                        raise
                    sleep(0.025)
            rc("mount/mount", {"fs": "TWODRIVE:", "mountPoint": str(target),
                               "vfsOpt": {"CacheMode": 3, "DirCacheTime": "5s", "WriteBack": "0s"}})
            assert target.is_mount()
            assert not rc("vfs/queue").get("queue")
            started = monotonic()
            listing = subprocess.Popen(["gio", "list", "-a", "standard::*,access::*", str(target)],
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            if not guarded:
                assert get_started.wait(4), "Official component should GET the unknown file"
                sleep(0.35)
                assert listing.poll() is None, "GIO should still be waiting for that GET"
                allow_get.set()
            stdout, stderr = listing.communicate(timeout=3)
            assert listing.returncode == 0 and UNKNOWN.encode() in stdout, stderr
            elapsed = monotonic() - started
            if guarded:
                assert not gets, "Directory enumeration triggered a cloud GET"
                allow_get.set()
                destination = Path(work) / "copy"
                subprocess.run(["gio", "copy", str(target / UNKNOWN), str(destination)],
                               check=True, timeout=5)
                assert destination.read_bytes() == FILES[UNKNOWN]
                with os.fdopen(os.open(target / DOCUMENT, os.O_RDONLY | os.O_NOATIME), "rb") as file:
                    assert file.read() == FILES[DOCUMENT], "Other tools may still open with O_NOATIME"
                assert UNKNOWN in gets and DOCUMENT in gets
                written = b"synthetic write through the mounted component\n"
                (target / "write.txt").write_bytes(written)
                deadline = monotonic() + 5
                while files.get("write.txt") != written:
                    assert monotonic() < deadline, "Ordinary write did not reach the WebDAV backend"
                    sleep(0.025)
            print(f"{'Patched' if guarded else 'Official'} FUSE: full GIO enumeration {elapsed:.3f}s"
                  + (", zero GETs while listing, explicit reads and write passed" if guarded else ", blocked by GET"))
        finally:
            allow_get.set()
            if listing and listing.poll() is None:
                listing.kill()
                listing.communicate(timeout=5)
            if target.is_mount() and daemon.poll() is None:
                rc("mount/unmount", {"mountPoint": str(target)})
            if daemon.poll() is None:
                daemon.terminate()
                daemon.wait(timeout=5)
            dav.shutdown()
            thread.join(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("official", type=Path)
    parser.add_argument("patched", type=Path)
    args = parser.parse_args()
    check_mount(str(args.official.resolve(strict=True)), guarded=False)
    check_mount(str(args.patched.resolve(strict=True)), guarded=True)


if __name__ == "__main__":
    main()
