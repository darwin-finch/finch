# oauth capsule: compatibility re-export of provider-neutral OAuth

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** this Finch compatibility facade only. Implementation, dialects, persistence,
and tests live in [`crates/finch-providers/src/oauth/AGENTS.md`](../../crates/finch-providers/src/oauth/AGENTS.md).

Callers continue to use `crate::oauth::Item`. Do not add application logic here.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-providers --lib -- oauth::` and
`./scripts/test_brains.sh cargo test --lib -- oauth::`.
