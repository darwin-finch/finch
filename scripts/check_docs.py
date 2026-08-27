#!/usr/bin/env python3
"""Bounded checks for Finch's current public documentation.

This intentionally checks only documents classified as current in docs/README.md.
Archived and design documents preserve historical claims and are outside this gate.
"""

from __future__ import annotations

import re
import subprocess
import sys
import tempfile
from pathlib import Path
from urllib.parse import unquote


ROOT = Path(__file__).resolve().parent.parent
CURRENT_DOCS = (
    Path("README.md"),
    Path("CONTRIBUTING.md"),
    Path("docs/README.md"),
    Path("docs/AUTOMATIC_TRAINING.md"),
    Path("docs/MCP_USER_GUIDE.md"),
    Path("docs/MACOS_GUI_AUTOMATION.md"),
    Path("docs/chatgpt-subscription-provider.md"),
)

# These are precise remnants of superseded public copy, rather than broad words
# that can legitimately appear in a limitation or migration note.
STALE_CLAIMS = {
    r"<100ms startup": "unverified startup metric",
    r"near-zero marginal cost": "unverified cost claim",
    r"Grok is the fastest free option": "unverified provider recommendation",
    r"with your permission before every action": "obsolete blanket approval claim",
    r"six model families are supported": "configuration mistaken for conformance",
    r"finch-macos-aarch64\.tar\.gz": "stale release artifact name",
    r"raw\.githubusercontent\.com/darwin-finch/finch/main/scripts/install\.sh": (
        "stale installer path"
    ),
    r"gpt-5\.6": "unverified model claim",
}

LINK_RE = re.compile(r"(?<!!)\[[^\]]*\]\(([^)]+)\)")
FENCE_RE = re.compile(r"^```(bash|sh)\s*$\n(.*?)^```\s*$", re.MULTILINE | re.DOTALL)
HEADING_RE = re.compile(r"^#{1,6}\s+(.+?)\s*#*\s*$", re.MULTILINE)


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
        if raw.startswith(("http://", "https://", "mailto:")):
            continue
        link_path, fragment = split_link(raw)
        if not link_path and not fragment:
            errors.append(f"{document}: unsupported local link syntax: {raw}")
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

    for pattern, description in STALE_CLAIMS.items():
        if re.search(pattern, combined, flags=re.IGNORECASE):
            errors.append(f"current docs contain {description} ({pattern})")

    if errors:
        for error in errors:
            print(f"docs check: {error}", file=sys.stderr)
        return 1

    print(f"docs check: {len(CURRENT_DOCS)} current documents passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
