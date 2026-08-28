#!/usr/bin/env python3
"""nativelite zero-dependency guard for a Rust crate (stdlib Python only).

Fails (exit 1) if the crate declares any third-party dependency. For Rust this
is authoritative: a crate cannot use code it does not declare in Cargo.toml, so
checking that ``[dependencies]``, ``[build-dependencies]`` and
``[dev-dependencies]`` are all empty is a complete guarantee — no source scan
needed. (Dev-deps are also required empty: nativelite tests use the built-in
``#[test]`` harness, which needs nothing external.)
"""
from __future__ import annotations

import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEP_TABLES = ("dependencies", "build-dependencies", "dev-dependencies")


def check_manifest() -> list[str]:
    data = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    problems: list[str] = []
    for table in DEP_TABLES:
        deps = data.get(table)
        if deps:
            problems.append(f"[{table}] must be empty, found: {sorted(deps)}")
    # Also reject target-specific dependency tables (e.g. [target.'cfg(..)'.dependencies]).
    for name, target in (data.get("target") or {}).items():
        for table in DEP_TABLES:
            if target.get(table):
                problems.append(
                    f"[target.{name}.{table}] must be empty, found: {sorted(target[table])}"
                )
    return problems


def main() -> int:
    problems = check_manifest()
    if problems:
        print("Dependency guard FAILED:")
        for p in problems:
            print(f"  - {p}")
        return 1
    print("Dependency guard OK: zero third-party dependencies.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
