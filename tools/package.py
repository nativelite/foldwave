#!/usr/bin/env python3
"""Build foldwave's download: one executable that installs itself. Stdlib only.

    python tools/package.py        # dist/foldwave-setup-<version>.exe

1. Microsoft's ConPTY package (pinned, SHA-256 checked by fetch_conpty.py),
   fetched once into target/conpty.
2. fwterm built in release with those files carried inside it: build.rs reads
   FOLDWAVE_CONPTY_DIR. The icon and version information are always built in.
3. The executable copied to dist/ as foldwave-setup-<version>.exe. Started
   under that name it installs foldwave for the user who runs it
   (crates/fwterm/src/install.rs), with no administrator rights.

This runs on the developer's machine only; the person installing needs nothing
but the one file.
"""

from __future__ import annotations

import hashlib
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tools"))

import fetch_conpty  # noqa: E402  (beside this file)


def version() -> str:
    text = (ROOT / "crates" / "fwterm" / "Cargo.toml").read_text(encoding="utf-8")
    found = re.search(r'^version = "([^"]+)"', text, re.MULTILINE)
    if not found:
        raise SystemExit("no version in crates/fwterm/Cargo.toml")
    return found.group(1)


def main() -> int:
    conpty = ROOT / "target" / "conpty"
    if fetch_conpty.check(conpty) != 0:
        fetch_conpty.install(fetch_conpty.fetch(), conpty)
    env = dict(os.environ, FOLDWAVE_CONPTY_DIR=str(conpty))
    build = ["cargo", "build", "--release", "--bin", "fwterm"]
    print(f"$ FOLDWAVE_CONPTY_DIR={conpty} {' '.join(build)}")
    code = subprocess.call(build, cwd=str(ROOT), env=env)
    if code:
        return code
    built = ROOT / "target" / "release" / "fwterm.exe"
    data = built.read_bytes()
    # The ConPTY files must be inside, or the install would have none.
    for name in fetch_conpty.FILES.values():
        if (conpty / name).read_bytes() not in data:
            print(f"{name} is not inside {built}: not packaged")
            return 1
    dist = ROOT / "dist"
    dist.mkdir(exist_ok=True)
    out = dist / f"foldwave-setup-{version()}.exe"
    shutil.copyfile(built, out)
    digest = hashlib.sha256(data).hexdigest()
    print(f"{out}  {len(data) / (1 << 20):.1f} MiB  sha256 {digest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
