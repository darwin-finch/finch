//! Compatibility tools for native macOS automation.
//!
//! The implementation lives in `runtime::automation` so direct tools and VM
//! programs use the same availability checks and host API path.

use crate::runtime::automation::{AutomationBroker, AutomationRequest};
use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GuiClickTool;

#[async_trait]
impl Tool for GuiClickTool {
    fn name(&self) -> &str {
        "gui_click"
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
        Ok(AutomationBroker::new(true).execute(request)?.to_string())
    }
}

pub struct GuiTypeTool;

#[async_trait]
impl Tool for GuiTypeTool {
    fn name(&self) -> &str {
        "gui_type"
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
        Ok(AutomationBroker::new(true).execute(request)?.to_string())
    }
}

pub struct GuiInspectTool;

#[async_trait]
impl Tool for GuiInspectTool {
    fn name(&self) -> &str {
        "gui_inspect"
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
        Ok(AutomationBroker::new(true).execute(request)?.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspect_schema_does_not_advertise_applescript_fallback() {
        let schema = GuiInspectTool.input_schema();
        let encoded = serde_json::to_string(&schema).unwrap();
        assert!(!encoded.contains("focused"));
        assert!(!encoded.contains("osascript"));
    }
}
