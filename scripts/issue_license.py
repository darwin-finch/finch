#!/usr/bin/env python3
"""
Finch commercial license key issuer.

Generates a signed FINCH-... key for a customer.

Usage:
    python scripts/issue_license.py <email> <name> [--years 1]

    # Load private key from 1Password (recommended):
    python scripts/issue_license.py buyer@example.com "Jane Doe"

    # Or point at a local PEM file:
    python scripts/issue_license.py buyer@example.com "Jane Doe" \
        --key ~/.finch/license_private.pem

Private key: stored in 1Password as "Finch License Signing Key (Ed25519)"
  Retrieve: op item get "Finch License Signing Key (Ed25519)" \
                --vault Employee --fields private_key --reveal

Requirements:
    pip install cryptography
"""

import argparse
import base64
import json
import os
import subprocess
import sys
from datetime import date, timedelta


def load_private_key_from_1password():
    """Fetch the private key PEM from 1Password via the op CLI."""
    try:
        result = subprocess.run(
            [
                "op", "item", "get",
                "Finch License Signing Key (Ed25519)",
                "--vault", "Employee",
                "--fields", "private_key",
                "--reveal",
            ],
            capture_output=True,
            text=True,
            check=True,
        )
        pem = result.stdout.strip()
        if not pem:
            raise RuntimeError("op returned empty output")
        return pem.encode()
    except FileNotFoundError:
        raise RuntimeError(
            "1Password CLI (op) not found. Install with: brew install 1password-cli\n"
            "Or use --key ~/.finch/license_private.pem"
        )
    except subprocess.CalledProcessError as e:
        raise RuntimeError(
            f"op failed (are you signed in? run: op signin):\n{e.stderr.strip()}"
        )


def load_private_key_from_file(path: str):
    """Load private key PEM from a file on disk."""
    path = os.path.expanduser(path)
    if not os.path.exists(path):
        raise RuntimeError(f"Key file not found: {path}")
    with open(path, "rb") as f:
        return f.read()


def issue_key(email: str, name: str, years: int, private_key_pem: bytes) -> str:
    """Sign a license payload and return a FINCH-... key string."""
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    from cryptography.hazmat.primitives.serialization import load_pem_private_key

    private_key: Ed25519PrivateKey = load_pem_private_key(
        private_key_pem, password=None
    )

    today = date.today()
    expiry = today + timedelta(days=365 * years)

    payload = json.dumps(
        {
            "sub": email,
            "name": name,
            "tier": "commercial",
            "iss": today.isoformat(),
            "exp": expiry.isoformat(),
        },
        separators=(",", ":"),
    ).encode()

    signature = private_key.sign(payload)

    def b64url(data: bytes) -> str:
        return base64.urlsafe_b64encode(data).rstrip(b"=").decode()

    return f"FINCH-{b64url(payload)}.{b64url(signature)}"


def main():
    parser = argparse.ArgumentParser(
        description="Issue a Finch commercial license key."
    )
    parser.add_argument("email", help="Customer email address")
    parser.add_argument("name", help="Customer display name (quoted if it has spaces)")
    parser.add_argument(
        "--years",
        type=int,
        default=1,
        help="License duration in years (default: 1)",
    )
    parser.add_argument(
        "--key",
        metavar="PATH",
        default=None,
        help="Path to private key PEM file (default: load from 1Password)",
    )
    args = parser.parse_args()

    # Load private key
    try:
        if args.key:
            pem = load_private_key_from_file(args.key)
            source = args.key
        else:
            pem = load_private_key_from_1password()
            source = "1Password"
    except RuntimeError as e:
        print(f"error: {e}", file=sys.stderr)
        sys.exit(1)

    # Generate key
    try:
        key = issue_key(args.email, args.name, args.years, pem)
    except ImportError:
        print(
            "error: cryptography package not installed. Run: pip install cryptography",
            file=sys.stderr,
        )
        sys.exit(1)
    except Exception as e:
        print(f"error: failed to sign key: {e}", file=sys.stderr)
        sys.exit(1)

    expiry = (date.today() + timedelta(days=365 * args.years)).isoformat()

    print(key)
    print(f"\n# Licensee:  {args.name} <{args.email}>", file=sys.stderr)
    print(f"# Expires:   {expiry}", file=sys.stderr)
    print(f"# Key from:  {source}", file=sys.stderr)


if __name__ == "__main__":
    main()
