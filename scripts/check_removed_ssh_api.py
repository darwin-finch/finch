#!/usr/bin/env python3
"""Compile a downstream positive control, then prove finch::ssh is unresolved."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
EXPECTED = "error[E0432]: unresolved import `finch::ssh`"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--no-default-features",
        action="store_true",
        help="probe Finch with its default Cargo features disabled",
    )
    return parser.parse_args()


def cargo_check(manifest: Path, environment: dict[str, str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["cargo", "check", "--manifest-path", str(manifest)],
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        env=environment,
    )


def main() -> int:
    args = parse_args()
    with tempfile.TemporaryDirectory(prefix="finch-ssh-api-probe-") as temporary:
        probe = Path(temporary)
        source = probe / "src/main.rs"
        source.parent.mkdir()
        manifest = probe / "Cargo.toml"
        feature_setting = ", default-features = false" if args.no_default_features else ""
        manifest.write_text(
            "[package]\n"
            'name = "finch-ssh-api-probe"\n'
            'version = "0.0.0"\n'
            'edition = "2021"\n'
            "publish = false\n\n"
            "[dependencies]\n"
            f"finch = {{ path = {json.dumps(str(ROOT))}{feature_setting} }}\n",
            encoding="utf-8",
        )
        environment = os.environ.copy()
        environment["CARGO_TERM_COLOR"] = "never"

        source.write_text(
            "fn main() { let _ = finch::ipc::IPC_PROTOCOL_VERSION; }\n",
            encoding="utf-8",
        )
        positive = cargo_check(manifest, environment)
        if positive.returncode != 0:
            print("Known-public-symbol positive control did not compile", file=sys.stderr)
            print(positive.stderr, file=sys.stderr)
            return 1

        source.write_text("use finch::ssh;\nfn main() {}\n", encoding="utf-8")
        negative = cargo_check(manifest, environment)
        if negative.returncode == 0:
            print("The removed finch::ssh public API compiled successfully", file=sys.stderr)
            return 1
        if EXPECTED not in negative.stderr:
            print("Negative probe failed without the expected unresolved import", file=sys.stderr)
            print(negative.stderr, file=sys.stderr)
            return 1

    feature_name = "no-default-features" if args.no_default_features else "default"
    print(
        f"Downstream API probe ({feature_name}): "
        "finch::ssh is absent after a successful positive control"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
