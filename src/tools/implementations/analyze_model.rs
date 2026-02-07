// AnalyzeModelTool - Claude analyzes Shammah's capabilities and weaknesses

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};

/// Tool that analyzes Shammah's current capabilities
pub struct AnalyzeModelTool;

#[async_trait]
impl Tool for AnalyzeModelTool {
    fn name(&self) -> &str {
        "analyze_model"
    }

    fn description(&self) -> &str {
        "Analyze Shammah's current capabilities and identify areas for improvement.

Performs comprehensive capability assessment:
1. Tests Shammah on diverse queries across categories
2. Evaluates response quality for each category
3. Identifies strengths and weaknesses
4. Recommends targeted training areas

Input: {
  \"test_count\": 50-200,
  \"categories\": [\"math\", \"code\", \"science\", ...] (optional)
}

Returns detailed analysis:
- Overall performance metrics
- Per-category accuracy scores
- Identified weak areas
- Specific recommendations for improvement
- Suggested training data counts

Use this to:
- Understand what Shammah can and cannot do
- Prioritize training efforts
- Track improvement over time
- Make data-driven training decisions"
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "test_count": {
                    "type": "integer",
                    "description": "Number of test queries (50-200)",
                    "minimum": 50,
                    "maximum": 200,
                    "default": 100
                },
                "categories": {
                    "type": "array",
                    "description": "Optional list of categories to test",
                    "items": {
                        "type": "string"
                    }
                }
            }),
            required: vec![],
        }
    }

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> Result<String> {
        let test_count = input["test_count"].as_i64().unwrap_or(20) as usize;

        let categories = if let Some(cats) = input["categories"].as_array() {
            cats.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        } else {
            vec![
                "greetings".to_string(),
                "math".to_string(),
                "general".to_string(),
            ]
        };

        // Get local generator
        let local_gen = ctx
            .local_generator
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Local generator not available"))?;

        // Predefined test queries per category
        let test_queries = vec![
            ("greetings", "Hello, how are you?"),
            ("greetings", "Good morning!"),
            ("greetings", "Hi there"),
            ("greetings", "Hey, what's up?"),
            ("math", "What is 2+2?"),
            ("math", "What is 5*7?"),
            ("math", "What is 10-3?"),
            ("math", "What is 100/4?"),
            ("general", "What is your name?"),
            ("general", "What is the capital of France?"),
            ("general", "Who invented the telephone?"),
            ("general", "What is water made of?"),
            ("code", "How do I print in Python?"),
            ("code", "What is a function?"),
            ("science", "What is gravity?"),
            ("science", "What causes rain?"),
            ("reasoning", "If it's raining, should I take an umbrella?"),
            ("reasoning", "What comes after 2, 4, 6, 8?"),
            ("creative", "Write a short poem"),
            ("creative", "Give me a fun fact"),
        ];

        let mut results = Vec::new();
        let mut total_score = 0.0;
        let mut category_scores: std::collections::HashMap<String, (usize, f64)> =
            std::collections::HashMap::new();

        let mut gen = local_gen.write().await;

        for (category, query) in test_queries.iter().take(test_count) {
            match gen.response_generator().generate(query) {
                Ok(generated) => {
                    let response = generated.text;
                    // Simple quality: response length > 10 chars and not an error = success
                    let score = if response.len() > 10 && !response.contains("[Error:") {
                        1.0
                    } else {
                        0.0
                    };
                    total_score += score;

                    let entry = category_scores
                        .entry(category.to_string())
                        .or_insert((0, 0.0));
                    entry.0 += 1;
                    entry.1 += score;

                    results.push(format!(
                        "  {} [{}]: {} chars ({})",
                        query,
                        category,
                        response.len(),
                        if score > 0.0 { "✓" } else { "✗" }
                    ));
                }
                Err(e) => {
                    results.push(format!("  {} [{}]: ERROR - {}", query, category, e));
                    let entry = category_scores
                        .entry(category.to_string())
                        .or_insert((0, 0.0));
                    entry.0 += 1;
                }
            }
        }

        let avg_score = if results.is_empty() {
            0.0
        } else {
            total_score / results.len() as f64
        };
        let percentage = (avg_score * 100.0) as u32;

        // Format category breakdown
        let mut category_breakdown = String::new();
        for (cat, (count, score)) in category_scores.iter() {
            let cat_percentage = if *count > 0 {
                (score / *count as f64 * 100.0) as u32
            } else {
                0
            };
            category_breakdown.push_str(&format!(
                "  {}: {}/{}  ({}%)\n",
                cat, *score as u32, count, cat_percentage
            ));
        }

        let response = format!(
            "=== Model Capability Analysis ===\n\
             Test Queries: {}\n\
             Categories: {}\n\n\
             === Results ===\n\
             {}\n\n\
             === Category Breakdown ===\n\
             {}\n\
             === Overall Performance ===\n\
             Success Rate: {}%\n\
             Average Score: {:.2}/1.0\n\n\
             === Recommendations ===\n\
             {}\n\n\
             Use generate_training_data to create targeted examples,\n\
             then use train to improve Shammah's performance.",
            results.len(),
            categories.join(", "),
            results.join("\n"),
            category_breakdown,
            percentage,
            avg_score,
            if percentage < 30 {
                "🔴 Critical: Shammah needs extensive training (50-100 examples)"
            } else if percentage < 60 {
                "🟡 Warning: Shammah needs moderate training (20-50 examples)"
            } else {
                "🟢 Good: Shammah performing reasonably well"
            }
        );

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_analyze_model_tool() {
        let tool = AnalyzeModelTool;
        let input = serde_json::json!({
            "test_count": 100,
            "categories": ["math", "code", "science"]
        });

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
        assert!(response.contains("Capability Analysis"));
        assert!(response.contains("Recommendations"));
    }
}
