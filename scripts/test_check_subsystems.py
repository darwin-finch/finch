#!/usr/bin/env python3
"""Regressions for scripts/check_subsystems.py, run against git-initialised fixture trees."""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_subsystems.py"
sys.path.insert(0, str(ROOT / "scripts"))
from check_subsystems import blank_comments_and_literals, check  # noqa: E402

MANIFEST = """\
version = 1

[global]
paths = ["src/lib.rs", "AGENTS.md", "CLAUDE.md", "subsystems.toml"]

[[excluded]]
path = "old/"
reason = "history"

[[subsystem]]
id = "vm"
layer = 0
paths = ["src/vm/"]
docs = ["src/vm/VM.md"]
depends_on = []
debt = [{ to = "app", issue = 541 }]

[[subsystem]]
id = "app"
layer = 1
paths = ["src/app/"]
depends_on = ["vm"]

[[subsystem]]
id = "docs"
paths = ["README.md"]
"""

SOURCES = {
    "src/lib.rs": "pub mod vm;\npub mod app;\n",
    "src/vm/mod.rs": "use crate::app::Hook;\npub struct Value;\n",
    "src/vm/VM.md": "# VM\n",
    "src/app/mod.rs": "use crate::vm::Value;\npub struct Hook;\n",
    "old/notes.md": "history\n",
    "README.md": "# Fixture\n",
    "CLAUDE.md": "# Instructions\n",
}


class Fixture:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.write("subsystems.toml", MANIFEST)
        for path, text in SOURCES.items():
            self.write(path, text)
        (self.root / "AGENTS.md").symlink_to("CLAUDE.md")
        self.git("init", "-q")
        self.stage()

    def git(self, *arguments: str) -> None:
        subprocess.run(["git", "-C", str(self.root), *arguments], check=True, capture_output=True)

    def stage(self) -> None:
        self.git("add", "-A")

    def write(self, path: str, text: str) -> None:
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def edit(self, path: str, old: str, new: str) -> None:
        target = self.root / path
        text = target.read_text()
        if old not in text:
            raise AssertionError(f"fixture edit target missing from {path}: {old!r}")
        target.write_text(text.replace(old, new, 1))

    def errors(self) -> list[str]:
        self.stage()
        return check(self.root)

    def close(self) -> None:
        self.temporary.cleanup()


class SubsystemManifestTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def assert_clean(self) -> None:
        errors = self.fixture.errors()
        self.assertEqual([], errors, f"fixture manifest should match its tree: {errors}")

    def assert_error(self, *fragments: str) -> None:
        errors = self.fixture.errors()
        joined = "\n".join(errors)
        self.assertTrue(errors, "mutated fixture unexpectedly passed the subsystem check")
        for fragment in fragments:
            self.assertIn(fragment, joined, f"missing actionable diagnostic {fragment!r} in:\n{joined}")

    def test_real_tree_matches_its_manifest(self) -> None:
        result = subprocess.run(
            [sys.executable, str(CHECKER)], capture_output=True, text=True, check=False,
        )
        self.assertEqual(0, result.returncode, f"subsystems.toml drifted from the tree:\n{result.stderr}")

    def test_fixture_passes(self) -> None:
        self.assert_clean()

    def test_unowned_file_fails(self) -> None:
        self.fixture.write("src/stray.rs", "pub fn stray() {}\n")
        self.assert_error("unowned tracked file: src/stray.rs")

    def test_equal_length_owners_are_ambiguous(self) -> None:
        self.fixture.edit("subsystems.toml", 'paths = ["README.md"]', 'paths = ["README.md", "src/app/"]')
        self.assert_error("ambiguous owner for src/app/mod.rs: ['app', 'docs']")

    def test_longest_prefix_wins_for_global_and_excluded(self) -> None:
        self.fixture.edit("subsystems.toml", 'paths = ["README.md"]', 'paths = ["README.md", "old/"]')
        self.fixture.edit("subsystems.toml", 'path = "old/"', 'path = "old/notes.md"')
        self.assert_clean()

    def test_path_entry_matching_nothing_fails(self) -> None:
        self.fixture.edit("subsystems.toml", 'path = "old/"', 'path = "gone/"')
        self.assert_error("excluded: path entry matches no tracked file: gone/", "unowned tracked file: old/notes.md")

    def test_missing_doc_target_fails(self) -> None:
        (self.fixture.root / "src/vm/VM.md").unlink()
        self.assert_error("subsystem 'vm': docs target is not a tracked file: src/vm/VM.md")

    def test_new_cross_subsystem_import_fails(self) -> None:
        self.fixture.write("src/cli/mod.rs", "pub struct Cli;\n")
        self.fixture.edit(
            "subsystems.toml", '[[subsystem]]\nid = "docs"',
            '[[subsystem]]\nid = "cli"\nlayer = 2\npaths = ["src/cli/"]\n\n[[subsystem]]\nid = "docs"',
        )
        self.fixture.edit("src/vm/mod.rs", "pub struct Value;", "pub struct Value;\nuse crate::cli::Cli;")
        self.assert_error(
            "undeclared dependency vm (layer 0) -> cli (layer 2), not down-layer",
            "src/vm/mod.rs:3 crate::cli",
        )

    def test_down_layer_import_closing_a_cycle_through_debt_fails(self) -> None:
        # With vm -> app recorded as debt, the down-layer app -> vm edge closes a cycle; being
        # down-layer does not exempt it from being declared.
        self.fixture.edit("subsystems.toml", 'depends_on = ["vm"]', "depends_on = []")
        self.assert_error("undeclared dependency app (layer 1) -> vm (layer 0), down-layer; add to depends_on")

    def test_removed_reference_makes_declared_edge_stale(self) -> None:
        self.fixture.edit("src/vm/mod.rs", "use crate::app::Hook;\n", "")
        self.assert_error("stale debt edge vm -> app: no production reference remains")

    def test_up_layer_depends_on_fails(self) -> None:
        self.fixture.edit("subsystems.toml", 'debt = [{ to = "app", issue = 541 }]', 'depends_on_extra = 0')
        self.fixture.edit("subsystems.toml", "depends_on = []\ndepends_on_extra = 0", 'depends_on = ["app"]')
        self.assert_error("subsystem 'vm' (layer 0): depends_on 'app' (layer 1) must point strictly down-layer")

    def test_edge_declared_twice_and_unknown_ids_fail(self) -> None:
        self.fixture.edit("subsystems.toml", 'depends_on = ["vm"]', 'depends_on = ["vm", "ghost"]\ndebt = [{ to = "vm", issue = 1 }]')
        self.assert_error("edge to 'vm' is declared twice", "depends_on names unknown subsystem 'ghost'")

    def test_debt_needs_an_integer_issue(self) -> None:
        self.fixture.edit("subsystems.toml", "issue = 541", 'issue = "soon"')
        self.assert_error("debt edge to 'app' needs an integer issue")

    def test_test_only_imports_are_ignored(self) -> None:
        self.fixture.edit(
            "src/app/mod.rs", "pub struct Hook;",
            "pub struct Hook;\n#[cfg(test)]\nmod tests {\n    use crate::cli::Probe;\n    const BRACE: char = '}';\n}\n"
            "#[cfg(test)]\nmod helpers;\n",
        )
        self.fixture.write("src/app/helpers.rs", "use crate::cli::Probe;\n")
        self.fixture.write("src/cli/mod.rs", "pub struct Probe;\n")
        self.fixture.edit(
            "subsystems.toml", '[[subsystem]]\nid = "docs"',
            '[[subsystem]]\nid = "cli"\nlayer = 2\npaths = ["src/cli/"]\n\n[[subsystem]]\nid = "docs"',
        )
        self.assert_clean()

    def test_comments_and_literals_do_not_create_edges(self) -> None:
        # A real `cli` subsystem makes any `crate::cli` that escapes blanking a failing edge.
        # Each line defeats a specific lexer shortcut: a flat comment regex ends the nested
        # comment early, and a lexer without raw strings leaves `crate::cli` between quotes.
        self.fixture.write("src/cli/mod.rs", "pub struct Cli;\n")
        self.fixture.edit(
            "subsystems.toml", '[[subsystem]]\nid = "docs"',
            '[[subsystem]]\nid = "cli"\nlayer = 2\npaths = ["src/cli/"]\n\n[[subsystem]]\nid = "docs"',
        )
        self.fixture.edit(
            "src/app/mod.rs", "pub struct Hook;",
            "pub struct Hook;\n// crate::cli::Nope\n/* outer /* inner */ crate::cli */\n"
            'const RAW: &str = r#"a " crate::cli " b"#;\n'
            "fn keep<'a>(value: &'a str) -> &'a str { value }\n",
        )
        self.assert_clean()

    def test_glob_literals_do_not_hide_later_imports(self) -> None:
        # A regex `/* ... */` strip would blank from the glob's `/*` to the next `*/`,
        # swallowing the only app -> vm import and making that declared edge stale.
        self.fixture.edit("src/app/mod.rs", "use crate::vm::Value;\n", "")
        self.fixture.edit(
            "src/app/mod.rs", "pub struct Hook;",
            'pub struct Hook;\nconst GLOB: &str = "**/*.rs";\nuse crate::vm::Value;\nconst DIR: &str = "src/**/";\n',
        )
        self.assert_clean()

    def test_grouped_crate_imports_are_scanned(self) -> None:
        self.fixture.write("src/cli/mod.rs", "pub struct Cli;\n")
        self.fixture.edit(
            "subsystems.toml", '[[subsystem]]\nid = "docs"',
            '[[subsystem]]\nid = "cli"\nlayer = 2\npaths = ["src/cli/"]\n\n[[subsystem]]\nid = "docs"',
        )
        self.fixture.edit("src/vm/mod.rs", "pub struct Value;", "pub struct Value;\nuse crate::{\n    cli::Cli,\n};")
        self.assert_error("undeclared dependency vm (layer 0) -> cli (layer 2)", "src/vm/mod.rs:4 crate::cli")

    def test_module_split_across_owners_fails(self) -> None:
        self.fixture.write("src/app/split.rs", "pub fn split() {}\n")
        self.fixture.edit("subsystems.toml", 'paths = ["README.md"]', 'paths = ["README.md", "src/app/split.rs"]')
        self.fixture.edit("subsystems.toml", '[[subsystem]]\nid = "docs"', '[[subsystem]]\nid = "docs"\nlayer = 3')
        self.assert_error("src/app: top-level module is split across owners ['app', 'docs']")

    def test_global_entry_beats_a_shorter_subsystem_prefix(self) -> None:
        # Non-Rust files may be carved out of a subsystem; Rust modules may not (split rule).
        self.fixture.write("src/vm/shared/schema.capnp", "@0xdeadbeef;\n")
        self.fixture.edit("subsystems.toml", 'paths = ["src/lib.rs",', 'paths = ["src/vm/shared/", "src/lib.rs",')
        self.assert_clean()
        self.fixture.write("src/vm/shared/codec.rs", "pub fn codec() {}\n")
        self.assert_error("src/vm: top-level module is split across owners ['global', 'vm']")

    def test_record_without_an_id_is_reported_not_raised(self) -> None:
        self.fixture.edit("subsystems.toml", 'id = "docs"\n', "")
        self.assert_error("subsystem record 3 needs a string id")

    def test_production_rust_in_a_layerless_record_fails(self) -> None:
        self.fixture.write("src/notes/mod.rs", "use crate::vm::Value;\n")
        self.fixture.edit("subsystems.toml", 'paths = ["README.md"]', 'paths = ["README.md", "src/notes/"]')
        self.assert_error("src/notes/mod.rs: production Rust owned by 'docs', which has no layer")

    def test_agents_alias_must_point_at_claude(self) -> None:
        (self.fixture.root / "AGENTS.md").unlink()
        self.fixture.write("AGENTS.md", "# Diverged\n")
        self.assert_error("AGENTS.md must be a symlink to CLAUDE.md")

    def test_excluded_entries_need_a_reason(self) -> None:
        self.fixture.edit("subsystems.toml", 'reason = "history"', 'reason = ""')
        self.assert_error("excluded path 'old/' needs a reason")

    def test_blanking_keeps_line_numbers_and_code(self) -> None:
        source = 'let a = "x\ny"; // crate::gone\nuse crate::kept;\nlet c = \'{\';\n'
        blanked = blank_comments_and_literals(source)
        self.assertEqual(source.count("\n"), blanked.count("\n"), "blanking must keep line numbers")
        self.assertIn("use crate::kept;", blanked, "code outside literals and comments must survive")
        self.assertNotIn("crate::gone", blanked, "comment text must be blanked")
        self.assertNotIn("'{'", blanked, "char literal contents must be blanked")


if __name__ == "__main__":
    unittest.main()
