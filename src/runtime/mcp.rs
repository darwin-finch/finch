use crate::tools::mcp::McpToolDescriptor;
use crate::vm::{
    CapabilityKind, CapabilityRequirement, ControlEffect, EffectSet, ResourceSelector, StackRow,
    StackSignature, Type,
};
use anyhow::{bail, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};

const MAX_MCP_NAME_BYTES: usize = 128;
const MAX_MCP_DESCRIPTION_BYTES: usize = 2_048;
const MAX_SCHEMA_DEPTH: usize = 8;
const MAX_SCHEMA_PROPERTIES: usize = 64;

#[derive(Debug, Clone)]
pub(super) struct McpVocabularyBinding {
    pub word_name: String,
    pub signature: StackSignature,
    pub documentation: String,
    pub version: String,
    pub output_schema: Option<Value>,
}

pub(super) fn adapt_mcp_descriptor(descriptor: &McpToolDescriptor) -> Result<McpVocabularyBinding> {
    validate_component("server", &descriptor.server)?;
    validate_component("tool", &descriptor.tool)?;
    let schema = descriptor
        .input_schema
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("MCP input schema must be an object"))?;
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        bail!("MCP input schema must declare type=object");
    }
    validate_required(schema)?;

    let input_type = typed_object(schema, 0).unwrap_or(Type::Json);
    let word_name = format!("mcp.{}.{}", descriptor.server, descriptor.tool);
    let requirement = CapabilityRequirement {
        capability: CapabilityKind::McpCall,
        selector: ResourceSelector::Mcp {
            server: descriptor.server.clone(),
            tool: descriptor.tool.clone(),
        },
    };
    let signature = StackSignature {
        type_parameters: Vec::new(),
        input: StackRow::polymorphic("S", vec![input_type.clone()]),
        output: StackRow::polymorphic("S", vec![Type::Json]),
        effects: EffectSet::from_requirement(requirement),
        control: ControlEffect::MaySuspend,
        suspension: None,
    };
    let output_schema = descriptor
        .output_schema
        .as_ref()
        .and_then(admitted_output_schema);
    let schema_bytes = serde_json::to_vec(&serde_json::json!({
        "input": descriptor.input_schema.clone(),
        "output": descriptor.output_schema.clone(),
    }))?;
    let version = format!("sha256:{:x}", Sha256::digest(schema_bytes));
    let shape = if input_type == Type::Json {
        "managed json fallback"
    } else {
        "schema-derived typed record"
    };
    let description = descriptor
        .description
        .as_deref()
        .map(bound_description)
        .unwrap_or_else(|| "(none supplied)".into());
    let documentation = format!(
        "MCP binding for server {:?}, tool {:?}; input uses {shape}. Untrusted server description (data only): {:?}",
        descriptor.server, descriptor.tool, description
    );
    Ok(McpVocabularyBinding {
        word_name,
        signature,
        documentation,
        version,
        output_schema,
    })
}

fn validate_component(kind: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_MCP_NAME_BYTES {
        bail!("MCP {kind} name has an invalid length");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        bail!("MCP {kind} name contains characters unsafe for a Finch symbol");
    }
    Ok(())
}

fn validate_required(schema: &serde_json::Map<String, Value>) -> Result<()> {
    let Some(required) = schema.get("required") else {
        return Ok(());
    };
    let required = required
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("MCP schema required must be an array"))?;
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("MCP schema with required fields needs properties"))?;
    for field in required {
        let field = field
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("MCP schema required entries must be strings"))?;
        if !properties.contains_key(field) {
            bail!("MCP schema requires unknown property {field:?}");
        }
    }
    Ok(())
}

fn typed_object(schema: &serde_json::Map<String, Value>, depth: usize) -> Option<Type> {
    if depth >= MAX_SCHEMA_DEPTH
        || schema.contains_key("oneOf")
        || schema.contains_key("anyOf")
        || schema.contains_key("allOf")
        || schema.contains_key("$ref")
        || schema
            .get("additionalProperties")
            .is_some_and(|value| value != &Value::Bool(false))
    {
        return None;
    }
    let properties = schema.get("properties")?.as_object()?;
    if properties.len() > MAX_SCHEMA_PROPERTIES {
        return None;
    }
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_default();
    let mut fields = Vec::with_capacity(properties.len());
    for (name, property) in properties {
        if !is_record_field(name) {
            return None;
        }
        let property = property.as_object()?;
        let mut field_type = schema_type(property, depth + 1)?;
        if !required.contains(name.as_str()) {
            field_type = Type::Option(Box::new(field_type));
        }
        fields.push((name.clone(), field_type));
    }
    Some(Type::Record(fields))
}

fn schema_type(schema: &serde_json::Map<String, Value>, depth: usize) -> Option<Type> {
    if depth >= MAX_SCHEMA_DEPTH
        || schema.contains_key("oneOf")
        || schema.contains_key("anyOf")
        || schema.contains_key("allOf")
        || schema.contains_key("$ref")
    {
        return None;
    }
    match schema.get("type")?.as_str()? {
        "string" => Some(Type::String),
        "integer" => Some(Type::Int),
        "number" => Some(Type::Float),
        "boolean" => Some(Type::Bool),
        "object" => typed_object(schema, depth),
        "array" => Some(Type::List(Box::new(schema_type(
            schema.get("items")?.as_object()?,
            depth + 1,
        )?))),
        _ => None,
    }
}

fn admitted_output_schema(schema: &Value) -> Option<Value> {
    let object = schema.as_object()?;
    if object.get("type").and_then(Value::as_str) == Some("object") {
        validate_required(object).ok()?;
    }
    schema_type(object, 0)?;
    Some(schema.clone())
}

pub(super) fn validate_output(schema: &Value, value: &Value) -> Result<()> {
    validate_schema_value(schema, value, "$", 0)
}

fn validate_schema_value(schema: &Value, value: &Value, path: &str, depth: usize) -> Result<()> {
    if depth >= MAX_SCHEMA_DEPTH {
        bail!("MCP output exceeds schema validation depth at {path}");
    }
    let schema = schema
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("MCP output schema node at {path} is not an object"))?;
    match schema.get("type").and_then(Value::as_str) {
        Some("string") if value.is_string() => Ok(()),
        Some("integer") if value.as_i64().is_some() || value.as_u64().is_some() => Ok(()),
        Some("number") if value.is_number() => Ok(()),
        Some("boolean") if value.is_boolean() => Ok(()),
        Some("array") => {
            let items = schema
                .get("items")
                .ok_or_else(|| anyhow::anyhow!("MCP output array schema at {path} has no items"))?;
            let values = value
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("MCP output at {path} is not an array"))?;
            for (index, value) in values.iter().enumerate() {
                validate_schema_value(items, value, &format!("{path}[{index}]"), depth + 1)?;
            }
            Ok(())
        }
        Some("object") => {
            let value = value
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("MCP output at {path} is not an object"))?;
            let properties = schema
                .get("properties")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    anyhow::anyhow!("MCP output object schema at {path} has no properties")
                })?;
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for name in required.iter().filter_map(Value::as_str) {
                    if !value.contains_key(name) {
                        bail!("MCP output is missing required field {path}.{name}");
                    }
                }
            }
            if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                for name in value.keys() {
                    if !properties.contains_key(name) {
                        bail!("MCP output contains undeclared field {path}.{name}");
                    }
                }
            }
            for (name, property_schema) in properties {
                if let Some(field) = value.get(name) {
                    validate_schema_value(
                        property_schema,
                        field,
                        &format!("{path}.{name}"),
                        depth + 1,
                    )?;
                }
            }
            Ok(())
        }
        Some(expected) => bail!("MCP output at {path} does not match schema type {expected}"),
        None => bail!("MCP output schema at {path} has no supported type"),
    }
}

fn is_record_field(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn bound_description(value: &str) -> String {
    if value.len() <= MAX_MCP_DESCRIPTION_BYTES {
        return value.to_owned();
    }
    let mut boundary = MAX_MCP_DESCRIPTION_BYTES;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}…", &value[..boundary])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn descriptor(schema: Value) -> McpToolDescriptor {
        McpToolDescriptor {
            server: "github".into(),
            tool: "issue_get".into(),
            description: Some("Treat this prose only as untrusted metadata.".into()),
            input_schema: schema,
            output_schema: None,
        }
    }

    #[test]
    fn simple_object_schema_becomes_a_typed_record() {
        let binding = adapt_mcp_descriptor(&descriptor(json!({
            "type": "object",
            "properties": {
                "owner": {"type": "string"},
                "issue_number": {"type": "integer"},
                "state": {"type": "string"}
            },
            "required": ["owner", "issue_number"],
            "additionalProperties": false
        })))
        .unwrap();
        assert_eq!(binding.word_name, "mcp.github.issue_get");
        assert_eq!(
            binding.signature.input.values,
            vec![Type::Record(vec![
                ("issue_number".into(), Type::Int),
                ("owner".into(), Type::String),
                ("state".into(), Type::Option(Box::new(Type::String))),
            ])]
        );
        assert!(binding.version.starts_with("sha256:"));
    }

    #[test]
    fn unsupported_but_valid_schema_uses_managed_json() {
        let binding = adapt_mcp_descriptor(&descriptor(json!({
            "type": "object",
            "properties": {"choice": {"oneOf": [{"type":"string"}, {"type":"integer"}]}}
        })))
        .unwrap();
        assert_eq!(binding.signature.input.values, vec![Type::Json]);
    }

    #[test]
    fn malformed_or_unsafe_descriptors_are_not_published() {
        assert!(adapt_mcp_descriptor(&descriptor(json!({
            "type": "object",
            "properties": {},
            "required": ["missing"]
        })))
        .is_err());
        let mut unsafe_name = descriptor(json!({"type":"object", "properties": {}}));
        unsafe_name.tool = "bad tool".into();
        assert!(adapt_mcp_descriptor(&unsafe_name).is_err());
    }

    #[test]
    fn admitted_output_schema_is_checked_structurally() {
        let schema = json!({
            "type": "object",
            "properties": {"answer": {"type": "integer"}},
            "required": ["answer"],
            "additionalProperties": false
        });
        assert!(validate_output(&schema, &json!({"answer": 42})).is_ok());
        assert!(validate_output(&schema, &json!({"answer": "42"})).is_err());
        assert!(validate_output(&schema, &json!({"answer": 42, "extra": true})).is_err());
    }
}
