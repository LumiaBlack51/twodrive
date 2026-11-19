#!/usr/bin/env python3
"""Build the Linux-only mount helper from the pinned Rclone source."""

import argparse
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


SOURCE = "https://github.com/rclone/rclone.git"
TAG = "v1.75.2"
COMMIT = "be8d6044eacb843b74d335db0c5fbe09222031fe"
PATCH = Path(__file__).resolve().parents[1] / "packaging/development/rclone-gio-mount.patch"


def build(destination):
    destination.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="rclone-mount-", dir=destination) as work:
        root = Path(work)
        source = root / "source"
        subprocess.run(["git", "-c", "advice.detachedHead=false", "clone", "--quiet",
                        "--depth", "1", "--branch", TAG, SOURCE, source], check=True)
        checked_out = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=source, text=True)
        if checked_out.strip() != COMMIT:
            raise ValueError("Rclone source commit does not match the pinned revision")
        subprocess.run(["git", "apply", "--index", "--unidiff-zero", "--whitespace=error", PATCH],
                       cwd=source, check=True)
        helper = root / "rclone-mount"
        env = dict(os.environ, GOOS="linux", CGO_ENABLED="0", GOTOOLCHAIN="local", GOWORK="off")
        subprocess.run(["go", "build", "-mod=readonly", "-trimpath", "-buildvcs=false",
                        "-o", helper, "./cmd/twodrive-mount"], cwd=source, env=env, check=True)
        license_file = root / "RCLONE-MOUNT-LICENSE.txt"
        shutil.copyfile(source / "COPYING", license_file)
        os.replace(license_file, destination / license_file.name)
        os.replace(helper, destination / helper.name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destination", type=Path, required=True)
    build(parser.parse_args().destination.resolve())


if __name__ == "__main__":
    main()
