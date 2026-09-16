//! External-style backend against the public `finch-generation` contract.

use anyhow::Result;
use finch_generation::{
    BackendKind, BackendRef, GenerationEvent, GenerationPorts, GenerationRequest,
    GenerationStrategy, GenerationSupervisor, ScriptedBackend, ScriptedStep, TerminalOutcome,
};
use finch_providers::EventProvenance;

#[tokio::main]
async fn main() -> Result<()> {
    let identity = BackendRef::new("test", "script-1", BackendKind::Test)?;
    let backend = ScriptedBackend::ready(
        identity.clone(),
        GenerationStrategy::CausalAutoregressive,
        vec![ScriptedStep::Event(GenerationEvent::TextDelta {
            text: "hello from the generation boundary".into(),
            provenance: EventProvenance {
                provider: identity.provider.clone(),
                model: identity.model.clone(),
                event: "text".into(),
                sequence: 1,
                opaque_replay: None,
            },
        })],
    );
    let supervisor = GenerationSupervisor::new(GenerationPorts::test());
    let request = GenerationRequest::new(
        vec![finch_providers::Message::user("ping")],
        identity,
        GenerationStrategy::CausalAutoregressive,
    );
    let mut stream = supervisor
        .run(std::sync::Arc::new(backend), request, None)
        .await?;
    while let Some(event) = stream.recv().await {
        match event? {
            GenerationEvent::TextDelta { text, .. } => println!("{text}"),
            GenerationEvent::Terminal(TerminalOutcome::Completed { text, .. }) => {
                println!("terminal: {text}");
            }
            GenerationEvent::Terminal(other) => {
                println!("terminal: {other:?}");
            }
            _ => {}
        }
    }
    Ok(())
}
