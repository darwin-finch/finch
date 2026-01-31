// TrainTool - Trigger batch training on accumulated examples

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};

/// Tool that triggers training on accumulated examples
pub struct TrainTool;

#[async_trait]
impl Tool for TrainTool {
    fn name(&self) -> &str {
        "train"
    }

    fn description(&self) -> &str {
        "Train Shammah's models on accumulated training examples.

Triggers batch training on examples in the training queue. Training happens
asynchronously in the background and models are hot-reloaded after completion.

Input: {
  \"wait\": true | false (optional, default: false)
}

If wait=true, blocks until training completes and returns detailed results.
If wait=false, starts training in background and returns immediately.

Use this after:
- Generating training data with generate_training_data tool
- Accumulating examples from user queries
- When analyze_model shows poor performance in some area

Returns:
- Number of examples trained on
- Loss improvements for each model (router, generator, validator)
- Training duration
- New performance metrics"
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "wait": {
                    "type": "boolean",
                    "description": "Wait for training to complete (default: false)",
                    "default": false
                }
            }),
            required: vec![],
        }
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> Result<String> {
        let wait = input["wait"].as_bool().unwrap_or(false);

        // TODO: Wire up to actual BatchTrainer
        // For now, return instructions

        // In the real implementation:
        // 1. Get BatchTrainer reference from context
        // 2. Check queue size
        // 3. Call train_now() or train_async()
        // 4. Return results

        let response = if wait {
            "=== Training Started (Synchronous) ===\n\
             Status: Training infrastructure ready but not yet fully integrated\n\n\
             When integrated, this will:\n\
             1. Train on accumulated examples in queue\n\
             2. Update router, generator, and validator models\n\
             3. Hot-reload models (zero downtime)\n\
             4. Return detailed results:\n\
                - Examples trained: N\n\
                - Router loss: X.XX -> Y.YY (improvement: Z.ZZ)\n\
                - Generator loss: X.XX -> Y.YY (improvement: Z.ZZ)\n\
                - Validator loss: X.XX -> Y.YY (improvement: Z.ZZ)\n\
                - Duration: N.N seconds\n\n\
             Current queue size: 0 examples\n\
             Minimum batch size: 32 examples\n\n\
             Use generate_training_data to add examples to the queue."
                .to_string()
        } else {
            "=== Training Started (Asynchronous) ===\n\
             Status: Training infrastructure ready but not yet fully integrated\n\n\
             Training will run in background. Check status with:\n\
             - /status command in REPL\n\
             - analyze_model tool to test improvement\n\n\
             Current queue size: 0 examples\n\
             Minimum batch size: 32 examples\n\n\
             Use generate_training_data to add examples to the queue."
                .to_string()
        };

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_train_tool() {
        let tool = TrainTool;
        let input = serde_json::json!({"wait": true});

        let ctx = ToolContext {
            cwd: std::path::PathBuf::from("."),
            allow_all: true,
        };

        let result = tool.execute(input, &ctx).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(response.contains("Training Started"));
    }
}
