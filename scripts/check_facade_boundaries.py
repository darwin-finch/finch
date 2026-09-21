#!/usr/bin/env python3
"""Enforce deliberate flat facades for root subsystems awaiting extraction."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
FACADES = ("server", "cli", "local", "config", "context")
PUBLIC_CHILD = re.compile(r"^\s*pub\s+mod\s+([A-Za-z_][A-Za-z0-9_]*)", re.MULTILINE)
FLAT_REEXPORT = re.compile(r"^\s*pub(?:\(crate\))?\s+use\s+", re.MULTILINE)


def check(root: Path) -> list[str]:
    errors: list[str] = []
    for facade in FACADES:
        directory = root / "src" / facade
        capsule = directory / "AGENTS.md"
        module = directory / "mod.rs"
        if not capsule.is_file():
            errors.append(f"src/{facade}/AGENTS.md: missing facade capsule")
        if not module.is_file():
            errors.append(f"src/{facade}/mod.rs: missing facade module")
            continue
        source = module.read_text()
        for match in PUBLIC_CHILD.finditer(source):
            errors.append(
                f"src/{facade}/mod.rs:{source.count(chr(10), 0, match.start()) + 1}: "
                f"public child module `{match.group(1)}` bypasses the flat facade"
            )
        if not FLAT_REEXPORT.search(source):
            errors.append(f"src/{facade}/mod.rs: facade has no explicit flat re-exports")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT, help="repository worktree to inspect")
    args = parser.parse_args()
    errors = check(args.root.resolve())
    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 1
    print("server, cli, local, config, and context facade boundaries are intact")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
