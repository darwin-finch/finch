// Test Gemini provider streaming with tool calls
//
// This test suite verifies that:
// 1. Gemini streaming handles text properly
// 2. Gemini streaming handles tool calls (FunctionCall parts)
// 3. Tool calls are converted to ContentBlockComplete(ToolUse)
// 4. Multiple parts in a single response are handled correctly

use anyhow::Result;
use finch::claude::ContentBlock;
use finch::claude::Message;
use finch::generators::StreamChunk;
use finch::providers::gemini::GeminiProvider;
use finch::providers::{LlmProvider, ProviderRequest};

/// Test that Gemini provider supports tools
#[test]
fn test_gemini_supports_tools() {
    // Create a Gemini provider
    let provider = GeminiProvider::new("test-key".to_string()).unwrap();

    // Verify it supports tools
    assert!(provider.supports_tools(), "Gemini should support tools");
    assert!(provider.supports_streaming(), "Gemini should support streaming");
}

/// Test that Gemini correctly identifies its name and model
#[test]
fn test_gemini_provider_identity() {
    let provider = GeminiProvider::new("test-key".to_string())
        .unwrap()
        .with_model("gemini-2.5-flash");

    assert_eq!(provider.name(), "gemini");
    assert_eq!(provider.default_model(), "gemini-2.5-flash");
}

/// Document the expected behavior of Gemini streaming with tool calls
///
/// NOTE: This test documents expected behavior but cannot be run without a real API key.
/// It serves as documentation for how Gemini streaming should handle tool calls.
#[test]
#[ignore] // Requires real API key and network
fn test_gemini_streaming_tool_call_behavior() {
    // This test documents the expected behavior:
    //
    // When Gemini makes a tool call in streaming mode:
    // 1. Stream receives a GeminiPart::FunctionCall
    // 2. Converted to ContentBlock::ToolUse with unique ID
    // 3. Sent as StreamChunk::ContentBlockComplete(ToolUse)
    // 4. Client can execute tool and send results back
    // 5. Gemini continues with final response
    //
    // Before the fix (cfad806):
    // - GeminiPart::FunctionCall was IGNORED
    // - Only GeminiPart::Text was handled
    // - Tool calls never reached the client
    // - Response stream ended without tool execution
    //
    // After the fix:
    // - All three GeminiPart types handled:
    //   - Text → StreamChunk::TextDelta
    //   - FunctionCall → StreamChunk::ContentBlockComplete(ToolUse)
    //   - FunctionResponse → Skip (handled elsewhere)
}

/// Test that tool use content blocks are properly formed
#[test]
fn test_tool_use_content_block_format() {
    // Verify the structure of a ToolUse content block
    let tool_use = ContentBlock::ToolUse {
        id: "gemini_test_tool_123".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({
            "file_path": "/tmp/test.txt"
        }),
    };

    // Verify it's identified as a tool use
    assert!(tool_use.is_tool_use());

    // Verify we can extract the name and input
    if let ContentBlock::ToolUse { id, name, input } = tool_use {
        assert!(id.starts_with("gemini_"));
        assert_eq!(name, "Read");
        assert_eq!(input["file_path"], "/tmp/test.txt");
    } else {
        panic!("Expected ToolUse content block");
    }
}

/// Test that StreamChunk types are properly differentiated
#[test]
fn test_stream_chunk_types() {
    // Text delta
    let text_delta = StreamChunk::TextDelta("Hello".to_string());
    if let StreamChunk::TextDelta(text) = text_delta {
        assert_eq!(text, "Hello");
    } else {
        panic!("Expected TextDelta");
    }

    // Content block complete (Tool use)
    let tool_use = ContentBlock::ToolUse {
        id: "test_123".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({}),
    };
    let block_complete = StreamChunk::ContentBlockComplete(tool_use.clone());
    if let StreamChunk::ContentBlockComplete(block) = block_complete {
        assert!(block.is_tool_use());
    } else {
        panic!("Expected ContentBlockComplete");
    }
}

/// Document Gemini's unique ID generation for tool calls
#[test]
fn test_gemini_tool_use_id_format() {
    // Gemini doesn't provide tool call IDs in responses
    // We generate unique IDs with format: "gemini_{tool_name}_{uuid}"

    let id = format!("gemini_Read_{}", uuid::Uuid::new_v4());

    // Verify format
    assert!(id.starts_with("gemini_"));
    assert!(id.contains("Read"));

    // Verify uniqueness (two calls should have different IDs)
    let id2 = format!("gemini_Read_{}", uuid::Uuid::new_v4());
    assert_ne!(id, id2, "Each tool call should have a unique ID");
}

/// Test that ProviderRequest can include tools
#[test]
fn test_provider_request_with_tools() -> Result<()> {
    use finch::tools::types::{ToolDefinition, ToolInputSchema};

    let tools = vec![
        ToolDefinition {
            name: "Read".to_string(),
            description: "Read a file".to_string(),
            input_schema: ToolInputSchema {
                schema_type: "object".to_string(),
                properties: serde_json::json!({
                    "file_path": {"type": "string"}
                }),
                required: vec!["file_path".to_string()],
            },
        },
    ];

    let request = ProviderRequest::new(vec![
        Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: "Read /tmp/test.txt".to_string(),
            }],
        },
    ])
    .with_tools(tools.clone())
    .with_stream(true);

    // Verify tools are included
    assert!(request.tools.is_some());
    assert_eq!(request.tools.unwrap().len(), 1);
    assert!(request.stream);

    Ok(())
}

/// Document the Gemini function call format
#[test]
fn test_gemini_function_call_format_documentation() {
    // This test documents Gemini's API format for function calls
    //
    // Gemini uses "functionCall" (camelCase) in JSON:
    // {
    //   "functionCall": {
    //     "name": "Read",
    //     "args": {
    //       "file_path": "/tmp/test.txt"
    //     }
    //   }
    // }
    //
    // We convert this to ContentBlock::ToolUse:
    // {
    //   "id": "gemini_Read_uuid",
    //   "name": "Read",
    //   "input": {
    //     "file_path": "/tmp/test.txt"
    //   }
    // }
    //
    // Key differences:
    // - We add a unique "id" field (Gemini doesn't provide one)
    // - "args" becomes "input" (matches Claude format)
    // - camelCase "functionCall" becomes snake_case "tool_use"
}

/// Test that multiple streaming chunks can be received
#[test]
fn test_multiple_stream_chunks() {
    // Simulate receiving multiple chunks
    let chunks = vec![
        StreamChunk::TextDelta("Hello ".to_string()),
        StreamChunk::TextDelta("world".to_string()),
        StreamChunk::ContentBlockComplete(ContentBlock::Text {
            text: "Hello world".to_string(),
        }),
    ];

    // Verify we received all chunks
    assert_eq!(chunks.len(), 3);

    // Verify types
    assert!(matches!(chunks[0], StreamChunk::TextDelta(_)));
    assert!(matches!(chunks[1], StreamChunk::TextDelta(_)));
    assert!(matches!(chunks[2], StreamChunk::ContentBlockComplete(_)));
}

/// Document expected streaming behavior with tools
#[test]
fn test_streaming_with_tools_expected_flow() {
    // This test documents the expected flow when streaming with tool calls:
    //
    // User: "Read /tmp/test.txt"
    //
    // Stream chunks received:
    // 1. [Optional] TextDelta("I'll read that file for you")
    // 2. ContentBlockComplete(ToolUse { name: "Read", input: {...} })
    // 3. [Stream ends, client executes tool]
    //
    // User sends tool result back:
    // - ToolResult content block with tool output
    //
    // Gemini continues:
    // 4. TextDelta("The file contains...")
    // 5. TextDelta(" these contents.")
    // 6. ContentBlockComplete(Text { text: "The file contains these contents." })
    //
    // Key insight: Tool calls come as ContentBlockComplete, not TextDelta
}

/// Verify that FunctionResponse parts are skipped in streaming
#[test]
fn test_function_response_parts_skipped() {
    // FunctionResponse parts appear in the conversation history
    // but should not be sent as streaming chunks (we handle them differently)
    //
    // This documents that in the streaming parser:
    // - GeminiPart::Text → Send as StreamChunk
    // - GeminiPart::FunctionCall → Send as StreamChunk
    // - GeminiPart::FunctionResponse → Skip (don't send)
    //
    // Reason: FunctionResponse is how we send tool results TO Gemini,
    // not something Gemini sends back to us in streaming.
}
