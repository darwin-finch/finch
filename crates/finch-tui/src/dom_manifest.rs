//! The DOM manifest: the serializable lowering of the widget tree and the
//! component views, the wire contract for the future Tauri client (#808)
//! (stage 4 of docs/TUI_DESIGN.md, "The DOM boundary", #1141 part 2).
//!
//! The maintainer's ruling: one ViewModel, two lowering methods, no fork.
//! The terminal mode lowers spans to SGR at paint (`span_render`); this mode
//! lowers the SAME tree to a serde `DynamicUiNode`. Components never write
//! HTML — the engine owns both lowerings; a component's own `render_dom`
//! survives only as an override hook, and none is implemented here.
//!
//! The wire shape is a versioned, documented contract (`docs/UI_MANIFEST.md`;
//! generated TS types under `dom/`). Prop keys, element types, and ids are
//! pinned by the golden-JSON test.

use finch_ui_model::{
    Axis, ComponentView, LiveToolView, MessageId, MessageStatus, OperationView, ProgressView,
    RowId, SayTurnStatus, SayTurnView, Span, SpanColor, SpanStyle, StaticTextKind, StaticTextView,
    Track, Widget, WorkRowStatus,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The manifest contract version. Bump on any shape change and update
/// `docs/UI_MANIFEST.md` in the same commit; consumers read this first.
pub const MANIFEST_VERSION: u32 = 1;

/// One node of the serializable UI manifest: an element type (the registry
/// key the JSX side maps to a component), a derived stable id, JSON-valued
/// props, and children. This is the wire format for Tauri (#808 attach).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "dom/ui_manifest/")]
pub struct DynamicUiNode {
    pub element_type: String,
    pub id: String,
    /// JSON-valued props (never pre-rendered strings): a GUI component reads
    /// the semantics and decides its own presentation. `BTreeMap` keeps the
    /// serialized key order stable so golden JSON and diffs are deterministic.
    pub props: std::collections::BTreeMap<String, serde_json::Value>,
    pub children: Vec<DynamicUiNode>,
}

impl DynamicUiNode {
    /// A node with no children.
    pub fn leaf(element_type: impl Into<String>, id: impl Into<String>) -> DynamicUiNode {
        DynamicUiNode {
            element_type: element_type.into(),
            id: id.into(),
            props: std::collections::BTreeMap::new(),
            children: Vec::new(),
        }
    }

    /// Set one JSON prop.
    pub fn with_prop(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> DynamicUiNode {
        self.props.insert(key.into(), value.into());
        self
    }

    /// Append a child node.
    pub fn with_child(mut self, child: DynamicUiNode) -> DynamicUiNode {
        self.children.push(child);
        self
    }
}

/// The versioned envelope: what crosses the wire. Consumers check
/// `manifest_version` before reading `root`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "dom/ui_manifest/")]
pub struct UiManifest {
    pub manifest_version: u32,
    pub root: DynamicUiNode,
}

/// One styled text segment on the wire — the manifest's own shape for a
/// [`Span`], deliberately decoupled from the vocabulary crate so the wire
/// contract carries no Rust dependency. Colours mirror [`SpanColor`]:
/// `indexed` is the xterm 0–15 slot, `rgb` a truecolour triple.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "dom/ui_manifest/")]
pub struct ManifestSpan {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<ManifestColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<ManifestColor>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dim: bool,
}

/// The manifest's colour shape.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "dom/ui_manifest/")]
pub enum ManifestColor {
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl From<SpanColor> for ManifestColor {
    fn from(color: SpanColor) -> ManifestColor {
        match color {
            SpanColor::Indexed(index) => ManifestColor::Indexed(index),
            SpanColor::Rgb(r, g, b) => ManifestColor::Rgb(r, g, b),
        }
    }
}

impl From<&Span> for ManifestSpan {
    fn from(span: &Span) -> ManifestSpan {
        let style: &SpanStyle = &span.style;
        ManifestSpan {
            text: span.text.clone(),
            fg: style.fg.map(ManifestColor::from),
            bg: style.bg.map(ManifestColor::from),
            bold: style.bold,
            dim: style.dim,
        }
    }
}

/// Lower styled spans to the wire shape. A span-free line lowers to `[]`.
pub fn manifest_spans(spans: &[Span]) -> Vec<ManifestSpan> {
    spans.iter().map(ManifestSpan::from).collect()
}

/// The manifest id of a message: the stable message uuid — no identity is
/// minted at lowering time, matching the RowId discipline.
pub fn manifest_id(message_id: &MessageId) -> String {
    message_id.to_string()
}

/// The manifest id of a semantic path under a message: `{uuid}#1.2` — the
/// append-only path segments joined by dots. A single-segment path `[1]` (the
/// say output region, the toggle hit target) reads `{uuid}#1`.
pub fn manifest_path_id(message_id: &MessageId, path: &[u32]) -> String {
    if path.is_empty() {
        manifest_id(message_id)
    } else {
        let segments: Vec<String> = path.iter().map(u32::to_string).collect();
        format!("{}#{}", manifest_id(message_id), segments.join("."))
    }
}

/// The manifest id of a RowId.
pub fn manifest_row_id(row_id: &RowId) -> String {
    manifest_path_id(&row_id.message_id, &row_id.path)
}

/// Lower one widget subtree to a manifest node. The generic lowering for the
/// engine's vocabulary; component-owned subtrees get their element types via
/// [`component_manifest`] (the registry keys a JSX consumer maps).
pub fn widget_manifest(widget: &Widget) -> DynamicUiNode {
    match widget {
        Widget::Stack { axis, children } => {
            let axis = match axis {
                Axis::Column => "column",
                Axis::Row => "row",
            };
            let mut node = DynamicUiNode::leaf("Stack", "").with_prop("axis", axis);
            for (track, child) in children {
                node.children
                    .push(widget_manifest(child).with_prop("track", track_manifest(track)));
            }
            node
        }
        Widget::Text { lines } => DynamicUiNode::leaf("Text", "").with_prop("lines", lines.clone()),
        Widget::Rule => DynamicUiNode::leaf("Rule", ""),
        Widget::Completions { rows } => {
            DynamicUiNode::leaf("Completions", "").with_prop("rows", rows.clone())
        }
        Widget::Composer { input_lines, ghost } => DynamicUiNode::leaf("Composer", "")
            .with_prop("inputLines", input_lines.clone())
            .with_prop(
                "ghost",
                ghost
                    .as_ref()
                    .map(|ghost| serde_json::json!(ghost))
                    .unwrap_or(serde_json::Value::Null),
            ),
        Widget::Viewport { lines } => {
            let children = lines
                .iter()
                .map(|line| {
                    let mut node = DynamicUiNode::leaf("Line", "");
                    if let Some(row_id) = &line.row_id {
                        node.id = manifest_row_id(row_id);
                    }
                    node = node
                        .with_prop("text", line.text.clone())
                        .with_prop("spans", serde_json::json!(manifest_spans(&line.spans)));
                    if let Some(expanded) = line.row_expanded {
                        node = node.with_prop("expanded", expanded);
                    }
                    node
                })
                .collect();
            DynamicUiNode::leaf("Viewport", "").with_children(children)
        }
        Widget::DialogCard { lines } => {
            DynamicUiNode::leaf("DialogCard", "").with_prop("lines", lines.clone())
        }
        Widget::Marked(key, inner) => {
            let mut node = widget_manifest(inner);
            let id = if node.id.is_empty() {
                format!("marked:{key}")
            } else {
                format!("{}:marked:{key}", node.id)
            };
            node.id = id;
            node
        }
    }
}

impl DynamicUiNode {
    fn with_children(mut self, children: Vec<DynamicUiNode>) -> DynamicUiNode {
        self.children = children;
        self
    }
}

/// Lower a component snapshot to its manifest subtree — the component's own
/// element types and props, typed VM data as JSON (the serialization
/// boundary the DOM-boundary ruling draws; components never write HTML).
pub fn component_manifest(view: &ComponentView) -> DynamicUiNode {
    match view {
        ComponentView::Say(say) => say_card_manifest(say),
        ComponentView::StaticText(static_text) => static_text_manifest(static_text),
        ComponentView::Progress(progress) => progress_manifest(progress),
        ComponentView::LiveTool(live_tool) => live_tool_manifest(live_tool),
        ComponentView::Operation(operation) => operation_manifest(operation),
    }
}

/// The say-turn card's manifest: the VM fields a GUI card component reads,
/// with the program and output regions as children carrying their semantic
/// paths (`#0` program, `#1` output — the toggle hit target's path).
pub fn say_card_manifest(view: &SayTurnView) -> DynamicUiNode {
    let status = match view.vm.status {
        SayTurnStatus::Running => "running",
        SayTurnStatus::Completed => "completed",
    };
    let program = DynamicUiNode::leaf("ProgramSource", manifest_path_id(&view.message_id, &[0]))
        .with_prop("language", view.vm.program.language.clone())
        .with_prop("lines", view.vm.program.lines.clone());
    let mut card = DynamicUiNode::leaf("SayTurnCard", manifest_id(&view.message_id))
        .with_prop("status", status)
        .with_prop("elapsedMs", view.elapsed.as_millis() as u64)
        .with_prop("showProgram", view.vm.show_program)
        .with_child(program);
    if let Some(output) = &view.vm.output {
        card = card.with_child(
            DynamicUiNode::leaf("Output", manifest_path_id(&view.message_id, &[1]))
                .with_prop("lines", output.lines.clone()),
        );
    }
    card
}

/// A static text message's manifest: kind + content lines.
pub fn static_text_manifest(view: &StaticTextView) -> DynamicUiNode {
    let kind = match view.kind {
        StaticTextKind::Info => "info",
        StaticTextKind::Error => "error",
        StaticTextKind::Success => "success",
        StaticTextKind::Warning => "warning",
        StaticTextKind::Plain => "plain",
    };
    DynamicUiNode::leaf("StaticText", "")
        .with_prop("kind", kind)
        .with_prop("lines", view.content_lines.clone())
}

/// A progress message's manifest.
pub fn progress_manifest(view: &ProgressView) -> DynamicUiNode {
    DynamicUiNode::leaf("Progress", "")
        .with_prop("label", view.label.clone())
        .with_prop("current", view.current)
        .with_prop("total", view.total)
        .with_prop("status", manifest_status(&view.status))
}

/// A live tool call's manifest.
pub fn live_tool_manifest(view: &LiveToolView) -> DynamicUiNode {
    DynamicUiNode::leaf("LiveTool", "")
        .with_prop("header", view.header.clone())
        .with_prop("lines", view.content_lines.clone())
        .with_prop("status", manifest_status(&view.status))
}

/// An operation message's manifest: chrome header plus one row per call.
pub fn operation_manifest(view: &OperationView) -> DynamicUiNode {
    let rows = view
        .rows
        .iter()
        .map(|row| {
            DynamicUiNode::leaf("OperationRow", "")
                .with_prop("label", row.label.clone())
                .with_prop("status", work_row_status_name(&row.status))
        })
        .collect();
    DynamicUiNode::leaf("Operation", "")
        .with_prop("header", view.header.clone())
        .with_prop("status", manifest_status(&view.status))
        .with_children(rows)
}

fn manifest_status(status: &MessageStatus) -> &'static str {
    match status {
        MessageStatus::InProgress => "in_progress",
        MessageStatus::Complete => "complete",
        MessageStatus::Failed => "failed",
    }
}

fn work_row_status_name(status: &WorkRowStatus) -> serde_json::Value {
    match status {
        WorkRowStatus::Running => serde_json::json!({ "state": "running" }),
        WorkRowStatus::Complete(summary) if summary.is_empty() => {
            serde_json::json!({ "state": "complete" })
        }
        WorkRowStatus::Complete(summary) => {
            serde_json::json!({ "state": "complete", "summary": summary })
        }
        WorkRowStatus::Error(error) => serde_json::json!({ "state": "error", "detail": error }),
    }
}

/// The track as the wire sees it: a tagged object a GUI layout maps onto its
/// own layout vocabulary.
fn track_manifest(track: &Track) -> serde_json::Value {
    match track {
        Track::Natural => serde_json::json!({ "kind": "natural" }),
        Track::Flex { weight, min } => {
            serde_json::json!({ "kind": "flex", "weight": weight, "min": min })
        }
        Track::Max { cap, track } => serde_json::json!({
            "kind": "max",
            "cap": cap,
            "track": track_manifest(track),
        }),
        Track::Side { min_width, track } => serde_json::json!({
            "kind": "side",
            "minWidth": min_width,
            "track": track_manifest(track),
        }),
    }
}

/// Wrap one root node into the versioned manifest envelope.
pub fn ui_manifest(root: DynamicUiNode) -> UiManifest {
    UiManifest {
        manifest_version: MANIFEST_VERSION,
        root,
    }
}

/// The manifest of one component view, enveloped.
pub fn component_ui_manifest(view: &ComponentView) -> UiManifest {
    ui_manifest(component_manifest(view))
}

/// The manifest of one widget subtree, enveloped.
pub fn widget_ui_manifest(widget: &Widget) -> UiManifest {
    ui_manifest(widget_manifest(widget))
}
