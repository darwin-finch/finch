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
    "src/model/AGENTS.md": "# model capsule\n",
    "src/app/AGENTS.md": "# app capsule\n",
    # A generic root-package module that reaches a workspace crate through the root facade's
    # `pub use ... as` alias (`crate::vm::Value`) *and* a direct crate-qualified path
    # (`finch_vm::Value`) from the same file — covers both spellings a caller might use. This is
    # deliberately not named after any real subsystem: FP-PROOF-003 (issue #949) found this fixture
    # had drifted onto `src/programs/`, which stopped existing the moment #869 extracted that
    # subsystem to `crates/finch-programs/`, so the alias/direct-path coverage below is kept but
    # renamed to something with no real-world referent, and the extracted-crate boundary itself is
    # asserted separately below against `crates/finch-programs/`, matching the real layout.
    "src/widgets/AGENTS.md": "# widgets capsule\n",
    "src/lib.rs": "pub use finch_vm as vm;\n",
    # A clean candidate: one outgoing reference, several incoming.
    "src/tools/mcp/mod.rs": "pub use client::Client;\nmod client;\n",
    "src/tools/mcp/client.rs": "use crate::tools::types::Definition;\npub struct Client;\n",
    "src/tools/types.rs": "pub struct Definition;\n",
    "src/tools/mod.rs": "pub mod mcp;\npub mod types;\nuse crate::tools::mcp::Client;\n",
    # A costly candidate: reaches three subsystems, so it is not a cheap cut.
    "src/app/codec/mod.rs": (
        "use crate::model::Value;\nuse crate::tools::types::Definition;\n"
        "use crate::tools::mcp::Client;\nuse crate::app::Shared;\npub struct Codec;\n"
    ),
    "src/app/mod.rs": "pub mod codec;\npub struct Shared;\nuse crate::tools::mcp::Client;\n",
    "src/model/mod.rs": "pub struct Value;\nuse crate::tools::mcp::Client;\n",
    # The root facade aliases the workspace crate exactly as Finch does. Cover both that alias and
    # a direct crate-qualified path from the same cross-package caller.
    "src/widgets/mod.rs": "use crate::vm::Value;\npub fn direct() -> finch_vm::Value { todo!() }\n",
    # Workspace-crate `crate::` paths are relative to that package, even when a root module has
    # the same name.
    "crates/finch-vm/AGENTS.md": "# finch-vm capsule\n",
    "crates/finch-vm/src/lib.rs": "mod codec;\nmod vm;\n",
    "crates/finch-vm/src/codec.rs": "use crate::vm::Value;\n",
    "crates/finch-vm/src/vm.rs": "pub struct Value;\n",
    # FP-PROOF-003 (issue #949): the real seam this tool must measure correctly today is an
    # *already-extracted* workspace crate, not an in-tree `src/` module — `crates/finch-programs/`
    # is exactly that (extracted from `src/programs/` in #869). Its own facade reaches another
    # workspace crate directly (`finch_vm::Value`, no `crate::` alias available to it, matching the
    # real crate's `use finch_vm::...` imports), and a root-package caller reaches back in via
    # `finch_programs::` (matching real callers such as `src/program_registry.rs`).
    "crates/finch-programs/AGENTS.md": "# finch-programs capsule\n",
    "crates/finch-programs/src/lib.rs": "use finch_vm::Value;\npub struct ProgramDefinition;\n",
    "src/registry/caller.rs": "use finch_programs::ProgramDefinition;\n",
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
        for expected in ("src/app/mod.rs", "src/model/mod.rs", "src/tools/mod.rs"):
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

    def test_workspace_crate_sources_use_their_own_crate_namespace(self) -> None:
        report = self.report("crates/finch-vm/")
        self.assertIn("3 files", report, report)
        self.assertIn("outgoing: 0 subsystem(s)", report, report)

    def test_workspace_alias_and_direct_paths_count_cross_package_edges(self) -> None:
        crate_report = self.report("crates/finch-vm/")
        self.assertIn("incoming: 3 reference(s) from 2 subsystem(s)", crate_report, crate_report)
        self.assertIn("widgets", crate_report, crate_report)
        widgets_report = self.report("src/widgets/")
        self.assertIn("outgoing: 1 subsystem(s)", widgets_report, widgets_report)
        self.assertRegex(widgets_report, r"finch-vm\s+2 reference\(s\)", widgets_report)

    def test_extracted_workspace_crate_boundary_matches_its_real_facade(self) -> None:
        # FP-PROOF-003 (issue #949): repairs a fixture that modelled the pre-#869 in-tree
        # `src/programs/` layout instead of the real extracted `crates/finch-programs/` boundary.
        # A workspace crate reaches another workspace crate only through a direct crate-qualified
        # path (it has no root-facade `crate::` alias to use), and root-package callers reach back
        # in the same way real callers like `src/program_registry.rs` do: `finch_programs::Name`.
        report = self.report("crates/finch-programs/")
        self.assertIn("outgoing: 1 subsystem(s)", report, report)
        self.assertRegex(report, r"finch-vm\s+1 reference\(s\)", report)
        self.assertIn("incoming: 1 reference(s) from 1 subsystem(s)", report, report)
        self.assertIn("registry", report, report)
        self.assertIn("src/registry/caller.rs", report, report)


if __name__ == "__main__":
    unittest.main()
