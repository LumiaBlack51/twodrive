#!/usr/bin/python3
"""Disposable rclone fixture for the first root-list retry."""
from pathlib import Path
import sys

assert "lsjson" in sys.argv
attempt = Path(sys.argv[sys.argv.index("--config") + 1]).with_suffix(".attempted")
if not attempt.exists():
    attempt.write_text("1")
    print("[{partial")
    sys.exit(5)
print("[]")
