#!/usr/bin/env python3
"""Docs gate for foldwave (stdlib only).

foldwave's CLAUDE.md requires, for documentation changes, that links resolve and
tables render. This makes that check reproducible instead of ad hoc:

1. every relative markdown link points at a file that exists;
2. every markdown table has a separator row and a consistent column count;
3. every section cross-reference in docs/design.md names a heading that exists.

External (http/https) links are not fetched: the gate must stay offline and
deterministic.
"""

from __future__ import annotations

import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LINK = re.compile(r"\[([^\]]+)\]\(([^)]+)\)")
HEADING = re.compile(r"^#{2,4}\s+(\d+(?:\.\d+[a-z]?)?)\.?\s", re.M)
SECTION_REF = re.compile("§(\\d+(?:\\.\\d+[a-z]?)?)")


def markdown_files() -> list[Path]:
    skip = {".git", "target", "__pycache__"}
    out = []
    for dirpath, dirnames, filenames in os.walk(ROOT):
        dirnames[:] = [d for d in dirnames if d not in skip]
        out.extend(Path(dirpath) / f for f in filenames if f.endswith(".md"))
    return sorted(out)


def check_links(path: Path, text: str) -> list[str]:
    problems = []
    for _, target in LINK.findall(text):
        if target.startswith(("http://", "https://", "#", "mailto:")):
            continue
        relative = target.split("#")[0]
        if not relative:
            continue
        if not (path.parent / relative).exists():
            problems.append(f"{path.relative_to(ROOT).as_posix()}: broken link -> {target}")
    return problems


def check_tables(path: Path, text: str) -> list[str]:
    problems = []
    name = path.relative_to(ROOT).as_posix()
    block: list[str] = []
    start = 0

    def flush(block: list[str], start: int) -> None:
        if len(block) < 2:
            return
        widths = {row.count("|") for row in block}
        if len(widths) > 1:
            problems.append(f"{name}:{start + 1}: ragged table, column counts {sorted(widths)}")
        if "---" not in block[1]:
            problems.append(f"{name}:{start + 2}: table has no separator row")

    for i, line in enumerate(text.split("\n")):
        stripped = line.strip()
        if stripped.startswith("|") and stripped.endswith("|"):
            if not block:
                start = i
            block.append(stripped)
        else:
            flush(block, start)
            block = []
    flush(block, start)
    return problems


def check_section_refs() -> list[str]:
    design = ROOT / "docs" / "design.md"
    if not design.exists():
        return []
    text = design.read_text(encoding="utf-8")
    headings = set(HEADING.findall(text))
    return [
        f"docs/design.md: dangling cross-reference to section {ref}"
        for ref in sorted(set(SECTION_REF.findall(text)))
        if ref not in headings
    ]


def main() -> int:
    problems: list[str] = []
    files = markdown_files()
    for path in files:
        text = path.read_text(encoding="utf-8")
        problems += check_links(path, text)
        problems += check_tables(path, text)
    problems += check_section_refs()

    if problems:
        print("Docs gate FAILED:")
        for p in problems:
            print(f"  - {p}")
        return 1
    print(f"Docs gate OK: {len(files)} files, links resolve, tables render, refs valid.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
