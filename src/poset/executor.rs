use super::{NodeKind, NodeStatus, Poset};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;

pub async fn execute_poset(
    poset: Arc<Mutex<Poset>>,
    generator: Arc<dyn crate::generators::Generator>,
    registry: Option<Arc<crate::tools::ToolRegistry>>,
    stack: Option<Arc<Mutex<Vec<String>>>>,
) -> Result<String> {
    let mut all_results: Vec<String> = Vec::new();

    loop {
        let ready = {
            let p = poset.lock().await;
            if p.is_complete() { break; }
            p.ready_nodes()
        };

        if ready.is_empty() {
            let done = poset.lock().await.is_complete();
            if done { break; }
            break; // deadlock guard
        }

        let handles: Vec<_> = ready.iter().map(|&node_id| {
            let p2 = Arc::clone(&poset);
            let g2 = Arc::clone(&generator);
            let r2 = registry.clone();
            let s2 = stack.clone();
            tokio::spawn(async move { exec_node(node_id, p2, g2, r2, s2).await })
        }).collect();

        for h in handles {
            if let Ok(Ok(r)) = h.await { all_results.push(r); }
        }
    }

    Ok(all_results.join("\n\n"))
}

async fn exec_node(
    node_id: usize,
    poset: Arc<Mutex<Poset>>,
    generator: Arc<dyn crate::generators::Generator>,
    registry: Option<Arc<crate::tools::ToolRegistry>>,
    stack: Option<Arc<Mutex<Vec<String>>>>,
) -> Result<String> {
    let (label, kind, ctx, node_tools, vocab, compiled_code, compiled_lang) = {
        let p = poset.lock().await;
        let node = p.node(node_id).ok_or_else(|| anyhow::anyhow!("missing node"))?;
        let pred_results: Vec<String> = p.predecessors(node_id).iter()
            .filter_map(|&pid| p.node(pid).and_then(|n| n.result.clone()))
            .collect();
        let vocab: Vec<(String, String)> = p.nodes.iter()
            .filter(|n| n.id != node_id && matches!(n.status, NodeStatus::Done))
            .map(|n| (format!("W{}", n.id), n.label.clone()))
            .collect();
        (
            node.label.clone(), node.kind.clone(), pred_results,
            node.tools.clone(), vocab,
            node.compiled_code.clone(), node.compiled_lang.clone(),
        )
    };

    { let mut p = poset.lock().await; if let Some(n) = p.node_mut(node_id) { n.status = NodeStatus::Running; } }

    // ── Fast path: compiled native code — no LLM needed ───────────────────────
    if let Some(code) = compiled_code {
        let lang = compiled_lang.as_deref().unwrap_or("bash");
        let output = run_compiled(lang, &code).await;
        let result = match output {
            Ok(out) => out,
            Err(e)  => format!("compile-exec error: {e}"),
        };
        {
            let mut p = poset.lock().await;
            if let Some(n) = p.node_mut(node_id) {
                n.status = super::NodeStatus::Done;
                n.result = Some(result.clone());
            }
        }
        return Ok(result);
    }

    let instruction = match kind {
        NodeKind::Task => "Complete this task",
        NodeKind::Constraint => "Apply this constraint",
        NodeKind::Question => "Answer this question",
        NodeKind::Observation => "Acknowledge this observation",
    };
    let context = if ctx.is_empty() { String::new() }
                  else { format!("\n\nPrior results:\n{}", ctx.join("\n---\n")) };

    // Describe the callable vocabulary so the AI knows what it can invoke.
    let vocab_note = if vocab.is_empty() {
        String::new()
    } else {
        let entries: Vec<String> = vocab.iter()
            .map(|(name, lbl)| format!("  {name}: {lbl}"))
            .collect();
        format!("\n\nAvailable words (call with the Call tool):\n{}", entries.join("\n"))
    };

    let prompt = format!("{instruction}: {label}{context}{vocab_note}");

    // Resolve tool definitions the node is allowed to use.
    // Always include the built-in `Call` tool so the node can invoke vocabulary words.
    let call_tool_def = crate::tools::types::ToolDefinition {
        name: "Call".to_string(),
        description: "Invoke an agreed-upon word from the shared vocabulary by name. \
                      Returns that word's result. Use this to compose words together.".to_string(),
        input_schema: crate::tools::types::ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "word": {
                    "type": "string",
                    "description": "The word name to call, e.g. \"W0\" or \"W3\""
                }
            }),
            required: vec!["word".to_string()],
        },
    };

    let tool_defs: Option<Vec<crate::tools::types::ToolDefinition>> = {
        let mut defs = vec![call_tool_def];
        if let Some(ref reg) = registry {
            if node_tools.is_empty() {
                // No explicit tool list — give the word access to everything.
                defs.extend(reg.definitions());
            } else {
                // Explicit list — restrict to those tools only.
                for name in &node_tools {
                    if let Some(t) = reg.get(name) {
                        defs.push(t.definition());
                    }
                }
            }
        }
        Some(defs)
    };

    // Initial message
    let mut messages = vec![crate::claude::Message {
        role: "user".to_string(),
        content: vec![crate::claude::ContentBlock::Text { text: prompt }],
    }];

    // Each word can do as many things as it needs — read files, grep patterns,
    // run commands, call other words.  Cap is generous; a word that finishes
    // early (no tool calls) exits immediately.
    const MAX_ROUNDS: usize = 40;
    let mut text_result = String::new();

    for _ in 0..MAX_ROUNDS {
        let response = match generator.generate(messages.clone(), tool_defs.clone()).await {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string();
                let mut p = poset.lock().await;
                if let Some(n) = p.node_mut(node_id) {
                    n.status = NodeStatus::Failed;
                    n.result = Some(format!("Error: {msg}"));
                }
                return Ok(format!("Error: {msg}"));
            }
        };

        text_result = response.text.clone();

        // No tool calls — done.
        if response.tool_uses.is_empty() {
            break;
        }

        // Append assistant turn with all content blocks.
        messages.push(crate::claude::Message {
            role: "assistant".to_string(),
            content: response.content_blocks.iter().map(|b| b.clone()).collect(),
        });

        // Execute each tool call and collect results.
        let mut tool_result_blocks: Vec<crate::claude::ContentBlock> = Vec::new();
        for tu in &response.tool_uses {
            let result = if tu.name == "Call" {
                // Built-in Call tool — invoke an agreed-upon vocabulary word.
                let word = tu.input["word"].as_str().unwrap_or("").trim().to_string();
                let word_id: Option<usize> = word.strip_prefix('W')
                    .and_then(|s| s.parse().ok());
                match word_id {
                    Some(wid) => {
                        let p = poset.lock().await;
                        match p.node(wid) {
                            Some(n) if matches!(n.status, NodeStatus::Done) => {
                                n.result.clone().unwrap_or_else(|| "(no result)".to_string())
                            }
                            Some(_) => format!("Word {word} is not yet done"),
                            None => format!("Word {word} not found in vocabulary"),
                        }
                    }
                    None => format!("Invalid word name: {word}"),
                }
            } else if let Some(ref reg) = registry {
                if let Some(tool) = reg.get(&tu.name) {
                    let ctx_obj = crate::tools::types::ToolContext {
                        conversation: None,
                        save_models: None,
                        batch_trainer: None,
                        local_generator: None,
                        tokenizer: None,
                        repl_mode: None,
                        plan_content: None,
                        live_output: None,
                        stack: stack.clone(),
                        poset: Some(Arc::clone(&poset)),
                    };
                    match tool.execute(tu.input.clone(), &ctx_obj).await {
                        Ok(output) => output,
                        Err(e) => format!("Tool error: {e}"),
                    }
                } else {
                    format!("Tool '{}' not available", tu.name)
                }
            } else {
                format!("No tool registry available for '{}'", tu.name)
            };

            tool_result_blocks.push(crate::claude::ContentBlock::tool_result(
                tu.id.clone(),
                result,
                None,
            ));
        }

        // Append tool results as user turn.
        messages.push(crate::claude::Message {
            role: "user".to_string(),
            content: tool_result_blocks,
        });
    }

    // Mark done and store result.
    {
        let mut p = poset.lock().await;
        if let Some(n) = p.node_mut(node_id) {
            n.status = NodeStatus::Done;
            n.result = Some(text_result.clone());
        }
    }
    Ok(text_result)
}

/// Execute a compiled word natively — no LLM, no external processes.
/// Words compile to Forth and run at CPU speed in the built-in interpreter.
async fn run_compiled(_lang: &str, code: &str) -> anyhow::Result<String> {
    crate::coforth::Forth::run(code)
}
