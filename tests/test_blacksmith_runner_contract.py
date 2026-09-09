#!/usr/bin/env python3
"""Contract tests for the canonical CI Blacksmith runner pilot."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github/workflows/ci.yml"
TOOLCHAIN_CONTRACT = ROOT / "tests/toolchain_contract.sh"
WORKFLOW_MANIFEST = ROOT / "scripts/ci_workflow_manifest.json"
BLACKSMITH_RUNNER = "blacksmith-2vcpu-ubuntu-2404"
FEATURES = ("default", "no-default-features")
FINAL_LINUX_NAMES = {
    feature: f"Test (ubuntu-24.04, {feature})" for feature in FEATURES
}
AB_LINUX_NAMES = {
    ("github", feature): f"Test A/B GitHub (ubuntu-24.04, {feature})"
    for feature in FEATURES
} | {
    ("blacksmith", feature): f"Test A/B Blacksmith (ubuntu-24.04, {feature})"
    for feature in FEATURES
}
EXPECTED_CONCURRENCY_GROUP = (
    "ci-${{ github.event_name == 'pull_request' && format('pr-{0}', "
    "github.event.pull_request.number) || format('push-{0}-{1}', github.ref, "
    "github.run_id) }}"
)


class ContractError(Exception):
    """An actionable violation of the Blacksmith pilot contract."""


def job_blocks(text: str) -> dict[str, str]:
    jobs_match = re.search(r"(?m)^jobs:\s*$", text)
    if jobs_match is None:
        raise ContractError("canonical CI workflow has no top-level jobs mapping")
    lines = text[jobs_match.end() :].splitlines(keepends=True)
    starts: list[tuple[str, int]] = []
    for index, line in enumerate(lines):
        match = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if match is not None:
            starts.append((match.group(1), index))
    blocks: dict[str, str] = {}
    for position, (name, start) in enumerate(starts):
        end = starts[position + 1][1] if position + 1 < len(starts) else len(lines)
        if name in blocks:
            raise ContractError(f"canonical CI workflow repeats job id {name!r}")
        blocks[name] = "".join(lines[start:end])
    return blocks


def matrix_entries(job: str) -> list[dict[str, str]]:
    matrix_match = re.search(
        r"(?ms)^      matrix:\s*\n        include:\s*\n(?P<body>.*?)(?=^    steps:\s*$)",
        job,
    )
    if matrix_match is None:
        raise ContractError(
            "canonical test job must use an explicit matrix include list; "
            f"job_text={job!r}"
        )
    entries: list[dict[str, str]] = []
    current: dict[str, str] | None = None
    for line in matrix_match.group("body").splitlines():
        start = re.match(r"^          - ([a-z_]+):\s*(.*)$", line)
        field = re.match(r"^            ([a-z_]+):\s*(.*)$", line)
        if start is not None:
            if current is not None:
                entries.append(current)
            current = {start.group(1): unquote(start.group(2))}
        elif field is not None and current is not None:
            key = field.group(1)
            if key in current:
                raise ContractError(
                    f"test matrix entry repeats field {key!r}: entry={current!r}"
                )
            current[key] = unquote(field.group(2))
        elif line.lstrip().startswith("#"):
            continue
        elif line.strip():
            raise ContractError(
                "test matrix contains an unparsed line; keep every cell explicit: "
                f"line={line!r} matrix_text={matrix_match.group('body')!r}"
            )
    if current is not None:
        entries.append(current)
    return entries


def unquote(value: str) -> str:
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        return value[1:-1]
    return value


def scalar(job: str, field: str) -> str:
    matches = re.findall(rf"(?m)^    {re.escape(field)}:\s*(.*?)\s*$", job)
    if len(matches) != 1:
        raise ContractError(
            f"job must declare exactly one {field!r} scalar: "
            f"matches={matches!r} job_text={job!r}"
        )
    return unquote(matches[0])


def validate_concurrency(text: str) -> None:
    before_jobs = text.split("\njobs:\n", 1)[0]
    match = re.search(
        r"(?ms)^concurrency:\s*\n"
        r"  group:\s*(?P<group>.*?)\s*\n"
        r"  cancel-in-progress:\s*(?P<cancel>.*?)\s*$",
        before_jobs,
    )
    if match is None:
        raise ContractError(
            "canonical CI must declare top-level concurrency with adjacent group and "
            f"cancel-in-progress fields: pre_jobs_text={before_jobs!r}"
        )
    group = match.group("group")
    cancel = match.group("cancel")
    if group != EXPECTED_CONCURRENCY_GROUP:
        raise ContractError(
            "concurrency group must use only the PR number on pull requests, but the "
            "ref plus run ID on pushes; this collapses same-PR runs without making PRs "
            "unique or collapsing independent pushes: "
            f"expected={EXPECTED_CONCURRENCY_GROUP!r} parsed={group!r}"
        )
    expected_cancel = "${{ github.event_name == 'pull_request' }}"
    if cancel != expected_cancel:
        raise ContractError(
            "cancel-in-progress must be enabled only for pull_request events so push "
            f"runs are never cancelled: expected={expected_cancel!r} parsed={cancel!r}"
        )


def validate_toolchain_wiring(text: str) -> None:
    invocation = "python3 tests/test_blacksmith_runner_contract.py"
    matches = [line for line in text.splitlines() if line.strip() == invocation]
    if len(matches) != 1:
        raise ContractError(
            "tests/toolchain_contract.sh must invoke the Blacksmith production-boundary "
            "contract exactly once: "
            f"expected_invocation={invocation!r} count={len(matches)} script={text!r}"
        )
    guard_fragments = (
        "blacksmith_contract_count=$(grep -Fxc",
        'if [[ "$blacksmith_contract_count" -ne 1 ]]; then',
        "must invoke '$blacksmith_contract_invocation' exactly once",
    )
    missing = [fragment for fragment in guard_fragments if text.count(fragment) != 1]
    if missing:
        raise ContractError(
            "tests/toolchain_contract.sh must independently guard its Blacksmith "
            "contract invocation: "
            f"missing_or_repeated={missing!r} script={text!r}"
        )


def validate_test_job(job: str) -> str:
    if scalar(job, "name") != "${{ matrix.check_name }}":
        raise ContractError(
            "canonical test check names must come from explicit matrix.check_name values: "
            f"parsed_name={scalar(job, 'name')!r}"
        )
    if scalar(job, "runs-on") != "${{ matrix.runner }}":
        raise ContractError(
            "canonical test runner must come from explicit matrix.runner values: "
            f"parsed_runs_on={scalar(job, 'runs-on')!r}"
        )

    entries = matrix_entries(job)
    required_fields = {
        "runner",
        "platform",
        "provider",
        "feature_name",
        "cargo_args",
        "check_name",
    }
    for entry in entries:
        if set(entry) != required_fields:
            raise ContractError(
                "every test matrix cell must explicitly name runner, platform, provider, "
                "feature, cargo args, and visible check name: "
                f"required={sorted(required_fields)!r} entry={entry!r} entries={entries!r}"
            )

    mac_entries = [entry for entry in entries if entry["platform"] == "macos-14"]
    expected_mac = {
        (
            "macos-14",
            "github",
            feature,
            "" if feature == "default" else "--no-default-features",
            f"Test (macos-14, {feature})",
        )
        for feature in FEATURES
    }
    actual_mac = {
        (
            entry["runner"],
            entry["provider"],
            entry["feature_name"],
            entry["cargo_args"],
            entry["check_name"],
        )
        for entry in mac_entries
    }
    if len(mac_entries) != 2 or actual_mac != expected_mac:
        raise ContractError(
            "the two macOS feature cells and their stable visible names must remain "
            "exactly once on native macos-14: "
            f"expected={sorted(expected_mac)!r} actual={sorted(actual_mac)!r} "
            f"raw_entries={mac_entries!r}"
        )

    linux_entries = [entry for entry in entries if entry["platform"] == "ubuntu-24.04"]
    unexpected_platforms = sorted(
        {entry["platform"] for entry in entries}
        - {"ubuntu-24.04", "macos-14"}
    )
    if unexpected_platforms:
        raise ContractError(
            "canonical test matrix contains unsupported platforms: "
            f"unexpected={unexpected_platforms!r} entries={entries!r}"
        )
    providers = {entry["provider"] for entry in linux_entries}
    if providers == {"github", "blacksmith"}:
        mode = "temporary-ab"
        expected_names = AB_LINUX_NAMES
    elif providers == {"blacksmith"}:
        mode = "final"
        expected_names = {
            ("blacksmith", feature): FINAL_LINUX_NAMES[feature] for feature in FEATURES
        }
    else:
        raise ContractError(
            "Linux test cells must be either the explicit GitHub/Blacksmith A/B pilot "
            "or the final Blacksmith-only pair: "
            f"providers={sorted(providers)!r} entries={linux_entries!r}"
        )

    expected_linux = {
        (
            "ubuntu-24.04" if provider == "github" else BLACKSMITH_RUNNER,
            provider,
            feature,
            "" if feature == "default" else "--no-default-features",
            expected_names[(provider, feature)],
        )
        for provider in providers
        for feature in FEATURES
    }
    actual_linux = [
        (
            entry["runner"],
            entry["provider"],
            entry["feature_name"],
            entry["cargo_args"],
            entry["check_name"],
        )
        for entry in linux_entries
    ]
    if len(actual_linux) != len(expected_linux) or set(actual_linux) != expected_linux:
        raise ContractError(
            "Linux feature/provider cells must exist exactly once with the reviewed x64 "
            "runner label, Cargo arguments, and visible names: "
            f"mode={mode!r} expected={sorted(expected_linux)!r} "
            f"actual={sorted(actual_linux)!r} raw_entries={linux_entries!r}"
        )
    return mode


def validate_native_jobs(blocks: dict[str, str]) -> None:
    windows = blocks.get("windows-format-contract")
    build = blocks.get("build")
    if windows is None or build is None:
        raise ContractError(
            "canonical CI must retain Windows formatting and release build jobs: "
            f"job_ids={sorted(blocks)!r}"
        )
    windows_state = (scalar(windows, "name"), scalar(windows, "runs-on"))
    expected_windows = ("Toolchain and formatting contract (Windows)", "windows-2025")
    if windows_state != expected_windows:
        raise ContractError(
            "Windows formatting must remain an unchanged native windows-2025 check: "
            f"expected={expected_windows!r} actual={windows_state!r}"
        )
    if scalar(build, "name") != "Build Release (${{ matrix.target }})":
        raise ContractError(
            "release check name must remain stable: "
            f"parsed_name={scalar(build, 'name')!r}"
        )
    if scalar(build, "runs-on") != "${{ matrix.os }}":
        raise ContractError(
            "release runner selection must remain on matrix.os: "
            f"parsed_runs_on={scalar(build, 'runs-on')!r}"
        )
    release_entries = matrix_entries(build)
    expected_release = {
        ("ubuntu-24.04", "x86_64-unknown-linux-gnu"),
        ("macos-14", "aarch64-apple-darwin"),
    }
    actual_release = [
        (entry.get("os", "<missing>"), entry.get("target", "<missing>"))
        for entry in release_entries
    ]
    if len(actual_release) != 2 or set(actual_release) != expected_release:
        raise ContractError(
            "release jobs must remain exactly once on native GitHub Linux and macOS: "
            f"expected={sorted(expected_release)!r} actual={sorted(actual_release)!r} "
            f"raw_entries={release_entries!r}"
        )


def validate_workflow(text: str) -> str:
    validate_concurrency(text)
    blocks = job_blocks(text)
    test = blocks.get("test")
    if test is None:
        raise ContractError(
            f"canonical CI workflow is missing test job; job_ids={sorted(blocks)!r}"
        )
    mode = validate_test_job(test)
    validate_native_jobs(blocks)
    blacksmith_jobs = [
        name for name, block in blocks.items() if BLACKSMITH_RUNNER in block
    ]
    if blacksmith_jobs != ["test"]:
        raise ContractError(
            "Blacksmith runner label is allowed only in the canonical test job: "
            f"jobs_with_label={blacksmith_jobs!r}"
        )
    return mode


def replace_once(text: str, old: str, new: str) -> str:
    count = text.count(old)
    if count != 1:
        raise AssertionError(
            f"mutation fixture expected one occurrence: old={old!r} count={count}"
        )
    return text.replace(old, new, 1)


def linux_cell(runner: str, provider: str, feature: str, check_name: str) -> str:
    cargo_args = '""' if feature == "default" else "--no-default-features"
    return (
        f"          - runner: {runner}\n"
        "            platform: ubuntu-24.04\n"
        f"            provider: {provider}\n"
        f"            feature_name: {feature}\n"
        f"            cargo_args: {cargo_args}\n"
        f"            check_name: {check_name}\n"
    )


def as_final(text: str) -> str:
    mode = validate_workflow(text)
    if mode == "final":
        return text
    for feature in FEATURES:
        text = replace_once(
            text,
            linux_cell(
                "ubuntu-24.04", "github", feature, AB_LINUX_NAMES[("github", feature)]
            ),
            "",
        )
        text = replace_once(
            text,
            f"            check_name: {AB_LINUX_NAMES[('blacksmith', feature)]}",
            f"            check_name: {FINAL_LINUX_NAMES[feature]}",
        )
    if validate_workflow(text) != "final":
        raise AssertionError("A/B-to-final test fixture conversion did not reach final mode")
    return text


def as_ab(text: str) -> str:
    mode = validate_workflow(text)
    if mode == "temporary-ab":
        return text
    for feature in FEATURES:
        final_cell = linux_cell(
            BLACKSMITH_RUNNER,
            "blacksmith",
            feature,
            FINAL_LINUX_NAMES[feature],
        )
        ab_cells = linux_cell(
            "ubuntu-24.04",
            "github",
            feature,
            AB_LINUX_NAMES[("github", feature)],
        ) + linux_cell(
            BLACKSMITH_RUNNER,
            "blacksmith",
            feature,
            AB_LINUX_NAMES[("blacksmith", feature)],
        )
        text = replace_once(text, final_cell, ab_cells)
    if validate_workflow(text) != "temporary-ab":
        raise AssertionError("final-to-A/B test fixture conversion did not reach A/B mode")
    return text


class BlacksmithRunnerContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.workflow = WORKFLOW.read_text(encoding="utf-8")
        cls.toolchain_contract = TOOLCHAIN_CONTRACT.read_text(encoding="utf-8")
        cls.manifest = json.loads(WORKFLOW_MANIFEST.read_text(encoding="utf-8"))

    def assert_rejected(self, text: str, diagnostic: str) -> None:
        with self.assertRaises(
            ContractError,
            msg=f"mutated workflow unexpectedly passed: expected_diagnostic={diagnostic!r}",
        ) as raised:
            validate_workflow(text)
        self.assertIn(
            diagnostic,
            str(raised.exception),
            "contract rejection omitted the actionable invariant: "
            f"expected={diagnostic!r} actual={str(raised.exception)!r}",
        )

    def test_repository_workflow_is_reviewed_ab_or_final_shape(self) -> None:
        mode = validate_workflow(self.workflow)
        self.assertIn(
            mode,
            {"temporary-ab", "final"},
            f"validator returned an unknown migration mode: mode={mode!r}",
        )

    def test_toolchain_contract_invokes_runner_contract_exactly_once(self) -> None:
        validate_toolchain_wiring(self.toolchain_contract)
        invocation = "python3 tests/test_blacksmith_runner_contract.py\n"
        without_invocation = replace_once(self.toolchain_contract, invocation, "")
        with self.assertRaisesRegex(
            ContractError,
            "production-boundary contract exactly once",
            msg=(
                "removing the Blacksmith contract invocation from the CI-wired shell "
                "boundary must be rejected"
            ),
        ):
            validate_toolchain_wiring(without_invocation)
        with tempfile.NamedTemporaryFile("w", suffix=".sh") as mutated_script:
            mutated_script.write(without_invocation)
            mutated_script.flush()
            result = subprocess.run(
                ["bash", mutated_script.name],
                cwd=ROOT,
                check=False,
                capture_output=True,
                text=True,
                timeout=5,
            )
        self.assertEqual(
            result.returncode,
            1,
            "shell guard must reject a removed Python contract invocation before any "
            "later toolchain work: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}",
        )
        self.assertIn(
            "must invoke 'python3 tests/test_blacksmith_runner_contract.py' exactly once; "
            "found 0",
            result.stderr,
            "shell guard rejection must name the missing production-boundary command: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}",
        )

    def test_manifest_fixtures_keep_final_stable_linux_names_and_counts(self) -> None:
        fixtures = self.manifest.get("fixtures")
        self.assertIsInstance(
            fixtures,
            dict,
            f"workflow manifest must contain a fixtures mapping: manifest={self.manifest!r}",
        )
        ci_record = self.manifest.get("workflows", {}).get("ci.yml", {})
        self.assertEqual(
            set(ci_record.get("activated_fixtures", [])),
            set(fixtures),
            "manifest stable-name checks assume every fixture activates canonical CI: "
            f"ci_record={ci_record!r} fixture_names={sorted(fixtures)!r}",
        )
        stable_names = set(FINAL_LINUX_NAMES.values())
        for fixture_name, fixture in fixtures.items():
            with self.subTest(fixture=fixture_name):
                checks = fixture.get("expected_checks", [])
                self.assertEqual(
                    fixture.get("expected_count"),
                    len(checks),
                    "manifest fixture count must equal its visible check inventory: "
                    f"fixture={fixture_name!r} declared={fixture.get('expected_count')!r} "
                    f"actual={len(checks)} checks={checks!r}",
                )
                self.assertEqual(
                    {name for name in checks if name in stable_names},
                    stable_names,
                    "every canonical-CI fixture must retain both stable Linux names: "
                    f"fixture={fixture_name!r} stable={sorted(stable_names)!r} "
                    f"checks={checks!r}",
                )
                self.assertFalse(
                    any(name.startswith("Test A/B ") for name in checks),
                    "final manifest inventory must not retain temporary A/B names: "
                    f"fixture={fixture_name!r} checks={checks!r}",
                )

    def test_runner_label_mutations_are_rejected(self) -> None:
        for replacement in (
            "blacksmth-2vcpu-ubuntu-2404",
            "blacksmith-2vcpu-ubuntu-2404-arm",
        ):
            with self.subTest(replacement=replacement):
                mutated = self.workflow.replace(BLACKSMITH_RUNNER, replacement)
                self.assert_rejected(mutated, "reviewed x64 runner label")

    def test_absent_or_duplicate_linux_feature_provider_cells_are_rejected(self) -> None:
        fixtures = (("temporary-ab", as_ab(self.workflow)), ("final", as_final(self.workflow)))
        for mode, fixture in fixtures:
            with self.subTest(mode=mode):
                name = (
                    AB_LINUX_NAMES[("blacksmith", "default")]
                    if mode == "temporary-ab"
                    else FINAL_LINUX_NAMES["default"]
                )
                cell = linux_cell(BLACKSMITH_RUNNER, "blacksmith", "default", name)
                self.assert_rejected(replace_once(fixture, cell, ""), "exactly once")
                self.assert_rejected(
                    replace_once(fixture, cell, cell + cell), "exactly once"
                )

    def test_macos_windows_and_release_runner_migrations_are_rejected(self) -> None:
        mutations = (
            (
                replace_once(
                    self.workflow,
                    "          - runner: macos-14\n"
                    "            platform: macos-14\n"
                    "            provider: github\n"
                    "            feature_name: default\n",
                    "          - runner: blacksmith-4vcpu-macos-14\n"
                    "            platform: macos-14\n"
                    "            provider: github\n"
                    "            feature_name: default\n",
                ),
                "macOS feature cells",
            ),
            (
                replace_once(
                    self.workflow,
                    "    runs-on: windows-2025\n",
                    "    runs-on: blacksmith-2vcpu-windows-2025\n",
                ),
                "Windows formatting",
            ),
            (
                replace_once(
                    self.workflow,
                    "          - os: ubuntu-24.04\n            target: x86_64-unknown-linux-gnu\n",
                    "          - os: blacksmith-2vcpu-ubuntu-2404\n"
                    "            target: x86_64-unknown-linux-gnu\n",
                ),
                "release jobs",
            ),
        )
        for mutated, diagnostic in mutations:
            with self.subTest(diagnostic=diagnostic):
                self.assert_rejected(mutated, diagnostic)

    def test_pr_cancellation_removal_or_push_cancellation_is_rejected(self) -> None:
        cancellation = "  cancel-in-progress: ${{ github.event_name == 'pull_request' }}"
        self.assert_rejected(
            replace_once(self.workflow, cancellation + "\n", ""),
            "top-level concurrency",
        )
        self.assert_rejected(
            replace_once(self.workflow, cancellation, "  cancel-in-progress: true"),
            "enabled only for pull_request",
        )

    def test_concurrency_group_keeps_pr_key_and_independent_push_identity(self) -> None:
        broken_groups = (
            (
                "ci-${{ github.event_name == 'pull_request' && "
                "format('pr-{0}-{1}', github.event.pull_request.number, github.run_id) "
                "|| format('push-{0}-{1}', github.ref, github.run_id) }}"
            ),
            (
                "ci-${{ github.event_name == 'pull_request' && format('pr-{0}', "
                "github.event.pull_request.number) || 'push' }}"
            ),
        )
        for broken_group in broken_groups:
            with self.subTest(group=broken_group):
                self.assert_rejected(
                    replace_once(self.workflow, EXPECTED_CONCURRENCY_GROUP, broken_group),
                    "only the PR number",
                )

    def test_temporary_ab_mode_preserves_provider_qualified_names(self) -> None:
        ab = as_ab(self.workflow)
        self.assertEqual(
            validate_workflow(ab),
            "temporary-ab",
            "synthetic A/B fixture must be valid before testing visible-name drift",
        )
        changed = replace_once(
            ab,
            f"            check_name: {AB_LINUX_NAMES[('github', 'default')]}",
            "            check_name: Test A/B Native Linux default",
        )
        self.assert_rejected(changed, "visible names")

    def test_final_mode_requires_original_stable_linux_check_names(self) -> None:
        final = as_final(self.workflow)
        self.assertEqual(
            validate_workflow(final),
            "final",
            "synthetic final mode must be accepted before testing stable-name drift",
        )
        changed = replace_once(
            final,
            "            check_name: Test (ubuntu-24.04, default)",
            "            check_name: Linux default",
        )
        self.assert_rejected(changed, "visible names")


def revision_text(revision: str, path: str) -> str:
    result = subprocess.run(
        ["git", "show", f"{revision}:{path}"],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=10,
    )
    if result.returncode != 0:
        raise ContractError(
            "could not read required contract file at requested revision: "
            f"revision={revision!r} path={path!r} stdout={result.stdout!r} "
            f"stderr={result.stderr!r}"
        )
    return result.stdout


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--revision",
        help="validate the committed canonical workflow at this exact git revision",
    )
    arguments, unittest_arguments = parser.parse_known_args()
    if arguments.revision is None:
        unittest.main(argv=[sys.argv[0], *unittest_arguments])
        return 0
    try:
        mode = validate_workflow(
            revision_text(arguments.revision, ".github/workflows/ci.yml")
        )
        validate_toolchain_wiring(
            revision_text(arguments.revision, "tests/toolchain_contract.sh")
        )
    except ContractError as error:
        print(f"Blacksmith runner contract failed: {error}", file=sys.stderr)
        return 1
    print(
        "Blacksmith runner contract passed: "
        f"revision={arguments.revision} mode={mode}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
