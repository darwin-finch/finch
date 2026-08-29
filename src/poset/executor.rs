use super::{NodeKind, NodeStatus, Poset};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;

pub async fn execute_poset(
    poset: Arc<Mutex<Poset>>,
    generator: Arc<dyn crate::generators::Generator>,
) -> Result<String> {
    let mut all_results: Vec<String> = Vec::new();

    loop {
        let ready = {
            let p = poset.lock().await;
            if p.is_complete() {
                break;
            }
            p.ready_nodes()
        };

        if ready.is_empty() {
            let done = poset.lock().await.is_complete();
            if done {
                break;
            }
            break; // deadlock guard
        }

        let handles: Vec<_> = ready
            .iter()
            .map(|&node_id| {
                let p2 = Arc::clone(&poset);
                let g2 = Arc::clone(&generator);
                tokio::spawn(async move { exec_node(node_id, p2, g2).await })
            })
            .collect();

        for h in handles {
            if let Ok(Ok(r)) = h.await {
                all_results.push(r);
            }
        }
    }

    Ok(all_results.join("\n\n"))
}

async fn exec_node(
    node_id: usize,
    poset: Arc<Mutex<Poset>>,
    generator: Arc<dyn crate::generators::Generator>,
) -> Result<String> {
    let (label, kind, ctx, node_tools, compiled_code, compiled_lang) = {
        let p = poset.lock().await;
        let node = p
            .node(node_id)
            .ok_or_else(|| anyhow::anyhow!("missing node"))?;
        let pred_results: Vec<String> = p
            .predecessors(node_id)
            .iter()
            .filter_map(|&pid| p.node(pid).and_then(|n| n.result.clone()))
            .collect();
        (
            node.label.clone(),
            node.kind.clone(),
            pred_results,
            node.tools.clone(),
            node.compiled_code.clone(),
            node.compiled_lang.clone(),
        )
    };

    {
        let mut p = poset.lock().await;
        if let Some(n) = p.node_mut(node_id) {
            n.status = NodeStatus::Running;
        }
    }

    // ── Fast path: compiled native code — no LLM needed ───────────────────────
    if let Some(code) = compiled_code {
        let lang = compiled_lang.as_deref().unwrap_or("forth");
        let output = run_compiled(lang, &code, &ctx).await;
        let (result, node_status) = match output {
            Ok(out) => (out, super::NodeStatus::Done),
            Err(e) => (format!("✗ {e}"), super::NodeStatus::Failed),
        };
        {
            let mut p = poset.lock().await;
            if let Some(n) = p.node_mut(node_id) {
                n.status = node_status;
                n.result = Some(result.clone());
            }
        }
        return Ok(result);
    }

    if !node_tools.is_empty() {
        let result = "Error: untyped tool-bearing poset nodes are disabled; use a reviewed typed \
                      Lisp/Co-Forth node with explicit capabilities"
            .to_string();
        let mut p = poset.lock().await;
        if let Some(n) = p.node_mut(node_id) {
            n.status = NodeStatus::Failed;
            n.result = Some(result.clone());
        }
        return Ok(result);
    }

    let instruction = match kind {
        NodeKind::Task => "Complete this task",
        NodeKind::Constraint => "Apply this constraint",
        NodeKind::Question => "Answer this question",
        NodeKind::Observation => "Acknowledge this observation",
    };
    let context = if ctx.is_empty() {
        String::new()
    } else {
        format!("\n\nPrior results:\n{}", ctx.join("\n---\n"))
    };

    let prompt = format!("{instruction}: {label}{context}");

    let text_result = match generator
        .generate(
            vec![crate::claude::Message {
                role: "user".to_string(),
                content: vec![crate::claude::ContentBlock::Text { text: prompt }],
            }],
            None,
        )
        .await
    {
        Ok(response) if response.tool_uses.is_empty() => response.text,
        Ok(_) => {
            let message = "Error: an inference-only poset node attempted an unreviewed tool call";
            let mut p = poset.lock().await;
            if let Some(n) = p.node_mut(node_id) {
                n.status = NodeStatus::Failed;
                n.result = Some(message.to_string());
            }
            return Ok(message.to_string());
        }
        Err(error) => {
            let message = format!("Error: {error}");
            let mut p = poset.lock().await;
            if let Some(n) = p.node_mut(node_id) {
                n.status = NodeStatus::Failed;
                n.result = Some(message.clone());
            }
            return Ok(message);
        }
    };

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

/// Execute a reviewed node without an LLM or external process. Both source
/// syntaxes enter the shared typed runtime; the poset is a scheduling/review
/// artifact and never selects the historical semiotic interpreter.
async fn run_compiled(
    lang: &str,
    code: &str,
    predecessor_results: &[String],
) -> anyhow::Result<String> {
    // Predecessor top-of-stack values are prepended as literals so dependent
    // arguments receive their inputs naturally via the stack.
    let mut prelude = String::new();
    for pred in predecessor_results {
        let trimmed = pred.trim();
        if trimmed.parse::<i64>().is_ok() {
            prelude.push_str(trimmed);
            prelude.push(' ');
        }
    }
    let language = match lang {
        "forth" => crate::programs::ProgramLanguage::Forth,
        "lisp" => crate::programs::ProgramLanguage::Lisp,
        other => anyhow::bail!("unsupported typed poset node language '{other}'"),
    };
    let source = if language == crate::programs::ProgramLanguage::Forth {
        format!("{prelude}{code}")
    } else {
        code.to_string()
    };
    let runtime = crate::runtime::ProgramRuntime::new();
    runtime.grant_typed_capability(crate::vm::CapabilityRequirement {
        capability: crate::vm::CapabilityKind::SessionEmit,
        selector: crate::vm::ResourceSelector::None,
    })?;
    let outcome = runtime
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language,
            source_id: Some("poset-node".into()),
            source,
            intent: "execute approved poset node".into(),
            effect: crate::programs::ExecutionEffect::Unclassified,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(runtime.revision()),
            budget: None,
        })
        .await?;
    if outcome.status != crate::runtime::outcome::ExecutionStatus::Completed {
        let detail = outcome
            .vm_diagnostics
            .first()
            .map(ToString::to_string)
            .or_else(|| outcome.diagnostics.first().cloned())
            .unwrap_or_else(|| format!("typed poset node ended with {:?}", outcome.status));
        anyhow::bail!(detail);
    }
    if !outcome.output.is_empty() {
        return Ok(outcome.output.trim_end().to_string());
    }
    Ok(outcome
        .values
        .iter()
        .map(display_program_value)
        .collect::<Vec<_>>()
        .join(" "))
}

fn display_program_value(value: &crate::programs::ProgramValue) -> String {
    use crate::programs::ProgramValue;
    match value {
        ProgramValue::Nil => "nil".into(),
        ProgramValue::Bool(value) => value.to_string(),
        ProgramValue::Int(value) => value.to_string(),
        ProgramValue::Float(value) => value.to_string(),
        ProgramValue::Symbol(value) | ProgramValue::String(value) => value.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| format!("{other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::run_compiled;

    #[tokio::test]
    async fn reviewed_forth_node_runs_in_typed_runtime_with_predecessor_values() {
        assert_eq!(
            run_compiled("forth", "2 *", &["21".into()]).await.unwrap(),
            "42"
        );
    }

    #[tokio::test]
    async fn reviewed_lisp_node_runs_in_the_same_typed_runtime() {
        assert_eq!(run_compiled("lisp", "(+ 20 22)", &[]).await.unwrap(), "42");
    }

    #[tokio::test]
    async fn reviewed_node_never_falls_back_to_legacy_forth() {
        let error = run_compiled("forth", "2 3 + .", &[]).await.unwrap_err();
        assert!(error.to_string().contains("unknown Co-Forth word '.'"));
    }
}
