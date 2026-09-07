#!/usr/bin/env python3
"""Mutation-sensitive regressions for Finch's canonical CI cache boundary."""

from __future__ import annotations

import hashlib
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_ci_cache_contract.rb"
FIXTURES = (
    ".github/workflows/ci.yml",
    ".github/workflows/release.yml",
    ".github/workflows/issue-185-spreadsheet-advisories.yml",
    ".github/workflows/issue-186-ssh-removal.yml",
    ".gitignore",
    "Cargo.lock",
    "scripts/configure_ci_cargo_home.sh",
    "scripts/configure_ci_sccache.sh",
    "scripts/stop_ci_sccache.sh",
    "scripts/ci_native_cache_key.py",
    "scripts/configure_ci_cargo_audit.sh",
    "scripts/ci_rustc_cache_wrapper.sh",
)


class Repository:
    def __init__(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        for relative in FIXTURES:
            target = self.root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / relative, target)

    def close(self) -> None:
        self.temp.cleanup()

    def replace(self, relative: str, old: str, new: str) -> None:
        path = self.root / relative
        contents = path.read_text(encoding="utf-8")
        if old not in contents:
            raise AssertionError(f"missing mutation source {old!r} in {relative}")
        path.write_text(contents.replace(old, new, 1), encoding="utf-8")

    def append_ci_job(self, run: str) -> None:
        path = self.root / ".github/workflows/ci.yml"
        with path.open("a", encoding="utf-8") as workflow:
            workflow.write(
                "\n  unexpected-cargo:\n"
                "    runs-on: ubuntu-24.04\n"
                "    steps:\n"
                "    - run: |\n"
                + "\n".join(f"        {line}" for line in run.splitlines())
                + "\n"
            )

    def run(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["ruby", str(CHECKER), str(self.root)],
            check=False,
            capture_output=True,
            text=True,
        )


class WorkflowContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = Repository()

    def tearDown(self) -> None:
        self.repo.close()

    def reject(self, message: str) -> None:
        result = self.repo.run()
        output = result.stdout + result.stderr
        self.assertEqual(result.returncode, 1, output)
        self.assertIn(message, output, output)

    def test_current_contract_passes(self) -> None:
        result = self.repo.run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("trusted-main-only 4x256MiB", result.stdout)

    def test_requires_tracked_lock(self) -> None:
        (self.repo.root / "Cargo.lock").unlink()
        self.reject("Cargo.lock must be tracked")

    def test_tracked_lock_cannot_be_ignored(self) -> None:
        with (self.repo.root / ".gitignore").open("a", encoding="utf-8") as ignore:
            ignore.write("\nCargo.lock\n")
        self.reject("Cargo.lock must not be ignored")

    def test_rejects_online_lock_generation_before_restore(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "    - name: Restore exact Cargo downloads\n",
            "    - run: cargo generate-lockfile\n\n    - name: Restore exact Cargo downloads\n",
        )
        self.reject("tracked Cargo.lock rather than online lock generation")

    def test_producer_restore_is_lookup_only(self) -> None:
        self.repo.replace(".github/workflows/ci.yml", "        lookup-only: true\n", "")
        self.reject("producer restore must be lookup-only")

    def test_producer_never_restores_fallback_union(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "        lookup-only: true",
            "        lookup-only: true\n        restore-keys: cargo-downloads-v2-",
        )
        self.reject("producer must not restore a fallback union")

    def test_consumer_requires_safe_trusted_fallback(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "        restore-keys: |\n          cargo-downloads-v2-",
            "        restore-keys: |\n          cargo-downloads-unsafe-",
        )
        self.reject("consumer conservative restore prefix is wrong")

    def test_wrong_download_key_prints_offending_key(self) -> None:
        self.repo.replace(".github/workflows/ci.yml", "cargo-downloads-v2-", "cargo-downloads-wrong-")
        self.reject('wrong download key "cargo-downloads-wrong-')

    def test_dependency_restore_failure_is_nonfatal(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "      continue-on-error: true\n      with:\n        path: |\n          ${{ runner.temp }}/finch-cargo-home/registry/cache",
            "      continue-on-error: false\n      with:\n        path: |\n          ${{ runner.temp }}/finch-cargo-home/registry/cache",
        )
        self.reject("download restore must be nonfatal")

    def test_producer_save_requires_exact_trusted_predicate(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "steps.cargo-downloads.outputs.cache-hit != 'true' && github.event_name == 'push'",
            "github.event_name == 'pull_request' && steps.cargo-downloads.outputs.cache-hit != 'true' && github.event_name == 'push'",
        )
        self.reject("save trust/miss condition is wrong")

    def test_alternate_cache_action_is_rejected(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "    - name: Build binary\n",
            "    - uses: Swatinem/rust-cache@v2\n\n    - name: Build binary\n",
        )
        self.reject("Swatinem/rust-cache@v2")

    def test_unreviewed_local_action_is_rejected(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "    - name: Build binary\n",
            "    - uses: ./unreviewed-build-action\n\n    - name: Build binary\n",
        )
        self.reject("./unreviewed-build-action")

    def test_mutable_cache_action_is_rejected(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "actions/cache/restore@0057852bfaa89a56745cba8c7296529d2fc39830",
            "actions/cache/restore@v4",
        )
        self.reject("not an approved immutable full-SHA pin")

    def test_mutable_toolchain_action_is_rejected(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "dtolnay/rust-toolchain@62ae3a85dbdd2bedbb5819da8ce45635129289a1",
            "dtolnay/rust-toolchain@1.98.0",
        )
        self.reject("not an approved immutable full-SHA pin")

    def test_checkout_credentials_are_not_persisted(self) -> None:
        self.repo.replace(".github/workflows/ci.yml", "        persist-credentials: false", "        persist-credentials: true")
        self.reject("checkout must set persist-credentials: false")

    def test_pr_never_restores_compiler_objects(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "      if: github.event_name == 'push' && github.ref == 'refs/heads/main'\n      uses: actions/cache/restore@",
            "      if: github.event_name == 'pull_request'\n      uses: actions/cache/restore@",
        )
        self.reject("must be trusted-main-only")

    def test_release_never_restores_compiler_objects(self) -> None:
        self.repo.replace(
            ".github/workflows/release.yml",
            "      - name: Build release binary\n",
            "      - run: scripts/configure_ci_sccache.sh\n\n      - name: Build release binary\n",
        )
        self.reject("tag builds must not restore or execute compiler caches")

    def test_remote_sccache_backend_is_rejected(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "    - name: Build binary\n",
            "    - run: echo SCCACHE_GHA_ENABLED=true\n\n    - name: Build binary\n",
        )
        self.reject("must not expose GitHub cache credentials to sccache")

    def test_github_env_authority_injection_is_rejected(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "    - name: Build binary\n",
            '    - run: echo "RUSTC_WRAPPER=evil" >> "$GITHUB_ENV"\n\n    - name: Build binary\n',
        )
        self.reject("configure compiler authority only inside reviewed helpers")

    def test_object_key_requires_256m_cap(self) -> None:
        self.repo.replace(".github/workflows/ci.yml", "cap-256m", "cap-unbounded")
        self.reject("object key")

    def test_object_key_requires_native_identity(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "native-${{ steps.native-cache.outputs.digest }}",
            "native-unknown",
        )
        self.reject("object key")

    def test_object_save_requires_successful_stop_measurement(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            " && steps.sccache-stop.outputs.save-ready == 'true'",
            "",
        )
        self.reject("object save trust/order predicate is wrong")

    def test_runtime_consumer_cannot_save_compiler_objects(self) -> None:
        marker = "  build:\n"
        insertion = (
            "    - name: Illicit runtime save\n"
            "      uses: actions/cache/save@0057852bfaa89a56745cba8c7296529d2fc39830\n"
            "      with:\n"
            "        path: ${{ runner.temp }}/finch-sccache-cache\n\n"
        )
        self.repo.replace(".github/workflows/ci.yml", marker, insertion + marker)
        self.reject("expected 0 compiler-object save; found 1")

    def test_test_matrix_preserves_bounded_writer_cardinality(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "            feature_name: no-default-features\n            cargo_args: --no-default-features",
            "            feature_name: default\n            cargo_args: --no-default-features",
        )
        self.reject("exactly one compiler-cache writer lane per OS")

    def test_release_matrix_matches_trusted_main_targets(self) -> None:
        self.repo.replace(
            ".github/workflows/release.yml",
            "            target: aarch64-apple-darwin",
            "            target: x86_64-apple-darwin",
        )
        self.reject("release matrix must match")

    def test_ci_jobs_cannot_elevate_permissions(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "  security:\n    name: Security Audit",
            "  security:\n    permissions:\n      contents: write\n    name: Security Audit",
        )
        self.reject("must not elevate workflow permissions")

    def test_release_writer_profile_matches_tag(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            '      CARGO_PROFILE_RELEASE_LTO: "false"',
            '      CARGO_PROFILE_RELEASE_LTO: "true"',
        )
        self.reject("trusted release writer profile does not match tag build")

    def test_sccache_linux_asset_digest_is_fixed(self) -> None:
        self.repo.replace(
            "scripts/configure_ci_sccache.sh",
            "67c4a96dd237c1f518f6b36083f270f9976d516f1e57fce891755ea782e50006",
            "0" * 64,
        )
        self.reject("67c4a96d")

    def test_sccache_macos_asset_digest_is_fixed(self) -> None:
        self.repo.replace(
            "scripts/configure_ci_sccache.sh",
            "0c560bfba31aef5bdfb4fb3d2677f6e61d71c5c00952f2a83344f47aa31f00f1",
            "f" * 64,
        )
        self.reject("0c560bfb")

    def test_sccache_archive_member_validation_is_required(self) -> None:
        self.repo.replace("scripts/configure_ci_sccache.sh", "archive has unexpected members", "archive accepted")
        self.reject("archive has unexpected members")

    def test_sccache_download_is_size_bounded(self) -> None:
        self.repo.replace("scripts/configure_ci_sccache.sh", "--max-filesize 8388608", "")
        self.reject("--max-filesize 8388608")

    def test_sccache_local_cap_is_required(self) -> None:
        self.repo.replace("scripts/configure_ci_sccache.sh", "SCCACHE_CACHE_SIZE=256M", "SCCACHE_CACHE_SIZE=10G")
        self.reject("SCCACHE_CACHE_SIZE=256M")

    def test_sccache_excludes_executable_outputs(self) -> None:
        self.repo.replace(
            "scripts/ci_rustc_cache_wrapper.sh",
            "--test|build_script_build|build-script-build",
            "build_script_build",
        )
        self.reject("--test|build_script_build|build-script-build")

    def test_stop_helper_must_enforce_256m_bound(self) -> None:
        self.repo.replace("scripts/stop_ci_sccache.sh", "262144", "1048576")
        self.reject("262144")

    def test_audit_fixed_digest_install_cannot_be_disabled(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "    - name: Install fixed-digest cargo-audit\n      run:",
            "    - name: Install fixed-digest cargo-audit\n      if: false\n      run:",
        )
        self.reject("fixed-digest audit install must be unconditional")

    def test_audit_verification_cannot_be_disabled(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "    - name: Verify pinned cargo-audit executable\n      run:",
            "    - name: Verify pinned cargo-audit executable\n      if: false\n      run:",
        )
        self.reject("audit verification must be unconditional")

    def test_audit_executable_cannot_be_compiled(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "      run: scripts/configure_ci_cargo_audit.sh",
            "      run: cargo install cargo-audit --version 0.22.2 --locked",
        )
        self.reject("must not compile or cache the cargo-audit executable")

    def test_audit_binary_digest_is_fixed(self) -> None:
        self.repo.replace(
            "scripts/configure_ci_cargo_audit.sh",
            "ab28a1bdb54db4d5d8ad5981cf1f959410370b3d28250dbd35f6a44248620e39",
            "0" * 64,
        )
        self.reject("ab28a1bd")

    def test_audit_archive_member_validation_is_required(self) -> None:
        self.repo.replace(
            "scripts/configure_ci_cargo_audit.sh",
            "archive has unexpected members",
            "archive accepted",
        )
        self.reject("archive has unexpected members")

    def test_audit_download_is_size_bounded(self) -> None:
        self.repo.replace("scripts/configure_ci_cargo_audit.sh", "--max-filesize 8388608", "")
        self.reject("--max-filesize 8388608")

    def test_release_creation_selects_repository_without_checkout(self) -> None:
        self.repo.replace(".github/workflows/release.yml", "          GH_REPO: ${{ github.repository }}\n", "")
        self.reject("must set GH_REPO to github.repository")

    def test_issue_185_uses_tracked_lock(self) -> None:
        self.repo.replace(
            ".github/workflows/issue-185-spreadsheet-advisories.yml",
            "cargo tree --locked",
            "cargo tree",
        )
        self.reject("must consume the tracked lock")

    def test_issue_186_cannot_rewrite_tracked_lock(self) -> None:
        self.repo.replace(
            ".github/workflows/issue-186-ssh-removal.yml",
            "          git diff --exit-code -- Cargo.lock\n",
            "          cargo generate-lockfile\n",
        )
        self.reject("must validate rather than rewrite the tracked lock")

    def test_issue_186_builds_cannot_update_tracked_lock(self) -> None:
        self.repo.replace(
            ".github/workflows/issue-186-ssh-removal.yml",
            "cargo build --locked --release",
            "cargo build --release",
        )
        self.reject("Cargo graph/build commands must use --locked")

    def test_issue_workflows_cannot_compile_cargo_audit(self) -> None:
        self.repo.replace(
            ".github/workflows/issue-185-spreadsheet-advisories.yml",
            "scripts/configure_ci_cargo_audit.sh",
            "cargo install cargo-audit --locked",
        )
        self.reject("must use the fixed-digest cargo-audit helper")

    def test_discovers_bash_wrapped_cargo(self) -> None:
        self.repo.append_ci_job('bash -c "cargo test --all-targets"')
        self.reject("new compiling Cargo job is outside the cache contract")

    def test_discovers_login_shell_wrapped_cargo(self) -> None:
        self.repo.append_ci_job('/bin/bash -lc "cargo test --all-targets"')
        self.reject("new compiling Cargo job is outside the cache contract")

    def test_discovers_command_substitution_cargo(self) -> None:
        self.repo.append_ci_job('echo "$(cargo test --all-targets)"')
        self.reject("new compiling Cargo job is outside the cache contract")

    def test_harmless_cargo_metadata_text_is_not_compile(self) -> None:
        self.repo.append_ci_job('echo "cargo test is the documented test command"')
        result = self.repo.run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


class HelperBoundaryTests(unittest.TestCase):
    def test_rustc_wrapper_bypasses_executables_and_caches_reusable_objects(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fake_rustc = root / "rustc"
            fake_sccache = root / "sccache"
            fake_rustc.write_text("#!/bin/sh\nprintf 'rustc\\n'\n", encoding="utf-8")
            fake_sccache.write_text("#!/bin/sh\nprintf 'sccache\\n'\n", encoding="utf-8")
            fake_rustc.chmod(0o755)
            fake_sccache.chmod(0o755)
            env = os.environ | {"SCCACHE_PATH": str(fake_sccache)}
            wrapper = ROOT / "scripts/ci_rustc_cache_wrapper.sh"

            for arguments in (["--crate-type", "bin"], ["--test"], ["--crate-name", "build_script_build"]):
                result = subprocess.run(
                    [str(wrapper), str(fake_rustc), *arguments],
                    env=env,
                    check=False,
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(result.stdout, "rustc\n", f"executable arguments were cached: {arguments!r}")

            reusable = subprocess.run(
                [str(wrapper), str(fake_rustc), "--crate-type", "rlib"],
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(reusable.returncode, 0, reusable.stdout + reusable.stderr)
            self.assertEqual(reusable.stdout, "sccache\n", "reusable rlib did not pass through sccache")

    def test_native_identity_hashes_runner_and_native_tool_versions(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            identities = {
                "capnp": "Cap'n Proto version 1.2.3",
                "cc": "Finch test cc 4.5.6",
                "ld": "Finch test ld 7.8.9",
            }
            for name, identity in identities.items():
                executable = fake_bin / name
                executable.write_text(f'#!/bin/sh\nprintf \'%s\\n\' "{identity}"\n', encoding="utf-8")
                executable.chmod(0o755)
            output = root / "output"
            env = os.environ | {
                "PATH": f"{fake_bin}:{os.environ['PATH']}",
                "RUNNER_OS": "Linux",
                "RUNNER_ARCH": "X64",
                "GITHUB_OUTPUT": str(output),
            }
            result = subprocess.run(
                [str(ROOT / "scripts/ci_native_cache_key.py")],
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )
            expected = hashlib.sha256(
                "\0".join(["Linux", "X64", *identities.values()]).encode()
            ).hexdigest()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(output.read_text(encoding="utf-8"), f"digest={expected}\n")
            self.assertIn(f"Linux/X64 {expected}", result.stdout)

    def test_pr_setup_exits_before_download_or_environment_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            github_env = root / "env"
            github_path = root / "path"
            github_output = root / "output"
            env = os.environ | {
                "RUNNER_TEMP": temporary,
                "GITHUB_WORKSPACE": str(ROOT),
                "GITHUB_ENV": str(github_env),
                "GITHUB_PATH": str(github_path),
                "GITHUB_OUTPUT": str(github_output),
                "GITHUB_EVENT_NAME": "pull_request",
                "GITHUB_REF": "refs/pull/384/merge",
                "RUNNER_OS": "Linux",
                "RUNNER_ARCH": "X64",
            }
            result = subprocess.run(
                [str(ROOT / "scripts/configure_ci_sccache.sh")],
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertFalse(github_env.exists(), github_env)
            self.assertFalse(github_output.exists(), github_output)

    def test_stop_helper_reports_save_ready_only_below_cap(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            sccache = fake_bin / "sccache"
            sccache.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            sccache.chmod(0o755)
            du = fake_bin / "du"
            du.write_text("#!/bin/sh\necho '42 cache'\n", encoding="utf-8")
            du.chmod(0o755)
            cache = root / "cache"
            cache.mkdir()
            output = root / "output"
            env = os.environ | {
                "PATH": f"{fake_bin}:{os.environ['PATH']}",
                "SCCACHE_PATH": str(sccache),
                "SCCACHE_DIR": str(cache),
                "GITHUB_OUTPUT": str(output),
            }
            result = subprocess.run(
                [str(ROOT / "scripts/stop_ci_sccache.sh")], env=env, check=False, capture_output=True, text=True
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("save-ready=true", output.read_text(encoding="utf-8"))
            self.assertIn("cache-size-kib=42", output.read_text(encoding="utf-8"))

    def test_stop_helper_refuses_cache_over_cap(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            for name, body in {
                "sccache": "#!/bin/sh\nexit 0\n",
                "du": "#!/bin/sh\necho '262145 cache'\n",
            }.items():
                path = fake_bin / name
                path.write_text(body, encoding="utf-8")
                path.chmod(0o755)
            cache = root / "cache"
            cache.mkdir()
            output = root / "output"
            env = os.environ | {
                "PATH": f"{fake_bin}:{os.environ['PATH']}",
                "SCCACHE_PATH": str(fake_bin / "sccache"),
                "SCCACHE_DIR": str(cache),
                "GITHUB_OUTPUT": str(output),
            }
            result = subprocess.run(
                [str(ROOT / "scripts/stop_ci_sccache.sh")], env=env, check=False, capture_output=True, text=True
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertNotIn("save-ready=true", output.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
