# ui-model capsule: terminal-independent application presentation vocabulary

Supplements the root [`AGENTS.md`](../../AGENTS.md), which still applies in full.

**Owns** `crates/finch-ui-model/src/`: stable retained-message and row identity, semantic transcript-line
metadata, terminal-independent widget data, pure line measurement, and the claiming layout pass.
These are plain data and pure functions shared by application components and render engines.

**Does not own** message/domain state, component-specific projection, terminal lifecycle,
`crossterm`, shadow-buffer painting, input handling, or application composition. Those layers
depend on this capsule; this capsule does not depend back on them.

**Facade:** [`src/lib.rs`](src/lib.rs) is the complete flat facade. There are no public child-module paths.
[`INTERFACE.md`](INTERFACE.md) is generated with
`python3 scripts/generate_interfaces.py --write` and must not be edited by hand.

**Dependencies:** none of Finch's other subsystems. `unicode-width` supplies the same terminal-cell
tables used before extraction, and `uuid` supplies stable message identity.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-ui-model`.
