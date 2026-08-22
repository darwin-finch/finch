use crate::vm::diagnostic::{
    DiagnosticPhase, SourceLanguage, SourceOrigin, SourceSpan, VmDiagnostic,
};
use crate::vm::effects::EffectSet;
use crate::vm::ir::{BasicBlock, Function, Instruction, LocatedInstruction, Module};
use crate::vm::signature::{ControlEffect, StackRow, StackSignature};
use crate::vm::types::{Type, TypedValue};
use crate::vm::verifier::{apply_signature_types, VerifiedModule, Verifier, Vocabulary};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
struct Token {
    value: TokenValue,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone)]
enum TokenValue {
    Word(String),
    String(String),
}

#[derive(Debug, Clone)]
struct LoopFrame {
    header: u32,
    exit: Option<u32>,
    stack: Vec<Type>,
    origin: SourceOrigin,
}

#[derive(Debug, Clone)]
struct IfFrame {
    else_block: u32,
    merge_block: u32,
    entry_stack: Vec<Type>,
    then_stack: Option<Vec<Type>>,
    origin: SourceOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlKind {
    If,
    Loop,
}

/// Compile user/model-entered Co-Forth source text directly into Finch typed
/// stack IR and run the common verifier. `initial_stack` is the actual typed VM
/// stack against which the program was composed.
pub fn compile_forth(
    source_id: &str,
    source: &str,
    initial_stack: Vec<Type>,
    vocabulary: &Vocabulary,
) -> Result<VerifiedModule, Vec<VmDiagnostic>> {
    compile_forth_with_functions(
        source_id,
        source,
        initial_stack,
        vocabulary,
        &BTreeMap::new(),
    )
}

pub fn compile_forth_with_functions(
    source_id: &str,
    source: &str,
    initial_stack: Vec<Type>,
    vocabulary: &Vocabulary,
    linked_functions: &BTreeMap<String, Function>,
) -> Result<VerifiedModule, Vec<VmDiagnostic>> {
    let (definitions, main_source) = extract_definitions(source_id, source)?;
    if definitions.is_empty() {
        return compile_forth_body_with_functions(
            source_id,
            source,
            initial_stack,
            vocabulary,
            linked_functions,
        );
    }

    let mut functions = linked_functions.clone();
    let mut local_vocabulary = vocabulary.clone();
    for definition in definitions {
        if vocabulary.contains_key(&definition.name)
            || functions.contains_key(&definition.name)
            || definition.name == "main"
        {
            return Err(vec![control_error(
                "E-FORTH-DEF-001",
                format!("word '{}' is already defined", definition.name),
                definition.origin,
            )]);
        }
        local_vocabulary.insert(definition.name.clone(), definition.signature.clone());
        let compiled = compile_forth_body_with_functions(
            source_id,
            &definition.body,
            definition.signature.input.values.clone(),
            &local_vocabulary,
            &functions,
        )?;
        let verified = &compiled.functions[&compiled.module.entry];
        let mut function = compiled.module.functions[&compiled.module.entry].clone();
        let actual_output = function.signature.output.values.clone();
        if actual_output != definition.signature.output.values {
            let mut diagnostic = control_error(
                "E-FORTH-DEF-002",
                format!(
                    "word '{}' declares output {:?} but its body leaves {:?}",
                    definition.name, definition.signature.output.values, actual_output
                ),
                definition.origin,
            );
            diagnostic.expected_types = definition.signature.output.values;
            diagnostic.found_types = actual_output;
            return Err(vec![diagnostic]);
        }
        if definition.declares_pure && !verified.inferred_effects.is_pure() {
            let mut diagnostic = control_error(
                "E-CAP-001",
                format!(
                    "word '{}' declares {{}} but requires {}",
                    definition.name, verified.inferred_effects
                ),
                definition.origin,
            );
            diagnostic.found_effects = verified.inferred_effects.clone();
            return Err(vec![diagnostic]);
        }
        function.name = definition.name.clone();
        function.signature = definition.signature;
        function.signature.effects = verified.inferred_effects.clone();
        local_vocabulary.insert(definition.name.clone(), function.signature.clone());
        functions.insert(definition.name, function);
    }

    compile_forth_body_with_functions(
        source_id,
        &main_source,
        initial_stack,
        &local_vocabulary,
        &functions,
    )
}

fn compile_forth_body_with_functions(
    source_id: &str,
    source: &str,
    initial_stack: Vec<Type>,
    vocabulary: &Vocabulary,
    linked_functions: &BTreeMap<String, Function>,
) -> Result<VerifiedModule, Vec<VmDiagnostic>> {
    let tokens = tokenize(source_id, source)?;
    let mut stack = initial_stack.clone();
    let mut effects = EffectSet::pure();
    let mut blocks = BTreeMap::from([(
        0,
        BasicBlock {
            id: 0,
            instructions: Vec::new(),
        },
    )]);
    let mut current = 0;
    let mut next_block = 1;
    let mut loops: Vec<LoopFrame> = Vec::new();
    let mut conditionals: Vec<IfFrame> = Vec::new();
    let mut control = Vec::new();

    let emit = |blocks: &mut BTreeMap<u32, BasicBlock>,
                current: u32,
                instruction: Instruction,
                origin: SourceOrigin| {
        blocks
            .get_mut(&current)
            .expect("current block exists")
            .instructions
            .push(LocatedInstruction {
                instruction,
                origin,
            });
    };

    for token in tokens {
        let origin = origin(source_id, source, token.start, token.end);
        if let TokenValue::Word(word) = &token.value {
            match word.as_str() {
                "if" => {
                    let condition = stack.pop().ok_or_else(|| {
                        vec![control_error(
                            "E-STACK-001",
                            "if requires a boolean condition",
                            origin.clone(),
                        )]
                    })?;
                    if condition != Type::Bool {
                        return Err(vec![VmDiagnostic::type_mismatch(
                            Type::Bool,
                            condition,
                            Some(origin),
                        )]);
                    }
                    let then_block = next_block;
                    let else_block = next_block + 1;
                    let merge_block = next_block + 2;
                    next_block += 3;
                    for id in [then_block, else_block, merge_block] {
                        blocks.insert(
                            id,
                            BasicBlock {
                                id,
                                instructions: Vec::new(),
                            },
                        );
                    }
                    emit(
                        &mut blocks,
                        current,
                        Instruction::Branch {
                            then_block,
                            else_block,
                        },
                        origin.clone(),
                    );
                    conditionals.push(IfFrame {
                        else_block,
                        merge_block,
                        entry_stack: stack.clone(),
                        then_stack: None,
                        origin,
                    });
                    control.push(ControlKind::If);
                    current = then_block;
                    continue;
                }
                "else" => {
                    if control.last() != Some(&ControlKind::If) {
                        return Err(vec![control_error(
                            "E-FORTH-CONTROL-001",
                            "else crosses an unclosed loop or has no matching if",
                            origin,
                        )]);
                    }
                    let Some(frame) = conditionals.last_mut() else {
                        return Err(vec![control_error(
                            "E-FORTH-IF-001",
                            "else has no matching if",
                            origin,
                        )]);
                    };
                    if frame.then_stack.is_some() {
                        return Err(vec![control_error(
                            "E-FORTH-IF-002",
                            "if may contain only one structural else",
                            origin,
                        )]);
                    }
                    frame.then_stack = Some(stack.clone());
                    emit(
                        &mut blocks,
                        current,
                        Instruction::Jump {
                            target: frame.merge_block,
                        },
                        origin,
                    );
                    current = frame.else_block;
                    stack = frame.entry_stack.clone();
                    continue;
                }
                "then" => {
                    if control.last() != Some(&ControlKind::If) {
                        return Err(vec![control_error(
                            "E-FORTH-CONTROL-001",
                            "then crosses an unclosed loop or has no matching if",
                            origin,
                        )]);
                    }
                    let Some(frame) = conditionals.pop() else {
                        return Err(vec![control_error(
                            "E-FORTH-IF-001",
                            "then has no matching if",
                            origin,
                        )]);
                    };
                    control.pop();
                    let merged_stack = if let Some(then_stack) = frame.then_stack {
                        if stack != then_stack {
                            return Err(vec![control_error(
                                "E-STACK-004",
                                "if branches leave incompatible stack types",
                                frame.origin,
                            )]);
                        }
                        stack.clone()
                    } else {
                        if stack != frame.entry_stack {
                            return Err(vec![control_error(
                                "E-STACK-004",
                                "if without else must preserve the stack",
                                frame.origin,
                            )]);
                        }
                        emit(
                            &mut blocks,
                            frame.else_block,
                            Instruction::Jump {
                                target: frame.merge_block,
                            },
                            origin.clone(),
                        );
                        frame.entry_stack
                    };
                    emit(
                        &mut blocks,
                        current,
                        Instruction::Jump {
                            target: frame.merge_block,
                        },
                        origin,
                    );
                    current = frame.merge_block;
                    stack = merged_stack;
                    continue;
                }
                "begin" => {
                    let header = next_block;
                    next_block += 1;
                    blocks.insert(
                        header,
                        BasicBlock {
                            id: header,
                            instructions: Vec::new(),
                        },
                    );
                    emit(
                        &mut blocks,
                        current,
                        Instruction::Jump { target: header },
                        origin.clone(),
                    );
                    current = header;
                    loops.push(LoopFrame {
                        header,
                        exit: None,
                        stack: stack.clone(),
                        origin,
                    });
                    control.push(ControlKind::Loop);
                    continue;
                }
                "while" => {
                    if control.last() != Some(&ControlKind::Loop) {
                        return Err(vec![control_error(
                            "E-FORTH-CONTROL-001",
                            "while crosses an unclosed conditional or has no matching begin",
                            origin,
                        )]);
                    }
                    let Some(frame) = loops.last_mut() else {
                        return Err(vec![control_error(
                            "E-FORTH-LOOP-001",
                            "while has no matching begin",
                            origin,
                        )]);
                    };
                    if frame.exit.is_some() {
                        return Err(vec![control_error(
                            "E-FORTH-LOOP-002",
                            "a begin loop may contain only one structural while",
                            origin,
                        )]);
                    }
                    let condition = stack.pop().ok_or_else(|| {
                        vec![control_error(
                            "E-STACK-001",
                            "while requires a boolean condition",
                            origin.clone(),
                        )]
                    })?;
                    if condition != Type::Bool {
                        return Err(vec![VmDiagnostic::type_mismatch(
                            Type::Bool,
                            condition,
                            Some(origin),
                        )]);
                    }
                    if stack != frame.stack {
                        return Err(vec![control_error(
                            "E-STACK-005",
                            "while condition must preserve the loop stack",
                            frame.origin.clone(),
                        )]);
                    }
                    let body = next_block;
                    let exit = next_block + 1;
                    next_block += 2;
                    blocks.insert(
                        body,
                        BasicBlock {
                            id: body,
                            instructions: Vec::new(),
                        },
                    );
                    blocks.insert(
                        exit,
                        BasicBlock {
                            id: exit,
                            instructions: Vec::new(),
                        },
                    );
                    emit(
                        &mut blocks,
                        current,
                        Instruction::Branch {
                            then_block: body,
                            else_block: exit,
                        },
                        origin,
                    );
                    frame.exit = Some(exit);
                    current = body;
                    continue;
                }
                "repeat" => {
                    if control.last() != Some(&ControlKind::Loop) {
                        return Err(vec![control_error(
                            "E-FORTH-CONTROL-001",
                            "repeat crosses an unclosed conditional or has no matching begin",
                            origin,
                        )]);
                    }
                    let Some(frame) = loops.pop() else {
                        return Err(vec![control_error(
                            "E-FORTH-LOOP-001",
                            "repeat has no matching begin",
                            origin,
                        )]);
                    };
                    control.pop();
                    let Some(exit) = frame.exit else {
                        return Err(vec![control_error(
                            "E-FORTH-LOOP-003",
                            "repeat requires a matching while",
                            origin,
                        )]);
                    };
                    if stack != frame.stack {
                        return Err(vec![control_error(
                            "E-STACK-005",
                            "loop body must preserve the loop stack",
                            frame.origin,
                        )]);
                    }
                    emit(
                        &mut blocks,
                        current,
                        Instruction::Jump {
                            target: frame.header,
                        },
                        origin,
                    );
                    current = exit;
                    stack = frame.stack;
                    continue;
                }
                "until" => {
                    if control.last() != Some(&ControlKind::Loop) {
                        return Err(vec![control_error(
                            "E-FORTH-CONTROL-001",
                            "until crosses an unclosed conditional or has no matching begin",
                            origin,
                        )]);
                    }
                    let Some(frame) = loops.pop() else {
                        return Err(vec![control_error(
                            "E-FORTH-LOOP-001",
                            "until has no matching begin",
                            origin,
                        )]);
                    };
                    control.pop();
                    if frame.exit.is_some() {
                        return Err(vec![control_error(
                            "E-FORTH-LOOP-004",
                            "use repeat to close a begin/while loop",
                            origin,
                        )]);
                    }
                    let condition = stack.pop().ok_or_else(|| {
                        vec![control_error(
                            "E-STACK-001",
                            "until requires a boolean condition",
                            origin.clone(),
                        )]
                    })?;
                    if condition != Type::Bool {
                        return Err(vec![VmDiagnostic::type_mismatch(
                            Type::Bool,
                            condition,
                            Some(origin),
                        )]);
                    }
                    if stack != frame.stack {
                        return Err(vec![control_error(
                            "E-STACK-005",
                            "loop body must preserve the loop stack",
                            frame.origin,
                        )]);
                    }
                    let exit = next_block;
                    next_block += 1;
                    blocks.insert(
                        exit,
                        BasicBlock {
                            id: exit,
                            instructions: Vec::new(),
                        },
                    );
                    emit(
                        &mut blocks,
                        current,
                        Instruction::Branch {
                            then_block: exit,
                            else_block: frame.header,
                        },
                        origin,
                    );
                    current = exit;
                    stack = frame.stack;
                    continue;
                }
                _ => {}
            }
        }
        let instruction = match token.value {
            TokenValue::String(value) => {
                stack.push(Type::String);
                Instruction::Constant {
                    value: TypedValue::String(value),
                }
            }
            TokenValue::Word(word) => {
                if let Ok(value) = word.parse::<i64>() {
                    stack.push(Type::Int);
                    Instruction::Constant {
                        value: TypedValue::Int(value),
                    }
                } else if word == "true" || word == "false" {
                    stack.push(Type::Bool);
                    Instruction::Constant {
                        value: TypedValue::Bool(word == "true"),
                    }
                } else if word == "dup" {
                    let value = stack.last().cloned().ok_or_else(|| {
                        vec![VmDiagnostic::error(
                            "E-STACK-001",
                            DiagnosticPhase::TypeInference,
                            "dup requires one value",
                            Some(origin.clone()),
                        )]
                    })?;
                    stack.push(value);
                    Instruction::Dup
                } else if word == "drop" {
                    stack.pop().ok_or_else(|| {
                        vec![VmDiagnostic::error(
                            "E-STACK-001",
                            DiagnosticPhase::TypeInference,
                            "drop requires one value",
                            Some(origin.clone()),
                        )]
                    })?;
                    Instruction::Drop
                } else if word == "swap" {
                    if stack.len() < 2 {
                        return Err(vec![VmDiagnostic::error(
                            "E-STACK-001",
                            DiagnosticPhase::TypeInference,
                            "swap requires two values",
                            Some(origin),
                        )]);
                    }
                    let len = stack.len();
                    stack.swap(len - 1, len - 2);
                    Instruction::Swap
                } else {
                    let Some(signature) = vocabulary.get(&word) else {
                        return Err(vec![VmDiagnostic::error(
                            "E-LINK-002",
                            DiagnosticPhase::NameResolution,
                            format!("unknown Co-Forth word '{word}'"),
                            Some(origin),
                        )]);
                    };
                    apply_signature_types(signature, &mut stack, &origin)
                        .map_err(|diagnostic| vec![diagnostic])?;
                    effects = effects.union(&signature.effects);
                    if signature.effects.0.len() == 1 {
                        Instruction::CapabilityRequest {
                            requirement: signature.effects.0.iter().next().unwrap().clone(),
                            input: signature.input.values.clone(),
                            output: signature.output.values.clone(),
                        }
                    } else {
                        Instruction::Call { function: word }
                    }
                }
            }
        };
        emit(&mut blocks, current, instruction, origin);
    }
    if let Some(frame) = loops.last() {
        return Err(vec![control_error(
            "E-FORTH-LOOP-005",
            "unterminated begin loop",
            frame.origin.clone(),
        )]);
    }
    if let Some(frame) = conditionals.last() {
        return Err(vec![control_error(
            "E-FORTH-IF-003",
            "unterminated if",
            frame.origin.clone(),
        )]);
    }
    emit(
        &mut blocks,
        current,
        Instruction::Return,
        SourceOrigin {
            language: SourceLanguage::Forth,
            span: Some(span(source_id, source, source.len(), source.len())),
            word: Some("<return>".into()),
            expansion: None,
        },
    );

    let function = Function {
        name: "main".into(),
        signature: StackSignature {
            type_parameters: Vec::new(),
            input: StackRow::closed(initial_stack),
            output: StackRow::closed(stack),
            effects,
            control: ControlEffect::Returns,
        },
        locals: Vec::new(),
        captures: Vec::new(),
        entry: 0,
        blocks,
    };
    let mut module = Module::single(function);
    for (name, function) in linked_functions {
        module.functions.insert(name.clone(), function.clone());
    }
    Verifier::new(vocabulary).verify(module)
}

struct ParsedDefinition {
    name: String,
    signature: StackSignature,
    declares_pure: bool,
    body: String,
    origin: SourceOrigin,
}

fn extract_definitions(
    source_id: &str,
    source: &str,
) -> Result<(Vec<ParsedDefinition>, String), Vec<VmDiagnostic>> {
    let tokens = tokenize(source_id, source)?;
    let mut definitions = Vec::new();
    let mut erased = source.as_bytes().to_vec();
    let mut cursor = 0;
    while cursor < tokens.len() {
        if !token_is(&tokens[cursor], ":") {
            cursor += 1;
            continue;
        }
        let definition_start = tokens[cursor].start;
        let Some(name_token) = tokens.get(cursor + 1) else {
            return Err(vec![control_error(
                "E-FORTH-DEF-003",
                ": requires a word name",
                origin(source_id, source, tokens[cursor].start, tokens[cursor].end),
            )]);
        };
        let TokenValue::Word(name) = &name_token.value else {
            return Err(vec![control_error(
                "E-FORTH-DEF-003",
                "a word name must be an identifier",
                origin(source_id, source, name_token.start, name_token.end),
            )]);
        };
        let Some(open) = tokens.get(cursor + 2) else {
            return Err(vec![definition_error(
                source_id,
                source,
                name_token,
                "a typed definition requires '( inputs -- outputs ! effects )'",
            )]);
        };
        if !token_is(open, "(") {
            return Err(vec![definition_error(
                source_id,
                source,
                open,
                "a typed definition requires '( inputs -- outputs ! effects )'",
            )]);
        }
        let close = (cursor + 3..tokens.len())
            .find(|index| token_is(&tokens[*index], ")"))
            .ok_or_else(|| {
                vec![definition_error(
                    source_id,
                    source,
                    open,
                    "unterminated typed word signature",
                )]
            })?;
        let signature_tokens = &tokens[cursor + 3..close];
        let (signature, declares_pure) =
            parse_definition_signature(source_id, source, signature_tokens, open)?;
        let body_start_index = close + 1;
        let end = (body_start_index..tokens.len())
            .find(|index| token_is(&tokens[*index], ";"))
            .ok_or_else(|| {
                vec![definition_error(
                    source_id,
                    source,
                    name_token,
                    "unterminated word definition",
                )]
            })?;
        if tokens[body_start_index..end]
            .iter()
            .any(|token| token_is(token, ":"))
        {
            return Err(vec![definition_error(
                source_id,
                source,
                &tokens[body_start_index],
                "nested word definitions are not allowed",
            )]);
        }
        let body_start = tokens
            .get(body_start_index)
            .map_or(tokens[end].start, |token| token.start);
        let body_end = tokens[end].start;
        let definition_end = tokens[end].end;
        definitions.push(ParsedDefinition {
            name: name.clone(),
            signature,
            declares_pure,
            body: source[body_start..body_end].to_string(),
            origin: origin(source_id, source, definition_start, definition_end),
        });
        for byte in &mut erased[definition_start..definition_end] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
        cursor = end + 1;
    }
    let main = String::from_utf8(erased).expect("erasing source preserves UTF-8 bytes");
    Ok((definitions, main))
}

fn parse_definition_signature(
    source_id: &str,
    source: &str,
    tokens: &[Token],
    fallback: &Token,
) -> Result<(StackSignature, bool), Vec<VmDiagnostic>> {
    let separator = tokens
        .iter()
        .position(|token| token_is(token, "--"))
        .ok_or_else(|| {
            vec![definition_error(
                source_id,
                source,
                fallback,
                "word signature is missing '--'",
            )]
        })?;
    let effect = tokens.iter().position(|token| token_is(token, "!"));
    let output_end = effect.unwrap_or(tokens.len());
    let input = parse_stack_types(source_id, source, &tokens[..separator])?;
    let output = parse_stack_types(source_id, source, &tokens[separator + 1..output_end])?;
    let declares_pure = if let Some(effect) = effect {
        let annotation = &tokens[effect + 1..];
        if annotation.len() == 1 && token_is(&annotation[0], "{}") {
            true
        } else if annotation.len() == 1 && token_is(&annotation[0], "infer") {
            false
        } else {
            return Err(vec![definition_error(
                source_id,
                source,
                tokens.get(effect).unwrap_or(fallback),
                "effect annotation must currently be '{}' or 'infer'",
            )]);
        }
    } else {
        false
    };
    Ok((
        StackSignature {
            type_parameters: Vec::new(),
            input: StackRow::polymorphic("S", input),
            output: StackRow::polymorphic("S", output),
            effects: EffectSet::pure(),
            control: ControlEffect::Returns,
        },
        declares_pure,
    ))
}

fn parse_stack_types(
    source_id: &str,
    source: &str,
    tokens: &[Token],
) -> Result<Vec<Type>, Vec<VmDiagnostic>> {
    tokens
        .iter()
        .filter(|token| !token_is(token, "S"))
        .map(|token| {
            let TokenValue::Word(name) = &token.value else {
                return Err(vec![definition_error(
                    source_id,
                    source,
                    token,
                    "stack type must be an identifier",
                )]);
            };
            match name.as_str() {
                "unit" => Ok(Type::Unit),
                "bool" => Ok(Type::Bool),
                "int" => Ok(Type::Int),
                "uint" => Ok(Type::UInt),
                "float" => Ok(Type::Float),
                "char" => Ok(Type::Char),
                "string" | "str" => Ok(Type::String),
                "bytes" => Ok(Type::Bytes),
                "dynamic" | "any" => Ok(Type::Dynamic),
                _ => Err(vec![definition_error(
                    source_id,
                    source,
                    token,
                    format!("unknown stack type '{name}'"),
                )]),
            }
        })
        .collect()
}

fn token_is(token: &Token, expected: &str) -> bool {
    matches!(&token.value, TokenValue::Word(value) if value == expected)
}

fn definition_error(
    source_id: &str,
    source: &str,
    token: &Token,
    message: impl Into<String>,
) -> VmDiagnostic {
    control_error(
        "E-FORTH-SIG-001",
        message,
        origin(source_id, source, token.start, token.end),
    )
}

fn control_error(code: &str, message: impl Into<String>, origin: SourceOrigin) -> VmDiagnostic {
    VmDiagnostic::error(code, DiagnosticPhase::TypeInference, message, Some(origin))
}

fn tokenize(source_id: &str, source: &str) -> Result<Vec<Token>, Vec<VmDiagnostic>> {
    let bytes = source.as_bytes();
    let mut cursor = 0;
    let mut tokens = Vec::new();
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor == bytes.len() {
            break;
        }
        if bytes[cursor] == b'\\' {
            while cursor < bytes.len() && bytes[cursor] != b'\n' {
                cursor += 1;
            }
            continue;
        }
        let start = cursor;
        if source[start..].starts_with("s\"") {
            cursor += 2;
            if cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            let content_start = cursor;
            while cursor < bytes.len() && bytes[cursor] != b'"' {
                cursor += 1;
            }
            if cursor == bytes.len() {
                return Err(vec![VmDiagnostic::error(
                    "E-READ-001",
                    DiagnosticPhase::Reader,
                    "unterminated Co-Forth string literal",
                    Some(origin(source_id, source, start, source.len())),
                )]);
            }
            tokens.push(Token {
                value: TokenValue::String(source[content_start..cursor].to_string()),
                start,
                end: cursor + 1,
            });
            cursor += 1;
            continue;
        }
        while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        tokens.push(Token {
            value: TokenValue::Word(source[start..cursor].to_string()),
            start,
            end: cursor,
        });
    }
    Ok(tokens)
}

fn origin(source_id: &str, source: &str, start: usize, end: usize) -> SourceOrigin {
    SourceOrigin {
        language: SourceLanguage::Forth,
        span: Some(span(source_id, source, start, end)),
        word: Some(source[start..end].to_string()),
        expansion: None,
    }
}

fn span(source_id: &str, source: &str, start: usize, end: usize) -> SourceSpan {
    let (start_line, start_column) = line_column(source, start);
    let (end_line, end_column) = line_column(source, end);
    SourceSpan {
        source_id: source_id.to_string(),
        start_byte: start,
        end_byte: end,
        start_line,
        start_column,
        end_line,
        end_column,
    }
}

fn line_column(source: &str, byte: usize) -> (usize, usize) {
    let prefix = &source[..byte];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix.chars().count() + 1, |(_, tail)| {
            tail.chars().count() + 1
        });
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::interpreter::{DenyCapabilities, Interpreter, InterpreterConfig};
    use crate::vm::{core_vocabulary, TypedValue};

    #[test]
    fn compiles_and_executes_user_forth_text() {
        let module =
            compile_forth("input.forth", "3 4 2 * +", Vec::new(), &core_vocabulary()).unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(11)]);
    }

    #[test]
    fn compiles_against_preexisting_typed_stack() {
        let module =
            compile_forth("input.forth", "2 *", vec![Type::Int], &core_vocabulary()).unwrap();
        let mut stack = vec![TypedValue::Int(9)];
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(18)]);
    }

    #[test]
    fn reports_source_location_for_type_error() {
        let errors = compile_forth(
            "input.forth",
            "s\" hello\" 2 +",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert_eq!(errors[0].code, "E-TYPE-002");
        assert_eq!(
            errors[0]
                .primary
                .as_ref()
                .unwrap()
                .span
                .as_ref()
                .unwrap()
                .start_line,
            1
        );
    }

    #[test]
    fn compiles_begin_until_loop() {
        let module = compile_forth(
            "input.forth",
            "begin true until 7",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(7)]);
    }

    #[test]
    fn rejects_loop_with_unstable_stack_shape() {
        let errors = compile_forth(
            "input.forth",
            "begin true while 1 repeat",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert!(errors.iter().any(|error| error.code == "E-STACK-005"));
    }

    #[test]
    fn infinite_loop_exhausts_fuel_and_rolls_back() {
        let module = compile_forth(
            "input.forth",
            "begin false until",
            vec![Type::Int],
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = vec![TypedValue::Int(42)];
        let error = Interpreter::new(
            &module,
            DenyCapabilities,
            InterpreterConfig {
                fuel: 10,
                ..InterpreterConfig::default()
            },
        )
        .execute(&mut stack)
        .unwrap_err();
        assert_eq!(error.code, "E-LIMIT-001");
        assert_eq!(stack, vec![TypedValue::Int(42)]);
    }

    #[test]
    fn compiles_typed_if_else_then() {
        let module = compile_forth(
            "input.forth",
            "true if 10 else 20 then",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(10)]);
    }

    #[test]
    fn rejects_if_branches_with_different_types() {
        let errors = compile_forth(
            "input.forth",
            "true if 10 else s\" twenty\" then",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert!(errors.iter().any(|error| error.code == "E-STACK-004"));
    }

    #[test]
    fn rejects_crossed_control_structures() {
        let errors = compile_forth(
            "input.forth",
            "begin true if repeat then",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert_eq!(errors[0].code, "E-FORTH-CONTROL-001");
    }
}
