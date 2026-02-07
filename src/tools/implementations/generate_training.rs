// GenerateTrainingDataTool - Claude creates targeted training examples for Shammah

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};

/// Tool that generates synthetic training data for Shammah
pub struct GenerateTrainingDataTool;

#[async_trait]
impl Tool for GenerateTrainingDataTool {
    fn name(&self) -> &str {
        "generate_training_data"
    }

    fn description(&self) -> &str {
        "Add training examples to improve Shammah's capabilities.

Claude (you) can create targeted training data by providing Q&A pairs:

Input: {
  \"examples\": [
    {\"query\": \"What is 2+2?\", \"response\": \"2+2 equals 4.\"},
    {\"query\": \"Explain photosynthesis\", \"response\": \"Photosynthesis is...\"}
  ]
}

Process:
1. Claude generates diverse queries and high-quality responses
2. Call this tool with the examples array
3. Examples are added to training queue
4. Use train tool to train immediately, or wait for automatic training

This enables:
- Rapid skill acquisition
- Targeted weakness improvement
- Curriculum learning (easy -> hard examples)
- Active learning (Claude identifies gaps and fills them)"
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "examples": {
                    "type": "array",
                    "description": "Array of Q&A pairs: [{\"query\": \"...\", \"response\": \"...\"}]",
                    "items": {
                        "type": "object",
                        "properties": {
                            "query": {
                                "type": "string",
                                "description": "The user query/question"
                            },
                            "response": {
                                "type": "string",
                                "description": "Claude's high-quality response"
                            }
                        },
                        "required": ["query", "response"]
                    }
                }
            }),
            required: vec!["examples".to_string()],
        }
    }

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> Result<String> {
        let examples_array = input["examples"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Missing 'examples' array"))?;

        if examples_array.is_empty() {
            return Ok("No examples provided. Please provide at least one example.".to_string());
        }

        let batch_trainer = ctx
            .batch_trainer
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Batch trainer not available"))?;

        let mut added = 0;
        let mut errors = Vec::new();

        for (i, example_obj) in examples_array.iter().enumerate() {
            let query = match example_obj["query"].as_str() {
                Some(q) => q,
                None => {
                    errors.push(format!("Example {}: missing 'query' field", i + 1));
                    continue;
                }
            };

            let response = match example_obj["response"].as_str() {
                Some(r) => r,
                None => {
                    errors.push(format!("Example {}: missing 'response' field", i + 1));
                    continue;
                }
            };

            use crate::training::batch_trainer::TrainingExample;
            let training_example = TrainingExample::new(
                query.to_string(),
                response.to_string(),
                false, // local_success = false (these are from Claude)
            )
            .with_quality(1.0); // Claude's responses are high quality

            let mut trainer = batch_trainer.write().await;
            trainer.add_example(training_example).await?;

            added += 1;
        }

        let queue_size = {
            let trainer = batch_trainer.read().await;
            trainer.queue_size().await
        };

        let mut response = format!(
            "=== Training Examples Added ===\n\
             Added: {} examples\n\
             Queue size: {} examples\n\
             Batch size: 32 (training triggers automatically)\n\n",
            added, queue_size
        );

        if !errors.is_empty() {
            response.push_str(&format!("=== Errors ===\n{}\n\n", errors.join("\n")));
        }

        response
            .push_str("Use the train tool to train immediately, or wait for automatic training.");

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_generate_training_tool_no_trainer() {
        let tool = GenerateTrainingDataTool;
        let input = serde_json::json!({
            "examples": [
                {"query": "What is 2+2?", "response": "2+2 equals 4."}
            ]
        });

        let ctx = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
        };

        let result = tool.execute(input, &ctx).await;
        // Should fail because batch_trainer is None
        assert!(result.is_err());
    }
}
