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
# subsystem manifest names, and never present historical or planning documents as current authority.
DESIGN_DOCUMENT = Path("DESIGN.md")
AGENTS_DOCUMENT = Path("AGENTS.md")
MANIFEST_DOCUMENT = Path("subsystems.toml")
DOCS_MAP = Path("docs/README.md")
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


def manifest_doc_paths(manifest: str) -> list[str] | None:
    """Every `docs` entry in the subsystem manifest except DESIGN.md itself; None if unparseable."""
    try:
        records = tomllib.loads(manifest).get("subsystem", [])
    except tomllib.TOMLDecodeError:
        return None
    paths: list[str] = []
    for record in records if isinstance(records, list) else []:
        for path in record.get("docs", []) if isinstance(record, dict) else []:
            if isinstance(path, str) and path != DESIGN_DOCUMENT.as_posix() and path not in paths:
                paths.append(path)
    return paths


def check_design_index(
    agents: str, manifest: str, design: str, docs_map: str, exists=None,
) -> list[str]:
    """Bind the root design index to the agent entry point and the documentation roles."""
    exists = exists or (lambda path: (ROOT / path).exists())
    errors: list[str] = []
    if DESIGN_DOCUMENT.as_posix() not in {target for target, _ in local_links(AGENTS_DOCUMENT, agents)}:
        errors.append(f"{AGENTS_DOCUMENT}: must link the root design index {DESIGN_DOCUMENT}")

    design_links = local_links(DESIGN_DOCUMENT, design)
    linked = {target for target, _ in design_links}
    paths = manifest_doc_paths(manifest)
    if paths is None:
        errors.append(f"{MANIFEST_DOCUMENT}: is not valid TOML")
        paths = []
    elif not paths:
        errors.append(f"{MANIFEST_DOCUMENT}: missing, or lists no subsystem docs entries")
    for path in paths:
        if not exists(path):
            errors.append(f"{MANIFEST_DOCUMENT}: docs entry does not exist: {path}")
        if path not in linked:
            errors.append(f"{DESIGN_DOCUMENT}: does not link docs entry named in {MANIFEST_DOCUMENT}: {path}")

    roles = local_links(DOCS_MAP, docs_map)
    historical = {target for target, section in roles if "historical" in section.casefold()}
    plans = {target for target, section in roles if section.casefold().startswith("design and planning")}
    if not historical or not plans:
        errors.append(f"{DOCS_MAP}: historical or design-and-planning section not found")
    for target, section in design_links:
        if target == "docs/archive" or target.startswith("docs/archive/") or target in historical:
            errors.append(
                f"{DESIGN_DOCUMENT}: cites historical or archived document {target} under "
                f"{section!r}; reach history through {DOCS_MAP} instead"
            )
        elif target in plans and section not in DESIGN_PLAN_SECTIONS:
            errors.append(
                f"{DESIGN_DOCUMENT}: design document {target} cited under {section!r}; "
                f"plans belong only under {DESIGN_PLAN_SECTIONS!r}"
            )
    return errors


def design_index_texts() -> tuple[str, str, str, str]:
    """Missing files read as empty so every rule reports what is absent instead of crashing."""
    def read(path: Path) -> str:
        return path.read_text() if path.is_file() else ""

    return (
        read(ROOT / AGENTS_DOCUMENT),
        read(ROOT / MANIFEST_DOCUMENT),
        read(ROOT / DESIGN_DOCUMENT),
        read(ROOT / DOCS_MAP),
    )


def design_index_self_test() -> list[str]:
    errors: list[str] = []
    if DESIGN_DOCUMENT not in CURRENT_DOCS:
        errors.append("root design index is not enrolled in CURRENT_DOCS")
    agents, manifest, design, docs_map = design_index_texts()
    if check_design_index(agents, manifest, design, docs_map):
        errors.append("current design index was rejected")
    table = manifest_doc_paths(manifest)
    if not table:
        return errors + ["subsystems.toml lists no docs; design index probes cannot run"]

    # AGENTS.md is what agents read; if it stops carrying the pointer (for example a real file
    # replacing the symlink), the gate must fail even though CLAUDE.md still links DESIGN.md.
    unlinked = check_design_index(
        re.sub(r"\(DESIGN\.md(?:#[^)]*)?\)", "(README.md)", agents), manifest, design, docs_map
    )
    if not any("must link the root design index" in error for error in unlinked):
        errors.append("AGENTS.md without the DESIGN.md pointer escaped")

    first_path = table[0]
    probes = (
        ("missing manifest docs path",
         (agents, manifest.replace(f'"{first_path}"', '"src/missing/GONE.md"', 1), design, docs_map),
         "docs entry does not exist: src/missing/GONE.md"),
        ("manifest doc not linked from DESIGN.md",
         (agents, manifest, design.replace(f"({first_path})", "(README.md)"), docs_map),
         f"does not link docs entry named in subsystems.toml: {first_path}"),
        ("manifest without docs",
         (agents, manifest.replace("docs = [", "references = ["), design, docs_map),
         "lists no subsystem docs entries"),
        ("unparseable manifest",
         (agents, manifest + "\n[[subsystem\n", design, docs_map),
         "subsystems.toml: is not valid TOML"),
        ("historical target",
         (agents, manifest, design.replace("## Composition\n", "## Composition\n\n[old](docs/ARCHITECTURE.md)\n", 1), docs_map),
         "cites historical or archived document docs/ARCHITECTURE.md"),
        ("archived target",
         (agents, manifest, design.replace("## Composition\n", "## Composition\n\n[old](docs/archive/x.md)\n", 1), docs_map),
         "cites historical or archived document docs/archive/x.md"),
        ("design document outside permitted sections",
         (agents, manifest, design.replace("## Subsystems\n", "## Subsystems\n\n[plan](docs/ROADMAP.md)\n", 1), docs_map),
         "design document docs/ROADMAP.md cited under 'Subsystems'"),
    )
    for label, texts, diagnostic in probes:
        found = check_design_index(*texts)
        if not any(diagnostic in error for error in found):
            errors.append(f"design index probe escaped ({label}); errors={found!r}")

    allowed = design.replace("## Open questions\n", "## Open questions\n\n[plan](docs/ROADMAP.md)\n", 1)
    if check_design_index(agents, manifest, allowed, docs_map):
        errors.append("a design document under Open questions was rejected")
    if not check_links(DESIGN_DOCUMENT, "[gone](docs/definitely-missing.md)"):
        errors.append("dead-link probe escaped the design index link gate")
    return errors


def self_test() -> int:
    """Exercise the important positive and negative controls for this gate."""
    errors: list[str] = design_index_self_test()
    if TRANSPORT_DOCUMENT not in CURRENT_DOCS:
        errors.append("native ChatGPT transport guide is not enrolled in CURRENT_DOCS")

    documents = {
        TRANSPORT_DOCUMENT: (ROOT / TRANSPORT_DOCUMENT).read_text(),
        OAUTH_DOCUMENT: (ROOT / OAUTH_DOCUMENT).read_text(),
    }
    for document, text in documents.items():
        errors.extend(check_truth_claims(document, text))

    for control in ("Finch supports GPT-5.6 Sol.", "GPT-5.6 is verified."):
        if not check_truth_claims(Path("docs/example.md"), control):
            errors.append(f"unsupported broad GPT-5.6 control escaped: {control!r}")

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
    errors.extend(check_design_index(*design_index_texts()))

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
