#!/usr/bin/env python3
"""Regressions for scripts/generate_interfaces.py against git-initialised fixture trees."""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GENERATOR = ROOT / "scripts/generate_interfaces.py"
sys.path.insert(0, str(ROOT / "scripts"))
from generate_interfaces import interfaces  # noqa: E402

MANIFEST = """\
version = 1

[global]
paths = ["subsystems.toml"]

[[subsystem]]
id = "vm"
layer = 0
paths = ["src/vm/"]
instructions = ["src/vm/AGENTS.md"]
facade = "src/vm/mod.rs"
interface = "src/vm/INTERFACE.md"
depends_on = []
"""

FACADE = """\
mod ir;
mod interpreter;

pub use interpreter::{run, Handler};
pub use ir::{Module, Phase};

/// The IR family this VM accepts.
pub const VERSION: u32 = 5;
"""

IR = """\
/// One verified module. Bodies stay private.
/// A second doc line that must not appear.
pub struct Module {
    pub name: String,
}

/// Where a diagnostic was raised.
pub enum Phase {
    /// Parsing, with commas, in prose.
    Parse,
    Verify(Detail),
}

pub struct Detail {
    pub why: String,
}

pub struct NotExported;
"""

INTERPRETER = """\
/// Run a verified module to completion.
pub fn run(
    module: &Module,
    budget: u64,
) -> Result<Detail, Phase> {
    todo!()
}

pub trait Handler {
    /// Handle one effect.
    fn handle(&mut self, phase: Phase) -> bool;
    fn finish(&self) -> String {
        String::new()
    }
}

fn private_helper() -> u32 {
    7
}
"""


class Fixture:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.write("subsystems.toml", MANIFEST)
        self.write("src/vm/mod.rs", FACADE)
        self.write("src/vm/ir.rs", IR)
        self.write("src/vm/interpreter.rs", INTERPRETER)
        self.write("src/vm/AGENTS.md", "# vm capsule\n")
        subprocess.run(["git", "-C", str(self.root), "init", "-q"], check=True)
        self.stage()
        self.generate()

    def write(self, relative: str, text: str) -> None:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def edit(self, relative: str, old: str, new: str) -> None:
        path = self.root / relative
        text = path.read_text()
        if old not in text:
            raise AssertionError(f"fixture edit target missing from {relative}: {old!r}")
        path.write_text(text.replace(old, new, 1))

    def stage(self) -> None:
        subprocess.run(["git", "-C", str(self.root), "add", "-A"], check=True, capture_output=True)

    def run(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        self.stage()
        return subprocess.run(
            [sys.executable, str(GENERATOR), "--root", str(self.root), *arguments],
            capture_output=True, text=True, check=False,
        )

    def generate(self) -> None:
        result = self.run("--write")
        assert result.returncode == 0, result.stderr

    def interface(self) -> str:
        return (self.root / "src/vm/INTERFACE.md").read_text()

    def close(self) -> None:
        self.temporary.cleanup()


class InterfaceGeneratorTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def assert_stale(self, *fragments: str) -> None:
        result = self.fixture.run()
        self.assertNotEqual(0, result.returncode, "a changed interface must fail the check")
        for fragment in fragments:
            self.assertIn(fragment, result.stderr, f"missing diagnostic {fragment!r} in: {result.stderr}")
        self.assertIn("--write", result.stderr, "the diagnostic must name the command that fixes it")

    def test_generated_interface_is_stable_and_matches(self) -> None:
        first = self.fixture.interface()
        self.fixture.generate()
        self.assertEqual(first, self.fixture.interface(), "generation must be deterministic")
        self.assertEqual(0, self.fixture.run().returncode, "a freshly generated interface must pass")

    def test_interface_carries_signatures_docs_variants_and_trait_methods(self) -> None:
        text = self.fixture.interface()
        for expected in (
            "pub struct Module { … }",
            "/// One verified module.",  # the summary sentence only
            "pub enum Phase { Parse, Verify }",
            "pub fn run(module: &Module, budget: u64) -> Result<Detail, Phase> { … }",
            "fn handle(&mut self, phase: Phase) -> bool;",
            "fn finish(&self) -> String;",
            "pub const VERSION: u32 = 5;",
        ):
            self.assertIn(expected, text, f"interface must carry {expected!r}:\n{text}")
        self.assertNotIn("A second doc line", text, "only the summary sentence belongs in the interface")
        self.assertNotIn("Bodies stay private", text, "the summary stops at the first sentence")
        self.assertNotIn("private_helper", text, "private items must not appear")
        self.assertNotIn("NotExported", text, "unexported items must not appear")
        self.assertNotIn("pub name: String", text, "field bodies must not appear")

    def test_referenced_but_unexported_types_are_reported(self) -> None:
        text = self.fixture.interface()
        self.assertIn("Referenced but not exported", text)
        self.assertIn("`Detail`", text, "run() returns Detail, which the facade does not export")

    def test_new_export_makes_the_interface_stale(self) -> None:
        self.fixture.edit("src/vm/mod.rs", "pub use ir::{Module, Phase};", "pub use ir::{Detail, Module, Phase};")
        self.assert_stale("src/vm/INTERFACE.md is stale", "Detail")

    def test_removed_export_makes_the_interface_stale(self) -> None:
        self.fixture.edit("src/vm/mod.rs", "pub use ir::{Module, Phase};", "pub use ir::Module;")
        self.assert_stale("src/vm/INTERFACE.md is stale")

    def test_changed_signature_makes_the_interface_stale(self) -> None:
        self.fixture.edit("src/vm/interpreter.rs", "budget: u64,", "budget: u32,")
        self.assert_stale("src/vm/INTERFACE.md is stale", "u32")

    def test_changed_doc_summary_makes_the_interface_stale(self) -> None:
        self.fixture.edit("src/vm/ir.rs", "/// One verified module.", "/// One checked module.")
        self.assert_stale("src/vm/INTERFACE.md is stale", "checked")

    def test_new_enum_variant_makes_the_interface_stale(self) -> None:
        self.fixture.edit("src/vm/ir.rs", "    Verify(Detail),", "    Verify(Detail),\n    Execute,")
        self.assert_stale("src/vm/INTERFACE.md is stale", "Execute")

    def test_private_changes_do_not_touch_the_interface(self) -> None:
        self.fixture.edit("src/vm/interpreter.rs", "fn private_helper() -> u32 {\n    7\n}", "fn private_helper() -> u32 {\n    9\n}")
        self.fixture.edit("src/vm/ir.rs", "pub struct NotExported;", "pub struct NotExported {\n    pub added: bool,\n}")
        self.assertEqual(0, self.fixture.run().returncode, "changes behind the facade must not churn the interface")

    def test_missing_interface_file_is_stale(self) -> None:
        (self.fixture.root / "src/vm/INTERFACE.md").unlink()
        self.assert_stale("src/vm/INTERFACE.md is stale")

    def test_real_tree_interfaces_match_their_facades(self) -> None:
        result = subprocess.run([sys.executable, str(GENERATOR)], capture_output=True, text=True, check=False)
        self.assertEqual(0, result.returncode, f"repository interfaces drifted:\n{result.stderr}")
        self.assertTrue(interfaces(ROOT), "the repository must generate at least one interface")


if __name__ == "__main__":
    unittest.main()
