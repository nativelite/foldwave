#!/usr/bin/env python3
"""Fetch the fonts fwterm ships and put them beside it, in `fonts/`.

Founder decisions, 2026-09-28: fwterm ships its fonts rather than relying on
the system's, and draws real italics rather than slanting the upright.

- **JetBrains Mono** (the default: simple, legible, made for code): its two
  variable files, upright and italic (`wght` 100..800; bold is 700).
- **Cascadia Mono** (`--font "Cascadia Mono"`): Windows installs only the
  upright; its italic is a separate file.

For each, this downloads one pinned release from GitHub, checks its SHA-256 (a
changed or tampered release is refused), and copies the variable files, with
the SIL Open Font License they come under, into `fonts/` in the given build
directory. fwterm's font catalog looks in that folder first, and loads JetBrains
Mono from it by default. Stdlib only.

    python tools/fetch_fonts.py                  # into target/release/fonts
    python tools/fetch_fonts.py target/debug
    python tools/fetch_fonts.py --check          # verify an existing copy
"""

from __future__ import annotations

import hashlib
import io
import sys
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Each package: where it comes from (SHA-256 measured 2026-09-28, two
# downloads agreeing), which files to take (zip path -> name in fonts/), and
# its license (inside the zip, or fetched with its own checksum).
PACKAGES = [
    {
        "name": "JetBrains Mono 2.304",
        "url": "https://github.com/JetBrains/JetBrainsMono/releases/download/"
        "v2.304/JetBrainsMono-2.304.zip",
        "sha256": "6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf",
        "files": {
            "fonts/variable/JetBrainsMono[wght].ttf": "JetBrainsMono.ttf",
            "fonts/variable/JetBrainsMono-Italic[wght].ttf": "JetBrainsMono-Italic.ttf",
            "OFL.txt": "JetBrainsMono-OFL.txt",
        },
    },
    {
        "name": "Cascadia Code 2407.24",
        "url": "https://github.com/microsoft/cascadia-code/releases/download/"
        "v2407.24/CascadiaCode-2407.24.zip",
        "sha256": "e67a68ee3386db63f48b9054bd196ea752bc6a4ebb4df35adce6733da50c8474",
        "files": {
            "ttf/CascadiaMono.ttf": "CascadiaMono.ttf",
            "ttf/CascadiaMonoItalic.ttf": "CascadiaMonoItalic.ttf",
        },
        # The release zip carries no license; the one at the same tag.
        "license": (
            "https://raw.githubusercontent.com/microsoft/cascadia-code/v2407.24/LICENSE",
            "51882cd3cdba4e16f220f44ddb08a635c38c44ea6e0975db2574f4be6f958238",
            "CascadiaCode-OFL.txt",
        ),
    },
]


def download(url: str, sha256: str, what: str) -> bytes:
    print(f"fetching {what} ...")
    with urllib.request.urlopen(url, timeout=300) as r:
        data = r.read()
    digest = hashlib.sha256(data).hexdigest()
    if digest != sha256:
        raise SystemExit(f"refusing {what}: SHA-256 {digest}, expected {sha256}")
    return data


def names(p: dict) -> list[str]:
    out = list(p["files"].values())
    if "license" in p:
        out.append(p["license"][2])
    return out


def install(fonts: Path) -> None:
    fonts.mkdir(parents=True, exist_ok=True)
    for p in PACKAGES:
        release = download(p["url"], p["sha256"], p["name"])
        with zipfile.ZipFile(io.BytesIO(release)) as z:
            for inner, name in p["files"].items():
                (fonts / name).write_bytes(z.read(inner))
                print(f"  {fonts / name}")
        if "license" in p:
            url, sha, name = p["license"]
            (fonts / name).write_bytes(download(url, sha, f"{p['name']} license"))
            print(f"  {fonts / name}")


def check(fonts: Path) -> int:
    missing = [n for p in PACKAGES for n in names(p) if not (fonts / n).is_file()]
    if missing:
        print(f"missing in {fonts}: {', '.join(missing)}")
        return 1
    print(f"{fonts}: JetBrains Mono and Cascadia Mono, with their licenses, present")
    return 0


def main(argv: list[str]) -> int:
    args = [a for a in argv if not a.startswith("--")]
    fonts = ROOT / (args[0] if args else "target/release") / "fonts"
    if "--check" in argv:
        return check(fonts)
    install(fonts)
    print(f"done: fwterm in {fonts.parent} has its fonts")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
