#!/usr/bin/env python3
"""Reject the removed Finch SSH API and its vulnerable Cargo dependency graph roots."""

from __future__ import annotations

import argparse
import posixpath
import subprocess
import sys
import tomllib
from collections.abc import Mapping
from pathlib import Path, PurePosixPath


ROOT = Path(__file__).resolve().parent.parent
FORBIDDEN_PACKAGES = {"rsa", "russh", "russh-cryptovec", "russh-keys"}
STRING_TOKEN = "\0rust-string:"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT, help="Git worktree to inspect")
    return parser.parse_args()


def tracked_paths(root: Path) -> list[PurePosixPath]:
    result = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"],
        check=True,
        capture_output=True,
    )
    return [PurePosixPath(raw.decode()) for raw in result.stdout.split(b"\0") if raw]


def dependency_errors(value: object, location: str = "Cargo.toml") -> list[str]:
    if not isinstance(value, Mapping):
        return []

    errors: list[str] = []
    for key, child in value.items():
        child_location = f"{location}.{key}"
        if key in {"dependencies", "dev-dependencies", "build-dependencies"} and isinstance(
            child, Mapping
        ):
            for dependency_name, declaration in child.items():
                package_name = dependency_name
                if isinstance(declaration, Mapping):
                    package_name = declaration.get("package", dependency_name)
                if dependency_name in FORBIDDEN_PACKAGES or package_name in FORBIDDEN_PACKAGES:
                    errors.append(
                        f"{child_location} declares forbidden package {package_name!r} "
                        f"as {dependency_name!r}"
                    )
        errors.extend(dependency_errors(child, child_location))
    return errors


def rust_tokens(source: str) -> list[str]:
    """Return Rust-ish tokens while retaining ordinary strings for literal include! paths."""
    tokens: list[str] = []
    index = 0
    length = len(source)
    while index < length:
        if source.startswith("//", index):
            newline = source.find("\n", index + 2)
            index = length if newline == -1 else newline + 1
            continue
        if source.startswith("/*", index):
            depth = 1
            index += 2
            while index < length and depth:
                if source.startswith("/*", index):
                    depth += 1
                    index += 2
                elif source.startswith("*/", index):
                    depth -= 1
                    index += 2
                else:
                    index += 1
            continue

        raw_prefix = next(
            (prefix for prefix in ("br", "cr", "r") if source.startswith(prefix, index)),
            None,
        )
        if raw_prefix is not None:
            raw = index + len(raw_prefix)
            hashes = raw
            while hashes < length and source[hashes] == "#":
                hashes += 1
            if hashes < length and source[hashes] == '"':
                delimiter = '"' + source[raw:hashes]
                end = source.find(delimiter, hashes + 1)
                if raw_prefix == "r":
                    value_end = length if end == -1 else end
                    tokens.append(STRING_TOKEN + source[hashes + 1 : value_end])
                index = length if end == -1 else end + len(delimiter)
                continue

        normal_prefix = next(
            (prefix for prefix in ('b"', 'c"', '"') if source.startswith(prefix, index)),
            None,
        )
        if normal_prefix is not None:
            retain = normal_prefix == '"'
            index += len(normal_prefix)
            value: list[str] = []
            while index < length:
                if source[index] == "\\":
                    if retain and index + 1 < length:
                        value.append(source[index + 1])
                    index += 2
                elif source[index] == '"':
                    index += 1
                    break
                else:
                    if retain:
                        value.append(source[index])
                    index += 1
            if retain:
                tokens.append(STRING_TOKEN + "".join(value))
            continue
        if source[index] == "'":
            end = index + 2
            if index + 1 < length and source[index + 1] == "\\":
                end += 1
            if end < length and source[end] == "'":
                index = end + 1
                continue

        if source.startswith("r#", index) and index + 2 < length and (
            source[index + 2].isalpha() or source[index + 2] == "_"
        ):
            index += 2
            start = index
            while index < length and (source[index].isalnum() or source[index] == "_"):
                index += 1
            tokens.append(source[start:index])
            continue
        if source[index].isalpha() or source[index] == "_":
            start = index
            index += 1
            while index < length and (source[index].isalnum() or source[index] == "_"):
                index += 1
            tokens.append(source[start:index])
            continue
        if source.startswith("::", index):
            tokens.append("::")
            index += 2
            continue
        if not source[index].isspace():
            tokens.append(source[index])
        index += 1
    return tokens


def forbidden_rust_surface(source: str) -> str | None:
    tokens = rust_tokens(source)
    for index, token in enumerate(tokens):
        if token in {"russh", "russh_keys"}:
            return f"forbidden crate identifier {token!r}"
        if token != "ssh":
            continue
        if index and tokens[index - 1] in {"mod", "as"}:
            return f"forbidden SSH declaration or alias near token {index}"
        if index >= 2 and tokens[index - 1] == "::" and tokens[index - 2] in {"crate", "finch"}:
            return f"forbidden Finch SSH path near token {index}"
        if index >= 2 and tokens[index - 1] == "crate" and tokens[index - 2] == "extern":
            return f"forbidden SSH extern crate near token {index}"

    for index, token in enumerate(tokens[:-2]):
        if token not in {"crate", "finch"} or tokens[index + 1 : index + 3] != ["::", "{"]:
            continue
        depth = 1
        cursor = index + 3
        while cursor < len(tokens) and depth:
            if tokens[cursor] == "{":
                depth += 1
            elif tokens[cursor] == "}":
                depth -= 1
            elif tokens[cursor] == "ssh":
                return f"forbidden grouped Finch SSH import near token {cursor}"
            cursor += 1

    index = 0
    while index < len(tokens):
        if tokens[index] != "pub":
            index += 1
            continue
        cursor = index + 1
        if cursor < len(tokens) and tokens[cursor] == "(":
            depth = 1
            cursor += 1
            while cursor < len(tokens) and depth:
                depth += tokens[cursor] == "("
                depth -= tokens[cursor] == ")"
                cursor += 1
        if cursor >= len(tokens) or tokens[cursor] != "use":
            index += 1
            continue
        end = cursor + 1
        while end < len(tokens) and tokens[end] != ";":
            if tokens[end] == "ssh":
                return f"forbidden public SSH re-export near token {end}"
            end += 1
        index = end + 1
    return None


def literal_includes(tokens: list[str]) -> list[str]:
    includes: list[str] = []
    for index in range(len(tokens) - 3):
        if (
            tokens[index : index + 3] == ["include", "!", "("]
            and tokens[index + 3].startswith(STRING_TOKEN)
        ):
            includes.append(tokens[index + 3][len(STRING_TOKEN) :])
    return includes


def check(root: Path) -> list[str]:
    root = root.resolve()
    tracked = tracked_paths(root)
    errors: list[str] = []

    ssh_paths = [
        path
        for path in tracked
        if path == PurePosixPath("src/ssh.rs") or path.parts[:2] == ("src", "ssh")
    ]
    if ssh_paths:
        errors.append("removed SSH module path is tracked: " + ", ".join(map(str, ssh_paths)))

    for path in tracked:
        if path.name != "Cargo.toml":
            continue
        manifest = tomllib.loads(root.joinpath(*path.parts).read_text(encoding="utf-8"))
        errors.extend(dependency_errors(manifest, str(path)))

    audit_path = root / ".cargo/audit.toml"
    if audit_path.is_file():
        audit = tomllib.loads(audit_path.read_text(encoding="utf-8"))
        ignore = audit.get("advisories", {}).get("ignore", [])
        if ignore:
            errors.append(".cargo/audit.toml must not ignore advisories")

    tracked_set = set(tracked)
    pending = [path for path in tracked if path.suffix == ".rs"]
    inspected: set[PurePosixPath] = set()
    while pending:
        path = pending.pop()
        if path in inspected:
            continue
        inspected.add(path)
        source = root.joinpath(*path.parts).read_text(encoding="utf-8")
        tokens = rust_tokens(source)
        reason = forbidden_rust_surface(source)
        if reason is not None:
            errors.append(f"{path}: removed SSH surface returned ({reason})")
        for include in literal_includes(tokens):
            included = PurePosixPath(posixpath.normpath(str(path.parent / include)))
            if included in tracked_set:
                pending.append(included)

    return errors


def main() -> int:
    args = parse_args()
    try:
        errors = check(args.root)
    except (
        OSError,
        subprocess.CalledProcessError,
        UnicodeDecodeError,
        tomllib.TOMLDecodeError,
    ) as error:
        print(f"SSH absence check could not inspect the worktree: {error}", file=sys.stderr)
        return 2
    if errors:
        for error in errors:
            print(f"SSH absence: {error}", file=sys.stderr)
        return 1
    print("SSH absence: removed API and direct dependency roots remain absent")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
