#!/usr/bin/env python3
"""Regression tests for scripts/install.sh destination resolution (#889).

The installer must upgrade the finch that wins PATH resolution in place,
fall back to the default only when no install exists, keep FINCH_INSTALL_DIR
as the override, and list any further installs instead of leaving them
silent. The script's download/verify flow is preserved; these tests exercise
the real installer offline through a fake PATH layout: a fake curl serves a
prebuilt release tarball and a fake sudo emulates or declines the
privileged write.
"""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
INSTALLER = ROOT / "scripts/install.sh"
STALE_VERSION = "finch 0.0.1-stale"
NEW_VERSION = "finch 9.9.9-fixture"
DEFAULT_DEST = Path("/usr/local/bin")

FINCH_STUB = """\
#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "{version}"
fi
exit 0
"""

CURL_STUB = """\
#!/bin/sh
out=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "-o" ]; then out="$arg"; fi
  prev="$arg"
done
cp "$FAKE_CURL_FIXTURE" "$out"
"""

SUDO_STUB = """\
#!/bin/sh
# Emulate the two outcomes that matter to the installer: denial (exit 1) and
# success (the privileged move, approximated by making the destination
# directory writable first, which is what root-level write access achieves).
if [ "${FAKE_SUDO_MODE:-fail}" != "succeed" ]; then
  exit 1
fi
dst="$3"
chmod u+w "$(dirname "$dst")"
shift
exec mv "$@"
"""


class InstallerSandbox:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.fakebin = self.root / "fakebin"
        self.fakebin.mkdir()
        self.release_tarball = self._build_release_tarball()
        self._write_stub(self.fakebin / "curl", CURL_STUB)
        self._write_stub(self.fakebin / "sudo", SUDO_STUB)

    def close(self) -> None:
        self.temporary.cleanup()

    def _write_stub(self, path: Path, body: str) -> None:
        path.write_text(body)
        path.chmod(path.stat().st_mode | 0o111)

    def _build_release_tarball(self) -> Path:
        payload = self.root / "release-payload"
        payload.mkdir()
        finch = payload / "finch"
        finch.write_text(FINCH_STUB.format(version=NEW_VERSION))
        finch.chmod(finch.stat().st_mode | 0o111)
        tarball = self.root / "release.tar.gz"
        subprocess.run(
            ["tar", "-czf", str(tarball), "-C", str(payload), "finch"],
            check=True,
        )
        return tarball

    def install_stale_finch(self, directory: Path) -> Path:
        directory.mkdir(parents=True, exist_ok=True)
        finch = directory / "finch"
        finch.write_text(FINCH_STUB.format(version=STALE_VERSION))
        finch.chmod(finch.stat().st_mode | 0o111)
        return finch

    def path_env(self, *first_dirs: Path) -> str:
        entries = [str(d) for d in first_dirs]
        entries.append(str(self.fakebin))
        entries.extend(["/usr/bin", "/bin"])
        return ":".join(entries)

    def run(self, *args: str, finch_install_dir: Path | None = None,
            sudo_mode: str = "fail", path_dirs: tuple[Path, ...] = ()) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        env.pop("FINCH_INSTALL_DIR", None)
        env["PATH"] = self.path_env(*path_dirs)
        env["HOME"] = str(self.root / "home")
        env["FAKE_CURL_FIXTURE"] = str(self.release_tarball)
        env["FAKE_SUDO_MODE"] = sudo_mode
        if finch_install_dir is not None:
            env["FINCH_INSTALL_DIR"] = str(finch_install_dir)
        return subprocess.run(
            ["bash", str(INSTALLER), *args],
            env=env,
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )


class InstallLocationResolutionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.env = InstallerSandbox()

    def tearDown(self) -> None:
        self.env.close()

    def safe_chmod(self, path: Path, mode: int) -> None:
        if path.exists():
            path.chmod(mode)

    def test_no_existing_install_resolves_to_default_dir(self) -> None:
        result = self.env.run("--resolve")

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("dest=/usr/local/bin\n", result.stdout)
        self.assertIn("source=default\n", result.stdout)
        self.assertIn("hit=none\n", result.stdout)
        self.assertIn("others=\n", result.stdout, result.stdout)

    def test_writable_path_hit_resolves_to_that_location(self) -> None:
        first = self.env.root / "first-bin"
        self.env.install_stale_finch(first)

        result = self.env.run("--resolve", path_dirs=(first,))

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f"dest={first}\n", result.stdout)
        self.assertIn("source=existing install on PATH\n", result.stdout)
        self.assertIn(f"hit={first / 'finch'}\n", result.stdout)
        self.assertIn("writable=yes\n", result.stdout)

    def test_writable_path_hit_is_upgraded_in_place(self) -> None:
        first = self.env.root / "first-bin"
        stale = self.env.install_stale_finch(first)

        result = self.env.run(path_dirs=(first,))

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("upgrading in place", result.stdout, result.stdout)
        self.assertIn(f"{first / 'finch'} — upgrading in place", result.stdout, result.stdout)
        upgraded = (first / "finch").read_text()
        self.assertIn(
            NEW_VERSION, upgraded,
            f"PATH hit was not replaced with the release payload; contents:\n{upgraded}",
        )
        self.assertNotIn("stale", upgraded, upgraded)
        self.assertTrue(stale.exists())

    def test_nonwritable_path_hit_fails_instead_of_falling_back_to_default(self) -> None:
        first = self.env.root / "first-bin"
        stale = self.env.install_stale_finch(first)
        first.chmod(0o555)
        self.addCleanup(self.safe_chmod, first, 0o755)
        if os.access(first, os.W_OK):
            self.skipTest("destination directory is still writable (running as root?)")

        result = self.env.run(path_dirs=(first,))

        self.assertNotEqual(
            result.returncode, 0,
            "a non-writable PATH hit with sudo declined must fail, not fall back "
            f"to the default; stdout:\n{result.stdout}stderr:\n{result.stderr}",
        )
        self.assertIn(str(first), result.stderr, result.stdout + result.stderr)
        self.assertIn("FINCH_INSTALL_DIR", result.stderr, result.stderr)
        self.assertNotIn("installed →", result.stdout, result.stdout)
        self.assertIn(
            STALE_VERSION, stale.read_text(),
            "the failed install must not have touched the existing binary",
        )

    def test_nonwritable_path_hit_upgrades_in_place_via_sudo(self) -> None:
        first = self.env.root / "first-bin"
        self.env.install_stale_finch(first)
        first.chmod(0o555)
        self.addCleanup(self.safe_chmod, first, 0o755)
        if os.access(first, os.W_OK):
            self.skipTest("destination directory is still writable (running as root?)")

        result = self.env.run(path_dirs=(first,), sudo_mode="succeed")

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        upgraded = (first / "finch").read_text()
        self.assertIn(
            NEW_VERSION, upgraded,
            f"sudo path did not upgrade the PATH hit in place; contents:\n{upgraded}",
        )

    def test_install_dir_override_wins_and_lists_existing_path_installs(self) -> None:
        first = self.env.root / "first-bin"
        self.env.install_stale_finch(first)
        override = self.env.root / "override-bin"
        override.mkdir()

        result = self.env.run(path_dirs=(first,), finch_install_dir=override)

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        installed = (override / "finch").read_text()
        self.assertIn(
            NEW_VERSION, installed,
            f"FINCH_INSTALL_DIR override was not honoured; contents:\n{installed}",
        )
        self.assertIn(
            STALE_VERSION, (first / "finch").read_text(),
            "the override must leave the existing PATH install untouched",
        )
        self.assertIn("other finch installs on PATH", result.stdout, result.stdout)
        self.assertIn(str(first / "finch"), result.stdout, result.stdout)

    def test_multiple_installs_upgrade_first_hit_and_list_others(self) -> None:
        first = self.env.root / "first-bin"
        second = self.env.root / "second-bin"
        self.env.install_stale_finch(first)
        self.env.install_stale_finch(second)

        resolve = self.env.run("--resolve", path_dirs=(first, second))
        install = self.env.run(path_dirs=(first, second))

        self.assertEqual(resolve.returncode, 0, resolve.stdout + resolve.stderr)
        self.assertIn(f"hit={first / 'finch'}\n", resolve.stdout, resolve.stdout)
        self.assertIn(
            f"others={second / 'finch'}\n", resolve.stdout,
            f"resolver must list the non-winning installs; stdout:\n{resolve.stdout}",
        )
        self.assertEqual(install.returncode, 0, install.stdout + install.stderr)
        upgraded = (first / "finch").read_text()
        self.assertIn(
            NEW_VERSION, upgraded,
            f"the first PATH hit was not upgraded; contents:\n{upgraded}",
        )
        untouched = (second / "finch").read_text()
        self.assertIn(
            STALE_VERSION, untouched,
            f"the non-winning install must stay untouched; contents:\n{untouched}",
        )
        self.assertIn("other finch installs on PATH", install.stdout, install.stdout)
        self.assertIn(str(second / "finch"), install.stdout, install.stdout)


if __name__ == "__main__":
    unittest.main()