# Finch UI manifest — generated TypeScript types

The `.ts` files here are **machine-generated** by
[ts-rs](https://github.com/Aleph-Alpha/ts-rs) from the Rust structs in
[`crates/finch-tui/src/dom_manifest.rs`](../../src/dom_manifest.rs); they are
regenerated whenever `cargo test -p finch-tui --lib` runs. Do not edit them
by hand — change the Rust structs, run the lib tests, and commit the
regenerated output together with the schema-doc bump.

- `ui_manifest/` — the wire types (`UiManifest`, `DynamicUiNode`,
  `ManifestSpan`, `ManifestColor`) plus the ts-rs-generated
  `serde_json/JsonValue` shim.
- `index.ts` — a barrel for consumers.

The wire contract, its version, and the JSON schema live in
[`docs/UI_MANIFEST.md`](../../../docs/UI_MANIFEST.md). The golden JSON
snapshot the tests pin is
[`crates/finch-tui/tests/fixtures/ui_manifest_say_card.json`](../../tests/fixtures/ui_manifest_say_card.json).