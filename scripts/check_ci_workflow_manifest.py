#!/usr/bin/env python3
"""Verify Finch's reviewed GitHub Actions documents and CI fixture inventories."""

from __future__ import annotations

import argparse
import hashlib
import json
import stat
import sys
from pathlib import Path
from typing import Any

try:
    import yaml
except ImportError as error:  # pragma: no cover - exercised by workflow bootstrap
    raise SystemExit(
        "CI workflow manifest: PyYAML is required "
        "(python3 -m pip install PyYAML==6.0.3)"
    ) from error


ROOT = Path(__file__).resolve().parent.parent
WORKFLOW_DIRECTORY = Path(".github/workflows")
MANIFEST_PATH = Path("scripts/ci_workflow_manifest.json")
SCHEMA = "finch-ci-workflow-manifest:v1"


class ContractError(Exception):
    """Actionable failure in the reviewed workflow contract."""


class UniqueBaseLoader(yaml.BaseLoader):
    """String-preserving YAML loader that rejects duplicate mapping keys."""


def construct_unique_mapping(
    loader: UniqueBaseLoader, node: yaml.MappingNode, deep: bool = False
) -> dict[str, Any]:
    mapping: dict[str, Any] = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if not isinstance(key, str):
            raise yaml.constructor.ConstructorError(
                "while constructing a mapping",
                node.start_mark,
                "workflow mapping keys must be scalar strings",
                key_node.start_mark,
            )
        if key in mapping:
            raise yaml.constructor.ConstructorError(
                "while constructing a mapping",
                node.start_mark,
                f"duplicate mapping key {key!r}",
                key_node.start_mark,
            )
        mapping[key] = loader.construct_object(value_node, deep=deep)
    return mapping


UniqueBaseLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, construct_unique_mapping
)


# These inventories are deliberately reviewed evidence, not inferred by an
# incomplete GitHub Actions evaluator. A workflow allocation change updates
# these constants and the manifest only after its real check-run set is reviewed.
CANONICAL_CHECKS = (
    "Build Release (aarch64-apple-darwin)",
    "Build Release (x86_64-unknown-linux-gnu)",
    "Runtime Authority (Ubuntu)",
    "Security Audit",
    "Test (macos-14, default)",
    "Test (macos-14, no-default-features)",
    "Test (ubuntu-24.04, default)",
    "Test (ubuntu-24.04, no-default-features)",
    "Toolchain and formatting contract",
    "Toolchain and formatting contract (Windows)",
)
HYGIENE_CHECKS = ("Tracked tree (macos-14)", "Tracked tree (ubuntu-24.04)")
BRAIN_CHECKS = (
    "Isolation boundaries (macos-14)",
    "Isolation boundaries (ubuntu-24.04)",
)
OAUTH_CHECKS = ("macos-oauth", "oauth", "windows-compile")
CHATGPT_AUTH_CHECKS = (
    "focused-auth (macos-14)",
    "focused-auth (ubuntu-24.04)",
    "windows-verifier-compile",
)


def checks(*groups: tuple[str, ...]) -> tuple[str, ...]:
    return tuple(sorted(item for group in groups for item in group))


EXPECTED_FIXTURES: dict[str, dict[str, Any]] = {
    "readme_only": {
        "changed_paths": ["README.md"],
        "expected_checks": list(
            checks(
                CANONICAL_CHECKS,
                HYGIENE_CHECKS,
                ("Current docs links, claims, and shell syntax",),
            )
        ),
        "expected_count": 13,
    },
    "ordinary_source": {
        "changed_paths": ["src/models/mod.rs"],
        "expected_checks": list(checks(CANONICAL_CHECKS, HYGIENE_CHECKS, BRAIN_CHECKS)),
        "expected_count": 14,
    },
    "brain_effect": {
        "changed_paths": ["src/brain/store.rs", "src/server/handlers.rs"],
        "expected_checks": list(
            checks(CANONICAL_CHECKS, HYGIENE_CHECKS, BRAIN_CHECKS, ("effect-audit",))
        ),
        "expected_count": 15,
    },
    "manifest_dependency": {
        "changed_paths": ["Cargo.toml", "Cargo.lock"],
        "expected_checks": list(
            checks(
                CANONICAL_CHECKS,
                HYGIENE_CHECKS,
                BRAIN_CHECKS,
                OAUTH_CHECKS,
                CHATGPT_AUTH_CHECKS,
            )
        ),
        "expected_count": 20,
    },
    "public_api": {
        "changed_paths": ["src/lib.rs"],
        "expected_checks": list(
            checks(CANONICAL_CHECKS, HYGIENE_CHECKS, BRAIN_CHECKS, OAUTH_CHECKS)
        ),
        "expected_count": 17,
    },
}

EXPECTED_PR_ACTIVE_WORKFLOWS = [
    "ci.yml",
    "docs.yml",
    "issue-105-oauth.yml",
    "issue-163-effect-audit.yml",
    "issue-187-subagent-fanout.yml",
    "issue-201-chatgpt-auth.yml",
    "issue-227-setup-preservation.yml",
    "issue-245-cargo-slot.yml",
    "issue-46-atomic-conversation.yml",
    "issue-56-brain-isolation.yml",
    "repository-hygiene.yml",
]

EXPECTED_FIXTURE_MEMBERSHIP = {
    "ci.yml": sorted(EXPECTED_FIXTURES),
    "docs.yml": ["readme_only"],
    "issue-104-chooser-catalog.yml": [],
    "issue-105-oauth.yml": ["manifest_dependency", "public_api"],
    "issue-163-effect-audit.yml": ["brain_effect"],
    "issue-187-subagent-fanout.yml": [],
    "issue-201-chatgpt-auth.yml": ["manifest_dependency"],
    "issue-227-setup-preservation.yml": [],
    "issue-245-cargo-slot.yml": [],
    "issue-46-atomic-conversation.yml": [],
    "issue-56-brain-isolation.yml": [
        "brain_effect",
        "manifest_dependency",
        "ordinary_source",
        "public_api",
    ],
    "issue-72-capability-contract.yml": [],
    "release.yml": [],
    "repository-hygiene.yml": sorted(EXPECTED_FIXTURES),
}


def json_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ContractError(f"manifest contains duplicate JSON key {key!r}")
        result[key] = value
    return result


def load_yaml_document(path: Path, display: str) -> Any:
    try:
        documents = list(yaml.load_all(path.read_text(encoding="utf-8"), Loader=UniqueBaseLoader))
    except (OSError, UnicodeError, yaml.YAMLError) as error:
        raise ContractError(f"{display}: workflow YAML could not be loaded: {error}") from error
    if len(documents) != 1 or not isinstance(documents[0], dict):
        raise ContractError(
            f"{display}: expected exactly one complete workflow mapping, found {len(documents)} document(s)"
        )
    return documents[0]


def semantic_digest(document: Any) -> str:
    canonical = json.dumps(
        document, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    return hashlib.sha256(canonical).hexdigest()


def workflow_records(root: Path) -> dict[str, dict[str, Any]]:
    directory = root / WORKFLOW_DIRECTORY
    paths = sorted((*directory.glob("*.yml"), *directory.glob("*.yaml")))
    if not paths:
        raise ContractError(f"{WORKFLOW_DIRECTORY}: no workflow documents were found")
    records: dict[str, dict[str, Any]] = {}
    for path in paths:
        relative = path.relative_to(root).as_posix()
        try:
            mode = path.lstat().st_mode
        except OSError as error:
            raise ContractError(
                f"{relative}: workflow file metadata could not be read: {error}"
            ) from error
        if stat.S_ISLNK(mode):
            raise ContractError(
                f"{relative}: workflow document must be a regular file, not a symbolic link"
            )
        if not stat.S_ISREG(mode):
            raise ContractError(
                f"{relative}: workflow document must be a regular file; "
                f"found mode {stat.filemode(mode)!r}"
            )
        records[path.name] = {
            "digest": semantic_digest(load_yaml_document(path, relative)),
            "activated_fixtures": EXPECTED_FIXTURE_MEMBERSHIP.get(path.name),
        }
    return records


def load_manifest(root: Path) -> dict[str, Any]:
    path = root / MANIFEST_PATH
    try:
        manifest = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=json_object)
    except (OSError, UnicodeError, json.JSONDecodeError, ContractError) as error:
        raise ContractError(f"{MANIFEST_PATH}: reviewed manifest could not be loaded: {error}") from error
    if not isinstance(manifest, dict):
        raise ContractError(f"{MANIFEST_PATH}: manifest root must be an object")
    return manifest


def compare_contract(root: Path) -> list[str]:
    errors: list[str] = []
    try:
        actual = workflow_records(root)
        manifest = load_manifest(root)
    except ContractError as error:
        return [str(error)]

    expected_root_keys = {"schema", "pr_active_workflows", "workflows", "fixtures"}
    if set(manifest) != expected_root_keys:
        errors.append(
            f"{MANIFEST_PATH}: root keys changed; expected={sorted(expected_root_keys)!r} "
            f"actual={sorted(manifest)!r}"
        )
    if manifest.get("schema") != SCHEMA:
        errors.append(
            f"{MANIFEST_PATH}: schema must be {SCHEMA!r}, found {manifest.get('schema')!r}"
        )

    reviewed = manifest.get("workflows")
    if not isinstance(reviewed, dict):
        return errors + [f"{MANIFEST_PATH}: 'workflows' must be a filename-to-record object"]

    actual_names = set(actual)
    expected_names = set(reviewed)
    for name in sorted(expected_names - actual_names):
        errors.append(f"{WORKFLOW_DIRECTORY / name}: reviewed workflow document is missing")
    for name in sorted(actual_names - expected_names):
        errors.append(
            f"{WORKFLOW_DIRECTORY / name}: unreviewed workflow document was added; review its actual GitHub allocation before updating {MANIFEST_PATH}"
        )
    for name in sorted(actual_names & expected_names):
        record = reviewed[name]
        if not isinstance(record, dict):
            errors.append(f"{MANIFEST_PATH}: workflow record {name!r} must be an object")
            continue
        expected_record_keys = {"digest", "activated_fixtures"}
        if set(record) != expected_record_keys:
            errors.append(
                f"{MANIFEST_PATH}: workflow record {name!r} keys changed; "
                f"expected={sorted(expected_record_keys)!r} actual={sorted(record)!r}"
            )
        expected_digest = record.get("digest")
        actual_digest = actual[name]["digest"]
        if not isinstance(expected_digest, str) or len(expected_digest) != 64 or any(
            character not in "0123456789abcdef" for character in expected_digest
        ):
            errors.append(
                f"{MANIFEST_PATH}: workflow {name!r} has invalid SHA-256 digest {expected_digest!r}"
            )
        if expected_digest != actual_digest:
            errors.append(
                f"{WORKFLOW_DIRECTORY / name}: reviewed semantic digest changed; "
                f"expected={expected_digest!r} actual={actual_digest!r}; review actual GitHub allocation before updating {MANIFEST_PATH}"
            )
        expected_membership = EXPECTED_FIXTURE_MEMBERSHIP.get(name)
        if record.get("activated_fixtures") != expected_membership:
            errors.append(
                f"{MANIFEST_PATH}: {name} fixture inventory changed; "
                f"expected={expected_membership!r} actual={record.get('activated_fixtures')!r}"
            )

    pr_active = manifest.get("pr_active_workflows")
    if pr_active != EXPECTED_PR_ACTIVE_WORKFLOWS:
        errors.append(
            f"{MANIFEST_PATH}: PR-active workflow inventory changed; "
            f"expected={EXPECTED_PR_ACTIVE_WORKFLOWS!r} actual={pr_active!r}"
        )

    fixtures = manifest.get("fixtures")
    if not isinstance(fixtures, dict):
        errors.append(f"{MANIFEST_PATH}: 'fixtures' must be an object")
    else:
        for name in sorted(set(EXPECTED_FIXTURES) | set(fixtures)):
            expected = EXPECTED_FIXTURES.get(name)
            actual_fixture = fixtures.get(name)
            if expected != actual_fixture:
                errors.append(
                    f"{MANIFEST_PATH}: representative fixture {name!r} changed; "
                    f"expected={expected!r} actual={actual_fixture!r}"
                )

    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--print-digests", action="store_true")
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    if arguments.print_digests:
        try:
            records = workflow_records(root)
        except ContractError as error:
            print(f"CI workflow manifest: {error}", file=sys.stderr)
            return 1
        print(json.dumps({name: record["digest"] for name, record in records.items()}, indent=2))
        return 0

    errors = compare_contract(root)
    if errors:
        for error in errors:
            print(f"CI workflow manifest: {error}", file=sys.stderr)
        return 1
    print(
        "CI workflow manifest: reviewed documents and representative fixture inventories passed"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
