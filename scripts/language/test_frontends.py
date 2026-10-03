#!/usr/bin/env python3
"""Regression tests for executing the normative grammars and both semantic-construction frontends."""

from __future__ import annotations

import copy
import json
import sys
import unittest
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
ROOT = SCRIPT_DIR.parents[1]
LANG = ROOT / "docs/language"
sys.path.insert(0, str(SCRIPT_DIR))

from check_language_spec import (  # noqa: E402
    check_grammar_corpus,
    semantic_digest,
    semantic_events,
    validate_grammar_terminals,
    validate_root_entrypoints,
    vector_streams,
    vector_trees,
)
from elaborate import ElaborationError, elaborate, strip_spans  # noqa: E402
from grammar_engine import Grammar, GrammarError, ReaderError, parse, parse_notation  # noqa: E402


def leaf_tokens(tree: dict) -> list[tuple[str, str]]:
    if "rule" not in tree:
        return [(tree["token"], tree["text"])]
    found: list[tuple[str, str]] = []
    for child in tree["children"]:
        found.extend(leaf_tokens(child))
    return found


class GrammarExecutionTests(unittest.TestCase):
    def test_corpus_is_accepted_and_rejected_as_declared(self) -> None:
        errors: list[str] = []
        check_grammar_corpus(errors)
        self.assertEqual(errors, [], "every corpus case must match and every production must be exercised")

    def test_float_is_not_split_into_integer_and_member_access(self) -> None:
        for syntax in ("colisp", "coforth"):  # the C-like frontend has integers only in 0.1's executable core
            tokens = leaf_tokens(parse(syntax, "submission", "1.5 -3.25f32 2e10"))
            self.assertEqual(
                tokens,
                [("float", "1.5"), ("negative_numeric", "-3.25f32"), ("float", "2e10")],
                f"{syntax} ordered choice must try float before integer; got {tokens!r}",
            )

    def test_colisp_mut_binding_is_not_read_as_a_binding_named_mut(self) -> None:
        ast = strip_spans(elaborate("colisp", "(let [mut x 1] x)"))
        self.assertEqual(
            (ast["name"], ast["mutable"]),
            ("x", True),
            f"`mut` must be the mutability marker, not an identifier: {ast!r}",
        )

    def test_coforth_record_constructor_wins_over_identifier_then_locals(self) -> None:
        ast = strip_spans(elaborate("coforth", "Point{ x: 1 2 + y: 4 }"))
        self.assertEqual(ast["form"], "record-construct", f"Name{{ ... }} must construct a record: {ast!r}")
        self.assertEqual([field["name"] for field in ast["fields"]], ["x", "y"], "a field value may span several words")

    def test_tagged_json_is_not_an_identifier_followed_by_a_vector(self) -> None:
        tree = parse("colisp", "submission", 'json[1, "a"]')
        kinds = [kind for kind, _ in leaf_tokens(tree)]
        self.assertIn("json_number", kinds, f"json[...] must be read by the JSON island grammar: {kinds!r}")
        self.assertNotIn("identifier", kinds, "the json tag must not be read as an identifier")

    def test_reserved_literal_spelling_cannot_be_bound(self) -> None:
        with self.assertRaises(ReaderError) as caught:
            parse("colisp", "submission", "(let [true 1] true)")
        self.assertEqual(caught.exception.code, "F-LEX-SYNTAX", f"unexpected reader failure {caught.exception}")

    def test_reserved_form_never_falls_back_to_a_call(self) -> None:
        with self.assertRaises(ReaderError):
            parse("colisp", "submission", "(if 1 2)")
        with self.assertRaises(ReaderError):
            parse("coforth", "submission", "1 endof")

    def test_envelope_root_is_not_a_parse_success_fallback(self) -> None:
        for syntax, source in (("colisp", "(+ 1 2)"), ("coforth", "1 2 +")):
            parse(syntax, "submission", source)
            with self.assertRaises(ReaderError, msg=f"{syntax} library root accepted an executing expression"):
                parse(syntax, "library", source)

    def test_parenthesized_comment_is_skipped_only_where_no_signature_is_expected(self) -> None:
        commented = strip_spans(elaborate("coforth", ": f ( x: int -- int ! plain ) ( a comment ) x 1 + ; 2 f"))
        plain = strip_spans(elaborate("coforth", ": f ( x: int -- int ! plain ) x 1 + ; 2 f"))
        self.assertEqual(commented, plain, "a comment must not change the constructed program")

    def test_left_recursive_grammar_is_rejected_when_loaded(self) -> None:
        directory = LANG / "grammar"
        mutated = SCRIPT_DIR / "__left_recursion_probe__"
        mutated.mkdir(exist_ok=True)
        try:
            for name in ("common.json", "colisp.json"):
                document = json.loads((directory / name).read_text())
                if name == "colisp.json":
                    document["productions"]["expression"]["alternatives"].insert(0, "expression identifier")
                (mutated / name).write_text(json.dumps(document))
            with self.assertRaisesRegex(GrammarError, "left-recursive"):
                Grammar("colisp", mutated)
        finally:
            for child in mutated.iterdir():
                child.unlink()
            mutated.rmdir()

    def test_notation_rejects_unbalanced_groups(self) -> None:
        self.assertEqual(parse_notation("'(' expression"), ("seq", [("lit", "("), ("sym", "expression")]), "a quoted paren is a literal")
        with self.assertRaisesRegex(GrammarError, "unbalanced group"):
            parse_notation("( expression")

    def test_every_reserved_word_is_a_real_terminal(self) -> None:
        common = json.loads((LANG / "grammar/common.json").read_text())
        self.assertEqual(validate_grammar_terminals(common), [], "common reserved words must be grammar terminals")
        for syntax in ("colisp", "coforth", "clike"):
            grammar = json.loads((LANG / f"grammar/{syntax}.json").read_text())
            self.assertEqual(
                validate_grammar_terminals(grammar, set(common["reserved_words"])),
                [],
                f"{syntax} reserved words must be grammar terminals",
            )

    def test_reserved_nonterminal_spelling_is_detected(self) -> None:
        common = json.loads((LANG / "grammar/common.json").read_text())
        common["reserved_words"].append("reslut")
        errors = validate_grammar_terminals(common)
        self.assertTrue(
            any("'reslut'" in error for error in errors),
            f"reserved typo without a grammar terminal escaped validation: {errors!r}",
        )

    def test_frontends_publish_envelope_selected_root_entrypoints(self) -> None:
        for syntax in ("colisp", "coforth", "clike"):
            grammar = json.loads((LANG / f"grammar/{syntax}.json").read_text())
            self.assertEqual(
                validate_root_entrypoints(grammar),
                [],
                f"{syntax} must distinguish inert module roots from executing roots before parsing",
            )

    def test_submission_fallback_cannot_replace_a_library_root(self) -> None:
        grammar = json.loads((LANG / "grammar/colisp.json").read_text())
        grammar["root_entrypoints"]["library"] = "submission"
        errors = validate_root_entrypoints(grammar)
        self.assertTrue(errors, "a library root must never parse through the executing submission entrypoint")


class SemanticConstructionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.vectors = json.loads((LANG / "fixtures/execution-vectors.json").read_text())["vectors"]

    def vector(self, name: str) -> dict:
        return next(item for item in self.vectors if item["id"] == name)

    def test_each_reader_builds_the_stored_ast_from_its_own_spelling(self) -> None:
        for vector in self.vectors:
            for syntax, tree in vector_trees(vector).items():
                with self.subTest(vector=vector["id"], syntax=syntax):
                    self.assertEqual(strip_spans(tree), vector["ast"], "reader output must equal the checked AST")

    def test_source_mutation_changes_the_constructed_ast(self) -> None:
        vector = self.vector("left-to-right-add")
        self.assertNotEqual(strip_spans(elaborate("colisp", vector["colisp"].replace("22", "23"))), vector["ast"])
        vector = self.vector("conditional-selects-then")
        self.assertNotEqual(strip_spans(elaborate("coforth", vector["coforth"].replace("true", "false"))), vector["ast"])

    def test_both_readers_share_one_digest_and_keep_their_own_spans(self) -> None:
        vector = self.vector("while-loop-state-threading")
        streams = vector_streams(vector, vector_trees(vector))
        self.assertEqual(
            semantic_digest(streams["colisp"]),
            semantic_digest(streams["coforth"]),
            "paired spellings must hash to one semantic digest",
        )
        spans = {syntax: [event["origin"]["span"] for event in stream["events"]] for syntax, stream in streams.items()}
        self.assertNotEqual(spans["colisp"], spans["coforth"], "each reader must report spans in its own source bytes")
        for syntax, stream in streams.items():
            root = stream["events"][0]["origin"]["span"]
            self.assertEqual(
                (root["start"], root["end"]),
                (0, len(vector[syntax].encode())),
                f"{syntax} root span must cover the whole form, not a placeholder",
            )

    def test_spans_are_utf8_byte_offsets(self) -> None:
        source = '(+ (len "é") 1)'
        tree = elaborate("colisp", source)
        literal = tree["arguments"][1]
        self.assertEqual(
            source.encode()[literal["$span"][0] : literal["$span"][1]],
            b"1",
            f"span {literal['$span']} must index bytes after a two-byte scalar",
        )

    def test_pattern_participates_in_the_semantic_digest(self) -> None:
        digests = []
        for source in ("(match 5 (n 1))", "(match 5 (_ 1))", "(match 5 (5 1))"):
            stream = semantic_events("fixture:pattern", "lisp", source, elaborate("colisp", source))
            digests.append(semantic_digest(stream))
        self.assertEqual(len(set(digests)), 3, "binder, wildcard, and literal patterns are different programs")

    def test_source_identity_is_bound_to_source_bytes(self) -> None:
        vector = self.vector("string-literal-escapes")
        stream = vector_streams(vector, vector_trees(vector))["colisp"]
        self.assertNotEqual(stream["source"]["sha256"], "0" * 64, "source digest must not be a placeholder")
        self.assertEqual(stream["source"]["byte_length"], len(vector["colisp"].encode("utf-8")))

    def test_coforth_statement_before_pending_value_keeps_evaluation_order(self) -> None:
        operations = {"log": {"kind": "emit", "parameters": 1, "result": False}}
        forth = strip_spans(elaborate("coforth", '1 "x" log 2 +', operations))
        lisp = strip_spans(elaborate("colisp", '(+ 1 (begin (log "x") 2))'))
        self.assertEqual(forth, lisp, "a statement word after a pending value runs before the next value")

    def test_coforth_rejects_statement_with_no_following_value(self) -> None:
        operations = {"log": {"kind": "emit", "parameters": 1, "result": False}}
        with self.assertRaisesRegex(ElaborationError, "last word"):
            elaborate("coforth", '1 "x" log', operations)

    def test_coforth_uninitialized_local_must_be_mut_and_assigned_before_use(self) -> None:
        with self.assertRaisesRegex(ElaborationError, "must be mut"):
            elaborate("coforth", "{ -- x: int } 1 to x x")
        with self.assertRaisesRegex(ElaborationError, "before its first assignment"):
            elaborate("coforth", "{ -- mut x: int } x")

    def test_coforth_name_of_a_callable_applies_it_wherever_it_was_bound(self) -> None:
        defined = strip_spans(elaborate("coforth", ": f ( x: int -- int ! plain ) x ; 41 f"))
        self.assertEqual(defined["items"][1]["form"], "call", f"a `:` word runs when named: {defined['items'][1]!r}")
        local = strip_spans(elaborate("coforth", "[ ( x: int -- int ! plain ) | x ] { f: callable<(int) -> int> -- } 41 f"))
        self.assertEqual(
            (local["body"]["form"], local["body"]["callee"]),
            ("call", "f"),
            f"a local of declared callable type runs when named, like a defined word: {local['body']!r}",
        )
        lisp = strip_spans(elaborate("colisp", "(let [f : callable<(int) -> int> (lambda ((x : int)) ! plain x)] (f 41))"))
        self.assertEqual(local, lisp, "both spellings of calling a local callable must build one program")

    def test_coforth_tick_pushes_and_call_applies(self) -> None:
        ticked = strip_spans(elaborate("coforth", ": work ( -- int ! plain ) 7 ; ' work spawn join"))
        lisp = strip_spans(elaborate("colisp", "(define (work) : int ! plain 7) (join (spawn work))"))
        self.assertEqual(ticked, lisp, "tick must push the named callable without applying it")
        applied = strip_spans(elaborate("coforth", "41 [ ( x: int -- int ! plain ) | x ] call"))
        self.assertEqual(applied, strip_spans(elaborate("colisp", "((lambda ((x : int)) ! plain x) 41)")))
        with self.assertRaisesRegex(ElaborationError, "signature is known"):
            elaborate("coforth", "1 { f -- } 41 ' f call")

    def test_guard_words_are_one_family_and_defer_is_an_ordinary_name(self) -> None:
        guard = strip_spans(elaborate("colisp", "(scope (on-exit ()) 1)"))["guards"][0]
        self.assertEqual(guard["reason"], "exit", f"on-exit is the always-run guard: {guard!r}")
        self.assertEqual(strip_spans(elaborate("colisp", "(defer 1)"))["callee"], "defer", "`defer` is no longer a reserved guard word")

    def test_clike_names_map_to_canonical_identifiers(self) -> None:
        camel = strip_spans(elaborate("clike", "raceAndReap(readFile2(x))"))
        self.assertEqual(
            (camel["callee"], camel["arguments"][0]["callee"]),
            ("race-and-reap", "read-file2"),
            f"camelCase must be the canonical hyphenated name: {camel!r}",
        )
        self.assertEqual(strip_spans(elaborate("clike", "`even?`(3)"))["callee"], "even?", "a raw identifier is taken verbatim")
        self.assertEqual(strip_spans(elaborate("clike", "IoError(3)"))["callee"], "IoError", "an uppercase-initial name is canonical as written")

    def test_clike_infix_and_blocks_build_the_colisp_tree(self) -> None:
        pairs = [
            ("1 + 2 * 3 - 4", "(- (+ 1 (* 2 3)) 4)"),
            ("a < b + 1", "(< a (+ b 1))"),
            ("-x * 2", "(* (- x) 2)"),
            ("{ f(); g() }", "(begin (f) (g))"),
            ("int function(int, move Token) pure nothrow nonSuspending f = g; f", "(let [f : callable<(int, steal Token) -> int ! pure | nothrow | non-suspending> g] f)"),
        ]
        for clike, lisp in pairs:
            with self.subTest(source=clike):
                self.assertEqual(
                    strip_spans(elaborate("clike", clike)),
                    strip_spans(elaborate("colisp", lisp)),
                    "the C-like spelling must construct the same program",
                )

    def test_clike_array_suffixes_lower_one_consistent_way(self) -> None:
        cases = {
            "int[3] v = x; v": "array<int,3>",
            "int[3][4][10] v = x; v": "array<array<array<int,3>,4>,10>",
            "int[3, 4, 10] v = x; v": "static-array<int,3,4,10>",
            "int[] v = x; v": "vector<int>",
            "int[][][] v = x; v": "vector<vector<vector<int>>>",
            "int[3][] v = x; v": "vector<array<int,3>>",
            "int[][3] v = x; v": "array<vector<int>,3>",
        }
        for source, expected in cases.items():
            with self.subTest(source=source):
                self.assertEqual(strip_spans(elaborate("clike", source))["type"], expected, "one bracket is one array and suffixes compose leftward")
        nested = strip_spans(elaborate("clike", "int[3][4] v = x; v"))
        lisp = strip_spans(elaborate("colisp", "(let [v : array<array<int, 3>, 4> x] v)"))
        self.assertEqual(nested, lisp, "the suffix form and the written-out type are one program")

    def test_triple_string_removes_common_indentation(self) -> None:
        ast = elaborate("colisp", '"""\n    a\n      b\n    """')
        self.assertEqual(ast["value"], "a\n  b\n", f"triple-string dedent produced {ast['value']!r}")

    def test_contract_sugar_normalizes_before_construction(self) -> None:
        plain = strip_spans(elaborate("colisp", "(define (f) : int ! plain 1) (+ (f) 0)"))
        spelled = strip_spans(elaborate("colisp", "(define (f) : int ! effects<> | nothrow | non-suspending 1) (+ (f) 0)"))
        self.assertEqual(plain, spelled, "`plain` must not survive into the semantic program")

    def test_copied_ast_is_not_aliased_by_span_stripping(self) -> None:
        tree = elaborate("colisp", "(+ 1 2)")
        before = copy.deepcopy(tree)
        strip_spans(tree)
        self.assertEqual(tree, before, "span stripping must not mutate the reader's tree")


if __name__ == "__main__":
    unittest.main()
