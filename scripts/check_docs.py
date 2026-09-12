#!/usr/bin/env python3
"""Bounded checks for Finch's current public documentation.

This intentionally checks only documents classified as current in docs/README.md.
Archived and design documents preserve historical claims and are outside this gate.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path
from urllib.parse import unquote


ROOT = Path(__file__).resolve().parent.parent
CURRENT_DOCS = (
    Path("README.md"),
    Path("CONTRIBUTING.md"),
    Path("CLAUDE.md"),
    Path("DESIGN.md"),
    Path("src/vm/AGENTS.md"),
    Path("src/memory/AGENTS.md"),
    Path("src/programs/AGENTS.md"),
    Path("docs/README.md"),
    Path("docs/AUTOMATIC_TRAINING.md"),
    Path("docs/MCP_USER_GUIDE.md"),
    Path("docs/MACOS_GUI_AUTOMATION.md"),
    Path("docs/OAUTH.md"),
    Path("docs/chatgpt-subscription-provider.md"),
    Path("docs/CHATGPT_SUBSCRIPTION_TRANSPORT.md"),
    Path("docs/REPOSITORY_HYGIENE.md"),
)

# Exact remnants of superseded public copy. These literals deliberately avoid
# banning legitimate names, configuration examples, limitations, or history.
STALE_CLAIMS = (
    ("project name: shammah", "obsolete product name"),
    ("<100ms startup", "unverified startup metric"),
    ("near-zero marginal cost", "unverified cost claim"),
    ("grok is the fastest free option", "unverified provider recommendation"),
    ("with your permission before every action", "obsolete blanket approval claim"),
    ("six model families are supported", "configuration mistaken for conformance"),
    ("finch-macos-aarch64.tar.gz", "stale release artifact name"),
    (
        "raw.githubusercontent.com/darwin-finch/finch/main/scripts/install.sh",
        "stale installer path",
    ),
    ("the hierarchical memory tree data structure is there", "unverified memory claim"),
    ("memtree ann search (cosine similarity)", "unverified ANN claim"),
    ('model = "gpt-4o"              # optional — default: gpt-4o', "stale model example"),
    (
        "finch daemon --bind 127.0.0.1:11435",
        "background port assigned to foreground daemon",
    ),
    (
        "finch daemon-start 127.0.0.1:8000",
        "foreground port assigned to background daemon",
    ),
)

UNSUPPORTED_MODEL_CLAIM_RES = (
    re.compile(
        r"\bFinch\s+(?:supports?|runs?|offers?|provides?)\s+[^.\n]{0,80}"
        r"gpt-5\.6(?:-sol)?\b",
        re.IGNORECASE,
    ),
    re.compile(
        r"\bgpt-5\.6(?:-sol)?\s+(?:is\s+)?"
        r"(?:supported|available|verified|release-ready)\b",
        re.IGNORECASE,
    ),
)
STALE_PRODUCT_NAME_RE = re.compile(r"\bShammah\b(?!\s+Chancellor\b)", re.IGNORECASE)
TRANSPORT_DOCUMENT = Path("docs/CHATGPT_SUBSCRIPTION_TRANSPORT.md")
OAUTH_DOCUMENT = Path("docs/OAUTH.md")
RESPONSES_REFERENCE = "https://developers.openai.com/api/reference/resources/responses/methods/create"
MODEL_REFERENCE = "https://developers.openai.com/api/docs/models/gpt-5.6-sol"
BROWSER_PKCE_SOURCE = "https://github.com/openai/codex/commit/3e4707b34b16e139fcb7ad11ab8445993b62bba1"
OAUTH_SOURCE = "https://github.com/openai/codex/commit/94cbbddafc1776d5e377bca1b05932c697e82238"
USAGE_EVIDENCE = "https://github.com/darwin-finch/finch/pull/349"
TOOL_EVIDENCE = "https://github.com/darwin-finch/finch/pull/351"
DOGFOOD_EVIDENCE = "https://github.com/darwin-finch/finch/issues/180#issuecomment-5557196748"
EVIDENCE_RULES = (
    (TRANSPORT_DOCUMENT, RESPONSES_REFERENCE, (("responses api",),)),
    (TRANSPORT_DOCUMENT, MODEL_REFERENCE, (("gpt-5.6 sol",),)),
    (OAUTH_DOCUMENT, BROWSER_PKCE_SOURCE, (("browser-pkce", "browser pkce"),)),
    (
        OAUTH_DOCUMENT,
        OAUTH_SOURCE,
        (("not registered",), ("openai oauth client",)),
    ),
    (
        TRANSPORT_DOCUMENT,
        USAGE_EVIDENCE,
        (("response.usage.attribution",), ("2026-09-05",)),
    ),
    (
        TRANSPORT_DOCUMENT,
        TOOL_EVIDENCE,
        (("spawn_agent",), ("2026-09-05",), ("never executes", "does not execute")),
    ),
    (
        TRANSPORT_DOCUMENT,
        DOGFOOD_EVIDENCE,
        (("spawn_agent",), ("lisp",), ("2026-09-05",)),
    ),
    (
        OAUTH_DOCUMENT,
        DOGFOOD_EVIDENCE,
        (("spawn_agent",), ("lisp",), ("2026-09-05",)),
    ),
)
REVISION_RULES = (
    (
        TRANSPORT_DOCUMENT,
        Path("src/providers/chatgpt_subscription.rs"),
        "CHATGPT_INFERENCE_PROTOCOL_REVISION",
    ),
    (
        OAUTH_DOCUMENT,
        Path("src/providers/chatgpt_oauth.rs"),
        "CHATGPT_OAUTH_PROTOCOL_REVISION",
    ),
)

LINK_RE = re.compile(r"(?<!!)\[[^\]]*\]\(([^)]+)\)")
FENCE_RE = re.compile(r"^```(bash|sh)\s*$\n(.*?)^```\s*$", re.MULTILINE | re.DOTALL)
HEADING_RE = re.compile(r"^#{1,6}\s+(.+?)\s*#*\s*$", re.MULTILINE)
SECTION_RE = re.compile(r"^##\s+(.+?)\s*$", re.MULTILINE)

# The root design index must be reachable from the agent entry point, link every document the
# capsule, and never present historical or planning documents as current authority.
DESIGN_DOCUMENT = Path("DESIGN.md")
AGENTS_DOCUMENT = Path("AGENTS.md")
DOCS_MAP = Path("docs/README.md")
# Superseded documents kept for history; the current design index must never cite them.
HISTORICAL_DOCUMENTS = ("docs/ARCHITECTURE.md", "docs/SUBSYSTEMS.md")
DESIGN_PLAN_SECTIONS = ("Intended direction", "Open questions")


def github_anchor(heading: str) -> str:
    heading = re.sub(r"<[^>]+>", "", heading).strip().lower()
    heading = re.sub(r"[^\w\- ]", "", heading, flags=re.UNICODE)
    return heading.replace(" ", "-")


def split_link(raw: str) -> tuple[str, str]:
    # Markdown titles are not used in the current set. Keep parsing deliberately
    # small and reject whitespace-containing destinations instead of guessing.
    destination = raw.strip()
    if destination.startswith("<") and destination.endswith(">"):
        destination = destination[1:-1]
    if any(character.isspace() for character in destination):
        return "", ""
    path, separator, fragment = destination.partition("#")
    return unquote(path), unquote(fragment) if separator else ""


def check_links(document: Path, text: str) -> list[str]:
    errors: list[str] = []
    for match in LINK_RE.finditer(text):
        raw = match.group(1)
        link_path, fragment = split_link(raw)
        if not link_path and not fragment:
            errors.append(f"{document}: unsupported local link syntax: {raw}")
            continue
        if link_path.startswith(("http://", "https://", "mailto:")):
            continue
        target = (ROOT / document.parent / link_path).resolve() if link_path else ROOT / document
        try:
            target.relative_to(ROOT)
        except ValueError:
            errors.append(f"{document}: local link escapes repository: {raw}")
            continue
        if not target.exists():
            errors.append(f"{document}: missing local link target: {raw}")
            continue
        if fragment and target.suffix.lower() == ".md":
            anchors = {github_anchor(heading) for heading in HEADING_RE.findall(target.read_text())}
            if fragment.lower() not in anchors:
                errors.append(f"{document}: missing heading for local link: {raw}")
    return errors


def check_shell_fences(document: Path, text: str) -> list[str]:
    errors: list[str] = []
    for index, match in enumerate(FENCE_RE.finditer(text), start=1):
        with tempfile.NamedTemporaryFile(mode="w", suffix=".sh") as script:
            script.write(match.group(2))
            script.flush()
            result = subprocess.run(
                ["bash", "-n", script.name], capture_output=True, text=True, check=False
            )
        if result.returncode:
            detail = result.stderr.strip() or "bash -n failed"
            errors.append(f"{document}: shell fence {index}: {detail}")
    return errors


def linked_paragraphs(text: str, url: str) -> list[str]:
    matches: list[str] = []
    for paragraph in re.split(r"\n\s*\n", text):
        for raw in LINK_RE.findall(paragraph):
            path, fragment = split_link(raw)
            destination = f"{path}#{fragment}" if fragment else path
            if destination == url:
                matches.append(paragraph)
                break
    return matches


def normalize_claim(text: str) -> str:
    return " ".join(text.casefold().split())


def rust_string_constant(source: str, name: str) -> str | None:
    match = re.search(
        rf"\bpub(?:\(crate\))?\s+const\s+{re.escape(name)}:\s*&str\s*=\s*\"([^\"]+)\";",
        source,
        flags=re.DOTALL,
    )
    return match.group(1) if match else None


def check_truth_claims(
    document: Path, text: str, source_overrides: dict[Path, str] | None = None
) -> list[str]:
    """Reject broad support copy and bind claims to adjacent exact authorities."""
    errors = [
        f"{document}: current docs contain an unsupported broad GPT-5.6 claim"
        for pattern in UNSUPPORTED_MODEL_CLAIM_RES
        if pattern.search(text)
    ]
    for rule_document, url, required_terms in EVIDENCE_RULES:
        if document != rule_document:
            continue
        paragraphs = linked_paragraphs(text, url)
        if not paragraphs:
            errors.append(
                f"{document}: exact evidence URL is missing; url={url!r}"
            )
            continue
        for paragraph in paragraphs:
            normalized = normalize_claim(paragraph)
            missing = [
                alternatives
                for alternatives in required_terms
                if not any(normalize_claim(term) in normalized for term in alternatives)
            ]
            if missing:
                observed = re.sub(r"\s+", " ", paragraph)[:240]
                errors.append(
                    f"{document}: evidence URL is bound to the wrong claim; url={url!r} "
                    f"missing={missing!r} observed={observed!r}"
                )

    for rule_document, source_path, constant_name in REVISION_RULES:
        if document != rule_document:
            continue
        source = (
            source_overrides[source_path]
            if source_overrides and source_path in source_overrides
            else (ROOT / source_path).read_text()
        )
        revision = rust_string_constant(source, constant_name)
        if revision is None:
            errors.append(f"{source_path}: missing Rust string constant {constant_name}")
            continue
        commit = re.search(r"@([0-9a-f]{40})", revision)
        if commit is None:
            errors.append(
                f"{source_path}: {constant_name} lacks a pinned 40-character commit: {revision!r}"
            )
            continue
        url = f"https://github.com/openai/codex/commit/{commit.group(1)}"
        paragraphs = linked_paragraphs(text, url)
        if not paragraphs:
            errors.append(
                f"{document}: documented {constant_name} does not match current source; "
                f"expected_revision={revision!r} expected_url={url!r} matches=0"
            )
            continue
        for paragraph in paragraphs:
            if revision not in paragraph:
                errors.append(
                    f"{document}: {constant_name} citation lacks the current source revision; "
                    f"expected_revision={revision!r} expected_url={url!r}"
                )
    return errors


CODE_IDENTIFIER = re.compile(r"`([a-z][a-z0-9_]*)`")
RUST_DEFINITION = re.compile(
    r"\b(?:fn|struct|enum|trait|const|static|type|mod|union|macro_rules!)\s+([A-Za-z_][A-Za-z0-9_]*)"
)
RUST_FIELD = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?([a-z_][a-z0-9_]*)\s*:", re.M)


def defined_identifiers(root: Path) -> set[str]:
    """Every name `src/` and `tests/` define, as items or as struct fields."""
    names: set[str] = set()
    for directory in ("src", "tests"):
        for path in (root / directory).rglob("*.rs"):
            text = path.read_text(errors="replace")
            names.update(RUST_DEFINITION.findall(text))
            names.update(RUST_FIELD.findall(text))
    return names


def check_cited_identifiers(document: Path, text: str, defined: set[str]) -> list[str]:
    """A citation next to a source path must name something the code still defines.

    An invariant is only as good as the test it names. When a test is renamed or deleted, the
    claim above it keeps reading as enforced, which is worse than saying nothing — so a citation
    on a line that also names a `.rs` file has to resolve. Only lines that name a file are checked:
    prose about a tool or a mode is not a claim about a symbol.
    """
    errors = []
    for number, line in enumerate(text.splitlines(), start=1):
        if ".rs" not in line:
            continue
        for name in CODE_IDENTIFIER.findall(line):
            if "_" not in name or name in defined:
                continue
            errors.append(
                f"{document}:{number} cites `{name}` beside a source path, but nothing in src/ or "
                "tests/ defines it; fix the citation or remove the claim it supports"
            )
    return errors


def check_stale_claims(text: str) -> list[str]:
    normalized = text.casefold()
    errors = [
        f"current docs contain {description}: {stale_claim!r}"
        for stale_claim, description in STALE_CLAIMS
        if stale_claim in normalized
    ]
    if STALE_PRODUCT_NAME_RE.search(text):
        errors.append("current docs contain obsolete standalone product name: 'Shammah'")
    return errors


def local_links(document: Path, text: str) -> list[tuple[str, str]]:
    """Return repository-relative local link targets with the `##` section each appears under."""
    headings = [(match.start(), match.group(1)) for match in SECTION_RE.finditer(text)]
    links: list[tuple[str, str]] = []
    for match in LINK_RE.finditer(text):
        path, _ = split_link(match.group(1))
        if not path or path.startswith(("http://", "https://", "mailto:")):
            continue
        section = next(
            (title for start, title in reversed(headings) if start < match.start()), ""
        )
        links.append((os.path.normpath((document.parent / path).as_posix()), section))
    return links


def capsule_paths(root: Path) -> list[str]:
    """Every source directory that carries a capsule, deepest path last."""
    return sorted(
        path.relative_to(root).as_posix()
        for path in (root / "src").rglob("AGENTS.md")
    )


def check_design_index(design: str, capsules: list[str], docs_map: str) -> list[str]:
    """The design index must reach every capsule and cite no historical document."""
    errors: list[str] = []
    for capsule in capsules:
        if capsule not in design:
            errors.append(f"{DESIGN_DOCUMENT}: does not link the capsule {capsule}")
    for match in LINK_RE.finditer(design):
        target = split_link(match.group(1))[0]
        if target.startswith("docs/archive/") or target in HISTORICAL_DOCUMENTS:
            errors.append(f"{DESIGN_DOCUMENT}: cites historical or archived document {target}")
    if "DESIGN.md" not in docs_map:
        errors.append("docs/README.md: does not list the root design index")
    return errors


def design_index_self_test(root: Path) -> list[str]:
    errors: list[str] = []
    if DESIGN_DOCUMENT not in CURRENT_DOCS:
        errors.append("root design index is not enrolled in CURRENT_DOCS")
    design = (root / DESIGN_DOCUMENT).read_text()
    docs_map = (root / DOCS_MAP).read_text()
    capsules = capsule_paths(root)
    if not capsules:
        return errors + ["no capsules found; design index probes cannot run"]
    if check_design_index(design, capsules, docs_map):
        errors.append("current design index was rejected")
    probes = (
        ("unlinked capsule",
         (design.replace(capsules[0], "src/missing/AGENTS.md"), capsules, docs_map),
         f"does not link the capsule {capsules[0]}"),
        ("historical target",
         (design.replace("## Composition\n", "## Composition\n\n[old](docs/ARCHITECTURE.md)\n", 1), capsules, docs_map),
         "cites historical or archived document docs/ARCHITECTURE.md"),
        ("archived target",
         (design.replace("## Composition\n", "## Composition\n\n[old](docs/archive/x.md)\n", 1), capsules, docs_map),
         "cites historical or archived document docs/archive/x.md"),
        ("design index missing from the documentation map",
         (design, capsules, docs_map.replace("DESIGN.md", "OTHER.md")),
         "does not list the root design index"),
    )
    for label, arguments, expected in probes:
        found = check_design_index(*arguments)
        if not any(expected in error for error in found):
            errors.append(f"design index probe escaped ({label}): expected {expected!r}, got {found}")
    return errors


def self_test() -> int:
    """Exercise the important positive and negative controls for this gate."""
    errors: list[str] = design_index_self_test(ROOT)
    if TRANSPORT_DOCUMENT not in CURRENT_DOCS:
        errors.append("native ChatGPT transport guide is not enrolled in CURRENT_DOCS")

    defined = defined_identifiers(ROOT)
    documents = {
        TRANSPORT_DOCUMENT: (ROOT / TRANSPORT_DOCUMENT).read_text(),
        OAUTH_DOCUMENT: (ROOT / OAUTH_DOCUMENT).read_text(),
    }
    for document, text in documents.items():
        errors.extend(check_truth_claims(document, text))

    for control in ("Finch supports GPT-5.6 Sol.", "GPT-5.6 is verified."):
        if not check_truth_claims(Path("docs/example.md"), control):
            errors.append(f"unsupported broad GPT-5.6 control escaped: {control!r}")

    # A citation beside a source path must resolve, or an invariant keeps reading as enforced
    # after its test is renamed away. Both directions, because a checker that flags everything is
    # as useless as one that flags nothing.
    example = Path("docs/example.md")
    if not check_cited_identifiers(example, "claim — `test_no_such_thing` in `src/x.rs`", defined):
        errors.append("a citation naming a test that does not exist escaped")
    if check_cited_identifiers(example, "claim — `is_readonly_bash` in `src/x.rs`", defined):
        errors.append("a citation naming a real definition was rejected")
    if check_cited_identifiers(example, "the `gui_click` primitive is internal", defined):
        errors.append("prose with no source path was treated as a citation")

    for control in (
        'model = "gpt-5.6-sol"',
        "GPT-5.6 Sol is not supported by older packaged releases.",
    ):
        if check_truth_claims(Path("docs/example.md"), control):
            errors.append(f"legitimate GPT-5.6 control was rejected: {control!r}")

    authority_mutants = (
        (TRANSPORT_DOCUMENT, RESPONSES_REFERENCE, "Responses API authority"),
        (TRANSPORT_DOCUMENT, MODEL_REFERENCE, "GPT-5.6 model authority"),
        (OAUTH_DOCUMENT, BROWSER_PKCE_SOURCE, "browser-PKCE source authority"),
        (OAUTH_DOCUMENT, "not registered", "independent OAuth registration disclaimer"),
    )
    for document, removed, label in authority_mutants:
        mutant = documents[document].replace(removed, "https://example.invalid")
        if not check_truth_claims(document, mutant):
            errors.append(f"missing {label} escaped the docs gate")

    swapped = documents[TRANSPORT_DOCUMENT].replace(USAGE_EVIDENCE, "SWAPPED_EVIDENCE", 1)
    swapped = swapped.replace(TOOL_EVIDENCE, USAGE_EVIDENCE, 1).replace(
        "SWAPPED_EVIDENCE", TOOL_EVIDENCE, 1
    )
    if not check_truth_claims(TRANSPORT_DOCUMENT, swapped):
        errors.append("swapped live-evidence URLs escaped paragraph-local binding")

    harmless_rephrase = documents[TRANSPORT_DOCUMENT].replace(
        "never executes it", "DOES   NOT\nEXECUTE it", 1
    ).replace("Lisp", "lIsP", 1)
    harmless_rephrase = harmless_rephrase.replace(
        f"]({TOOL_EVIDENCE})", f"](<{TOOL_EVIDENCE}>)", 1
    )
    if check_truth_claims(TRANSPORT_DOCUMENT, harmless_rephrase) or check_links(
        TRANSPORT_DOCUMENT, harmless_rephrase
    ):
        errors.append("case, whitespace, semantic rephrase, or angle-link control was rejected")

    for document, source_path, constant_name in REVISION_RULES:
        revision = rust_string_constant((ROOT / source_path).read_text(), constant_name)
        assert revision is not None
        commit = re.search(r"@([0-9a-f]{40})", revision)
        assert commit is not None
        url = f"https://github.com/openai/codex/commit/{commit.group(1)}"
        paragraph = linked_paragraphs(documents[document], url)[0]
        duplicated = f"{documents[document]}\n\n{paragraph}\n"
        if check_truth_claims(document, duplicated):
            errors.append(f"duplicate truthful {constant_name} citation was rejected")

    source_path = REVISION_RULES[0][1]
    source = (ROOT / source_path).read_text()
    revision = rust_string_constant(source, REVISION_RULES[0][2])
    assert revision is not None
    mutated_source = source.replace(revision, f"{revision}-source-mutation", 1)
    if not check_truth_claims(
        TRANSPORT_DOCUMENT,
        documents[TRANSPORT_DOCUMENT],
        {source_path: mutated_source},
    ):
        errors.append("source-only transport revision mutation escaped the docs gate")

    if not check_stale_claims("Shammah reads configuration"):
        errors.append("standalone stale product name escaped")
    if check_stale_claims("Shammah Chancellor maintains Finch"):
        errors.append("the maintainer's full name was rejected as stale product copy")

    dead_link = "[missing](definitely-missing-current-doc.md)"
    if not check_links(TRANSPORT_DOCUMENT, dead_link):
        errors.append("dead-link probe escaped the enrolled-guide link gate")

    bad_fence = "```sh\nif then\n```\n"
    if not check_shell_fences(TRANSPORT_DOCUMENT, bad_fence):
        errors.append("invalid-shell-fence probe escaped the enrolled-guide syntax gate")

    if errors:
        for error in errors:
            print(f"docs checker self-test: {error}", file=sys.stderr)
        return 1
    print("docs checker self-test: exact claim, authority, link, and shell probes passed")
    return 0


def main() -> int:
    errors: list[str] = []
    combined = ""
    defined = defined_identifiers(ROOT)
    for document in CURRENT_DOCS:
        path = ROOT / document
        if not path.is_file():
            errors.append(f"missing current document: {document}")
            continue
        text = path.read_text()
        combined += f"\n{text}"
        errors.extend(check_links(document, text))
        errors.extend(check_shell_fences(document, text))
        errors.extend(check_truth_claims(document, text))
        errors.extend(check_cited_identifiers(document, text, defined))
    errors.extend(check_design_index((ROOT / DESIGN_DOCUMENT).read_text(), capsule_paths(ROOT), (ROOT / DOCS_MAP).read_text()))

    # The package description is published to package indexes and mirrored far
    # more widely than any document here, so it is held to the same standard.
    manifest = ROOT / "Cargo.toml"
    if manifest.is_file():
        for line in manifest.read_text().splitlines():
            if line.startswith("description"):
                combined += f"\n{line}"
                break
    else:
        errors.append("missing Cargo.toml")

    errors.extend(check_stale_claims(combined))

    if errors:
        for error in errors:
            print(f"docs check: {error}", file=sys.stderr)
        return 1

    print(f"docs check: {len(CURRENT_DOCS)} current documents passed")
    return 0


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        raise SystemExit(self_test())
    if sys.argv[1:]:
        print("usage: check_docs.py [--self-test]", file=sys.stderr)
        raise SystemExit(2)
    raise SystemExit(main())
