# oauth capsule: compatibility re-export of provider-neutral OAuth

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** this Finch compatibility facade only. Implementation, dialects, persistence,
and tests live in the private `oauth` module of
[`crates/finch-providers/src/oauth/AGENTS.md`](../../crates/finch-providers/src/oauth/AGENTS.md),
re-exported flat from that crate's facade.

The [README](README.md) traces ChatGPT and Grok CLI callers. [`mod.rs`](mod.rs) is the
facade; rustdoc renders methods on re-exported types. Callers continue to use
`crate::oauth::Item`. Do not add application logic or recreate a signature catalog here.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-providers --lib -- oauth::` and
`./scripts/test_brains.sh cargo test --lib -- oauth::`.
