// Server-Sent Events parsing for Claude API streaming

use serde::{Deserialize, Serialize};

/// Server-Sent Event from Claude API
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StreamEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub index: Option<usize>,
    pub delta: Option<StreamDelta>,
    pub content_block: Option<ContentBlock>,
}

/// Content block metadata from content_block_start events
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// Delta within a streaming event
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StreamDelta {
    #[serde(rename = "type")]
    pub delta_type: String,
    pub text: Option<String>,
}

impl StreamEvent {
    /// Check if this event contains a text delta
    pub fn is_text_delta(&self) -> bool {
        self.event_type == "content_block_delta"
            && self
                .delta
                .as_ref()
                .map(|d| d.delta_type == "text_delta")
                .unwrap_or(false)
    }

    /// Check if this event signals a tool_use block starting
    pub fn is_tool_use_start(&self) -> bool {
        self.event_type == "content_block_start"
            && self
                .content_block
                .as_ref()
                .map(|cb| cb.block_type == "tool_use")
                .unwrap_or(false)
    }

    /// Extract text from the event if available
    pub fn text(&self) -> Option<&str> {
        self.delta.as_ref()?.text.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_text_delta() {
        let json = r#"{
            "type": "content_block_delta",
            "index": 0,
            "delta": {
                "type": "text_delta",
                "text": "Hello"
            }
        }"#;

        let event: StreamEvent = serde_json::from_str(json).unwrap();
        assert!(event.is_text_delta());
        assert_eq!(event.text(), Some("Hello"));
    }

    #[test]
    fn test_parse_non_text_event() {
        let json = r#"{
            "type": "message_start",
            "message": {}
        }"#;

        let event: StreamEvent = serde_json::from_str(json).unwrap();
        assert!(!event.is_text_delta());
        assert_eq!(event.text(), None);
    }

    #[test]
    fn test_parse_tool_use_start() {
        let json = r#"{
            "type": "content_block_start",
            "index": 0,
            "content_block": {
                "type": "tool_use",
                "id": "toolu_123",
                "name": "read"
            }
        }"#;

        let event: StreamEvent = serde_json::from_str(json).unwrap();
        assert!(event.is_tool_use_start());
        assert!(!event.is_text_delta());
    }

    #[test]
    fn test_parse_text_block_start() {
        let json = r#"{
            "type": "content_block_start",
            "index": 0,
            "content_block": {
                "type": "text"
            }
        }"#;

        let event: StreamEvent = serde_json::from_str(json).unwrap();
        assert!(!event.is_tool_use_start());
        assert!(!event.is_text_delta());
    }
}
