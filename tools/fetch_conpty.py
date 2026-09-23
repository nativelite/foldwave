#!/usr/bin/env python3
"""Fetch Microsoft's ConPTY package and put its console host beside fwterm.

Design section 11 row 6 (founder decision, 2026-09-23): programs that need a
console run under Microsoft's `Microsoft.Windows.Console.ConPTY` package (MIT),
shipped with foldwave - `conpty.dll` plus the `OpenConsole.exe` it starts from
its own folder. fwterm loads a `conpty.dll` found beside `fwterm.exe` and falls
back to the system's conhost if it is missing or cannot start.

This downloads one pinned version from nuget.org, checks its SHA-256 (a
changed or tampered package is refused), and copies the x64 `conpty.dll` and
`OpenConsole.exe` into the given build directory. Stdlib only.

    python tools/fetch_conpty.py                  # into target/release
    python tools/fetch_conpty.py target/debug
    python tools/fetch_conpty.py --check          # verify an existing copy
"""

from __future__ import annotations

import hashlib
import io
import sys
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PACKAGE = "Microsoft.Windows.Console.ConPTY"
VERSION = "1.24.260710001"
URL = f"https://www.nuget.org/api/v2/package/{PACKAGE}/{VERSION}"
# SHA-256 of the .nupkg measured in design section 8.6 (fwprobe, 2026-09-23).
SHA256 = "175640566a3b59c4b132070ee96c2c77e5ab7edd2e92732a5eb3610bbf63d90e"
# Where the x64 files sit inside the package, and their names beside fwterm.
FILES = {
    "runtimes/win-x64/native/conpty.dll": "conpty.dll",
    "build/native/runtimes/x64/OpenConsole.exe": "OpenConsole.exe",
}


def fetch() -> bytes:
    print(f"fetching {PACKAGE} {VERSION} from nuget.org ...")
    with urllib.request.urlopen(URL, timeout=120) as r:
        data = r.read()
    digest = hashlib.sha256(data).hexdigest()
    if digest != SHA256:
        raise SystemExit(
            f"refusing the package: SHA-256 {digest}, expected {SHA256}"
        )
    return data


def install(data: bytes, dest: Path) -> None:
    dest.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        for inner, name in FILES.items():
            (dest / name).write_bytes(z.read(inner))
            print(f"  {dest / name}")


def check(dest: Path) -> int:
    missing = [n for n in FILES.values() if not (dest / n).is_file()]
    if missing:
        print(f"missing beside fwterm in {dest}: {', '.join(missing)}")
        return 1
    print(f"{dest}: conpty.dll and OpenConsole.exe present")
    return 0


def main(argv: list[str]) -> int:
    args = [a for a in argv if not a.startswith("--")]
    dest = ROOT / (args[0] if args else "target/release")
    if "--check" in argv:
        return check(dest)
    install(fetch(), dest)
    print(f"done: fwterm in {dest} now hosts consoles with OpenConsole {VERSION}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
