//! Compact surface-type grammar shared by Finch language frontends.

use crate::{DiagnosticPhase, EffectSet, Type, VmDiagnostic};

/// Parse the compact type spelling shared by CoLisp annotations and Co-Forth
/// stack signatures.
pub fn parse_type_name(name: &str) -> Result<Type, Vec<VmDiagnostic>> {
    match name {
        "unit" | "nil" => Ok(Type::Unit),
        "bool" => Ok(Type::Bool),
        "int" => Ok(Type::Int),
        "uint" => Ok(Type::UInt),
        "float" => Ok(Type::Float),
        "char" => Ok(Type::Char),
        "string" | "str" => Ok(Type::String),
        "bytes" => Ok(Type::Bytes),
        "json" => Ok(Type::Json),
        "dynamic" | "any" => Ok(Type::Dynamic),
        _ => parse_record_type(name)
            .or_else(|| parse_variant_type(name))
            .or_else(|| parse_generic_type(name))
            .ok_or_else(|| {
                vec![VmDiagnostic::error(
                    "E-TYPE-009",
                    DiagnosticPhase::TypeInference,
                    format!("unknown type '{name}'"),
                    None,
                )]
            }),
    }
}

fn parse_record_type(name: &str) -> Option<Type> {
    let fields = name.strip_prefix("record{")?.strip_suffix('}')?;
    if fields.is_empty() {
        return Some(Type::Record(Vec::new()));
    }
    let mut parsed = Vec::new();
    for field in split_type_arguments(fields)? {
        let (field_name, field_type) = field.split_once(':')?;
        let field_name = field_name.trim();
        if field_name.is_empty()
            || !field_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || parsed.iter().any(|(existing, _)| existing == field_name)
        {
            return None;
        }
        parsed.push((
            field_name.to_string(),
            parse_type_name(field_type.trim()).ok()?,
        ));
    }
    Some(Type::Record(parsed))
}

fn parse_variant_type(name: &str) -> Option<Type> {
    let alternatives = name.strip_prefix("variant{")?.strip_suffix('}')?;
    if alternatives.is_empty() {
        return None;
    }
    let mut parsed = Vec::new();
    for alternative in split_variant_alternatives(alternatives)? {
        let alternative = alternative.trim();
        let (tag, payload) = if alternative.ends_with(')') {
            let open = alternative.find('(')?;
            let tag = alternative[..open].trim();
            let payload = &alternative[open + 1..alternative.len() - 1];
            if payload.is_empty() {
                return None;
            }
            (tag, Some(parse_type_name(payload).ok()?))
        } else {
            (alternative, None)
        };
        if tag.is_empty()
            || !tag
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || parsed.iter().any(|(existing, _)| existing == tag)
        {
            return None;
        }
        parsed.push((tag.to_string(), payload));
    }
    Some(Type::Variant(parsed))
}

fn split_variant_alternatives(source: &str) -> Option<Vec<&str>> {
    let mut alternatives = Vec::new();
    let mut angle_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut start = 0;
    for (index, character) in source.char_indices() {
        match character {
            '<' => angle_depth += 1,
            '>' => angle_depth = angle_depth.checked_sub(1)?,
            '{' => brace_depth += 1,
            '}' => brace_depth = brace_depth.checked_sub(1)?,
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.checked_sub(1)?,
            '|' if angle_depth == 0 && brace_depth == 0 && paren_depth == 0 => {
                let alternative = source[start..index].trim();
                if alternative.is_empty() {
                    return None;
                }
                alternatives.push(alternative);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    if angle_depth != 0 || brace_depth != 0 || paren_depth != 0 {
        return None;
    }
    let alternative = source[start..].trim();
    if alternative.is_empty() {
        return None;
    }
    alternatives.push(alternative);
    Some(alternatives)
}

fn parse_generic_type(name: &str) -> Option<Type> {
    let (head, arguments) = name.split_once('<')?;
    let inner = arguments.strip_suffix('>')?;
    let arguments = split_type_arguments(inner)?;
    let one = || {
        (arguments.len() == 1)
            .then(|| parse_type_name(arguments[0]).ok())
            .flatten()
    };
    match head {
        "list" => one().map(Type::list),
        "option" => one().map(|inner| Type::Option(Box::new(inner))),
        "task" => one().map(|inner| Type::Task(Box::new(inner))),
        "fiber" if arguments.len() == 2 => Some(Type::Fiber(
            Box::new(parse_type_name(arguments[0]).ok()?),
            Box::new(parse_type_name(arguments[1]).ok()?),
        )),
        "stream" => one().map(|inner| Type::Stream(Box::new(inner))),
        "resource" => (arguments.len() == 1).then(|| Type::Resource(arguments[0].to_string())),
        "capability" => (arguments.len() == 1).then(|| Type::Capability(arguments[0].to_string())),
        "map" if arguments.len() == 2 => Some(Type::Map(
            Box::new(parse_type_name(arguments[0]).ok()?),
            Box::new(parse_type_name(arguments[1]).ok()?),
        )),
        "result" if arguments.len() == 2 => Some(Type::result(
            parse_type_name(arguments[0]).ok()?,
            parse_type_name(arguments[1]).ok()?,
        )),
        "fn" if !arguments.is_empty() => {
            let (result, inputs) = arguments.split_last()?;
            Some(Type::Function {
                arguments: inputs
                    .iter()
                    .map(|input| parse_type_name(input).ok())
                    .collect::<Option<Vec<_>>>()?,
                result: Box::new(parse_type_name(result).ok()?),
                effects: EffectSet::pure(),
                suspension: None,
            })
        }
        _ => None,
    }
}

fn split_type_arguments(source: &str) -> Option<Vec<&str>> {
    let mut arguments = Vec::new();
    let mut angle_depth = 0usize;
    let mut record_depth = 0usize;
    let mut start = 0;
    for (index, character) in source.char_indices() {
        match character {
            '<' => angle_depth += 1,
            '>' => angle_depth = angle_depth.checked_sub(1)?,
            '{' => record_depth += 1,
            '}' => record_depth = record_depth.checked_sub(1)?,
            ',' if angle_depth == 0 && record_depth == 0 => {
                let argument = source[start..index].trim();
                if argument.is_empty() {
                    return None;
                }
                arguments.push(argument);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    if angle_depth != 0 || record_depth != 0 {
        return None;
    }
    let argument = source[start..].trim();
    if argument.is_empty() {
        return None;
    }
    arguments.push(argument);
    Some(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compact_pure_function_types_shared_by_frontends() {
        assert_eq!(
            parse_type_name("fn<int,int>").unwrap(),
            Type::Function {
                arguments: vec![Type::Int],
                result: Box::new(Type::Int),
                effects: EffectSet::pure(),
                suspension: None,
            }
        );
        assert_eq!(
            parse_type_name("fn<int>").unwrap(),
            Type::Function {
                arguments: Vec::new(),
                result: Box::new(Type::Int),
                effects: EffectSet::pure(),
                suspension: None,
            }
        );
    }

    #[test]
    fn parses_nested_parameterized_type_annotations() {
        assert_eq!(
            parse_type_name("result<option<list<int>>,string>").unwrap(),
            Type::result(Type::Option(Box::new(Type::list(Type::Int))), Type::String)
        );
        assert_eq!(
            parse_type_name("stream<list<string>>").unwrap(),
            Type::Stream(Box::new(Type::list(Type::String)))
        );
        assert_eq!(
            parse_type_name("fiber<int,string>").unwrap(),
            Type::Fiber(Box::new(Type::Int), Box::new(Type::String))
        );
        assert_eq!(
            parse_type_name("record{name:string,meta:map<string,list<int>>}").unwrap(),
            Type::Record(vec![
                ("name".into(), Type::String),
                (
                    "meta".into(),
                    Type::Map(Box::new(Type::String), Box::new(Type::list(Type::Int))),
                ),
            ])
        );
        assert_eq!(
            parse_type_name("variant{none|some(int)|metadata(record{name:string})}").unwrap(),
            Type::Variant(vec![
                ("none".into(), None),
                ("some".into(), Some(Type::Int)),
                (
                    "metadata".into(),
                    Some(Type::Record(vec![("name".into(), Type::String)])),
                ),
            ])
        );
    }
}
