#!/usr/bin/env python3
"""nativelite zero-dependency guard for the foldwave workspace (stdlib only).

Fails (exit 1) if the workspace root or any member crate declares a third-party
dependency. For Rust this is authoritative: a crate cannot use code it does not
declare, so checking that ``[dependencies]``, ``[build-dependencies]`` and
``[dev-dependencies]`` are all empty is a complete guarantee -- no source scan
needed. Dev-dependencies are required empty too: nativelite tests use the
built-in ``#[test]`` harness, which needs nothing external.

Workspace-aware: it also rejects ``[workspace.dependencies]``, which would let a
member inherit a dependency without naming it.
"""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEP_TABLES = ("dependencies", "build-dependencies", "dev-dependencies")


def check_tables(where: str, data: dict) -> list[str]:
    """Reject every non-empty dependency table in one manifest."""
    problems: list[str] = []
    for table in DEP_TABLES:
        deps = data.get(table)
        if deps:
            problems.append(f"{where}: [{table}] must be empty, found: {sorted(deps)}")
    # Target-specific tables, e.g. [target.'cfg(windows)'.dependencies].
    for name, target in (data.get("target") or {}).items():
        for table in DEP_TABLES:
            if target.get(table):
                problems.append(
                    f"{where}: [target.{name}.{table}] must be empty, "
                    f"found: {sorted(target[table])}"
                )
    return problems


def manifests() -> list[Path]:
    """The workspace root manifest plus every member manifest."""
    root_manifest = ROOT / "Cargo.toml"
    found = [root_manifest]
    data = tomllib.loads(root_manifest.read_text(encoding="utf-8"))
    for member in (data.get("workspace") or {}).get("members", []):
        found.extend(sorted(ROOT.glob(f"{member}/Cargo.toml")))
    return found


def main() -> int:
    problems: list[str] = []
    root_data = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))

    # A workspace-level dependency table would let members inherit silently.
    workspace_deps = (root_data.get("workspace") or {}).get("dependencies")
    if workspace_deps:
        problems.append(
            f"Cargo.toml: [workspace.dependencies] must be empty, "
            f"found: {sorted(workspace_deps)}"
        )

    checked = manifests()
    for path in checked:
        rel = path.relative_to(ROOT).as_posix()
        problems.extend(check_tables(rel, tomllib.loads(path.read_text(encoding="utf-8"))))

    if problems:
        print("Dependency guard FAILED:")
        for p in problems:
            print(f"  - {p}")
        return 1
    print(f"Dependency guard OK: zero third-party dependencies ({len(checked)} manifests).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
