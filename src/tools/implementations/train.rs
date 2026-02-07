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

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> Result<String> {
        let wait = input["wait"].as_bool().unwrap_or(false);

        // Get batch trainer from context
        let batch_trainer = ctx
            .batch_trainer
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Batch trainer not available"))?;

        // Check queue size
        let queue_size = {
            let trainer = batch_trainer.read().await;
            trainer.queue_size().await
        };

        if queue_size == 0 {
            return Ok(format!(
                "=== Training Status ===\n\
                 Queue Size: 0 examples\n\
                 Status: No examples in queue\n\n\
                 To train Shammah:\n\
                 1. Use generate_training_data to create examples\n\
                 2. Use Claude to provide high-quality responses\n\
                 3. Examples will be added to training queue\n\
                 4. Run this tool again to train\n\n\
                 Note: Batch training requires at least 1 example."
            ));
        }

        if wait {
            // Synchronous training
            let mut trainer = batch_trainer.write().await;
            let start = std::time::Instant::now();

            match trainer.train_now().await {
                Ok(result) => {
                    let duration = start.elapsed().as_secs_f64();

                    Ok(format!(
                        "=== Training Completed (Synchronous) ===\n\
                         Examples trained: {}\n\
                         Duration: {:.2} seconds\n\n\
                         === Model Improvements ===\n\
                         Router:\n\
                         - Old loss: {:.4}\n\
                         - New loss: {:.4}\n\
                         - Improvement: {:.1}%\n\n\
                         Generator:\n\
                         - Old loss: {:.4}\n\
                         - New loss: {:.4}\n\
                         - Improvement: {:.1}%\n\n\
                         Validator:\n\
                         - Old loss: {:.4}\n\
                         - New loss: {:.4}\n\
                         - Improvement: {:.1}%\n\n\
                         Status: Models updated successfully\n\
                         Test with query_local_model to see improvements!",
                        result.examples_count,
                        duration,
                        result.router_old_loss,
                        result.router_new_loss,
                        (result.router_old_loss - result.router_new_loss) / result.router_old_loss
                            * 100.0,
                        result.generator_old_loss,
                        result.generator_new_loss,
                        (result.generator_old_loss - result.generator_new_loss)
                            / result.generator_old_loss
                            * 100.0,
                        result.validator_old_loss,
                        result.validator_new_loss,
                        (result.validator_old_loss - result.validator_new_loss)
                            / result.validator_old_loss
                            * 100.0,
                    ))
                }
                Err(e) => Ok(format!(
                    "=== Training Failed ===\n\
                     Error: {}\n\n\
                     Queue size: {} examples\n\
                     Try again or check logs for details.",
                    e, queue_size
                )),
            }
        } else {
            // Asynchronous training
            let mut trainer = batch_trainer.write().await;

            match trainer.train_async().await {
                Ok(_) => Ok(format!(
                    "=== Training Started (Asynchronous) ===\n\
                     Queue size: {} examples\n\
                     Status: Training running in background\n\n\
                     Training will complete shortly. Check progress with:\n\
                     - query_local_model tool to test responses\n\
                     - analyze_model tool to see overall performance\n\n\
                     Note: Models will be automatically updated when training completes.",
                    queue_size
                )),
                Err(e) => Ok(format!(
                    "=== Training Failed to Start ===\n\
                     Error: {}\n\n\
                     Queue size: {} examples\n\
                     Try again or check logs for details.",
                    e, queue_size
                )),
            }
        }
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
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
        };

        let result = tool.execute(input, &ctx).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(response.contains("Training Started"));
    }
}
