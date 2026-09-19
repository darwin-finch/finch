#!/usr/bin/env python3
"""Check the reviewed semantic shape of Finch pull-request workflows."""

from __future__ import annotations

import argparse
import itertools
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = Path(".github/workflows")

EXPECTED_WORKFLOWS = (
    "ci-main-breakage.yml",
    "ci-superseded-run-cancellation.yml",
    "ci.yml", "docs.yml", "issue-201-chatgpt-auth.yml",
    "issue-56-brain-isolation.yml", "release.yml", "repository-hygiene.yml",
)

# Trusted default-branch controller for canonical CI supersession. It is not
# pull-request active; membership is the empty fixture set.
CANCELLATION_WORKFLOW = "ci-superseded-run-cancellation.yml"
CANCELLATION_JOB = "cancel-superseded"
CANCELLATION_JOB_IF = (
    "github.event.action == 'requested' || github.event.workflow_run.run_attempt > 1"
)
CANCELLATION_PERMISSIONS = {"actions": "write", "pull-requests": "read"}
CANCELLATION_WORKFLOW_RUN = {
    "workflows": ["CI"],
    "types": ["requested", "in_progress"],
}
CANCELLATION_STEP = "Cancel superseded canonical CI runs"
CANCELLATION_TOKEN = "${{ github.token }}"

# Trusted default-branch controller that keeps one open "CI failed on main"
# issue as long as main-branch CI is red. Main-only jobs (#848) make a red
# main push the only detection point for the release preflight, the
# release-mode atomic history regression, and the macOS suite.
BREAKAGE_WORKFLOW = "ci-main-breakage.yml"
BREAKAGE_JOB = "report-main-breakage"
BREAKAGE_JOB_IF = "github.event.workflow_run.head_branch == 'main'"
BREAKAGE_PERMISSIONS = {"actions": "read", "issues": "write"}
BREAKAGE_WORKFLOW_RUN = {"workflows": ["CI", "Brain isolation security"], "types": ["completed"]}
BREAKAGE_STEP = "Update the main breakage issue"
BREAKAGE_TOKEN = "${{ github.token }}"
BREAKAGE_API_DEFAULT = "def api(method, path, expected=(200, 201), payload=None):"
BREAKAGE_ALLOWED_RUNS = (
    'ALLOWED_RUNS = {"CI": ("push",), "Brain isolation security": ("push", "schedule")}'
)
BREAKAGE_ISSUE_TITLES = (
    'ISSUE_TITLES = {"CI": "CI failed on main",'
    ' "Brain isolation security": "Brain isolation failed on main"}'
)
BREAKAGE_LABEL = 'LABEL = "ci-main-breakage"'

# Exact triggers are reviewed separately from fixture activation so a path change cannot hide
# merely because none of the representative fixtures exercises it.
EXPECTED_PATHS: dict[str, tuple[str, ...] | None] = {
    "ci.yml": None,
    "docs.yml": (
        "**.md", "scripts/check_docs.py",
        ".agents/skills/finch-backlog/scripts/test-review-protocol",
        ".github/workflows/docs.yml",
    ),
    "issue-201-chatgpt-auth.yml": (
        "Cargo.toml", "src/lib.rs", "src/oauth/**", "src/config/**",
        "crates/finch-providers/Cargo.toml",
        "crates/finch-providers/src/chatgpt_oauth.rs",
        "crates/finch-providers/src/credentials.rs",
        "crates/finch-providers/src/model_catalog.rs",
        "crates/finch-providers/src/oauth/**",
        "crates/finch-providers/src/openai_jwks.rs",
        "src/providers/mod.rs", "src/cli/chatgpt_auth.rs",
        "src/cli/setup_wizard.rs", "src/main.rs", "docs/OAUTH.md",
        ".github/issue-105-windows-probe/**", ".github/issue-201-windows-probe/**",
        ".github/workflows/issue-201-chatgpt-auth.yml",
    ),
    # Isolation-owned paths plus the three documents scripts/test_brain_isolation.sh scans
    # directly. Ordinary query/TUI/provider files stay out; the escape-API scan below still
    # covers them on every PR.
    "issue-56-brain-isolation.yml": (
        ".github/workflows/issue-56-brain-isolation.yml", "Cargo.toml", "Cargo.lock",
        "build.rs", "schema/**", "src/bin/finch-test-supervisor.rs", "src/brain/**",
        "src/daemon/**", "src/ipc/**", "src/node/**", "src/server/**",
        "src/client/daemon_client.rs", "src/cli/repl_event/brain_handler.rs",
        "scripts/test_brains.sh", "scripts/test_brain_isolation.sh",
        "scripts/with-cargo-slot", "scripts/test-with-cargo-slot",
        "tests/**", "docs/AUTOMATIC_TRAINING.md", "docs/DEVELOPMENT.md",
        "tests/README.md",
    ),
    "repository-hygiene.yml": None,
}

ISOLATION_WORKFLOW = "issue-56-brain-isolation.yml"
ISOLATION_SCHEDULE = [{"cron": "0 9 * * 1"}]
ISOLATION_JOBS = {"isolation-boundaries": "ubuntu-24.04", "isolation-boundaries-macos": "macos-14"}

# The complete fatal sequence each isolation platform must run, in order.
# Duplicate crate-wide cargo check and no_external_provider_binary_test stay on
# the Test job. Pull requests skip the release supervisor; the harness already
# treats a missing release pin as skip (#841).
ISOLATION_STEPS: tuple[tuple[str, tuple[str, ...]], ...] = (
    ("Build isolation supervisor", (
        "set -euo pipefail",
        "pin_debug=target/debug/finch-test-supervisor-pinned",
        "if [[ ! -x \"$pin_debug\" ]]; then",
        "cargo test --bin finch-test-supervisor signed_device_bits_serialize_as_parseable_u64_identity -- --exact",
        "cargo build --bin finch-test-supervisor",
        "install -m 0555 target/debug/finch-test-supervisor \"$pin_debug\"",
        "fi",
        "echo \"isolation-supervisor-image-sha256=$(shasum -a 256 \"$pin_debug\" | awk '{print $1}')\"",
        "echo \"FINCH_TEST_SUPERVISOR_BIN=${GITHUB_WORKSPACE}/${pin_debug}\" >> \"$GITHUB_ENV\"",
        "if [[ \"${GITHUB_EVENT_NAME}\" != pull_request ]]; then",
        "cargo build --release --bin finch-test-supervisor",
        "install -m 0555 target/release/finch-test-supervisor target/release/finch-test-supervisor-pinned",
        "fi",
    )),
    # The module, not two of its tests: a name list leaves anything added to the module scheduled
    # nowhere, and a test that never runs under the contract reports pass without asserting (#614).
    ("Brain isolation boundaries under the supervisor contract", (
        "./scripts/test_brains.sh cargo test --lib brain::isolation_tests:: -- --nocapture",
    )),
    ("Reject rewritten proof at the production constructor", (
        "./scripts/test_brains.sh cargo test --lib server::tests::production_constructor_rejects_rewritten_proof_and_accepts_exact_restore -- --nocapture",
    )),
    ("Reject fixture traversal before external mutation", (
        "./scripts/test_brains.sh cargo test --lib supervised_http_fixture_rejects -- --nocapture",
        "./scripts/test_brains.sh cargo test --lib server::tests::supervised_http_fixture_pins_state_root_across_ancestor_swap -- --exact --nocapture",
        "./scripts/test_brains.sh cargo test --lib ipc::server::tests::supervised_ipc_listener_ancestor_swap_never_mutates_replacement_path -- --exact --nocapture",
    )),
    ("Exercise real Brain and server paths behind the guard", (
        "./scripts/test_brains.sh cargo test --lib brain::store -- --nocapture",
        "./scripts/test_brains.sh cargo test --lib server::brain_service -- --nocapture",
        "./scripts/test_brains.sh cargo test --lib server::tests::production_constructor_persists_named_brain_only_in_isolated_home -- --exact --nocapture",
        "./scripts/test_brains.sh cargo test --lib server::tests::production_constructor_rejects_unverified_environment_before_store_mutation -- --exact --nocapture",
        "./scripts/test_brains.sh cargo test --test daemon_integration_test test_daemon_spawn_and_health -- --exact --ignored --nocapture",
    )),
    ("Keep worker node identity inside disposable state", (
        "tests/worker_node_isolation_contract.sh",
        "./scripts/test_brains.sh cargo test --test worker_integration_test -- --nocapture",
    )),
    ("Exercise the complete synthetic isolation harness", ("./scripts/test_brain_isolation.sh",)),
)

# Ordinary CI may touch the supervisor only to warm the macOS cache family on trusted main.
SUPERVISED_MARKERS = (
    "scripts/test_brains.sh", "scripts/test_brain_isolation.sh",
    "worker_node_isolation_contract.sh", "finch-test-supervisor",
)
SUPERVISOR_WARM_STEP = "Warm macOS isolation supervisor cache on trusted main"
SUPERVISOR_WARM_COMMANDS = (
    "cargo test --bin finch-test-supervisor --no-run",
    "cargo build --bin finch-test-supervisor",
    "cargo build --release --bin finch-test-supervisor",
)

# Mirrors the escape-API allowlist in scripts/test_brain_isolation.sh so ordinary source PRs,
# which no longer run that harness, still reject unauthorized process-group/session escapes.
ESCAPE_API_ROOTS = ("scripts", "src", "tests")
ESCAPE_API_SELF = "scripts/test_brain_isolation.sh"
ESCAPE_API = re.compile(r"(?:^|[^A-Za-z0-9_])(?:setsid|setpgid|process_group\(|set[ \t\n\v\f\r]+-m)")
ESCAPE_API_ALLOWLIST = (
    "src/bin/finch-test-supervisor.rs:if libc::setpgid(0, 0) == -1 {",
    "src/brain/mod.rs:.process_group(0)",
    "src/brain/mod.rs:if nix::libc::setpgid(0, 0) == -1 {",
    "src/daemon/spawn.rs:if nix::libc::setsid() == -1 {",
    "tests/no_external_provider_binary_test.rs:.process_group(0);",
)

EXPECTED_PULL_REQUEST_OPTIONS = {
    name: ({"branches": ("main",)} if name in {
        "ci.yml", "issue-201-chatgpt-auth.yml",
    } else {})
    for name in EXPECTED_PATHS
}

# Reviewed runner labels per workflow. An unavailable or misspelled label
# queues forever, so the inventory is pinned; availability itself is proven
# by the runs (the #518 Blacksmith pilot records runner identity separately).
EXPECTED_RUNNERS = {
    "ci.yml": ("blacksmith-8vcpu-ubuntu-2404", "macos-14", "ubuntu-24.04"),
    "ci-main-breakage.yml": ("ubuntu-24.04",),
    "ci-superseded-run-cancellation.yml": ("ubuntu-24.04",),
    "docs.yml": ("ubuntu-24.04",),
    "issue-201-chatgpt-auth.yml": ("windows-2022",),
    "issue-56-brain-isolation.yml": ("macos-14", "ubuntu-24.04"),
    "release.yml": ("${{ matrix.os }}", "ubuntu-latest"),
    "repository-hygiene.yml": ("ubuntu-24.04",),
}

# Main-only jobs keep platform suites and release preflights off the
# pull-request merge gate. Equality is the contract: do not invent a parser
# for `if:`.
MAIN_ONLY_IF = "github.event_name == 'push' && github.ref == 'refs/heads/main'"

# (workflow, job) pairs that must carry MAIN_ONLY_IF, with the reason a PR
# must not pay their cost.
MAIN_ONLY_JOBS = {
    ("ci.yml", "test-macos"): "the 120-minute macOS suite is not a pull-request merge gate",
    ("ci.yml", "build"): "release preflight compiles are not pull-request merge gates",
    ("ci.yml", "test-no-default"): "the no-default-features suite is not a pull-request merge gate",
}

# Weekly-scheduled jobs that must skip pull requests but still run on main,
# the weekly schedule, and manual dispatch. Equality is the contract.
NON_PR_IF = "github.event_name != 'pull_request'"

EXPECTED_CHECKS = {
    "ci.yml": (
        "Runtime Authority (Ubuntu)",
        "Security Audit",
        "Test (ubuntu-24.04, default)",
        "Toolchain and formatting contract",
    ),
    "docs.yml": ("Current docs links, claims, and shell syntax",),
    "issue-201-chatgpt-auth.yml": ("windows-verifier-compile",),
    ISOLATION_WORKFLOW: ("Isolation boundaries (ubuntu-24.04)",),
    "repository-hygiene.yml": ("Tracked tree (ubuntu-24.04)",),
}

# Checks every PR runs: all of ci.yml plus the tracked-tree hygiene job.
ALWAYS_CHECKS = (*EXPECTED_CHECKS["ci.yml"], *EXPECTED_CHECKS["repository-hygiene.yml"])
ISOLATION_CHECKS = EXPECTED_CHECKS[ISOLATION_WORKFLOW]

EXPECTED_FIXTURES = {
    "readme_only": (("README.md",), (
        *ALWAYS_CHECKS, "Current docs links, claims, and shell syntax",
    )),
    "ordinary_source": (("src/models/mod.rs",), ALWAYS_CHECKS),
    "ordinary_query_tui_provider": (
        ("src/cli/query.rs", "src/cli/tui/mod.rs", "src/providers/anthropic.rs"), ALWAYS_CHECKS,
    ),
    "brain_effect": (("src/brain/store.rs", "src/server/handlers.rs"), (
        *ALWAYS_CHECKS, *ISOLATION_CHECKS,
    )),
    "isolation_harness": (("scripts/test_brain_isolation.sh",), (
        *ALWAYS_CHECKS, *ISOLATION_CHECKS,
    )),
    "isolation_integration_test": (("tests/daemon_integration_test.rs",), (
        *ALWAYS_CHECKS, *ISOLATION_CHECKS,
    )),
    "supervisor_binary": (("src/bin/finch-test-supervisor.rs",), (
        *ALWAYS_CHECKS, *ISOLATION_CHECKS,
    )),
    "manifest_dependency": (("Cargo.toml", "Cargo.lock"), (
        *ALWAYS_CHECKS, *ISOLATION_CHECKS, "windows-verifier-compile",
    )),
    "public_api": (("src/lib.rs",), (*ALWAYS_CHECKS, "windows-verifier-compile")),
}

RUST_CACHE_ACTION = "Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6"
ACTIONS_CACHE_ACTION = "actions/cache@v4"
SCCACHE_ACTION = "mozilla-actions/sccache-action@fc920bf0ec8de6ee65d409111f7ec508035751ba"  # v0.0.11
SUPERVISOR_IMAGE_CACHE_NAME = "Restore pinned isolation supervisor"
SUPERVISOR_IMAGE_CACHE_KEY = (
    "isolation-supervisor-${{ runner.os }}-${{ runner.arch }}-rust-1.98.0-"
    "${{ hashFiles('src/bin/finch-test-supervisor.rs', 'src/brain/mod.rs', "
    "'Cargo.lock', 'rust-toolchain.toml', 'build.rs') }}"
)
GRAPH_HASH = "${{ hashFiles('Cargo.lock', '**/Cargo.toml', 'rust-toolchain.toml', '.cargo/config', '.cargo/config.toml') }}"
MATRIX_CACHE_KEY = (
    "finch-cargo-v5-${{ matrix.os }}-${{ runner.arch }}-${{ matrix.target }}-"
    "rust-1.98.0-${{ matrix.cache_family }}-" + GRAPH_HASH
)
MAIN_SAVE_IF = "${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}"
COMMON_CACHE_INPUTS = {
    "cache-provider": "github",
    "cache-targets": True,
    "cache-bin": False,
    "cache-workspace-crates": False,
    "cache-all-crates": False,
    "cache-on-failure": False,
}


CI_SHARED_ENV = {
    "CARGO_TERM_COLOR": "always",
    "RUST_BACKTRACE": 1,
    "CARGO_BUILD_JOBS": 2,
    "CARGO_PROFILE_RELEASE_LTO": "false",
    "CARGO_PROFILE_RELEASE_CODEGEN_UNITS": "16",
}
MACOS_FAMILY = "debug-default_supervisor-release_apple-release-default"


def literal_cache_key(platform: str, target: str, family: str) -> str:
    return (
        f"finch-cargo-v5-{platform}-${{{{ runner.arch }}}}-{target}-"
        f"rust-1.98.0-{family}-{GRAPH_HASH}"
    )


# The six approved cache identities (#384). Locations may share one; none may add another.
EXPECTED_CACHE_IDENTITIES = frozenset(
    literal_cache_key(platform, target, family) for platform, target, family in (
        ("ubuntu-24.04", "x86_64-unknown-linux-gnu", "debug-default_all-features-clippy_release-default"),
        ("ubuntu-24.04", "x86_64-unknown-linux-gnu", "debug-no-default-features"),
        ("ubuntu-24.04", "x86_64-unknown-linux-gnu", "release-default-lto-false-codegen-units-16"),
        ("ubuntu-24.04", "x86_64-unknown-linux-gnu", "cargo-audit-0.22.2"),
        ("ubuntu-24.04", "x86_64-unknown-linux-gnu", "debug-default-test-debug-0_supervisor-release"),
        ("macos-14", "aarch64-apple-darwin", MACOS_FAMILY),
    )
)


TEST_JOB_ENV = {
    "CARGO_BUILD_JOBS": 6,
    "CARGO_PROFILE_RELEASE_LTO": "false",
    "CARGO_PROFILE_RELEASE_CODEGEN_UNITS": "16",
    "CARGO_TERM_COLOR": "always",
    "RUST_BACKTRACE": 1,
    "RUSTC_WRAPPER": "sccache",
}

# test-macos and build take no job-level CARGO_BUILD_JOBS override, so their
# effective environment is the workflow-level default plus sccache (#938).
NO_OVERRIDE_JOB_ENV = {**CI_SHARED_ENV, "RUSTC_WRAPPER": "sccache"}

CACHE_SPECS = {
    ("ci.yml", "test"): {
        "name": "Restore compatible Cargo dependencies and build artifacts",
        "shared-key": MATRIX_CACHE_KEY,
        "save-if": MAIN_SAVE_IF,
        "before": "Compile all targets",
        "env": TEST_JOB_ENV,
    },
    ("ci.yml", "test-no-default"): {
        "name": "Restore compatible Cargo dependencies and build artifacts",
        "shared-key": literal_cache_key(
            "ubuntu-24.04", "x86_64-unknown-linux-gnu", "debug-no-default-features",
        ),
        "save-if": MAIN_SAVE_IF,
        "before": "Compile all targets",
        "env": TEST_JOB_ENV,
    },
    ("ci.yml", "test-macos"): {
        "name": "Restore compatible Cargo dependencies and build artifacts",
        "shared-key": literal_cache_key("macos-14", "aarch64-apple-darwin", MACOS_FAMILY),
        "save-if": MAIN_SAVE_IF,
        "before": "Build binary",
        "env": NO_OVERRIDE_JOB_ENV,
    },
    ("ci.yml", "runtime-authority"): {
        "name": "Restore Linux default-family Cargo state",
        "shared-key": literal_cache_key(
            "ubuntu-24.04", "x86_64-unknown-linux-gnu",
            "debug-default_all-features-clippy_release-default",
        ),
        "save-if": False,
        "before": "Run runtime authority regressions",
        "env": TEST_JOB_ENV,
    },
    ("ci.yml", "build"): {
        "name": "Restore compatible Linux release Cargo state",
        "shared-key": literal_cache_key(
            "ubuntu-24.04", "x86_64-unknown-linux-gnu",
            "release-default-lto-false-codegen-units-16",
        ),
        "save-if": MAIN_SAVE_IF,
        "before": "Build release binary",
        "env": NO_OVERRIDE_JOB_ENV,
    },
    ("ci.yml", "security"): {
        "name": "Restore cargo-audit 0.22.2",
        "shared-key": literal_cache_key(
            "ubuntu-24.04", "x86_64-unknown-linux-gnu", "cargo-audit-0.22.2",
        ),
        "save-if": MAIN_SAVE_IF,
        "before": "Install cargo-audit 0.22.2 on cache miss",
        "cache-targets": False,
        "cache-bin": True,
        "id": "cargo-audit-cache",
    },
    (ISOLATION_WORKFLOW, "isolation-boundaries"): {
        "name": "Restore compatible isolation Cargo state",
        "shared-key": literal_cache_key(
            "ubuntu-24.04", "x86_64-unknown-linux-gnu",
            "debug-default-test-debug-0_supervisor-release",
        ),
        "save-if": MAIN_SAVE_IF,
        "before": "Restore pinned isolation supervisor",
        "env": {
            "CARGO_BUILD_JOBS": 2, "CARGO_PROFILE_TEST_DEBUG": 0,
            "CARGO_TERM_COLOR": "always", "RUST_BACKTRACE": 1,
        },
    },
    # A restore-only consumer of the ci.yml macOS family: same key and effective environment,
    # never a writer, so it adds a cache location without adding a cache identity.
    (ISOLATION_WORKFLOW, "isolation-boundaries-macos"): {
        "name": "Restore the macOS default Cargo family",
        "shared-key": literal_cache_key("macos-14", "aarch64-apple-darwin", MACOS_FAMILY),
        "save-if": False,
        "before": "Restore pinned isolation supervisor",
        "env": CI_SHARED_ENV,
    },
    ("release.yml", "build-release"): {
        "name": "Restore compatible Cargo dependencies and build artifacts",
        "shared-key": MATRIX_CACHE_KEY,
        "save-if": False,
        "before": "Install Linux dependencies",
    },
}

# The five canonical ci.yml Cargo-compiling jobs additionally run sccache as a
# compilation-unit cache, complementary to the CACHE_SPECS target-tree cache
# (#938). Bounded to ci.yml: issue-56-brain-isolation.yml and release.yml are
# left to a focused follow-up rather than widened here.
SCCACHE_SPECS = {
    location: {"name": "Set up sccache"}
    for location in (
        ("ci.yml", "test"),
        ("ci.yml", "test-no-default"),
        ("ci.yml", "test-macos"),
        ("ci.yml", "runtime-authority"),
        ("ci.yml", "build"),
    )
}


class ContractError(Exception):
    pass


def load_yaml(path: Path) -> dict[str, Any]:
    """Use Ruby's stock Psych parser; Python's standard library has no YAML parser."""
    program = r"""
require 'yaml'; require 'json'
begin
  source = STDIN.read
  stream = Psych.parse_stream(source)
  raise 'workflow must contain exactly one YAML document' unless stream.children.length == 1
  root = stream.children[0].root
  raise 'workflow document must be a mapping' unless root.is_a?(Psych::Nodes::Mapping)
  visit = lambda do |node|
    if node.is_a?(Psych::Nodes::Mapping)
      keys = node.children.each_slice(2).map do |key, value|
        raise "mapping key at line #{key.start_line + 1} must be a scalar" unless key.is_a?(Psych::Nodes::Scalar)
        visit.call(value)
        key
      end
      duplicate = keys.group_by(&:value).find { |_, matches| matches.length > 1 }
      raise "duplicate YAML key #{duplicate[0].inspect} at line #{duplicate[1][1].start_line + 1}" if duplicate
    elsif node.respond_to?(:children) && node.children
      node.children.each { |child| visit.call(child) }
    end
  end
  visit.call(root)
  on_key = root.children.each_slice(2).map(&:first).find { |key| key.value == 'on' }
  raise "workflow root is missing the actual 'on' key" unless on_key
  document = YAML.safe_load(source, permitted_classes: [], aliases: false)
  raise 'YAML key conversion collision is unsupported' unless document.length == root.children.length / 2
  converted_on = on_key.plain ? true : 'on'
  document['on'] = document.delete(converted_on) unless converted_on == 'on'
  print JSON.generate(document)
rescue => error
  warn error.message
  exit 1
end
"""
    try:
        result = subprocess.run(
            ["ruby", "-e", program], input=path.read_text(), text=True,
            capture_output=True, check=False,
        )
    except (OSError, UnicodeError) as error:
        raise ContractError(f"{path}: cannot parse workflow YAML with Ruby Psych: {error}") from error
    if result.returncode:
        raise ContractError(f"{path}: invalid workflow YAML: {result.stderr.strip()}")
    try:
        document = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ContractError(f"{path}: YAML parser returned invalid JSON: {error}") from error
    if not isinstance(document, dict):
        raise ContractError(f"{path}: workflow document must be a mapping")
    return document


def event_contract(
    document: dict[str, Any], display: str, event: str,
) -> dict[str, tuple[str, ...]] | bool:
    triggers = document["on"]
    if not isinstance(triggers, dict):
        raise ContractError(f"{display}: on must be a mapping")
    if "pull_request_target" in triggers:
        raise ContractError(f"{display}: on.pull_request_target is unsupported by the reviewed PR allocation")
    if event not in triggers:
        return False
    options = triggers[event]
    if options is None:
        return {}
    if not isinstance(options, dict):
        raise ContractError(f"{display}: on.{event} must be a mapping or null")
    supported = {"branches", "branches-ignore", "types", "paths", "paths-ignore"}
    unsupported = set(options) - supported
    if unsupported:
        raise ContractError(f"{display}: unsupported {event} keys affecting activation: {sorted(unsupported)}")
    for alternatives in (("branches", "branches-ignore"), ("paths", "paths-ignore")):
        if set(alternatives) <= set(options):
            raise ContractError(f"{display}: on.{event} cannot combine {alternatives[0]} and {alternatives[1]}")
    contract: dict[str, tuple[str, ...]] = {}
    for key, values in options.items():
        if not isinstance(values, list) or not values or not all(isinstance(item, str) for item in values):
            raise ContractError(f"{display}: on.{event}.{key} must be a nonempty string list")
        if len(values) != len(set(values)):
            raise ContractError(f"{display}: on.{event}.{key} contains duplicates")
        contract[key] = tuple(values)
    return contract


def pull_request_contract(document: dict[str, Any], display: str) -> dict[str, tuple[str, ...]] | bool:
    return event_contract(document, display, "pull_request")


MATRIX_REFERENCE = re.compile(r"\$\{\{\s*matrix\.([A-Za-z_][A-Za-z0-9_-]*)\s*\}\}")


def expanded_checks(document: dict[str, Any], display: str) -> tuple[str, ...]:
    jobs = document.get("jobs")
    if not isinstance(jobs, dict) or not jobs:
        raise ContractError(f"{display}: jobs must be a nonempty mapping")
    names: list[str] = []
    for job_id, job in jobs.items():
        if not isinstance(job_id, str) or not isinstance(job, dict):
            raise ContractError(f"{display}: each job must have a string id and mapping body")
        if job.get("if") in (MAIN_ONLY_IF, NON_PR_IF):
            continue
        explicit_name = job.get("name")
        if explicit_name is not None and not isinstance(explicit_name, str):
            raise ContractError(f"{display}: job {job_id!r} name must be a string")
        strategy = job.get("strategy", {})
        if not isinstance(strategy, dict):
            raise ContractError(f"{display}: job {job_id!r} strategy must be a mapping")
        matrix = strategy.get("matrix")
        if matrix is None:
            rows = [{}]
        else:
            if not isinstance(matrix, dict) or not matrix:
                raise ContractError(f"{display}: job {job_id!r} matrix must be a nonempty mapping")
            if "exclude" in matrix:
                raise ContractError(f"{display}: job {job_id!r} uses unsupported matrix.exclude allocation syntax")
            axes = [(key, value) for key, value in matrix.items() if key != "include"]
            includes = matrix.get("include")
            if axes and includes is not None:
                raise ContractError(f"{display}: job {job_id!r} mixes axes and matrix.include; allocation is unsupported")
            if includes is not None:
                if not isinstance(includes, list) or not includes or not all(isinstance(row, dict) for row in includes):
                    raise ContractError(f"{display}: job {job_id!r} matrix.include must be a nonempty mapping list")
                rows = includes
            else:
                if not all(isinstance(key, str) and isinstance(values, list) and values for key, values in axes):
                    raise ContractError(f"{display}: job {job_id!r} matrix axes must be nonempty lists")
                keys = [key for key, _ in axes]
                rows = [dict(zip(keys, values)) for values in itertools.product(*(values for _, values in axes))]
        for row in rows:
            if not all(isinstance(key, str) and isinstance(value, (str, int, float, bool)) for key, value in row.items()):
                raise ContractError(f"{display}: job {job_id!r} matrix rows must contain scalar values")
            if explicit_name is None:
                name = job_id if not row else f"{job_id} ({', '.join(map(str, row.values()))})"
            else:
                def replace(match: re.Match[str]) -> str:
                    key = match.group(1)
                    if key not in row:
                        raise ContractError(f"{display}: job {job_id!r} name references missing matrix key {key!r}")
                    return str(row[key]).lower() if isinstance(row[key], bool) else str(row[key])
                name = MATRIX_REFERENCE.sub(replace, explicit_name)
                if "${{" in name:
                    raise ContractError(f"{display}: job {job_id!r} name uses unsupported allocation expression {name!r}")
            names.append(name)
    duplicates = sorted({name for name in names if names.count(name) > 1})
    if duplicates:
        raise ContractError(f"{display}: duplicate expanded check names: {duplicates}")
    return tuple(sorted(names))


def glob_matches(pattern: str, path: str) -> bool:
    source = pattern[1:] if pattern.startswith("!") else pattern
    if not source or any(character in source for character in "[]{}\\"):
        raise ContractError(f"unsupported pull_request.paths pattern {pattern!r}")
    pieces: list[str] = []
    index = 0
    while index < len(source):
        if source[index:index + 3] == "**/":
            pieces.append("(?:.*/)?")
            index += 3
        elif source[index:index + 2] == "**":
            pieces.append(".*")
            index += 2
        elif source[index] == "*":
            pieces.append("[^/]*")
            index += 1
        elif source[index] == "?":
            pieces.append("[^/]")
            index += 1
        else:
            pieces.append(re.escape(source[index]))
            index += 1
    return re.fullmatch("".join(pieces), path) is not None


def workflow_activates(contract: dict[str, tuple[str, ...]], changed: tuple[str, ...]) -> bool:
    paths = contract.get("paths")
    ignored = contract.get("paths-ignore")
    if paths is None and ignored is None:
        return True
    if ignored is not None:
        return any(not any(glob_matches(pattern, path) for pattern in ignored) for path in changed)
    def path_matches(path: str) -> bool:
        active = False
        for pattern in paths:
            if glob_matches(pattern, path):
                active = not pattern.startswith("!")
        return active

    return any(path_matches(path) for path in changed)


def shell_commands(run: Any) -> tuple[str, ...]:
    if not isinstance(run, str):
        return ()
    joined = run.replace("\\\n", " ")
    return tuple(" ".join(line.split()) for line in joined.splitlines() if line.strip())


def active_owner_job_errors(
    documents: dict[str, dict[str, Any]], workflow: str, job_id: str, expected_runner: str,
) -> list[str]:
    job = documents.get(workflow, {}).get("jobs", {}).get(job_id)
    if not isinstance(job, dict):
        return [f"{workflow}: required owner job {job_id!r} is missing"]
    errors: list[str] = []
    if job.get("runs-on") != expected_runner or job.get("if") is not None:
        errors.append(
            f"{workflow}: owner job {job_id!r} must run actively on {expected_runner}"
        )
    if job.get("continue-on-error") not in (None, False):
        errors.append(f"{workflow}: owner job {job_id!r} must gate failure")
    return errors


def required_step_errors(
    documents: dict[str, dict[str, Any]], workflow: str, job_id: str, name: str,
    expected_if: str | None, expected_shell: str | None, commands: tuple[str, ...],
) -> list[str]:
    errors: list[str] = []
    matches: list[tuple[str, str, dict[str, Any]]] = []
    for owner_workflow, document in documents.items():
        jobs = document.get("jobs")
        if not isinstance(jobs, dict):
            continue
        for owner_job, job in jobs.items():
            if not isinstance(job, dict) or not isinstance(job.get("steps"), list):
                continue
            for step in job["steps"]:
                if isinstance(step, dict) and step.get("name") == name:
                    matches.append((owner_workflow, owner_job, step))
    owners = tuple((owner_workflow, owner_job) for owner_workflow, owner_job, _ in matches)
    if len(matches) != 1 or owners != ((workflow, job_id),):
        return [
            f"required step {name!r} must occur exactly once in {workflow}:{job_id}; "
            f"actual={owners!r}"
        ]
    step = matches[0][2]
    if step.get("if") != expected_if:
        errors.append(
            f"{workflow}: step {name!r} condition changed; "
            f"expected={expected_if!r} actual={step.get('if')!r}"
        )
    if step.get("shell") != expected_shell:
        errors.append(
            f"{workflow}: step {name!r} shell changed; "
            f"expected={expected_shell!r} actual={step.get('shell')!r}"
        )
    if step.get("continue-on-error") not in (None, False):
        errors.append(f"{workflow}: step {name!r} must gate failure")
    actual_commands = shell_commands(step.get("run"))
    if actual_commands != commands:
        errors.append(
            f"{workflow}: step {name!r} commands changed; "
            f"expected={commands!r} actual={actual_commands!r}"
        )
    return errors


def step_order_errors(
    documents: dict[str, dict[str, Any]], workflow: str, job_id: str,
    earlier_names: tuple[str, ...], before_name: str,
) -> list[str]:
    """Require unique preflight steps to precede the named expensive setup boundary."""
    job = documents.get(workflow, {}).get("jobs", {}).get(job_id)
    if not isinstance(job, dict) or not isinstance(job.get("steps"), list):
        return []
    positions: dict[str, list[int]] = {}
    for index, step in enumerate(job["steps"]):
        if isinstance(step, dict) and isinstance(step.get("name"), str):
            positions.setdefault(step["name"], []).append(index)
    required = (*earlier_names, before_name)
    if any(len(positions.get(name, ())) != 1 for name in required):
        return []  # Missing/duplicate steps already have more specific diagnostics.
    boundary = positions[before_name][0]
    late = tuple(name for name in earlier_names if positions[name][0] >= boundary)
    if not late:
        return []
    return [
        f"{workflow}: job {job_id!r} preflight steps must precede {before_name!r}; "
        f"late={late!r}"
    ]


def cache_action(uses: Any) -> bool:
    if not isinstance(uses, str):
        return False
    lowered = uses.lower()
    return (
        lowered.startswith("actions/cache@")
        or "rust-cache@" in lowered
        or "sccache-action@" in lowered
    )


def relevant_rust_env(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        return {}
    prefixes = ("CARGO", "CC", "CFLAGS", "CXX", "CMAKE", "RUST")
    return {key: item for key, item in value.items() if isinstance(key, str) and key.startswith(prefixes)}


def cache_contract_errors(documents: dict[str, dict[str, Any]]) -> list[str]:
    """Check the small, explicit cache allocation; do not interpret arbitrary Actions code."""
    errors: list[str] = []
    found: dict[tuple[str, str], list[tuple[int, dict[str, Any]]]] = {}
    supervisor_image_found: dict[tuple[str, str], list[tuple[int, dict[str, Any]]]] = {}
    sccache_found: dict[tuple[str, str], list[tuple[int, dict[str, Any]]]] = {}
    for workflow, document in documents.items():
        jobs = document.get("jobs", {})
        if not isinstance(jobs, dict):
            continue
        for job_id, job in jobs.items():
            if not isinstance(job, dict) or not isinstance(job.get("steps"), list):
                continue
            for index, step in enumerate(job["steps"]):
                if not isinstance(step, dict) or not cache_action(step.get("uses")):
                    continue
                uses = step.get("uses")
                location = (workflow, job_id)
                if isinstance(uses, str) and "rust-cache@" in uses.lower():
                    found.setdefault(location, []).append((index, step))
                elif uses == ACTIONS_CACHE_ACTION:
                    supervisor_image_found.setdefault(location, []).append((index, step))
                elif isinstance(uses, str) and "sccache-action@" in uses.lower():
                    sccache_found.setdefault(location, []).append((index, step))
                else:
                    errors.append(
                        f"{workflow}: job {job_id!r} uses unreviewed cache action {uses!r}"
                    )

    expected_locations = set(CACHE_SPECS)
    actual_locations = set(found)
    if actual_locations != expected_locations:
        errors.append(
            "Cargo cache allocation changed; "
            f"missing={sorted(expected_locations - actual_locations)!r} "
            f"unexpected={sorted(actual_locations - expected_locations)!r}"
        )

    for location in sorted(expected_locations & actual_locations):
        workflow, job_id = location
        matches = found[location]
        if len(matches) != 1:
            errors.append(
                f"{workflow}: job {job_id!r} must contain exactly one Cargo cache step; "
                f"actual={len(matches)}"
            )
            continue
        cache_index, step = matches[0]
        spec = CACHE_SPECS[location]
        if step.get("name") != spec["name"]:
            errors.append(
                f"{workflow}: job {job_id!r} cache step name changed; "
                f"expected={spec['name']!r} actual={step.get('name')!r}"
            )
        if step.get("uses") != RUST_CACHE_ACTION:
            errors.append(
                f"{workflow}: job {job_id!r} must pin the reviewed rust-cache action; "
                f"actual={step.get('uses')!r}"
            )
        if step.get("continue-on-error") is not True:
            errors.append(f"{workflow}: job {job_id!r} cache failure must remain nonfatal")
        if step.get("if") is not None:
            errors.append(f"{workflow}: job {job_id!r} cache step must run actively")
        expected_inputs = dict(COMMON_CACHE_INPUTS)
        expected_inputs.update({
            "shared-key": spec["shared-key"],
            "save-if": spec["save-if"],
        })
        for optional in ("cache-targets", "cache-bin"):
            if optional in spec:
                expected_inputs[optional] = spec[optional]
        actual_inputs = step.get("with")
        if isinstance(actual_inputs, dict) and actual_inputs.get("add-job-id-key") is False:
            actual_inputs = {key: value for key, value in actual_inputs.items() if key != "add-job-id-key"}
        if actual_inputs != expected_inputs:
            errors.append(
                f"{workflow}: job {job_id!r} cache inputs changed; "
                f"expected={expected_inputs!r} actual={actual_inputs!r}"
            )
        if step.get("id") != spec.get("id"):
            errors.append(
                f"{workflow}: job {job_id!r} cache id changed; "
                f"expected={spec.get('id')!r} actual={step.get('id')!r}"
            )

        job = documents[workflow]["jobs"][job_id]
        steps = job["steps"]
        toolchains = [
            index for index, candidate in enumerate(steps)
            if isinstance(candidate, dict)
            and candidate.get("uses") == "dtolnay/rust-toolchain@1.98.0"
        ]
        boundaries = [
            index for index, candidate in enumerate(steps)
            if isinstance(candidate, dict) and candidate.get("name") == spec["before"]
        ]
        if len(toolchains) != 1 or len(boundaries) != 1:
            errors.append(
                f"{workflow}: job {job_id!r} cache ordering boundary is ambiguous; "
                f"toolchains={toolchains!r} before={boundaries!r}"
            )
        elif not toolchains[0] < cache_index < boundaries[0]:
            errors.append(
                f"{workflow}: job {job_id!r} cache must run after the pinned toolchain "
                f"and before {spec['before']!r}"
            )
        job_env = relevant_rust_env(job.get("env"))
        if relevant_rust_env(step.get("env")) or (job_env and "env" not in spec):
            errors.append(
                f"{workflow}: job {job_id!r} must not override action-hashed Rust/Cargo "
                "environment at the cache boundary"
            )
        if "env" in spec:
            effective = {**relevant_rust_env(documents[workflow].get("env")), **job_env}
            if effective != spec["env"]:
                errors.append(
                    f"{workflow}: job {job_id!r} effective Cargo/Rust environment changed; "
                    f"expected={spec['env']!r} actual={effective!r}"
                )

    expected_sccache_locations = set(SCCACHE_SPECS)
    actual_sccache_locations = set(sccache_found)
    if actual_sccache_locations != expected_sccache_locations:
        errors.append(
            "sccache allocation changed; "
            f"missing={sorted(expected_sccache_locations - actual_sccache_locations)!r} "
            f"unexpected={sorted(actual_sccache_locations - expected_sccache_locations)!r}"
        )

    for location in sorted(expected_sccache_locations & actual_sccache_locations):
        workflow, job_id = location
        matches = sccache_found[location]
        if len(matches) != 1:
            errors.append(
                f"{workflow}: job {job_id!r} must contain exactly one sccache setup step; "
                f"actual={len(matches)}"
            )
            continue
        sccache_index, step = matches[0]
        spec = SCCACHE_SPECS[location]
        if step.get("name") != spec["name"]:
            errors.append(
                f"{workflow}: job {job_id!r} sccache step name changed; "
                f"expected={spec['name']!r} actual={step.get('name')!r}"
            )
        if step.get("uses") != SCCACHE_ACTION:
            errors.append(
                f"{workflow}: job {job_id!r} must pin the reviewed sccache action; "
                f"actual={step.get('uses')!r}"
            )
        # sccache-action only needs to install the sccache binary before
        # anything in the job execs a compiler through RUSTC_WRAPPER; unlike
        # the target-tree cache restore, it has no dependency on the pinned
        # toolchain step, so no ordering is required against it. In the
        # 'test' job specifically, sccache must precede the early Cargo-slot
        # self-test (which builds against the runner's preinstalled default
        # toolchain before "Install repository Rust toolchain" even runs).
        cargo_cache_matches = found.get(location, [])
        if len(cargo_cache_matches) != 1:
            errors.append(
                f"{workflow}: job {job_id!r} sccache ordering boundary is ambiguous; "
                f"cargo_cache={cargo_cache_matches!r}"
            )
        elif not sccache_index < cargo_cache_matches[0][0]:
            errors.append(
                f"{workflow}: job {job_id!r} sccache must run before the Cargo "
                "target-tree cache restore"
            )

    expected_supervisor_image_jobs = {
        (ISOLATION_WORKFLOW, "isolation-boundaries"),
        (ISOLATION_WORKFLOW, "isolation-boundaries-macos"),
    }
    if set(supervisor_image_found) != expected_supervisor_image_jobs:
        errors.append(
            "isolation supervisor image cache allocation changed; "
            f"missing={sorted(expected_supervisor_image_jobs - set(supervisor_image_found))!r} "
            f"unexpected={sorted(set(supervisor_image_found) - expected_supervisor_image_jobs)!r}"
        )
    expected_supervisor_paths = (
        "target/debug/finch-test-supervisor",
        "target/debug/finch-test-supervisor-pinned",
    )
    for location in sorted(expected_supervisor_image_jobs & set(supervisor_image_found)):
        workflow, job_id = location
        matches = supervisor_image_found[location]
        if len(matches) != 1:
            errors.append(
                f"{workflow}: job {job_id!r} must contain exactly one isolation supervisor image cache; "
                f"actual={len(matches)}"
            )
            continue
        _, step = matches[0]
        if step.get("name") != SUPERVISOR_IMAGE_CACHE_NAME:
            errors.append(
                f"{workflow}: job {job_id!r} supervisor image cache name changed; "
                f"expected={SUPERVISOR_IMAGE_CACHE_NAME!r} actual={step.get('name')!r}"
            )
        if step.get("uses") != ACTIONS_CACHE_ACTION:
            errors.append(
                f"{workflow}: job {job_id!r} supervisor image cache must pin {ACTIONS_CACHE_ACTION}; "
                f"actual={step.get('uses')!r}"
            )
        if step.get("if") is not None:
            errors.append(f"{workflow}: job {job_id!r} supervisor image cache must run actively")
        if step.get("continue-on-error") not in (None, False):
            errors.append(
                f"{workflow}: job {job_id!r} supervisor image cache failure must remain fatal"
            )
        actual_inputs = step.get("with") if isinstance(step.get("with"), dict) else {}
        actual_path = actual_inputs.get("path")
        actual_paths = tuple(
            line.strip() for line in str(actual_path or "").splitlines() if line.strip()
        )
        if actual_paths != expected_supervisor_paths or actual_inputs.get("key") != SUPERVISOR_IMAGE_CACHE_KEY:
            errors.append(
                f"{workflow}: job {job_id!r} supervisor image cache inputs changed; "
                f"expected_paths={expected_supervisor_paths!r} expected_key={SUPERVISOR_IMAGE_CACHE_KEY!r} "
                f"actual={actual_inputs!r}"
            )

    identities: set[str] = set()
    for (workflow, job_id), matches in found.items():
        strategy = documents[workflow]["jobs"][job_id].get("strategy")
        matrix = strategy.get("matrix") if isinstance(strategy, dict) else None
        include = matrix.get("include") if isinstance(matrix, dict) else None
        rows = [row for row in include if isinstance(row, dict)] if isinstance(include, list) else []
        for _, step in matches:
            key = (step.get("with") or {}).get("shared-key")
            if not isinstance(key, str):
                continue
            for row in rows or [{}]:
                identities.add(MATRIX_REFERENCE.sub(
                    lambda match: str(row.get(match.group(1), match.group(0))), key,
                ))
    if identities != EXPECTED_CACHE_IDENTITIES:
        errors.append(
            f"Cargo cache identity set changed; expected {len(EXPECTED_CACHE_IDENTITIES)} "
            f"identities across {len(CACHE_SPECS)} locations; "
            f"missing={sorted(EXPECTED_CACHE_IDENTITIES - identities)!r} "
            f"unexpected={sorted(identities - EXPECTED_CACHE_IDENTITIES)!r}"
        )

    expected_modes = {
        ("ci.yml", "runtime-authority"): "read",
        ("release.yml", "build-release"): "read",
        ("release.yml", "create-release"): "none",
    }
    for workflow, document in documents.items():
        jobs = document.get("jobs", {})
        if not isinstance(jobs, dict):
            continue
        for job_id, job in jobs.items():
            if not isinstance(job, dict):
                continue
            expected_mode = expected_modes.get((workflow, job_id))
            actual_mode = job.get("cache-mode")
            if actual_mode != expected_mode:
                errors.append(
                    f"{workflow}: job {job_id!r} cache-mode changed; "
                    f"expected={expected_mode!r} actual={actual_mode!r}"
                )

    ci_env = relevant_rust_env(documents.get("ci.yml", {}).get("env"))
    release_env = relevant_rust_env(documents.get("release.yml", {}).get("env"))
    if ci_env != CI_SHARED_ENV or release_env != CI_SHARED_ENV:
        errors.append(
            "ci.yml and release.yml must retain identical action-hashed Cargo/Rust "
            f"environment; expected={CI_SHARED_ENV!r} ci={ci_env!r} release={release_env!r}"
        )

    expected_test_matrix = [
        {
            "os": "ubuntu-24.04", "target": "x86_64-unknown-linux-gnu",
            "feature_name": "default",
            "cache_family": "debug-default_all-features-clippy_release-default",
            "cargo_args": "", "timeout_minutes": 90,
        },
    ]
    actual_test_matrix = (
        documents.get("ci.yml", {}).get("jobs", {}).get("test", {})
        .get("strategy", {}).get("matrix", {}).get("include")
    )
    if actual_test_matrix != expected_test_matrix:
        errors.append(
            "ci.yml: test cache compatibility matrix changed; "
            f"expected={expected_test_matrix!r} actual={actual_test_matrix!r}"
        )

    expected_release_matrix = [
        {
            "os": "macos-14", "target": "aarch64-apple-darwin",
            "asset_name": "finch-macos-arm64",
            "cache_family": "debug-default_supervisor-release_apple-release-default",
        },
        {
            "os": "ubuntu-24.04", "target": "x86_64-unknown-linux-gnu",
            "asset_name": "finch-linux-x86_64",
            "cache_family": "release-default-lto-false-codegen-units-16",
        },
    ]
    actual_release_matrix = (
        documents.get("release.yml", {}).get("jobs", {}).get("build-release", {})
        .get("strategy", {}).get("matrix", {}).get("include")
    )
    if actual_release_matrix != expected_release_matrix:
        errors.append(
            "release.yml: build-release cache compatibility matrix changed; "
            f"expected={expected_release_matrix!r} actual={actual_release_matrix!r}"
        )

    release = documents.get("release.yml", {})
    if "permissions" in release:
        errors.append("release.yml: workflow-wide permissions must remain absent")
    release_jobs = release.get("jobs", {})
    for job_id, expected in (
        ("build-release", {"contents": "read"}),
        ("create-release", {"contents": "write"}),
    ):
        actual = release_jobs.get(job_id, {}).get("permissions")
        if actual != expected:
            errors.append(
                f"release.yml: job {job_id!r} permissions changed; "
                f"expected={expected!r} actual={actual!r}"
            )

    errors.extend(required_step_errors(
        documents, "ci.yml", "security", "Install cargo-audit 0.22.2 on cache miss",
        "steps.cargo-audit-cache.outputs.cache-hit != 'true'", None,
        ("cargo install cargo-audit --version 0.22.2 --locked",),
    ))
    errors.extend(required_step_errors(
        documents, "ci.yml", "security", "Verify cargo-audit 0.22.2",
        None, None,
        ('test "$(cargo-audit --version)" = "cargo-audit 0.22.2"',),
    ))
    return errors


def migrated_boundary_errors(documents: dict[str, dict[str, Any]]) -> list[str]:
    errors: list[str] = []
    errors.extend(active_owner_job_errors(documents, "ci.yml", "test", "blacksmith-8vcpu-ubuntu-2404"))

    errors.extend(required_step_errors(
        documents, "ci.yml", "test", "Prove validated request tokens cannot be forged",
        "matrix.feature_name == 'default'", None,
        ("cargo test --doc -- ValidatedProviderRequest",),
    ))
    errors.extend(required_step_errors(
        documents, "ci.yml", "build", "Run release-mode atomic history regression",
        None, None,
        ("cargo test --release --target x86_64-unknown-linux-gnu --lib cli::conversation::tests -- --nocapture",),
    ))
    errors.extend(required_step_errors(
        documents, "ci.yml", "test", "Verify shared skill discovery",
        "matrix.feature_name == 'default'", "bash", (
            "test -L .claude/skills/finch-backlog",
            'test "$(cd .agents/skills/finch-backlog && pwd -P)" = "$(cd .claude/skills/finch-backlog && pwd -P)"',
        ),
    ))
    errors.extend(required_step_errors(
        documents, "ci.yml", "test", "Check and exercise the Cargo slot",
        "matrix.feature_name == 'default'", "bash", (
            "bash -n .agents/skills/finch-backlog/scripts/with-cargo-slot "
            ".agents/skills/finch-backlog/scripts/test-with-cargo-slot "
            ".agents/skills/finch-backlog/scripts/reclaim-cargo-targets "
            ".agents/skills/finch-backlog/scripts/test-reclaim-cargo-targets",
            ".agents/skills/finch-backlog/scripts/test-with-cargo-slot",
            ".agents/skills/finch-backlog/scripts/test-reclaim-cargo-targets",
        ),
    ))
    errors.extend(step_order_errors(
        documents, "ci.yml", "test", (
            "Verify shared skill discovery", "Check and exercise the Cargo slot",
        ), "Install repository Rust toolchain",
    ))

    auth = documents.get("issue-201-chatgpt-auth.yml", {})
    jobs = auth.get("jobs")
    if not isinstance(jobs, dict) or tuple(jobs) != ("windows-verifier-compile",):
        errors.append("issue-201-chatgpt-auth.yml: exactly one Windows verifier job is required")
    else:
        job = jobs["windows-verifier-compile"]
        if not isinstance(job, dict):
            errors.append("issue-201-chatgpt-auth.yml: Windows verifier job must be a mapping")
    errors.extend(active_owner_job_errors(
        documents, "issue-201-chatgpt-auth.yml", "windows-verifier-compile", "windows-2022",
    ))
    errors.extend(required_step_errors(
        documents, "issue-201-chatgpt-auth.yml", "windows-verifier-compile",
        "Compile exact authentication sources on Windows", None, None, (
            "cargo check --manifest-path .github/issue-105-windows-probe/Cargo.toml",
            "cargo check --manifest-path .github/issue-201-windows-probe/Cargo.toml",
        ),
    ))

    errors.extend(push_contract_errors(documents, "issue-201-chatgpt-auth.yml"))
    return errors


def push_contract_errors(documents: dict[str, dict[str, Any]], workflow: str) -> list[str]:
    """Require a main-branch push trigger whose paths equal the reviewed PR paths."""
    document = documents.get(workflow)
    if document is None:
        return []
    try:
        push = event_contract(document, (WORKFLOWS / workflow).as_posix(), "push")
    except ContractError as error:
        return [str(error)]
    if push is False:
        return [f"{workflow}: path-filtered push to main is required"]
    errors: list[str] = []
    expected = {"branches": ("main",), "paths": EXPECTED_PATHS[workflow]}
    for key in sorted(set(push) | set(expected)):
        actual_value = push.get(key)
        expected_value = expected.get(key)
        equal = (
            set(actual_value or ()) == set(expected_value or ())
            if key == "paths" else actual_value == expected_value
        )
        if not equal:
            errors.append(
                f"{workflow}: push.{key} changed; "
                f"expected={expected_value!r} actual={actual_value!r}"
            )
    return errors


def isolation_errors(documents: dict[str, dict[str, Any]]) -> list[str]:
    """Both platforms run the full fatal sequence on owned paths, weekly, and on demand."""
    document = documents.get(ISOLATION_WORKFLOW)
    if document is None:
        return []
    errors = push_contract_errors(documents, ISOLATION_WORKFLOW)
    triggers = document.get("on")
    if not isinstance(triggers, dict) or triggers.get("schedule") != ISOLATION_SCHEDULE:
        actual = triggers.get("schedule") if isinstance(triggers, dict) else None
        errors.append(
            f"{ISOLATION_WORKFLOW}: weekly schedule changed; "
            f"expected={ISOLATION_SCHEDULE!r} actual={actual!r}"
        )
    if not isinstance(triggers, dict) or "workflow_dispatch" not in triggers:
        errors.append(f"{ISOLATION_WORKFLOW}: manual workflow_dispatch trigger is required")

    # A `defaults.run` shell or directory silently rewrites every fatal step below it.
    if "defaults" in document:
        errors.append(f"{ISOLATION_WORKFLOW}: workflow-level defaults would rewrite every fatal step; remove them")
    jobs = document.get("jobs")
    jobs = jobs if isinstance(jobs, dict) else {}
    if set(jobs) != set(ISOLATION_JOBS):
        errors.append(
            f"{ISOLATION_WORKFLOW}: isolation job inventory changed; "
            f"expected={sorted(ISOLATION_JOBS)!r} actual={sorted(jobs)!r}"
        )
    for job_id, runner in ISOLATION_JOBS.items():
        job = documents.get(ISOLATION_WORKFLOW, {}).get("jobs", {}).get(job_id)
        expected_if = NON_PR_IF if job_id == "isolation-boundaries-macos" else None
        if expected_if is None:
            errors.extend(active_owner_job_errors(documents, ISOLATION_WORKFLOW, job_id, runner))
        elif not isinstance(job, dict) or job.get("if") != expected_if or job.get("runs-on") != runner:
            errors.append(
                f"{ISOLATION_WORKFLOW}: owner job {job_id!r} must run on {runner} with "
                f"if: {expected_if!r} (trusted main, weekly, dispatch — never pull requests)"
            )
        job = jobs.get(job_id)
        steps = job.get("steps") if isinstance(job, dict) else None
        if not isinstance(steps, list):
            continue
        if "defaults" in job:
            errors.append(
                f"{ISOLATION_WORKFLOW}: job {job_id!r} ({runner}) defaults would rewrite its fatal steps; remove them"
            )
        positions: list[int] = []
        for name, commands in ISOLATION_STEPS:
            matches = [
                (index, step) for index, step in enumerate(steps)
                if isinstance(step, dict) and step.get("name") == name
            ]
            where = f"{ISOLATION_WORKFLOW}: job {job_id!r} ({runner}) fatal step {name!r}"
            if len(matches) != 1:
                errors.append(f"{where} must occur exactly once; actual={len(matches)}")
                continue
            index, step = matches[0]
            positions.append(index)
            if step.get("if") is not None:
                errors.append(f"{where} must run unconditionally; actual if={step.get('if')!r}")
            if step.get("continue-on-error") not in (None, False):
                errors.append(f"{where} must gate failure")
            for key in ("shell", "working-directory"):
                if key in step:
                    errors.append(f"{where} must use the default {key}; actual {key}={step[key]!r}")
            actual_commands = shell_commands(step.get("run"))
            if actual_commands != commands:
                errors.append(
                    f"{where} commands changed; expected={commands!r} actual={actual_commands!r}"
                )
        if positions != sorted(positions):
            errors.append(f"{ISOLATION_WORKFLOW}: job {job_id!r} ({runner}) fatal steps are out of order")
    return errors


def runner_label_errors(documents: dict[str, dict[str, Any]]) -> list[str]:
    """Pin the reviewed runner-label inventory; a misspelled label queues forever."""
    errors: list[str] = []
    for workflow, expected in EXPECTED_RUNNERS.items():
        document = documents.get(workflow)
        if document is None:
            continue
        jobs = document.get("jobs")
        labels = {
            job.get("runs-on") for job in jobs.values()
            if isinstance(job, dict)
        } if isinstance(jobs, dict) else set()
        if labels != set(expected):
            errors.append(
                f"{workflow}: runner label inventory changed; "
                f"expected={sorted(expected)!r} actual={sorted(labels)!r}"
            )
    return errors


def main_only_job_errors(documents: dict[str, dict[str, Any]]) -> list[str]:
    """Platform suites and release preflights stay trusted-main-only, never PR gates."""
    errors: list[str] = []
    for (workflow, job_id), reason in MAIN_ONLY_JOBS.items():
        job = documents.get(workflow, {}).get("jobs", {}).get(job_id)
        if not isinstance(job, dict) or job.get("if") != MAIN_ONLY_IF:
            errors.append(
                f"{workflow}: job {job_id!r} must stay main-only ({MAIN_ONLY_IF!r}); {reason}"
            )
    return errors


def ordinary_ci_supervision_errors(documents: dict[str, dict[str, Any]]) -> list[str]:
    """Keep supervised isolation out of ordinary CI except the trusted-main cache warm."""
    errors = required_step_errors(
        documents, "ci.yml", "test-macos", SUPERVISOR_WARM_STEP,
        None, None, SUPERVISOR_WARM_COMMANDS,
    )
    jobs = documents.get("ci.yml", {}).get("jobs")
    for job_id, job in (jobs.items() if isinstance(jobs, dict) else ()):
        steps = job.get("steps") if isinstance(job, dict) else None
        for step in steps if isinstance(steps, list) else ():
            if not isinstance(step, dict) or step.get("name") == SUPERVISOR_WARM_STEP:
                continue
            for command in shell_commands(step.get("run")):
                if any(marker in command for marker in SUPERVISED_MARKERS):
                    errors.append(
                        f"ci.yml: job {job_id!r} step {step.get('name')!r} runs supervised "
                        f"isolation work in ordinary CI; move it to {ISOLATION_WORKFLOW}: {command!r}"
                    )
    return errors


def escape_api_errors(root: Path) -> list[str]:
    """Scan every Rust and shell source for process-group/session escapes outside the allowlist."""
    actual: list[str] = []
    for top in ESCAPE_API_ROOTS:
        base = root / top
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*")):
            relative = path.relative_to(root).as_posix()
            if path.suffix not in (".rs", ".sh") or not path.is_file() or relative == ESCAPE_API_SELF:
                continue
            for line in path.read_text(encoding="utf-8", errors="replace").split("\n"):
                if ESCAPE_API.search(line):
                    actual.append(f"{relative}:{line.lstrip()}")
    remaining = list(actual)
    missing = []
    for entry in ESCAPE_API_ALLOWLIST:
        if entry in remaining:
            remaining.remove(entry)
        else:
            missing.append(entry)
    if not remaining and not missing:
        return []
    return [
        "escape-API allowlist changed (setsid/setpgid/process_group/set -m); "
        f"unauthorized={remaining!r} missing_allowlisted={missing!r}"
    ]


def cancellation_controller_errors(documents: dict[str, dict[str, Any]]) -> list[str]:
    """Check the effective privileged envelope, not a line-set/regex oracle.

    Whitespace-equivalent duplicate YAML keys such as ``if : false`` or
    ``actions : read`` are already rejected by ``load_yaml``. This function
    then checks the parsed mapping GitHub would execute: one workflow_run
    trigger, exact cancel permissions, and the required job condition.
    """
    document = documents.get(CANCELLATION_WORKFLOW)
    if document is None:
        return []
    errors: list[str] = []
    display = CANCELLATION_WORKFLOW
    if document.get("name") != "Cancel superseded CI runs":
        errors.append(
            f"{display}: workflow name changed; "
            f"expected='Cancel superseded CI runs' actual={document.get('name')!r}"
        )
    triggers = document.get("on")
    if not isinstance(triggers, dict) or set(triggers) != {"workflow_run"}:
        errors.append(
            f"{display}: trusted controller must subscribe only to workflow_run; "
            f"actual={triggers!r}"
        )
    elif triggers.get("workflow_run") != CANCELLATION_WORKFLOW_RUN:
        errors.append(
            f"{display}: workflow_run trigger changed; "
            f"expected={CANCELLATION_WORKFLOW_RUN!r} actual={triggers.get('workflow_run')!r}"
        )
    if document.get("permissions") != CANCELLATION_PERMISSIONS:
        errors.append(
            f"{display}: effective permissions changed; "
            f"expected={CANCELLATION_PERMISSIONS!r} actual={document.get('permissions')!r}"
        )
    if "concurrency" in document:
        errors.append(
            f"{display}: concurrency groups cannot encode source-age ordering; remove them"
        )
    jobs = document.get("jobs")
    if not isinstance(jobs, dict) or tuple(jobs) != (CANCELLATION_JOB,):
        actual = list(jobs) if isinstance(jobs, dict) else jobs
        errors.append(
            f"{display}: exactly one job {CANCELLATION_JOB!r} is required; actual={actual!r}"
        )
        return errors
    job = jobs[CANCELLATION_JOB]
    if not isinstance(job, dict):
        errors.append(f"{display}: job {CANCELLATION_JOB!r} must be a mapping")
        return errors
    if job.get("if") != CANCELLATION_JOB_IF:
        errors.append(
            f"{display}: job {CANCELLATION_JOB!r} condition changed; "
            f"expected={CANCELLATION_JOB_IF!r} actual={job.get('if')!r}"
        )
    if job.get("runs-on") != "ubuntu-24.04":
        errors.append(
            f"{display}: job {CANCELLATION_JOB!r} must run on ubuntu-24.04; "
            f"actual={job.get('runs-on')!r}"
        )
    if job.get("timeout-minutes") != 5:
        errors.append(
            f"{display}: job {CANCELLATION_JOB!r} timeout-minutes must be 5; "
            f"actual={job.get('timeout-minutes')!r}"
        )
    if "permissions" in job:
        errors.append(
            f"{display}: job {CANCELLATION_JOB!r} must inherit workflow permissions; "
            f"actual={job.get('permissions')!r}"
        )
    if "concurrency" in job:
        errors.append(f"{display}: job {CANCELLATION_JOB!r} must not set a concurrency group")
    if job.get("continue-on-error") not in (None, False):
        errors.append(f"{display}: job {CANCELLATION_JOB!r} must gate failure")
    steps = job.get("steps")
    if not isinstance(steps, list) or len(steps) != 1 or not isinstance(steps[0], dict):
        errors.append(f"{display}: job {CANCELLATION_JOB!r} must contain exactly one trusted step")
        return errors
    step = steps[0]
    if step.get("name") != CANCELLATION_STEP:
        errors.append(f"{display}: trusted step name changed; actual={step.get('name')!r}")
    if "uses" in step:
        errors.append(
            f"{display}: trusted controller must not checkout or run an action; "
            f"uses={step.get('uses')!r}"
        )
    if step.get("env") != {"TOKEN": CANCELLATION_TOKEN}:
        errors.append(
            f"{display}: trusted step token binding changed; "
            f"expected={{'TOKEN': {CANCELLATION_TOKEN!r}}} actual={step.get('env')!r}"
        )
    run = step.get("run")
    if not isinstance(run, str) or "python3 - <<'PYTHON'" not in run or not run.rstrip().endswith("PYTHON"):
        errors.append(f"{display}: trusted step must run one literal PYTHON heredoc")
    if step.get("if") is not None:
        errors.append(
            f"{display}: trusted step must run whenever the job runs; "
            f"actual if={step.get('if')!r}"
        )
    if step.get("continue-on-error") not in (None, False):
        errors.append(f"{display}: trusted step must gate failure")
    return errors


def breakage_controller_errors(documents: dict[str, dict[str, Any]]) -> list[str]:
    """Check the effective privileged envelope of the main breakage filer.

    One workflow_run subscription, exact read/write split (Actions read to list
    failed jobs, Issues write to file and close), exactly one trusted heredoc
    step, and the pinned issue title and label that give the dedupe its stable
    identity across runs.
    """
    document = documents.get(BREAKAGE_WORKFLOW)
    if document is None:
        return []
    errors: list[str] = []
    display = BREAKAGE_WORKFLOW
    if document.get("name") != "CI main breakage":
        errors.append(
            f"{display}: workflow name changed; "
            f"expected='CI main breakage' actual={document.get('name')!r}"
        )
    triggers = document.get("on")
    if not isinstance(triggers, dict) or set(triggers) != {"workflow_run"}:
        errors.append(
            f"{display}: trusted controller must subscribe only to workflow_run; "
            f"actual={triggers!r}"
        )
    elif triggers.get("workflow_run") != BREAKAGE_WORKFLOW_RUN:
        errors.append(
            f"{display}: workflow_run trigger changed; "
            f"expected={BREAKAGE_WORKFLOW_RUN!r} actual={triggers.get('workflow_run')!r}"
        )
    if document.get("permissions") != BREAKAGE_PERMISSIONS:
        errors.append(
            f"{display}: effective permissions changed; "
            f"expected={BREAKAGE_PERMISSIONS!r} actual={document.get('permissions')!r}"
        )
    if "concurrency" in document:
        errors.append(
            f"{display}: concurrency groups cannot encode completion ordering; remove them"
        )
    if "defaults" in document:
        errors.append(
            f"{display}: workflow-level defaults would rewrite the trusted step; remove them"
        )
    jobs = document.get("jobs")
    if not isinstance(jobs, dict) or tuple(jobs) != (BREAKAGE_JOB,):
        actual = list(jobs) if isinstance(jobs, dict) else jobs
        errors.append(
            f"{display}: exactly one job {BREAKAGE_JOB!r} is required; actual={actual!r}"
        )
        return errors
    job = jobs[BREAKAGE_JOB]
    if not isinstance(job, dict):
        errors.append(f"{display}: job {BREAKAGE_JOB!r} must be a mapping")
        return errors
    if job.get("if") != BREAKAGE_JOB_IF:
        errors.append(
            f"{display}: job {BREAKAGE_JOB!r} condition changed; "
            f"expected={BREAKAGE_JOB_IF!r} actual={job.get('if')!r}"
        )
    if job.get("runs-on") != "ubuntu-24.04":
        errors.append(
            f"{display}: job {BREAKAGE_JOB!r} must run on ubuntu-24.04; "
            f"actual={job.get('runs-on')!r}"
        )
    if job.get("timeout-minutes") != 5:
        errors.append(
            f"{display}: job {BREAKAGE_JOB!r} timeout-minutes must be 5; "
            f"actual={job.get('timeout-minutes')!r}"
        )
    if "permissions" in job:
        errors.append(
            f"{display}: job {BREAKAGE_JOB!r} must inherit workflow permissions; "
            f"actual={job.get('permissions')!r}"
        )
    if "concurrency" in job:
        errors.append(f"{display}: job {BREAKAGE_JOB!r} must not set a concurrency group")
    if job.get("continue-on-error") not in (None, False):
        errors.append(f"{display}: job {BREAKAGE_JOB!r} must gate failure")
    steps = job.get("steps")
    for step in steps if isinstance(steps, list) else ():
        if isinstance(step, dict) and "uses" in step:
            errors.append(
                f"{display}: trusted controller must not checkout or run an action; "
                f"uses={step.get('uses')!r}"
            )
    if not isinstance(steps, list) or len(steps) != 1 or not isinstance(steps[0], dict):
        errors.append(f"{display}: job {BREAKAGE_JOB!r} must contain exactly one trusted step")
        return errors
    step = steps[0]
    if step.get("name") != BREAKAGE_STEP:
        errors.append(f"{display}: trusted step name changed; actual={step.get('name')!r}")
    if "uses" in step:
        errors.append(
            f"{display}: trusted controller must not checkout or run an action; "
            f"uses={step.get('uses')!r}"
        )
    if step.get("env") != {"TOKEN": BREAKAGE_TOKEN}:
        errors.append(
            f"{display}: trusted step token binding changed; "
            f"expected={{'TOKEN': {BREAKAGE_TOKEN!r}}} actual={step.get('env')!r}"
        )
    run = step.get("run")
    if not isinstance(run, str) or "python3 - <<'PYTHON'" not in run or not run.rstrip().endswith("PYTHON"):
        errors.append(f"{display}: trusted step must run one literal PYTHON heredoc")
    else:
        for pinned, what in (
            (BREAKAGE_API_DEFAULT, "API default accepted-status set"),
            (BREAKAGE_ALLOWED_RUNS, "run allowlist"),
            (BREAKAGE_ISSUE_TITLES, "breakage issue titles"),
            (BREAKAGE_LABEL, "breakage issue label"),
        ):
            if pinned not in run:
                errors.append(f"{display}: trusted step {what} changed; expected {pinned!r}")
    if step.get("if") is not None:
        errors.append(
            f"{display}: trusted step must run whenever the job runs; "
            f"actual if={step.get('if')!r}"
        )
    if step.get("continue-on-error") not in (None, False):
        errors.append(f"{display}: trusted step must gate failure")
    return errors


def compare_contract(root: Path) -> list[str]:
    directory = root / WORKFLOWS
    actual_files = tuple(sorted(path.name for path in directory.glob("*.y*ml")))
    errors: list[str] = []
    if actual_files != EXPECTED_WORKFLOWS:
        errors.append(f"workflow inventory changed; expected={EXPECTED_WORKFLOWS!r} actual={actual_files!r}")
    parsed: dict[str, tuple[dict[str, tuple[str, ...]], tuple[str, ...]]] = {}
    documents: dict[str, dict[str, Any]] = {}
    for name in sorted(set(actual_files) & set(EXPECTED_WORKFLOWS)):
        display = (WORKFLOWS / name).as_posix()
        try:
            document = load_yaml(directory / name)
            documents[name] = document
            contract = pull_request_contract(document, display)
            if contract is False:
                continue
            checks = expanded_checks(document, display)
            parsed[name] = (contract, checks)
        except ContractError as error:
            errors.append(str(error))
    actual_pr = set(parsed)
    expected_pr = set(EXPECTED_PATHS)
    if actual_pr != expected_pr:
        errors.append(f"PR-active workflow inventory changed; expected={sorted(expected_pr)!r} actual={sorted(actual_pr)!r}")
    for name in sorted(actual_pr & expected_pr):
        contract, checks = parsed[name]
        expected_contract = dict(EXPECTED_PULL_REQUEST_OPTIONS[name])
        if EXPECTED_PATHS[name] is not None:
            expected_contract["paths"] = EXPECTED_PATHS[name]
        for key in sorted(set(contract) | set(expected_contract)):
            actual_value = contract.get(key)
            expected_value = expected_contract.get(key)
            equal = (
                set(actual_value or ()) == set(expected_value or ())
                if key == "paths" else actual_value == expected_value
            )
            if not equal:
                errors.append(
                    f"{WORKFLOWS / name}: pull_request.{key} changed; "
                    f"expected={expected_value!r} actual={actual_value!r}"
                )
        expected = tuple(sorted(EXPECTED_CHECKS[name]))
        if checks != expected:
            errors.append(f"{WORKFLOWS / name}: expanded check allocation changed; expected={expected!r} actual={checks!r}")
    all_checks = [check for _, checks in parsed.values() for check in checks]
    duplicates = sorted({name for name in all_checks if all_checks.count(name) > 1})
    if duplicates:
        errors.append(f"PR-active workflows have duplicate expanded check names: {duplicates}")
    for fixture, (changed, expected) in EXPECTED_FIXTURES.items():
        try:
            actual = tuple(sorted(
                check for contract, checks in parsed.values() if workflow_activates(contract, changed)
                for check in checks
            ))
        except ContractError as error:
            errors.append(f"fixture {fixture!r}: {error}")
            continue
        duplicates = sorted({name for name in actual if actual.count(name) > 1})
        if duplicates:
            errors.append(f"fixture {fixture!r}: duplicate check names: {duplicates}")
        wanted = tuple(sorted(expected))
        if actual != wanted:
            errors.append(
                f"fixture {fixture!r}: expected check names/count changed; "
                f"expected_count={len(wanted)} actual_count={len(actual)} "
                f"missing={sorted(set(wanted) - set(actual))!r} "
                f"unexpected={sorted(set(actual) - set(wanted))!r}"
            )
    errors.extend(migrated_boundary_errors(documents))
    errors.extend(main_only_job_errors(documents))
    errors.extend(runner_label_errors(documents))
    errors.extend(isolation_errors(documents))
    errors.extend(ordinary_ci_supervision_errors(documents))
    errors.extend(cache_contract_errors(documents))
    errors.extend(cancellation_controller_errors(documents))
    errors.extend(breakage_controller_errors(documents))
    errors.extend(escape_api_errors(root))
    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=ROOT)
    arguments = parser.parse_args()
    errors = compare_contract(arguments.root.resolve())
    if errors:
        for error in errors:
            print(f"CI workflow contract: {error}", file=sys.stderr)
        return 1
    print("CI workflow contract: semantic inventory and check allocation passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
