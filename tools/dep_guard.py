#!/usr/bin/env python3
"""nativelite dependency guard for the foldwave workspace (stdlib Python only).

The org rule is **zero third-party dependencies**, not zero dependencies:
foldwave composes nativelite's own crates, exactly as atrium does. So a
``[dependencies]`` entry passes only when it is an org crate --- either a
``package = "nativelite-*"`` entry carrying a version (the crates.io publish
form, optionally with a path for local development) or a git dependency on
``github.com/nativelite/``. Anything else fails.

``[build-dependencies]`` and ``[dev-dependencies]`` must be empty everywhere:
nativelite tests use the built-in ``#[test]`` harness, which needs nothing
external.

Workspace-aware: the root manifest, ``[workspace.dependencies]`` and every
member manifest are all checked under the same rule, so a member cannot inherit
a dependency the guard never saw.
"""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ORG_GIT_PREFIX = "https://github.com/nativelite/"
ORG_PACKAGE_PREFIX = "nativelite-"
EMPTY_TABLES = ("build-dependencies", "dev-dependencies")


def is_org_crate(spec: object) -> bool:
    """True for a nativelite org crate, by package name or by org git URL."""
    if not isinstance(spec, dict):
        # A bare version string, e.g. serde = "1.0", is always third-party.
        return False
    git = spec.get("git") or ""
    if git.startswith(ORG_GIT_PREFIX):
        return True
    package = spec.get("package") or ""
    return package.startswith(ORG_PACKAGE_PREFIX) and bool(spec.get("version"))


def check_dependency_table(where: str, table: str, deps: dict) -> list[str]:
    """Every entry must be an org crate."""
    problems = []
    for name, spec in (deps or {}).items():
        if not is_org_crate(spec):
            problems.append(
                f"{where}: [{table}] {name!r} must be a nativelite org crate "
                f"(package = 'nativelite-*' with a version, or an org git dep); "
                f"found: {spec!r}"
            )
    return problems


def check_manifest(where: str, data: dict) -> list[str]:
    problems = check_dependency_table(where, "dependencies", data.get("dependencies"))
    for table in EMPTY_TABLES:
        if data.get(table):
            problems.append(f"{where}: [{table}] must be empty, found: {sorted(data[table])}")
    for target_name, target in (data.get("target") or {}).items():
        problems += check_dependency_table(
            where, f"target.{target_name}.dependencies", target.get("dependencies")
        )
        for table in EMPTY_TABLES:
            if target.get(table):
                problems.append(f"{where}: [target.{target_name}.{table}] must be empty")
    return problems


def member_manifests(root_data: dict) -> list[Path]:
    found: list[Path] = []
    for member in (root_data.get("workspace") or {}).get("members", []):
        found.extend(sorted(ROOT.glob(f"{member}/Cargo.toml")))
    return found


def main() -> int:
    root_manifest = ROOT / "Cargo.toml"
    root_data = tomllib.loads(root_manifest.read_text(encoding="utf-8"))

    problems = check_manifest("Cargo.toml", root_data)
    problems += check_dependency_table(
        "Cargo.toml",
        "workspace.dependencies",
        (root_data.get("workspace") or {}).get("dependencies"),
    )

    manifests = member_manifests(root_data)
    for path in manifests:
        rel = path.relative_to(ROOT).as_posix()
        problems += check_manifest(rel, tomllib.loads(path.read_text(encoding="utf-8")))

    if problems:
        print("Dependency guard FAILED:")
        for p in problems:
            print(f"  - {p}")
        return 1
    print(
        f"Dependency guard OK: nativelite crates + stdlib only "
        f"({len(manifests) + 1} manifests)."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
