#!/usr/bin/env python3
"""Fetch Microsoft's Cascadia Mono (regular and italic) and put it beside fwterm.

Founder decision, 2026-09-28: fwterm ships Cascadia Mono's real italic rather
than slanting the upright. Windows installs only `CascadiaMono.ttf` (upright,
a `wght` 200..700 variable font); the italic is a separate file. This
downloads one pinned Cascadia Code release from Microsoft's GitHub, checks its
SHA-256 (a changed or tampered release is refused), and copies the two
variable Mono files - `CascadiaMono.ttf` and `CascadiaMonoItalic.ttf`, bold
and bold italic drawn from them at `wght` 700 - into `fonts/` in the given
build directory, with the SIL Open Font License they come under (also checked
by SHA-256). fwterm's font catalog looks in that folder first. Stdlib only.

    python tools/fetch_cascadia.py                  # into target/release/fonts
    python tools/fetch_cascadia.py target/debug
    python tools/fetch_cascadia.py --check          # verify an existing copy
"""

from __future__ import annotations

import hashlib
import io
import sys
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VERSION = "2407.24"
URL = (
    "https://github.com/microsoft/cascadia-code/releases/download/"
    f"v{VERSION}/CascadiaCode-{VERSION}.zip"
)
# SHA-256 of the release zip, measured 2026-09-28 (two downloads agreed).
SHA256 = "e67a68ee3386db63f48b9054bd196ea752bc6a4ebb4df35adce6733da50c8474"
# The license at the same tag; the release zip carries none.
LICENSE_URL = (
    f"https://raw.githubusercontent.com/microsoft/cascadia-code/v{VERSION}/LICENSE"
)
LICENSE_SHA256 = "51882cd3cdba4e16f220f44ddb08a635c38c44ea6e0975db2574f4be6f958238"
# Files taken from the zip, and their names in fonts/.
FILES = {
    "ttf/CascadiaMono.ttf": "CascadiaMono.ttf",
    "ttf/CascadiaMonoItalic.ttf": "CascadiaMonoItalic.ttf",
}
LICENSE_NAME = "CascadiaCode-OFL.txt"


def download(url: str, sha256: str, what: str) -> bytes:
    print(f"fetching {what} ...")
    with urllib.request.urlopen(url, timeout=300) as r:
        data = r.read()
    digest = hashlib.sha256(data).hexdigest()
    if digest != sha256:
        raise SystemExit(f"refusing {what}: SHA-256 {digest}, expected {sha256}")
    return data


def install(release: bytes, license: bytes, fonts: Path) -> None:
    fonts.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(io.BytesIO(release)) as z:
        for inner, name in FILES.items():
            (fonts / name).write_bytes(z.read(inner))
            print(f"  {fonts / name}")
    (fonts / LICENSE_NAME).write_bytes(license)
    print(f"  {fonts / LICENSE_NAME}")


def check(fonts: Path) -> int:
    names = [*FILES.values(), LICENSE_NAME]
    missing = [n for n in names if not (fonts / n).is_file()]
    if missing:
        print(f"missing in {fonts}: {', '.join(missing)}")
        return 1
    print(f"{fonts}: Cascadia Mono regular and italic, and their license, present")
    return 0


def main(argv: list[str]) -> int:
    args = [a for a in argv if not a.startswith("--")]
    fonts = ROOT / (args[0] if args else "target/release") / "fonts"
    if "--check" in argv:
        return check(fonts)
    release = download(URL, SHA256, f"Cascadia Code {VERSION}")
    license = download(LICENSE_URL, LICENSE_SHA256, "its license (SIL OFL 1.1)")
    install(release, license, fonts)
    print(f"done: fwterm in {fonts.parent} has Cascadia Mono {VERSION} with italic")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
