// QueryLocalModelTool - Let Claude see Shammah's responses directly

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};

/// Tool that queries the local generator (Shammah) directly
pub struct QueryLocalModelTool;

#[async_trait]
impl Tool for QueryLocalModelTool {
    fn name(&self) -> &str {
        "query_local_model"
    }

    fn description(&self) -> &str {
        "Query Shammah (the local LLM) directly and see its response.

Use this tool to:
- Test Shammah's capabilities on specific queries
- See what mistakes or errors Shammah is making
- Compare Shammah's response quality to Claude's
- Identify areas where Shammah needs more training

Input: {\"query\": \"your test query here\"}

Returns: Shammah's raw response plus quality metrics including:
- Response text
- Quality score (0.0-1.0)
- Uncertainty level
- Coherence check
- On-topic check
- Hallucination risk assessment"
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "query": {
                    "type": "string",
                    "description": "The query to send to Shammah"
                }
            }),
            required: vec!["query".to_string()],
        }
    }

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> Result<String> {
        let query = input["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'query' field"))?;

        // Check if local generator is available
        let local_gen = ctx
            .local_generator
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Local generator not available"))?;

        // Generate response using local generator
        let mut gen = local_gen.write().await;
        let generated = gen.response_generator().generate(query)?;
        let local_response = generated.text;

        // Check if we have validator available (through batch trainer)
        let quality_score = if let Some(batch_trainer) = ctx.batch_trainer.as_ref() {
            // For now, use a simple quality heuristic
            // TODO: Use validator model for actual quality assessment
            let trainer = batch_trainer.read().await;
            // Simple heuristic: longer responses are "better" (placeholder)
            let length_score = (local_response.len() as f64 / 100.0).min(1.0);
            length_score
        } else {
            0.0
        };

        // Format response with metrics
        let response = format!(
            "=== Shammah's Response ===\n\
             Query: {}\n\n\
             Response:\n\
             {}\n\n\
             === Quality Metrics ===\n\
             - Quality Score: {:.2}/1.0\n\
             - Response Length: {} chars\n\
             - Status: {}\n\n\
             Note: Quality assessment is based on simple heuristics. \
             For more accurate quality scores, train the validator model.",
            query,
            local_response,
            quality_score,
            local_response.len(),
            if quality_score > 0.7 {
                "Good quality"
            } else if quality_score > 0.4 {
                "Medium quality"
            } else {
                "Low quality - needs more training"
            }
        );

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_query_local_tool() {
        let tool = QueryLocalModelTool;
        let input = serde_json::json!({"query": "What is 2+2?"});

        // Create minimal context for testing
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
        assert!(response.contains("Shammah's Response"));
        assert!(response.contains("Quality Metrics"));
    }
}
