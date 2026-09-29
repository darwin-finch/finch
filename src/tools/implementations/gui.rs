//! Compatibility tools for native macOS automation.
//!
//! The implementation lives in the runtime automation broker so direct tools and VM
//! programs use the same availability checks and host API path.

use crate::runtime::{AutomationBroker, AutomationRequest};
use crate::tools::types::{ToolContext, ToolInputSchema};
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use finch_programs::ExecutionEffect;
use serde_json::{json, Value};

/// `enabled` mirrors `config.features.gui_automation` at registration time
/// (#421: the tool now always registers on macOS, so this flag — not
/// registration itself — is what tells `AutomationBroker` whether to report
/// `AutomationState::Disabled` versus checking real Accessibility trust).
pub struct GuiClickTool {
    enabled: bool,
}

impl GuiClickTool {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

#[async_trait]
impl Tool for GuiClickTool {
    fn name(&self) -> &str {
        "gui_click"
    }

    fn effect(&self) -> ExecutionEffect {
        ExecutionEffect::ExternalWrite
    }

    fn description(&self) -> &str {
        "Click native macOS screen coordinates through Finch's automation broker. Requires the gui_automation feature and Accessibility consent."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "x": { "type": "number", "description": "Screen X coordinate" },
                "y": { "type": "number", "description": "Screen Y coordinate" },
                "button": {
                    "type": "string",
                    "enum": ["left", "right", "middle"],
                    "default": "left"
                },
                "count": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 3,
                    "default": 1
                }
            }),
            required: vec!["x".to_string(), "y".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let request = AutomationRequest::Click {
            x: input["x"].as_f64().context("gui_click: missing x")?,
            y: input["y"].as_f64().context("gui_click: missing y")?,
            button: input["button"].as_str().unwrap_or("left").to_string(),
            count: input["count"].as_u64().unwrap_or(1).try_into()?,
        };
        Ok(AutomationBroker::new(self.enabled)
            .execute(request)?
            .to_string())
    }
}

/// `enabled` mirrors `config.features.gui_automation`; see `GuiClickTool`'s
/// doc comment for why registration and the flag are now separate.
pub struct GuiTypeTool {
    enabled: bool,
}

impl GuiTypeTool {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

#[async_trait]
impl Tool for GuiTypeTool {
    fn name(&self) -> &str {
        "gui_type"
    }

    fn effect(&self) -> ExecutionEffect {
        ExecutionEffect::ExternalWrite
    }

    fn description(&self) -> &str {
        "Type text through Finch's native macOS automation broker. Requires the gui_automation feature and Accessibility consent."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "text": { "type": "string", "description": "Text to type" },
                "delay_ms": { "type": "integer", "minimum": 0, "default": 0 }
            }),
            required: vec!["text".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let request = AutomationRequest::Type {
            text: input["text"]
                .as_str()
                .context("gui_type: missing text")?
                .to_string(),
            delay_ms: input["delay_ms"].as_u64().unwrap_or(0),
        };
        Ok(AutomationBroker::new(self.enabled)
            .execute(request)?
            .to_string())
    }
}

/// `enabled` mirrors `config.features.gui_automation`; see `GuiClickTool`'s
/// doc comment for why registration and the flag are now separate. Unlike
/// the other two tools, `query: "availability"` reports state even when
/// disabled — that is the whole point of this tool existing unconditionally.
pub struct GuiInspectTool {
    enabled: bool,
}

impl GuiInspectTool {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

#[async_trait]
impl Tool for GuiInspectTool {
    fn name(&self) -> &str {
        "gui_inspect"
    }

    fn effect(&self) -> ExecutionEffect {
        ExecutionEffect::WorkspaceRead
    }

    fn description(&self) -> &str {
        "Inspect displays, on-screen window IDs, or automation availability through native macOS APIs. Does not invoke AppleScript or a shell."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "query": {
                    "type": "string",
                    "enum": ["availability", "screen", "windows"]
                }
            }),
            required: vec!["query".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let request = match input["query"]
            .as_str()
            .context("gui_inspect: missing query")?
        {
            "availability" => AutomationRequest::Availability,
            "screen" => AutomationRequest::Displays,
            "windows" => AutomationRequest::Windows,
            other => anyhow::bail!("gui_inspect: unsupported query '{other}'"),
        };
        Ok(AutomationBroker::new(self.enabled)
            .execute(request)?
            .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspect_schema_does_not_advertise_applescript_fallback() {
        let schema = GuiInspectTool::new(true).input_schema();
        let encoded = serde_json::to_string(&schema).unwrap();
        assert!(!encoded.contains("focused"));
        assert!(!encoded.contains("osascript"));
    }

    #[tokio::test]
    async fn test_gui_click_reports_disabled_setting_not_absence_when_flag_is_off() {
        // #421: registration is now unconditional on macOS; the flag only
        // controls what AutomationBroker reports. A disabled tool must name
        // the setting that turns it on, not merely fail generically.
        let tool = GuiClickTool::new(false);
        let input = json!({"x": 10.0, "y": 20.0});
        let err = tool
            .execute(input, &ToolContext::default())
            .await
            .expect_err("gui_click must fail, not silently succeed, while gui_automation is off");
        let message = err.to_string();
        assert!(
            message.contains("disabled") && message.contains("configuration"),
            "expected the disabled-by-configuration message from \
             AutomationAvailability::unavailable_message(), got: {message}"
        );
    }

    #[tokio::test]
    async fn test_gui_inspect_availability_reports_disabled_state_even_when_flag_is_off() {
        // The one query that must always succeed regardless of `enabled`,
        // so a model can discover *why* the other two tools will fail
        // before attempting them.
        let tool = GuiInspectTool::new(false);
        let input = json!({"query": "availability"});
        let result = tool
            .execute(input, &ToolContext::default())
            .await
            .expect("gui_inspect availability query must succeed even when disabled");
        assert!(
            result.contains("\"disabled\""),
            "expected the availability JSON to report state=disabled, got: {result}"
        );
    }
}
