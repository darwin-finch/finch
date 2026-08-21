//! Read-only model tools for discovering the persistent program vocabulary.

use crate::memory::MemorySystem;
use crate::programs::ProgramRef;
use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;

/// Search program names, documentation, signatures, and source keywords.
pub struct SearchVocabularyTool {
    memory: Arc<MemorySystem>,
}

impl SearchVocabularyTool {
    pub fn new(memory: Arc<MemorySystem>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl Tool for SearchVocabularyTool {
    fn name(&self) -> &str {
        "search_vocabulary"
    }

    fn description(&self) -> &str {
        "Search the live Finch Forth/Lisp program registry before writing a new program. Returns compact identities, versions, signatures, and descriptions without source."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "query": {
                    "type": "string",
                    "description": "Capability or program to find"
                },
                "limit": {
                    "type": "integer",
                    "default": 8,
                    "description": "Maximum results (1-20)"
                }
            }),
            required: vec!["query".to_string()],
        }
    }

    async fn execute(&self, params: Value, _context: &ToolContext<'_>) -> Result<String> {
        let query = params["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required parameter: query"))?;
        let limit = params["limit"].as_u64().unwrap_or(8).clamp(1, 20) as usize;
        let definitions = self.memory.search_program_definitions(query, limit).await?;
        if definitions.is_empty() {
            return Ok("No matching programs in the current VM vocabulary.".to_string());
        }
        Ok(definitions
            .iter()
            .map(|definition| {
                format!(
                    "{}@{} v{} [{}; {}; {}] — {}",
                    definition.name,
                    definition.reference.id,
                    definition.reference.version,
                    definition.language.as_str(),
                    definition
                        .signature
                        .as_deref()
                        .unwrap_or("signature unknown"),
                    definition.trust.as_str(),
                    definition.documentation
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

/// Inspect exact source and metadata for one immutable program version.
pub struct InspectProgramTool {
    memory: Arc<MemorySystem>,
}

impl InspectProgramTool {
    pub fn new(memory: Arc<MemorySystem>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl Tool for InspectProgramTool {
    fn name(&self) -> &str {
        "inspect_program"
    }

    fn description(&self) -> &str {
        "Inspect the exact source, capabilities, dependencies, tests, and hashes of one immutable Finch program version returned by search_vocabulary."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "id": { "type": "string", "description": "Program UUID" },
                "version": { "type": "integer", "description": "Immutable version number" }
            }),
            required: vec!["id".to_string(), "version".to_string()],
        }
    }

    async fn execute(&self, params: Value, _context: &ToolContext<'_>) -> Result<String> {
        let id = params["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required parameter: id"))?;
        let version = params["version"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("missing required parameter: version"))?;
        let reference = ProgramRef {
            id: Uuid::from_str(id)
                .map_err(|error| anyhow::anyhow!("invalid program id: {error}"))?,
            version,
        };
        let definition = self
            .memory
            .get_program_definition(&reference)
            .await?
            .ok_or_else(|| anyhow::anyhow!("program version not found"))?;
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "id": definition.reference.id,
            "version": definition.reference.version,
            "name": definition.name,
            "language": definition.language,
            "source": definition.source,
            "documentation": definition.documentation,
            "signature": definition.signature,
            "capabilities": definition.capabilities,
            "dependencies": definition.dependencies,
            "tests": definition.tests,
            "scope": definition.scope,
            "trust": definition.trust,
            "source_hash": definition.source_hash,
            "environment_hash": definition.environment_hash,
        }))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vocabulary_tools_have_read_only_discovery_schemas() {
        let temp = tempfile::TempDir::new().unwrap();
        let memory = Arc::new(
            MemorySystem::new(crate::memory::MemoryConfig {
                db_path: temp.path().join("memory.db"),
                use_neural_embeddings: false,
                ..crate::memory::MemoryConfig::default()
            })
            .unwrap(),
        );
        let search = SearchVocabularyTool::new(memory.clone());
        let inspect = InspectProgramTool::new(memory);
        assert_eq!(search.name(), "search_vocabulary");
        assert_eq!(search.input_schema().required, vec!["query"]);
        assert_eq!(inspect.name(), "inspect_program");
        assert_eq!(inspect.input_schema().required, vec!["id", "version"]);
    }
}
