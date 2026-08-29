#!/usr/bin/env python3
"""Regression tests for the permanent Finch SSH-removal contract."""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_no_ssh_surface.py"


class ContractRepository:
    def __init__(
        self, manifest: str = "[package]\nname = \"probe\"\nversion = \"0.1.0\"\n"
    ) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        self.write("Cargo.toml", manifest)
        self.write("src/lib.rs", "pub fn available() {}\n")
        self.track("Cargo.toml", "src/lib.rs")

    def close(self) -> None:
        self.temporary.cleanup()

    def write(self, name: str, contents: str) -> None:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents, encoding="utf-8")

    def track(self, *names: str) -> None:
        subprocess.run(["git", "-C", str(self.root), "add", "--", *names], check=True)

    def run(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(self.root)],
            check=False,
            capture_output=True,
            text=True,
        )


class SshRemovalContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = ContractRepository()

    def tearDown(self) -> None:
        self.repo.close()

    def assert_rejected(self, expected: str) -> None:
        result = self.repo.run()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(expected, result.stderr)

    def test_accepts_tree_without_ssh_surface(self) -> None:
        result = self.repo.run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_accepts_unrelated_ssh_words_in_values_comments_and_literals(self) -> None:
        self.repo.write(
            "src/lib.rs",
            "pub fn protocol_label() -> &'static str {\n"
            "    // Historical text may say: pub mod ssh; use russh.\n"
            "    let ssh = r#\"pub use transport as ssh;\"#;\n"
            "    ssh\n"
            "}\n",
        )
        self.repo.track("src/lib.rs")
        result = self.repo.run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_c_and_cr_literals_do_not_hide_following_safe_code(self) -> None:
        self.repo.write(
            "src/lib.rs",
            "use std::ffi::CStr;\n"
            "const NORMAL: &CStr = c\"pub mod ssh;\";\n"
            "const RAW: &CStr = cr##\"pub use transport as ssh;\"##;\n"
            "pub fn available() {}\n",
        )
        self.repo.track("src/lib.rs")
        result = self.repo.run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_c_and_cr_literals_cannot_desynchronize_the_scanner(self) -> None:
        for literal in ('c"ignored"', 'cr##"ignored"##'):
            with self.subTest(literal=literal):
                self.repo.write("src/lib.rs", f"const LABEL: &_ = {literal}; pub mod ssh;\n")
                self.repo.track("src/lib.rs")
                self.assert_rejected("removed SSH surface returned")

    def test_rejects_restored_ssh_module_path(self) -> None:
        self.repo.write("src/ssh/mod.rs", "pub struct Session;\n")
        self.repo.track("src/ssh/mod.rs")
        self.assert_rejected("removed SSH module path is tracked")

    def test_rejects_restored_flat_ssh_module_path(self) -> None:
        self.repo.write("src/ssh.rs", "pub struct Session;\n")
        self.repo.track("src/ssh.rs")
        self.assert_rejected("removed SSH module path is tracked")

    def test_rejects_restored_public_export(self) -> None:
        self.repo.write("src/lib.rs", "pub mod ssh;\n")
        self.repo.track("src/lib.rs")
        self.assert_rejected("removed SSH surface returned")

    def test_rejects_raw_identifier_module_export(self) -> None:
        self.repo.write("src/lib.rs", "pub mod r#ssh;\n")
        self.repo.track("src/lib.rs")
        self.assert_rejected("removed SSH surface returned")

    def test_rejects_public_alias_export(self) -> None:
        self.repo.write("src/lib.rs", "pub use transport as ssh;\n")
        self.repo.track("src/lib.rs")
        self.assert_rejected("removed SSH surface returned")

    def test_rejects_public_nested_reexport(self) -> None:
        self.repo.write("src/lib.rs", "pub use transport::ssh;\n")
        self.repo.track("src/lib.rs")
        self.assert_rejected("removed SSH surface returned")

    def test_rejects_grouped_public_reexport(self) -> None:
        self.repo.write("src/lib.rs", "pub use transport::{other, ssh};\n")
        self.repo.track("src/lib.rs")
        self.assert_rejected("removed SSH surface returned")

    def test_rejects_grouped_finch_and_crate_imports(self) -> None:
        for source in ("use finch::{other, ssh};\n", "use crate::{ssh, other};\n"):
            with self.subTest(source=source):
                self.repo.write("src/lib.rs", source)
                self.repo.track("src/lib.rs")
                self.assert_rejected("removed SSH surface returned")

    def test_rejects_comment_separated_module_declaration(self) -> None:
        self.repo.write("src/lib.rs", "pub mod /* compatibility */ ssh;\n")
        self.repo.track("src/lib.rs")
        self.assert_rejected("removed SSH surface returned")

    def test_rejects_comment_separated_public_alias(self) -> None:
        self.repo.write("src/lib.rs", "pub use transport as /* compatibility */ ssh;\n")
        self.repo.track("src/lib.rs")
        self.assert_rejected("removed SSH surface returned")

    def test_rejects_direct_or_renamed_forbidden_dependency(self) -> None:
        self.repo.write(
            "Cargo.toml",
            "[package]\nname = \"probe\"\nversion = \"0.1.0\"\n"
            "[target.'cfg(windows)'.dependencies]\n"
            "transport = { package = \"russh\", version = \"0.60\" }\n",
        )
        self.repo.track("Cargo.toml")
        self.assert_rejected("declares forbidden package 'russh' as 'transport'")

    def test_rejects_russh_reference_outside_old_module(self) -> None:
        self.repo.write("build.rs", "use russh::client;\n")
        self.repo.track("build.rs")
        self.assert_rejected("removed SSH surface returned")

    def test_rejects_raw_ssh_path_outside_src(self) -> None:
        self.repo.write("tests/compat.rs", "use finch::r#ssh::Session;\n")
        self.repo.track("tests/compat.rs")
        self.assert_rejected("removed SSH surface returned")

    def test_rejects_ssh_surface_in_literal_include_source(self) -> None:
        self.repo.write("src/lib.rs", 'include!("generated.inc");\n')
        self.repo.write("src/generated.inc", "pub mod /* hidden */ ssh;\n")
        self.repo.track("src/lib.rs", "src/generated.inc")
        self.assert_rejected("src/generated.inc: removed SSH surface returned")

    def test_rejects_nonempty_audit_ignore_with_valid_whitespace(self) -> None:
        self.repo.write(
            ".cargo/audit.toml",
            "  [advisories]\nignore = [ \"RUSTSEC-2026-0154\" ]\n",
        )
        self.repo.track(".cargo/audit.toml")
        self.assert_rejected("must not ignore advisories")


class CurrentTreeContractTests(unittest.TestCase):
    def test_current_tracked_tree_passes(self) -> None:
        result = subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(ROOT)],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
