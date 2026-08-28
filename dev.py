#!/usr/bin/env python3
"""Local dev runner for pty-rs — stdlib Python driving cargo, no task runner.

The same `python dev.py check` gate as every nativelite package, so the muscle
memory is identical across languages. Here `check` is the zero-dependency guard
plus `cargo test` (unit + integration + doctests):

  python dev.py check                 # guard + cargo test (what CI runs)
  python dev.py test                  # cargo test
  python dev.py build                 # cargo build --release
  python dev.py fmt                   # cargo fmt --check
  python dev.py guard                 # zero-dependency guard
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


def guard() -> int:
    return run(PY, "tools/dep_guard.py")


def check() -> int:
    return guard() or test()


COMMANDS = {"test": test, "build": build, "fmt": fmt, "guard": guard, "check": check}


def main(argv: list[str]) -> int:
    cmd = argv[1] if len(argv) > 1 else "check"
    fn = COMMANDS.get(cmd)
    if fn is None:
        print(f"unknown command {cmd!r}; choose from: {', '.join(COMMANDS)}")
        return 2
    return fn()


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
