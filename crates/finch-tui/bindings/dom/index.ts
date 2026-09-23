// Generated TS types for the Finch UI manifest wire contract (#1141).
// The `bindings/dom/ui_manifest/*.ts` files are machine-generated from the
// Rust structs by ts-rs (regenerated on `cargo test -p finch-tui --lib`);
// this barrel exists so consumers import from one place.
// The contract is documented in docs/UI_MANIFEST.md; the Rust source of
// truth is crates/finch-tui/src/dom_manifest.rs.
export type { DynamicUiNode } from "./ui_manifest/DynamicUiNode";
export type { ManifestColor } from "./ui_manifest/ManifestColor";
export type { ManifestSpan } from "./ui_manifest/ManifestSpan";
export type { UiManifest } from "./ui_manifest/UiManifest";
export const FINCH_UI_MANIFEST_VERSION = 1;