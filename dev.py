#!/usr/bin/env python3
"""Local dev runner for foldwave -- stdlib Python driving cargo, no task runner.

The same `python dev.py check` gate as every nativelite package, so the muscle
memory is identical across repos. There is no CI (GitHub Actions are off by
founder request), so this local run is the sole gate and must be green before
every push.

  python dev.py check                 # guard + cargo fmt --check + cargo test
  python dev.py test                  # cargo test (all targets + doctests)
  python dev.py build                 # cargo build --release
  python dev.py fmt                   # cargo fmt --check
  python dev.py fix                   # cargo fmt (rewrites files)
  python dev.py guard                 # zero-dependency guard
  python dev.py docs                  # docs-only gate: links and tables
  python dev.py tables                # generated tables are up to date
  python dev.py run -- [args]         # cargo run --release --bin resmon
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
PY = sys.executable


def run(*args: str) -> int:
    print(f"$ {' '.join(args)}")
    return subprocess.call(args, cwd=str(ROOT))


def test() -> int:
    return run("cargo", "test", "--all-targets") or run("cargo", "test", "--doc")


def build() -> int:
    return run("cargo", "build", "--release")


def fmt() -> int:
    return run("cargo", "fmt", "--check")


def fix() -> int:
    return run("cargo", "fmt")


def guard() -> int:
    return run(PY, "tools/dep_guard.py")


def docs() -> int:
    return run(PY, "tools/docs_gate.py")


def tables() -> int:
    return run(PY, "tools/gen_boxdraw.py", "--check")


def run_bin() -> int:
    extra = sys.argv[2:]
    if extra and extra[0] == "--":
        extra = extra[1:]
    return run("cargo", "run", "--release", "--bin", "resmon", "--", *extra)


def check() -> int:
    """The gate: dependency guard, docs, generated tables, formatting, then
    the full suite."""
    return guard() or docs() or tables() or fmt() or test()


COMMANDS = {
    "check": check,
    "test": test,
    "build": build,
    "fmt": fmt,
    "fix": fix,
    "guard": guard,
    "docs": docs,
    "tables": tables,
    "run": run_bin,
}


def main(argv: list[str]) -> int:
    cmd = argv[1] if len(argv) > 1 else "check"
    fn = COMMANDS.get(cmd)
    if fn is None:
        print(f"unknown command {cmd!r}; choose from: {', '.join(COMMANDS)}")
        return 2
    return fn()


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
