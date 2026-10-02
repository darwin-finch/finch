#!/usr/bin/env python3
"""issue_worktree.sh adds predictably and refuses context-free removal."""

from __future__ import annotations

import subprocess
import tempfile
from pathlib import Path

SCRIPT = Path(__file__).resolve().with_name("issue_worktree.sh")


def git(cwd: Path, *args: str) -> None:
    subprocess.run(
        ["git", "-c", "commit.gpgsign=false", *args],
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    )


def init_repo(tmp_path: Path) -> None:
    git(tmp_path, "init", "-b", "main")
    git(tmp_path, "config", "user.email", "t@example.com")
    git(tmp_path, "config", "user.name", "t")
    (tmp_path / "README").write_text("x\n")
    git(tmp_path, "add", "README")
    git(tmp_path, "commit", "-m", "init")


def test_add_stdout_is_absolute_path(tmp_path: Path) -> None:
    init_repo(tmp_path)
    result = subprocess.run(
        [str(SCRIPT), "add", "99", "HEAD"],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    lines = result.stdout.splitlines()
    assert len(lines) == 1, repr(result.stdout)
    path = Path(lines[0])
    expected = tmp_path / ".worktrees" / "issue-99"
    assert path.is_absolute()
    assert path.resolve() == expected.resolve()
    assert path.is_dir()
    assert result.stdout == f"{path}\n"
    assert "HEAD is now" not in result.stdout


def test_remove_refuses_destructive_cleanup(tmp_path: Path) -> None:
    init_repo(tmp_path)
    added = subprocess.run(
        [str(SCRIPT), "add", "99", "HEAD"],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )
    assert added.returncode == 0, added.stderr
    worktree = Path(added.stdout.strip())
    (worktree / "uncommitted").write_text("valuable\n")
    removed = subprocess.run(
        [str(SCRIPT), "remove", "99"],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )
    assert removed.returncode == 2
    assert "refusing destructive local cleanup" in removed.stderr
    assert worktree.is_dir()
    assert (worktree / "uncommitted").read_text() == "valuable\n"
    git(tmp_path, "show-ref", "--verify", "refs/heads/issue-99")


def main() -> int:
    with tempfile.TemporaryDirectory() as directory:
        test_add_stdout_is_absolute_path(Path(directory))
    with tempfile.TemporaryDirectory() as directory:
        test_remove_refuses_destructive_cleanup(Path(directory))
    print("ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
