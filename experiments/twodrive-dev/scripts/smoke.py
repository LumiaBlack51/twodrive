#!/usr/bin/env python3
"""Two real CLI processes, disposable files; optional isolated Linux FUSE mount."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.request


def wait_for(check, processes, seconds=45):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if any(process.poll() is not None for process in processes):
            raise RuntimeError("CLI process exited before smoke test completed")
        try:
            value = check()
            if value:
                return value
        except (OSError, ValueError):
            pass
        time.sleep(0.1)
    raise TimeoutError("smoke test timed out")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--network", choices=["lan", "public", "relay-only"], default="lan")
    parser.add_argument("--mount", action="store_true")
    parser.add_argument("--read-only", action="store_true")
    parser.add_argument("--keep-state", action="store_true", help="retain disposable test state/logs for diagnostics")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    network = ["--no-relay"] if args.network == "lan" else (["--relay-only"] if args.network == "relay-only" else [])
    from contextlib import nullcontext
    temporary = nullcontext(tempfile.mkdtemp(prefix="twodrive-dev-smoke-")) if args.keep_state else tempfile.TemporaryDirectory(prefix="twodrive-dev-smoke-")
    with temporary as directory:
        sandbox = Path(directory)
        if args.keep_state:
            print(f"Disposable test state: {sandbox}", flush=True)
        root = sandbox / "share"
        root.mkdir()
        mounted = sandbox / "mount"
        mounted.mkdir()
        ticket = sandbox / "invitation.json"
        client_state = sandbox / "client"
        data = bytes(range(251)) * (2 * 1024 * 1024 // 251)
        (root / "seed.bin").write_bytes(data)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        processes = []
        logs = []
        try:
            for name in ["server", "client"]:
                log = open(sandbox / f"{name}.log", "w+")
                logs.append(log)
                if name == "server":
                    command = [str(binary), "--state", str(sandbox / name), "serve", "--root", str(root), "--invite-file", str(ticket), *network]
                    if not args.read_only:
                        command += ["--write"]
                else:
                    command = [str(binary), "--state", str(client_state), "connect", "--ticket-file", str(ticket), "--listen", f"127.0.0.1:{port}", *network]
                    if args.mount:
                        command += ["--mount", str(mounted)]
                        if not args.read_only:
                            command += ["--write"]
                processes.append(subprocess.Popen(command, stdout=log, stderr=log))
                if name == "server":
                    wait_for(lambda: ticket.is_file() and json.loads(ticket.read_text()), processes)
            credentials = wait_for(lambda: json.loads((client_state / "credentials.json").read_text()), processes)
            authorization = "Basic " + base64.b64encode(f"{credentials['username']}:{credentials['password']}".encode()).decode()

            def request(path, method="GET", body=None, headers=None):
                request_headers = {"Authorization": authorization, **(headers or {})}
                req = urllib.request.Request(f"http://127.0.0.1:{port}/{path}", data=body, headers=request_headers, method=method)
                with urllib.request.urlopen(req, timeout=30) as response:
                    return response.read()

            downloaded = wait_for(lambda: request("seed.bin"), processes)
            assert hashlib.sha256(downloaded).digest() == hashlib.sha256(data).digest()
            if args.read_only:
                try:
                    request("denied.bin", "PUT", b"denied")
                    raise AssertionError("read-only server accepted PUT")
                except urllib.error.HTTPError as error:
                    assert error.code == 403
            else:
                request("roundtrip.bin", "PUT", data, {"If-None-Match": "*"})
                assert request("roundtrip.bin") == data
                request("roundtrip.bin", "MOVE", headers={"Destination": f"http://127.0.0.1:{port}/moved.bin", "Overwrite": "F"})
                request("moved.bin", "DELETE")
                assert not (root / "moved.bin").exists()
            if args.mount:
                wait_for(lambda: os.path.ismount(mounted), processes)
                assert (mounted / "seed.bin").read_bytes() == data
                if args.read_only:
                    try:
                        (mounted / "denied.txt").write_text("denied")
                        raise AssertionError("read-only mount accepted a write")
                    except OSError as error:
                        import errno
                        assert error.errno == errno.EROFS
                    assert not (root / "denied.txt").exists()
                    print("PASS: kernel read-only FUSE mount; server denies PUT")
                    return
                folder = mounted / "new-folder"
                folder.mkdir()
                local = folder / "written.txt"
                with local.open("wb") as file:
                    file.write(b"isolated FUSE save")
                    file.flush()
                    os.fsync(file.fileno())
                # A child saved before its parent's cloud acknowledgement is
                # deferred to the engine's 60-second recovery scan. Include that
                # existing retry interval in this integration test's deadline.
                upload_started = time.monotonic()
                wait_for(lambda: (root / "new-folder/written.txt").read_bytes() == b"isolated FUSE save", processes, seconds=90)
                print(f"Mount save confirmed after {time.monotonic() - upload_started:.1f}s", flush=True)
                folder.rename(mounted / "renamed-folder")
                wait_for(lambda: (root / "renamed-folder/written.txt").read_bytes() == b"isolated FUSE save", processes, seconds=90)
                (mounted / "renamed-folder/written.txt").unlink()
                (mounted / "renamed-folder").rmdir()
                wait_for(lambda: not (root / "renamed-folder").exists(), processes, seconds=90)
            logs[1].flush()
            client_output = (sandbox / "client.log").read_text()
            if args.network == "relay-only":
                assert "transport=relay" in client_output
            print(f"PASS: two CLI processes, network={args.network}, encrypted peer GET/PUT/MOVE/DELETE, {len(data)} bytes checked")
            if args.mount:
                print("PASS: isolated FUSE hydration, durable save/upload, directory move and delete")
        except Exception:
            for name, log in zip(["server", "client"], logs):
                log.flush()
                log.seek(0)
                print(f"{name} failure log:\n{log.read()[-4000:]}", flush=True)
            raise
        finally:
            if os.path.ismount(mounted):
                subprocess.run(["fusermount3", "-u", str(mounted)], check=True, timeout=10)
            for process in reversed(processes):
                process.terminate()
            for process in processes:
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            if any(process.returncode not in [0, -15] for process in processes):
                for log in logs:
                    log.flush()
                    log.seek(0)
                    print(log.read()[-2000:])
            for log in logs:
                log.close()


if __name__ == "__main__":
    main()
