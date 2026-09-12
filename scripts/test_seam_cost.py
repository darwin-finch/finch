#!/usr/bin/env python3
"""Regressions for scripts/seam_cost.py against a git-initialised fixture tree."""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/seam_cost.py"

SOURCES = {
    # A directory is a module when it carries a capsule beside a facade; nothing else declares it.
    "src/tools/mcp/AGENTS.md": "# mcp capsule\n",
    "src/tools/AGENTS.md": "# tools capsule\n",
    "src/vm/AGENTS.md": "# vm capsule\n",
    "src/app/AGENTS.md": "# app capsule\n",
    # A clean candidate: one outgoing reference, several incoming.
    "src/tools/mcp/mod.rs": "pub use client::Client;\nmod client;\n",
    "src/tools/mcp/client.rs": "use crate::tools::types::Definition;\npub struct Client;\n",
    "src/tools/types.rs": "pub struct Definition;\n",
    "src/tools/mod.rs": "pub mod mcp;\npub mod types;\nuse crate::tools::mcp::Client;\n",
    # A costly candidate: reaches three subsystems, so it is not a cheap cut.
    "src/app/codec/mod.rs": (
        "use crate::vm::Value;\nuse crate::tools::types::Definition;\n"
        "use crate::tools::mcp::Client;\nuse crate::app::Shared;\npub struct Codec;\n"
    ),
    "src/app/mod.rs": "pub mod codec;\npub struct Shared;\nuse crate::tools::mcp::Client;\n",
    "src/vm/mod.rs": "pub struct Value;\nuse crate::tools::mcp::Client;\n",
}


class SeamCostTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        for path, text in SOURCES.items():
            target = self.root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text)
        subprocess.run(["git", "-C", str(self.root), "init", "-q"], check=True)
        subprocess.run(["git", "-C", str(self.root), "add", "-A"], check=True, capture_output=True)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def report(self, candidate: str) -> str:
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--root", str(self.root), candidate],
            capture_output=True, text=True, check=False,
        )
        self.assertEqual(0, result.returncode, result.stderr)
        return result.stdout

    def test_a_nested_candidate_counts_who_reaches_into_it(self) -> None:
        # The number that decides whether a facade is worth writing: who calls in, and from where.
        report = self.report("src/tools/mcp/")
        self.assertIn("incoming: 4 reference(s) from 3 subsystem(s)", report, report)
        for expected in ("src/app/mod.rs", "src/vm/mod.rs", "src/tools/mod.rs"):
            self.assertIn(expected, report, f"caller {expected} missing from:\n{report}")

    def test_a_candidate_that_reaches_out_is_flagged(self) -> None:
        # The mistake this script exists to prevent: reading a directory name as a boundary.
        report = self.report("src/app/codec/")
        self.assertIn("outgoing: 4 subsystem(s)", report, report)
        self.assertIn("above two is not a cheap cut", report, report)

    def test_references_inside_the_candidate_are_not_counted_as_edges(self) -> None:
        # `mcp/mod.rs` -> `mcp/client.rs` is internal; counting it would make every seam look costly.
        report = self.report("src/tools/mcp/")
        self.assertIn("outgoing: 1 subsystem(s)", report, report)

    def test_a_single_file_candidate_counts_its_callers(self) -> None:
        # A file names a module just as a directory does. Building its module path as if it were a
        # directory yields `tools::mcp.rs`, which matches nothing, and the tool then reports a
        # heavily-used file as having no callers at all.
        report = self.report("src/tools/types.rs")
        self.assertIn("incoming: 2 reference(s)", report, report)
        self.assertIn("src/app/codec/mod.rs", report, report)

    def test_an_empty_path_says_so_rather_than_reporting_zeroes(self) -> None:
        report = self.report("src/nowhere/")
        self.assertIn("no tracked Rust files", report, report)


if __name__ == "__main__":
    unittest.main()
