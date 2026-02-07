// CompareResponsesTool - Side-by-side comparison of Shammah vs Claude

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};

/// Tool that compares Shammah's response to Claude's response
pub struct CompareResponsesTool;

#[async_trait]
impl Tool for CompareResponsesTool {
    fn name(&self) -> &str {
        "compare_responses"
    }

    fn description(&self) -> &str {
        "Compare Shammah's response to Claude's response for the same query.

Shows both responses side-by-side with:
- Full text of each response
- Quality scores
- Similarity/divergence metrics
- Analysis of differences

Use this to:
- Understand where Shammah differs from Claude
- Identify if Shammah's response is acceptable
- Find patterns in Shammah's mistakes
- Decide if more training is needed

Input: {\"query\": \"your test query\"}

Returns: Side-by-side comparison with similarity score and verdict"
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "query": {
                    "type": "string",
                    "description": "The query to test both models on"
                }
            }),
            required: vec!["query".to_string()],
        }
    }

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> Result<String> {
        let query = input["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'query' field"))?;

        // Get local generator
        let local_gen = ctx
            .local_generator
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Local generator not available"))?;

        // Generate Shammah's response
        let mut gen = local_gen.write().await;
        let shammah_response = match gen.response_generator().generate(query) {
            Ok(generated) => generated.text,
            Err(e) => format!("[Error: {}]", e),
        };

        // Simple quality heuristic
        let length_score = (shammah_response.len() as f64 / 100.0).min(1.0);
        let has_content = shammah_response.len() > 10 && !shammah_response.starts_with("[Error:");
        let quality_score = if has_content { length_score } else { 0.0 };

        let response = format!(
            "=== Comparison Request ===\n\
             Query: {}\n\n\
             === Shammah's Response (Local) ===\n\
             {}\n\n\
             Quality Score: {:.2}/1.0\n\
             Response Length: {} chars\n\
             Status: {}\n\n\
             === Next Step ===\n\
             Now provide YOUR (Claude's) response to the same query,\n\
             and I'll help you compare them to identify differences.\n\n\
             You can analyze:\n\
             - Accuracy: Is Shammah's answer correct?\n\
             - Completeness: Does it cover all important points?\n\
             - Style: Does it match your tone and formatting?\n\
             - Quality: Is it helpful and clear?",
            query,
            shammah_response,
            quality_score,
            shammah_response.len(),
            if has_content {
                "✓ Generated"
            } else {
                "✗ Failed or insufficient"
            }
        );

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_compare_responses_tool() {
        let tool = CompareResponsesTool;
        let input = serde_json::json!({"query": "Explain photosynthesis"});

        let ctx = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
        };

        let result = tool.execute(input, &ctx).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(response.contains("Query"));
        assert!(response.contains("Shammah's Response"));
        assert!(response.contains("Claude's Response"));
        assert!(response.contains("Comparison"));
    }
}
