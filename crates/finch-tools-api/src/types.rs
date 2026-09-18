//! Core types for the tool execution surface.
//!
//! Wire-compatible with the provider tool-use format. The context carries
//! only what the API can name; application-bound per-call state arrives
//! through the [`HostModeState`] and [`EffectAuditAuthority`] ports the
//! composition root injects.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::vm_effect::VmEffectEnvelope;
use finch_vm::VmSideEffect;
use std::sync::Arc;
use tokio::sync::RwLock;

pub use finch_providers::{ToolDefinition, ToolInputSchema, ToolUse};

/// Per-tool presentation binding. Plain tools stream text lines; typed VM
/// programs may additionally deliver portable, structured UI effects. The
/// coordinator supplies the concrete shadow-buffer projection.
pub trait LiveOutputSink: Send + Sync {
    fn line(&self, text: String);

    /// Preserve the historical live-text callback behavior for hosts that do
    /// not care about richer UI events. Specialized sinks override this to
    /// project the complete portable event stream.
    fn vm_side_effect(&self, effect: VmSideEffect) {
        if let finch_vm::HostSideEffect::Emit { text } = effect.event {
            self.line(text);
        }
    }

    /// Receive an event together with its ProgramRun identity. Hosts that do
    /// not need durable handles can keep implementing `vm_side_effect`; a
    /// proposal/IDE adapter can override this to retain `(execution_id,
    /// sequence)` for an explicit later resume.
    fn vm_effect_envelope(&self, envelope: VmEffectEnvelope) {
        self.vm_side_effect(envelope.effect);
    }

    /// True only for a host that owns the explicit proposal lifecycle and can
    /// resume deferred program effects. Plain output projections must leave
    /// this false so compatibility editor behavior remains available.
    fn defer_program_effects(&self) -> bool {
        false
    }
}

impl<F> LiveOutputSink for F
where
    F: Fn(String) + Send + Sync,
{
    fn line(&self, text: String) {
        self(text);
    }
}

pub type LiveOutput = Arc<dyn LiveOutputSink>;

/// Handle to the host's live session mode state.
///
/// The concrete state is the session's live REPL mode, which this API surface
/// cannot name: approval flows read its autonomy policy, and plan-mode tools
/// read and transition it. The composition root injects a handle wrapping the
/// same shared lock the REPL holds, so carriers observe the live mode exactly
/// as a direct field read did. Carriers downcast through [`Self::as_any`]
/// into the concrete handle; a downcast failure is a wiring error, never a
/// silently absent mode.
pub trait HostModeState: Send + Sync {
    /// Downcast support: the concrete mode state is application-bound.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Opaque daemon-issued authority for physical effects in one named-Brain
/// provider/tool loop. Ordinary sessions and non-program tools receive
/// `None`; provenance fields can never manufacture this capability.
///
/// The concrete authority type is application-bound (the runtime's
/// `RunnerEffectAuditControl`, constructed only inside the runtime). Carriers
/// reach it through [`Self::as_any`] and must treat a downcast failure as an
/// error, never as an absent authority.
pub trait EffectAuditAuthority: Send + Sync {
    /// Downcast support: the concrete authority type is application-bound.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Context passed to tools during execution
pub struct ToolContext<'a> {
    /// Optional function to save model weights (for restart tools)
    pub save_models: Option<&'a (dyn Fn() -> Result<()> + Send + Sync)>,

    /// Optional plan content storage
    pub plan_content: Option<Arc<RwLock<Option<String>>>>,

    /// Optional live-output callback for streaming tools (e.g. bash).
    /// Called once per output line while the tool is running.
    /// Allows the WorkUnit row to show a live scrolling preview.
    pub live_output: Option<LiveOutput>,

    /// Host session-mode state injected by the composition root (the live
    /// REPL mode today). Approval flows consult it before opening an
    /// interactive review; plan-mode tools read and transition it.
    pub host_mode_state: Option<Arc<dyn HostModeState>>,

    /// Opaque daemon-issued authority for physical effects in one named-Brain
    /// provider/tool loop. Ordinary sessions and non-program tools receive
    /// `None`; provenance fields can never manufacture this capability.
    pub effect_audit: Option<Arc<dyn EffectAuditAuthority>>,

    /// True when the REPL (or another caller) already obtained approval.
    ///
    /// The TUI dialog includes "Yes, and don't ask again for: edit:*". After
    /// that grant, `$EDITOR` must not open again — that second review is what
    /// blocked autonomous iteration. AutoAccept sets this too.
    pub skip_interactive_review: bool,
}

impl Default for ToolContext<'_> {
    fn default() -> Self {
        Self {
            save_models: None,
            plan_content: None,
            live_output: None,
            host_mode_state: None,
            effect_audit: None,
            skip_interactive_review: false,
        }
    }
}

/// Tool execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(tool_use_id: String, content: String) -> Self {
        Self {
            tool_use_id,
            content,
            is_error: false,
        }
    }

    pub fn error(tool_use_id: String, error_message: String) -> Self {
        Self {
            tool_use_id,
            content: error_message,
            is_error: true,
        }
    }
}

/// Extended ContentBlock enum to support tool use
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

impl ContentBlock {
    /// Check if this is a text block
    pub fn is_text(&self) -> bool {
        matches!(self, ContentBlock::Text { .. })
    }

    /// Check if this is a tool use block
    pub fn is_tool_use(&self) -> bool {
        matches!(self, ContentBlock::ToolUse { .. })
    }

    /// Check if this is a tool result block
    pub fn is_tool_result(&self) -> bool {
        matches!(self, ContentBlock::ToolResult { .. })
    }

    /// Extract text from text block
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentBlock::Text { text } => Some(text),
            _ => None,
        }
    }

    /// Extract tool use from tool use block
    pub fn as_tool_use(&self) -> Option<ToolUse> {
        match self {
            ContentBlock::ToolUse { id, name, input } => Some(ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_use_id_generation() {
        let id = ToolUse::generate_id();
        assert!(id.starts_with("toolu_"));
        assert_eq!(id.len(), 30); // "toolu_" + 24 chars
    }

    #[test]
    fn test_tool_result_success() {
        let result = ToolResult::success("toolu_123".to_string(), "Success".to_string());
        assert_eq!(result.tool_use_id, "toolu_123");
        assert_eq!(result.content, "Success");
        assert!(!result.is_error);
    }

    #[test]
    fn test_tool_result_error() {
        let result = ToolResult::error("toolu_123".to_string(), "Failed".to_string());
        assert_eq!(result.tool_use_id, "toolu_123");
        assert_eq!(result.content, "Failed");
        assert!(result.is_error);
    }

    #[test]
    fn test_content_block_text_serialization() {
        let block = ContentBlock::Text {
            text: "Hello".to_string(),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"text\":\"Hello\""));
    }

    #[test]
    fn test_content_block_tool_use_serialization() {
        let block = ContentBlock::ToolUse {
            id: "toolu_123".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({"command": "ls"}),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"tool_use\""));
        assert!(json.contains("\"name\":\"bash\""));
    }

    #[test]
    fn test_simple_input_schema() {
        let schema = ToolInputSchema::simple(vec![
            ("file_path", "The path to the file to read"),
            ("encoding", "The file encoding (utf-8, ascii, etc.)"),
        ]);

        assert_eq!(schema.schema_type, "object");
        assert_eq!(schema.required.len(), 2);
        assert!(schema.required.contains(&"file_path".to_string()));
        assert!(schema.required.contains(&"encoding".to_string()));
    }

    #[test]
    fn test_tool_use_id_uniqueness() {
        let ids: Vec<String> = (0..20).map(|_| ToolUse::generate_id()).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "All generated IDs must be unique");
    }

    #[test]
    fn test_tool_use_id_prefix() {
        for _ in 0..10 {
            let id = ToolUse::generate_id();
            assert!(
                id.starts_with("toolu_"),
                "ID must start with 'toolu_': {}",
                id
            );
        }
    }

    #[test]
    fn test_tool_use_new_has_generated_id() {
        let t = ToolUse::new("bash".to_string(), serde_json::json!({"command": "ls"}));
        assert!(t.id.starts_with("toolu_"));
        assert_eq!(t.name, "bash");
        assert_eq!(t.input["command"], "ls");
    }

    #[test]
    fn test_tool_result_success_fields() {
        let r = ToolResult::success("id_1".to_string(), "output".to_string());
        assert_eq!(r.tool_use_id, "id_1");
        assert_eq!(r.content, "output");
        assert!(!r.is_error);
    }

    #[test]
    fn test_tool_result_error_fields() {
        let r = ToolResult::error("id_2".to_string(), "boom".to_string());
        assert_eq!(r.tool_use_id, "id_2");
        assert_eq!(r.content, "boom");
        assert!(r.is_error);
    }

    #[test]
    fn test_content_block_is_text() {
        let text = ContentBlock::Text {
            text: "hi".to_string(),
        };
        assert!(text.is_text());
        assert!(!text.is_tool_use());
        assert!(!text.is_tool_result());
    }

    #[test]
    fn test_content_block_is_tool_use() {
        let tu = ContentBlock::ToolUse {
            id: "x".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({}),
        };
        assert!(tu.is_tool_use());
        assert!(!tu.is_text());
        assert!(!tu.is_tool_result());
    }

    #[test]
    fn test_content_block_is_tool_result() {
        let tr = ContentBlock::ToolResult {
            tool_use_id: "x".to_string(),
            content: "ok".to_string(),
            is_error: None,
        };
        assert!(tr.is_tool_result());
        assert!(!tr.is_text());
        assert!(!tr.is_tool_use());
    }

    #[test]
    fn test_content_block_as_text_some() {
        let block = ContentBlock::Text {
            text: "hello".to_string(),
        };
        assert_eq!(block.as_text(), Some("hello"));
    }

    #[test]
    fn test_content_block_as_text_none_for_non_text() {
        let block = ContentBlock::ToolUse {
            id: "x".to_string(),
            name: "y".to_string(),
            input: serde_json::json!({}),
        };
        assert!(block.as_text().is_none());
    }

    #[test]
    fn test_content_block_as_tool_use_some() {
        let block = ContentBlock::ToolUse {
            id: "call_1".to_string(),
            name: "grep".to_string(),
            input: serde_json::json!({"pattern": "fn main"}),
        };
        let tu = block.as_tool_use().unwrap();
        assert_eq!(tu.id, "call_1");
        assert_eq!(tu.name, "grep");
        assert_eq!(tu.input["pattern"], "fn main");
    }

    #[test]
    fn test_content_block_as_tool_use_none_for_non_tool_use() {
        let block = ContentBlock::Text {
            text: "hi".to_string(),
        };
        assert!(block.as_tool_use().is_none());
    }

    #[test]
    fn test_tool_input_schema_empty_params() {
        let schema = ToolInputSchema::simple(vec![]);
        assert_eq!(schema.schema_type, "object");
        assert!(schema.required.is_empty());
    }

    #[test]
    fn test_tool_input_schema_serialization() {
        let schema = ToolInputSchema::simple(vec![("cmd", "The command")]);
        let json = serde_json::to_string(&schema).unwrap();
        assert!(json.contains("\"type\":\"object\""));
        assert!(json.contains("\"cmd\""));
    }

    #[test]
    fn test_tool_result_serde_roundtrip() {
        let r = ToolResult::success("id_99".to_string(), "hello".to_string());
        let json = serde_json::to_string(&r).unwrap();
        let back: ToolResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tool_use_id, r.tool_use_id);
        assert_eq!(back.content, r.content);
        assert_eq!(back.is_error, r.is_error);
    }
}
