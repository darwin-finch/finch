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
id = "app"
layer = 1
paths = ["src/app/"]

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

pub use interpreter::{inspect, run, Handler};
pub use ir::{Detail as Reason, Module, Phase};

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
#[derive(Debug, Clone)]
pub enum Phase {
    /// Parsing, with commas, in prose.
    #[serde(rename = "parse, really")]
    Parse,
    Verify(Detail),
}

pub struct Detail {
    pub why: String,
}

pub struct NotExported;

impl Module {
    /// Load a module from source.
    pub fn parse(text: &str) -> Result<Self, Phase> {
        todo!()
    }

    /// Private to the subsystem; never part of the interface.
    fn rebuild(&mut self) {}

    pub(crate) fn seal(&self) {}
}

impl std::fmt::Display for Module {
    /// A trait method, stated by the trait and not repeated per type.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Ok(())
    }
}
"""

INTERPRETER = """\
/// Run a verified module to completion.
pub fn run(
    module: &Module,
    budget: u64,
) -> Result<Detail, Phase> {
    todo!()
}

/// Inspect private state that the facade does not export.
pub fn inspect(state: &NotExported) -> u32 {
    0
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
        self.write("src/app/mod.rs", "/// A type another subsystem re-exports.\npub struct Shared;\n")
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
            "pub enum Phase { Parse, Verify }",  # attributes must not masquerade as variants
            "pub fn run(module: &Module, budget: u64) -> Result<Detail, Phase> { … }",
            "fn handle(&mut self, phase: Phase) -> bool;",
            "fn finish(&self) -> String;",
            "pub const VERSION: u32 = 5;",
        ):
            self.assertIn(expected, text, f"interface must carry {expected!r}:\n{text}")
        self.assertNotIn("A second doc line", text, "only the summary sentence belongs in the interface")
        self.assertNotIn("Bodies stay private", text, "the summary stops at the first sentence")
        self.assertNotIn("private_helper", text, "private items must not appear")
        self.assertNotIn("pub name: String", text, "field bodies must not appear")

    def test_renamed_export_uses_the_name_callers_write(self) -> None:
        text = self.fixture.interface()
        self.assertIn("pub struct Detail { … }", text, "the renamed item's signature must appear")
        self.assertIn("Reason", text, "the interface must mention the exported name, not only the source name")

    def test_attributes_do_not_become_variants(self) -> None:
        text = self.fixture.interface()
        self.assertNotIn("serde", text, "an attribute must never be rendered as an enum variant")
        self.assertNotIn("rename", text, "attribute contents must not leak into the interface")

    def test_nested_use_groups_are_expanded(self) -> None:
        self.fixture.edit("src/vm/mod.rs", "pub use ir::{Detail as Reason, Module, Phase};", "pub use ir::{{Detail as Reason, Module}, Phase as Phase2};")
        result = self.fixture.run("--write")
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("Phase2", self.fixture.interface(), "every name in a group must be expanded")

    def test_cross_subsystem_reexport_says_where_it_comes_from(self) -> None:
        self.fixture.edit("src/vm/mod.rs", "pub use interpreter::{inspect, run, Handler};", "pub use crate::app::Shared;\npub use interpreter::{inspect, run, Handler};")
        result = self.fixture.run("--write")
        self.assertEqual(0, result.returncode, result.stderr)
        text = self.fixture.interface()
        self.assertIn("pub struct Shared;", text, "a re-export from another subsystem must be listed")
        self.assertIn("Re-exported from `app`", text, "and must say which subsystem owns it")

    def test_ambiguous_definition_fails_rather_than_guessing(self) -> None:
        self.fixture.write("src/app/other.rs", "/// A second definition of the same name.\npub struct Shared;\n")
        self.fixture.edit("src/vm/mod.rs", "pub use interpreter::{inspect, run, Handler};", "pub use crate::app::Shared;\npub use interpreter::{inspect, run, Handler};")
        result = self.fixture.run("--write")
        self.assertNotEqual(0, result.returncode, "two definitions of one name must not be guessed between")
        self.assertIn("defined in more than one place", result.stderr, result.stderr)
        self.assertIn("src/app/other.rs", result.stderr, "the diagnostic must name the candidates")

    def test_glob_import_fails_loudly(self) -> None:
        self.fixture.edit("src/vm/mod.rs", "pub use ir::{Detail as Reason, Module, Phase};", "pub use ir::*;")
        result = self.fixture.run("--write")
        self.assertNotEqual(0, result.returncode, "a glob export must not be silently rendered")
        self.assertIn("glob", result.stderr, result.stderr)
        self.assertIn("refusing to write", result.stderr, result.stderr)

    def test_unresolved_export_fails_loudly(self) -> None:
        self.fixture.edit("src/vm/mod.rs", "pub use ir::{Detail as Reason, Module, Phase};", "pub use ir::{Ghost, Module, Phase};")
        result = self.fixture.run("--write")
        self.assertNotEqual(0, result.returncode, "an export with no definition must fail, not be written")
        self.assertIn("no definition found for exported `Ghost`", result.stderr, result.stderr)

    def test_referenced_but_unexported_types_are_reported(self) -> None:
        text = self.fixture.interface()
        self.assertIn("Referenced but not exported", text, f"inspect() takes a type the facade omits:\n{text}")
        self.assertIn("`NotExported`", text, "the unexported type must be named")
        self.assertNotIn("`Detail`", text.split("Referenced but not exported")[1], "a renamed export is reachable, not missing")

    def test_new_export_makes_the_interface_stale(self) -> None:
        self.fixture.edit("src/vm/mod.rs", "pub use ir::{Detail as Reason, Module, Phase};", "pub use ir::{Detail as Reason, Module, NotExported, Phase};")
        self.assert_stale("src/vm/INTERFACE.md is stale", "NotExported")

    def test_removed_export_makes_the_interface_stale(self) -> None:
        self.fixture.edit("src/vm/mod.rs", "pub use ir::{Detail as Reason, Module, Phase};", "pub use ir::Module;")
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

    def test_public_methods_reach_the_interface_with_their_docs(self) -> None:
        # A type without its constructors is not an interface: a caller can see `Module` exists
        # and still have no way to obtain one.
        interface = self.fixture.interface()
        self.assertIn("impl Module {", interface, f"the type's inherent block is missing:\n{interface}")
        self.assertIn("pub fn parse(text: &str) -> Result<Self, Phase>;", interface)
        self.assertIn("/// Load a module from source.", interface)

    def test_methods_callers_cannot_reach_stay_out(self) -> None:
        interface = self.fixture.interface()
        self.assertNotIn("rebuild", interface, "a private method is not part of the interface")
        self.assertNotIn("seal", interface, "a pub(crate) method is not reachable from outside")
        self.assertNotIn("fmt", interface, "a trait implementation is stated by the trait")

    def test_a_new_public_method_makes_the_interface_stale(self) -> None:
        # The invariant that matters: adding to a subsystem's surface cannot merge silently.
        self.fixture.edit(
            "src/vm/ir.rs", "    pub(crate) fn seal(&self) {}",
            "    pub(crate) fn seal(&self) {}\n\n    pub fn verify(&self) -> bool {\n        true\n    }",
        )
        self.assert_stale("src/vm/INTERFACE.md is stale", "verify")

    def test_a_changed_method_signature_makes_the_interface_stale(self) -> None:
        self.fixture.edit("src/vm/ir.rs", "pub fn parse(text: &str)", "pub fn parse(text: &[u8])")
        self.assert_stale("src/vm/INTERFACE.md is stale", "&[u8]")

    def test_missing_interface_file_is_stale(self) -> None:
        (self.fixture.root / "src/vm/INTERFACE.md").unlink()
        self.assert_stale("src/vm/INTERFACE.md is stale")

    def test_real_tree_interfaces_match_their_facades(self) -> None:
        result = subprocess.run([sys.executable, str(GENERATOR)], capture_output=True, text=True, check=False)
        self.assertEqual(0, result.returncode, f"repository interfaces drifted:\n{result.stderr}")
        generated, problems = interfaces(ROOT)
        self.assertEqual([], problems, "the repository's facades must parse cleanly")
        self.assertTrue(generated, "the repository must generate at least one interface")


if __name__ == "__main__":
    unittest.main()
