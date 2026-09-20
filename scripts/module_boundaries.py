#!/usr/bin/env python3
"""Shared module-boundary rules: what counts as a module, and who owns a path.

The tree is the record. A directory is a module when it carries a capsule (`AGENTS.md`) beside a
facade (`mod.rs`, or `src/lib.rs` for a workspace crate) — nothing else declares it, so there is no
manifest to keep in sync and no way for the two to disagree. `seam_cost.py` and any other script
that needs to reason about module boundaries shares these definitions rather than restating them.
"""

from __future__ import annotations

from pathlib import Path

CAPSULE = "AGENTS.md"
FACADE = "mod.rs"
CRATE_FACADE = "src/lib.rs"


def module_directories(files: list[str]) -> list[str]:
    """Directories that are modules with a stated capsule and facade."""
    tracked = set(files)
    return sorted(
        f"{Path(path).parent.as_posix()}/"
        for path in files
        if Path(path).name == CAPSULE
        and Path(path).parent != Path(".")
        and any(
            (Path(path).parent / facade).as_posix() in tracked
            for facade in (FACADE, CRATE_FACADE)
        )
    )


def facade_path(files: list[str], directory: str) -> str:
    """Return the tracked facade for a source module or workspace library crate."""
    tracked = set(files)
    for relative in (FACADE, CRATE_FACADE):
        candidate = f"{directory}{relative}"
        if candidate in tracked:
            return candidate
    raise ValueError(f"module directory has no facade: {directory}")


def module_identifier(directory: str) -> str:
    """Name a source module or workspace crate as its callers do."""
    if directory.startswith("src/"):
        return directory.removeprefix("src/").rstrip("/").replace("/", "::")
    if directory.startswith("crates/"):
        return Path(directory.rstrip("/")).name
    return directory.rstrip("/").replace("/", "::")


def owning_module(directories: list[str], path: str) -> str | None:
    """The innermost module directory containing a path, named as a caller would say it.

    A path under no capsule still belongs somewhere, so it falls back to its top-level directory:
    a re-export should always be able to say where it came from.
    """
    best = max((d for d in directories if path.startswith(d)), key=len, default=None)
    if best:
        return module_identifier(best)
    parts = path.split("/")
    return parts[1].removesuffix(".rs") if len(parts) > 1 and parts[0] == "src" else None
