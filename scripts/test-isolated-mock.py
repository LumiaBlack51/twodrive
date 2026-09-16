#!/usr/bin/env python3
"""Exercise only disposable mock storage with explicit CLI and daemon binaries.

No service management or live configuration discovery. Leaves the sandbox and
logs for inspection, and unmounts only the mount created by this invocation.
"""
import argparse
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import time


def wait_for(check, process, label):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise AssertionError(f"isolated daemon exited during {label}")
        if check():
            return
        time.sleep(0.05)
    raise AssertionError(f"timed out: {label}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", required=True, type=Path)
    parser.add_argument("--daemon", required=True, type=Path)
    args = parser.parse_args()
    cli, daemon = args.cli.resolve(strict=True), args.daemon.resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix="twodrive-isolated-mock-"))
    mount = root / "mount"
    env = dict(os.environ, XDG_CONFIG_HOME=str(root / "config"),
               XDG_DATA_HOME=str(root / "data"), XDG_CACHE_HOME=str(root / "xdg-cache"),
               TWODRIVE_MOUNT_DIR=str(mount), TWODRIVE_BACKEND="mock")
    print(f"Isolated sandbox: {root}", flush=True)

    def run(*command):
        return subprocess.run([str(cli), *command], env=env, check=True,
                              capture_output=True, text=True, timeout=20).stdout

    run("init-mock")
    database = root / "data/twodrive/twodrive.sqlite3"

    def record(path):
        with sqlite3.connect(database) as connection:
            return connection.execute("SELECT state, cache_path, cloud_remote_id FROM files WHERE path = ?", (path,)).fetchone()

    assert record("/README-cloud.txt")[0] == "online_only"
    with (root / "daemon.log").open("w") as log:
        process = subprocess.Popen([str(daemon)], env=env, stdout=log, stderr=log)
        try:
            wait_for(lambda: os.path.ismount(mount), process, "mount")
            sample = mount / "README-cloud.txt"
            assert b"Welcome to the mock OneDrive tree." in sample.read_bytes()
            assert record("/README-cloud.txt")[0] == "cached"
            # Durable local writes, upload acknowledgement, and rename/delete.
            created = mount / "refactor-smoke.txt"
            with created.open("wb") as output:
                output.write(b"isolated refactor payload\n")
                output.flush()
                os.fsync(output.fileno())
            wait_for(lambda: record("/refactor-smoke.txt")[0] == "cached", process, "upload")
            assert created.read_bytes() == b"isolated refactor payload\n"
            renamed = mount / "renamed-smoke.txt"
            created.rename(renamed)
            assert renamed.read_bytes() == b"isolated refactor payload\n"
            run("pin", "/README-cloud.txt")
            assert "effective_pinned=true\n" in run("status-path", "/README-cloud.txt")
            run("unpin", "/README-cloud.txt")
            run("release", "/README-cloud.txt")
            assert record("/README-cloud.txt")[0] == "online_only"
            assert b"Welcome" in sample.read_bytes()
            renamed.unlink()
            assert record("/renamed-smoke.txt") is None
            assert not (root / "config/twodrive/tokens.json").exists()
            print("PASS: isolated daemon hydration/write/upload/move/delete/pin/release", flush=True)
        finally:
            if os.path.ismount(mount):
                subprocess.run(["fusermount3", "-u", str(mount)], check=True, timeout=10)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=10)
            assert not os.path.ismount(mount)


if __name__ == "__main__":
    main()
