# License System

**Purpose:** Offline Ed25519 commercial license key validation.

## Key format

```
FINCH-<base64url(JSON payload)>.<base64url(Ed25519 signature)>
```

**Payload:**
```json
{"sub":"user@example.com","name":"Jane Doe","tier":"commercial","iss":"2026-01-15","exp":"2027-01-15"}
```

## Validation flow (offline, no network required)

1. Strip `FINCH-` prefix
2. Split on `.` → payload_b64, sig_b64
3. Decode base64url — **returns `Err`, never panics on malformed input**
4. Verify Ed25519 signature against embedded public key
5. Parse JSON; check `exp` date against today
6. Return `ParsedLicense` with name, email, expiry

**Enforcement:** Honor system — no blocking; weekly startup notice to Noncommercial users.

## CLI / REPL commands

```bash
finch license status
finch license activate --key <FINCH-...>
finch license remove
```

REPL: `/license`, `/license status`, `/license activate <key>`, `/license remove`

## Issuing keys

See `~/.claude/CLAUDE.md` for credentials and step-by-step instructions.

## Key files

- `src/license/mod.rs` — `validate_key()`, `validate_key_with_vk()`, `ParsedLicense`; 8 unit tests
- `src/config/settings.rs` — `LicenseConfig`, `LicenseType`
- `scripts/issue_license.py` — key signing script (requires `cryptography` pip package)
