# local capsule: local-model pattern classification and response generation

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/local/`: `LocalGenerator`, pattern classification, and template response generation
for the optional local-model path. Model loading belongs to `src/models`; provider-independent
generation contracts belong to `finch-generation`; Finch adapters belong to `src/generators`.

**Facade:** `generator` and `patterns` are private children. Callers outside this directory use
flat `crate::local::{LocalGenerator, GeneratedResponse, TemplateGenerator, PatternClassifier,
QueryPattern}` imports. Do not expose child-module paths as a shortcut.

**Dependencies:** model adapters, generation responses, provider messages, tool wire definitions,
training support, and configuration. This module generates candidate responses; it does not own
tool execution, provider transport, model bootstrap, or application policy.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- local::`. Run
`python3 scripts/check_facade_boundaries.py` whenever the module surface changes, and run the full
supervised workspace suite before extraction.
