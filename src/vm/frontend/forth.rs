use crate::vm::diagnostic::{
    DiagnosticPhase, SourceLanguage, SourceOrigin, SourceSpan, VmDiagnostic,
};
use crate::vm::effects::EffectSet;
use crate::vm::interpreter::UiOperation;
use crate::vm::ir::{BasicBlock, Function, Instruction, LocatedInstruction, Module};
use crate::vm::signature::{ControlEffect, StackRow, StackSignature};
use crate::vm::types::{Type, TypedValue};
use crate::vm::verifier::{apply_signature_types, VerifiedModule, Verifier, Vocabulary};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
struct LocalBinding {
    name: String,
    ty: Type,
    origin: SourceOrigin,
}

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
    label: Option<String>,
    header: u32,
    exit: Option<u32>,
    stack: Vec<Type>,
    origin: SourceOrigin,
}

#[derive(Debug, Clone)]
struct IfFrame {
    else_block: u32,
    merge_block: u32,
    /// The stack supplied to the else branch.  Ordinary `if` uses the same
    /// entry row for both branches; a structured match consumes its tagged
    /// value in each branch and supplies the selected payload only to the
    /// corresponding branch.
    entry_stack: Vec<Type>,
    then_stack: Option<Vec<Type>>,
    match_condition: Option<MatchCondition>,
    else_payload: Option<Type>,
    origin: SourceOrigin,
}

/// A conventional Co-Forth integer `case` expression.  The selector stays on
/// the operand stack while arms are tested, but is removed before either a
/// selected arm or an explicit `otherwise` arm runs.  This makes every arm
/// start with the same stack row and keeps the construct a small lowering to
/// the existing typed branch IR rather than a second dispatch mechanism.
#[derive(Debug, Clone)]
struct CaseFrame {
    end_block: u32,
    selector_stack: Vec<Type>,
    arm_output: Option<Vec<Type>>,
    next_else: Option<u32>,
    arm_open: bool,
    in_default: bool,
    origin: SourceOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchCondition {
    Option,
    Result,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlKind {
    If,
    Loop,
    Case,
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
    let mut definition_names = BTreeSet::new();
    for definition in &definitions {
        if vocabulary.contains_key(&definition.name)
            || functions.contains_key(&definition.name)
            || definition.name == "main"
            || !definition_names.insert(definition.name.clone())
        {
            return Err(vec![control_error(
                "E-FORTH-DEF-001",
                format!("word '{}' is already defined", definition.name),
                definition.origin.clone(),
            )]);
        }
        // A declared-pure signature is complete authority information, so it
        // can safely be made visible before compiling bodies. This supports
        // pure mutually-recursive words without an untyped forward reference.
        // `! infer` words intentionally remain sequential: their effects are
        // learned from their body and must not be guessed for a sibling call.
        if definition.declares_pure {
            local_vocabulary.insert(definition.name.clone(), definition.signature.clone());
        }
    }
    for definition in definitions {
        local_vocabulary.insert(definition.name.clone(), definition.signature.clone());
        let compiled = compile_forth_body_with_locals(
            source_id,
            &definition.body,
            definition.signature.input.values.clone(),
            &local_vocabulary,
            &functions,
            &definition.locals,
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
        function.documentation = definition.documentation;
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
    compile_forth_body_with_locals(
        source_id,
        source,
        initial_stack,
        vocabulary,
        linked_functions,
        &[],
    )
}

fn compile_forth_body_with_locals(
    source_id: &str,
    source: &str,
    initial_stack: Vec<Type>,
    vocabulary: &Vocabulary,
    linked_functions: &BTreeMap<String, Function>,
    locals: &[LocalBinding],
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
    let mut cases: Vec<CaseFrame> = Vec::new();
    let mut control = Vec::new();
    let local_indexes = locals
        .iter()
        .enumerate()
        .map(|(index, local)| (local.name.as_str(), (index as u32, local.ty.clone())))
        .collect::<BTreeMap<_, _>>();

    let emit = |blocks: &mut BTreeMap<u32, BasicBlock>,
                current: u32,
                instruction: Instruction,
                origin: SourceOrigin| {
        let block = blocks.get_mut(&current).expect("current block exists");
        // A structured `break`/`continue` is a terminator.  Parsing continues
        // so enclosing `if`/loop forms can close their alternate reachable
        // paths, but no synthetic merge/back-edge may be appended after that
        // terminator in the same basic block.
        if block
            .instructions
            .last()
            .is_some_and(|located| located.instruction.is_terminator())
        {
            return;
        }
        block.instructions.push(LocatedInstruction {
            instruction,
            origin,
        });
    };

    let mut token_index = 0;
    // A Co-Forth `locals| a b |` declaration names the declared inputs in
    // bottom-to-top stack order. Store the top value first, exactly as a Lisp
    // parameter prologue does, so the function body begins with a clean
    // shared data stack and its values live only in this activation frame.
    for (index, local) in locals.iter().enumerate().rev() {
        let found = stack.pop().ok_or_else(|| {
            vec![control_error(
                "E-FORTH-LOCAL-003",
                "locals declaration requires a corresponding typed input",
                local.origin.clone(),
            )]
        })?;
        if found != local.ty {
            return Err(vec![VmDiagnostic::type_mismatch(
                local.ty.clone(),
                found,
                Some(local.origin.clone()),
            )]);
        }
        emit(
            &mut blocks,
            current,
            Instruction::LocalSet {
                index: index as u32,
            },
            local.origin.clone(),
        );
    }
    while token_index < tokens.len() {
        let token = tokens[token_index].clone();
        let origin = origin(source_id, source, token.start, token.end);
        if let TokenValue::Word(word) = &token.value {
            if word == "[']" {
                let Some(target) = tokens.get(token_index + 1) else {
                    return Err(vec![control_error(
                        "E-FORTH-QUOTE-001",
                        "['] requires a persistent typed word name",
                        origin.clone(),
                    )]);
                };
                let TokenValue::Word(target_name) = &target.value else {
                    return Err(vec![control_error(
                        "E-FORTH-QUOTE-001",
                        "quotation target must be a word name",
                        origin,
                    )]);
                };
                let Some(function) = linked_functions.get(target_name) else {
                    return Err(vec![control_error(
                        "E-FORTH-QUOTE-002",
                        format!("quotation target '{target_name}' is not a typed word"),
                        origin,
                    )]);
                };
                let signature = function.signature.clone();
                stack.push(Type::Function {
                    arguments: signature.input.values.clone(),
                    result: Box::new(
                        signature
                            .output
                            .values
                            .last()
                            .cloned()
                            .unwrap_or(Type::Unit),
                    ),
                    effects: signature.effects.clone(),
                });
                emit(
                    &mut blocks,
                    current,
                    Instruction::MakeClosure {
                        function: target_name.clone(),
                        capture_count: 0,
                        signature,
                    },
                    origin,
                );
                token_index += 2;
                continue;
            }
            if word == "execute" {
                let Type::Function {
                    arguments,
                    result,
                    effects: closure_effects,
                } = stack.pop().ok_or_else(|| {
                    vec![control_error(
                        "E-STACK-001",
                        "execute requires a quotation on top of the stack",
                        origin.clone(),
                    )]
                })?
                else {
                    return Err(vec![control_error(
                        "E-TYPE-011",
                        "execute requires a typed quotation",
                        origin,
                    )]);
                };
                let signature = StackSignature {
                    type_parameters: Vec::new(),
                    input: StackRow::polymorphic("S", arguments),
                    output: StackRow::polymorphic("S", vec![(*result).clone()]),
                    effects: closure_effects.clone(),
                    control: ControlEffect::Returns,
                };
                apply_signature_types(&signature, &mut stack, &origin)
                    .map_err(|diagnostic| vec![diagnostic])?;
                effects = effects.union(&closure_effects);
                emit(
                    &mut blocks,
                    current,
                    Instruction::CallClosure { signature },
                    origin,
                );
                token_index += 1;
                continue;
            }
            match word.as_str() {
                "case" => {
                    let selector = stack.last().cloned().ok_or_else(|| {
                        vec![control_error(
                            "E-FORTH-CASE-001",
                            "case requires an integer selector",
                            origin.clone(),
                        )]
                    })?;
                    if selector != Type::Int {
                        return Err(vec![VmDiagnostic::type_mismatch(
                            Type::Int,
                            selector,
                            Some(origin),
                        )]);
                    }
                    let end_block = next_block;
                    next_block += 1;
                    blocks.insert(
                        end_block,
                        BasicBlock {
                            id: end_block,
                            instructions: Vec::new(),
                        },
                    );
                    let selector_stack = stack.clone();
                    emit(&mut blocks, current, Instruction::Dup, origin.clone());
                    stack.push(Type::Int);
                    cases.push(CaseFrame {
                        end_block,
                        selector_stack,
                        arm_output: None,
                        next_else: None,
                        arm_open: false,
                        in_default: false,
                        origin,
                    });
                    control.push(ControlKind::Case);
                    token_index += 1;
                    continue;
                }
                "of" => {
                    if control.last() != Some(&ControlKind::Case) {
                        return Err(vec![control_error(
                            "E-FORTH-CASE-002",
                            "of has no matching case",
                            origin,
                        )]);
                    }
                    let Some(frame) = cases.last_mut() else {
                        unreachable!("case control has a case frame");
                    };
                    if frame.arm_open || frame.in_default {
                        return Err(vec![control_error(
                            "E-FORTH-CASE-003",
                            "of must follow case or endof and precede otherwise/endcase",
                            origin,
                        )]);
                    }
                    let comparison = vocabulary
                        .get("=")
                        .expect("core vocabulary contains integer equality");
                    apply_signature_types(comparison, &mut stack, &origin)
                        .map_err(|diagnostic| vec![diagnostic])?;
                    effects = effects.union(&comparison.effects);
                    emit(
                        &mut blocks,
                        current,
                        Instruction::Call {
                            function: "=".into(),
                        },
                        origin.clone(),
                    );
                    // The comparison result controls the branch; it is not
                    // part of either arm's typed entry row.
                    stack.pop();
                    let then_block = next_block;
                    let else_block = next_block + 1;
                    next_block += 2;
                    for id in [then_block, else_block] {
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
                    frame.next_else = Some(else_block);
                    frame.arm_open = true;
                    current = then_block;
                    // The selector is only needed along the non-matching
                    // path. A selected arm begins from the original lower
                    // stack row, exactly like an `if` branch.
                    emit(&mut blocks, current, Instruction::Drop, origin.clone());
                    stack.pop();
                    token_index += 1;
                    continue;
                }
                "endof" => {
                    if control.last() != Some(&ControlKind::Case) {
                        return Err(vec![control_error(
                            "E-FORTH-CASE-004",
                            "endof has no matching case",
                            origin,
                        )]);
                    }
                    let Some(frame) = cases.last_mut() else {
                        unreachable!("case control has a case frame");
                    };
                    if !frame.arm_open {
                        return Err(vec![control_error(
                            "E-FORTH-CASE-005",
                            "endof requires a preceding of arm",
                            origin,
                        )]);
                    }
                    if let Some(expected) = &frame.arm_output {
                        if stack != *expected {
                            return Err(vec![control_error(
                                "E-STACK-004",
                                "case arms leave incompatible stack types",
                                frame.origin.clone(),
                            )]);
                        }
                    } else {
                        frame.arm_output = Some(stack.clone());
                    }
                    emit(
                        &mut blocks,
                        current,
                        Instruction::Jump {
                            target: frame.end_block,
                        },
                        origin.clone(),
                    );
                    current = frame.next_else.take().expect("open arm has else block");
                    stack = frame.selector_stack.clone();
                    frame.arm_open = false;
                    let next_is_terminal = matches!(
                        tokens.get(token_index + 1).map(|token| &token.value),
                        Some(TokenValue::Word(next)) if next == "otherwise" || next == "endcase"
                    );
                    if !next_is_terminal {
                        emit(&mut blocks, current, Instruction::Dup, origin.clone());
                        stack.push(Type::Int);
                    }
                    token_index += 1;
                    continue;
                }
                "otherwise" => {
                    if control.last() != Some(&ControlKind::Case) {
                        return Err(vec![control_error(
                            "E-FORTH-CASE-006",
                            "otherwise has no matching case",
                            origin,
                        )]);
                    }
                    let Some(frame) = cases.last_mut() else {
                        unreachable!("case control has a case frame");
                    };
                    if frame.arm_open || frame.in_default || stack != frame.selector_stack {
                        return Err(vec![control_error(
                            "E-FORTH-CASE-007",
                            "otherwise must follow endof and may appear only once",
                            origin,
                        )]);
                    }
                    emit(&mut blocks, current, Instruction::Drop, origin.clone());
                    stack.pop();
                    frame.in_default = true;
                    token_index += 1;
                    continue;
                }
                "endcase" => {
                    if control.last() != Some(&ControlKind::Case) {
                        return Err(vec![control_error(
                            "E-FORTH-CASE-008",
                            "endcase has no matching case",
                            origin,
                        )]);
                    }
                    let Some(frame) = cases.pop() else {
                        unreachable!("case control has a case frame");
                    };
                    control.pop();
                    if frame.arm_open {
                        return Err(vec![control_error(
                            "E-FORTH-CASE-009",
                            "case arm must end with endof before endcase",
                            frame.origin,
                        )]);
                    }
                    let Some(arm_output) = frame.arm_output else {
                        return Err(vec![control_error(
                            "E-FORTH-CASE-010",
                            "case requires at least one of ... endof arm",
                            frame.origin,
                        )]);
                    };
                    if frame.in_default {
                        if stack != arm_output {
                            return Err(vec![control_error(
                                "E-STACK-004",
                                "case arms leave incompatible stack types",
                                frame.origin,
                            )]);
                        }
                    } else {
                        // Standard Forth's no-match path discards the selector.
                        // It is valid only if that path has the same result row
                        // as a selected arm.
                        emit(&mut blocks, current, Instruction::Drop, origin.clone());
                        stack.pop();
                        if stack != arm_output {
                            return Err(vec![control_error(
                                "E-STACK-004",
                                "case without otherwise must leave the same stack row as every arm",
                                frame.origin,
                            )]);
                        }
                    }
                    emit(
                        &mut blocks,
                        current,
                        Instruction::Jump {
                            target: frame.end_block,
                        },
                        origin,
                    );
                    current = frame.end_block;
                    stack = arm_output;
                    token_index += 1;
                    continue;
                }
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
                        match_condition: None,
                        else_payload: None,
                        origin,
                    });
                    control.push(ControlKind::If);
                    current = then_block;
                    token_index += 1;
                    continue;
                }
                "if-some" | "if-ok" => {
                    let option = stack.pop().ok_or_else(|| {
                        vec![control_error(
                            "E-STACK-001",
                            format!("{word} requires a tagged condition"),
                            origin.clone(),
                        )]
                    })?;
                    let (then_type, else_type, condition) = match option {
                        Type::Option(inner) if word == "if-some" => {
                            ((*inner).clone(), None, MatchCondition::Option)
                        }
                        Type::Result(ok, err) if word == "if-ok" => {
                            ((*ok).clone(), Some((*err).clone()), MatchCondition::Result)
                        }
                        _ => {
                            return Err(vec![control_error(
                                "E-TYPE-012",
                                if word == "if-some" {
                                    "if-some requires an option<T> condition"
                                } else {
                                    "if-ok requires a result<T,E> condition"
                                },
                                origin,
                            )]);
                        }
                    };

                    // Keep one copy of the tagged value live across the
                    // branch, and branch on a second copy. Each branch
                    // consumes the live value and receives only its selected
                    // payload.
                    emit(&mut blocks, current, Instruction::Dup, origin.clone());
                    emit(
                        &mut blocks,
                        current,
                        Instruction::Call {
                            function: if condition == MatchCondition::Option {
                                "is-some".into()
                            } else {
                                "is-ok".into()
                            },
                        },
                        origin.clone(),
                    );
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
                        match_condition: Some(condition),
                        else_payload: else_type,
                        origin: origin.clone(),
                    });
                    control.push(ControlKind::If);
                    current = then_block;
                    emit(
                        &mut blocks,
                        current,
                        Instruction::Call {
                            function: if condition == MatchCondition::Option {
                                "unwrap".into()
                            } else {
                                "result-unwrap".into()
                            },
                        },
                        origin,
                    );
                    stack.push(then_type);
                    token_index += 1;
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
                        origin.clone(),
                    );
                    current = frame.else_block;
                    stack = frame.entry_stack.clone();
                    if let Some(condition) = frame.match_condition {
                        match condition {
                            MatchCondition::Option => {
                                emit(&mut blocks, current, Instruction::Drop, origin.clone())
                            }
                            MatchCondition::Result => {
                                emit(
                                    &mut blocks,
                                    current,
                                    Instruction::Call {
                                        function: "result-error".into(),
                                    },
                                    origin.clone(),
                                );
                                stack.push(
                                    frame
                                        .else_payload
                                        .clone()
                                        .expect("result match stores error type"),
                                );
                            }
                        }
                    }
                    token_index += 1;
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
                        if frame.match_condition.is_some() {
                            return Err(vec![control_error(
                                "E-FORTH-IF-003",
                                "structured matches require an else branch so the alternate value is consumed",
                                frame.origin,
                            )]);
                        }
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
                    token_index += 1;
                    continue;
                }
                "begin" | "begin:" => {
                    let label = if word == "begin:" {
                        let Some(Token {
                            value: TokenValue::Word(label),
                            ..
                        }) = tokens.get(token_index + 1)
                        else {
                            return Err(vec![control_error(
                                "E-FORTH-LOOP-006",
                                "begin: requires a loop label",
                                origin,
                            )]);
                        };
                        if label.is_empty()
                            || matches!(
                                label.as_str(),
                                "if" | "else" | "then" | "while" | "repeat" | "until"
                            )
                            || loops
                                .iter()
                                .any(|frame| frame.label.as_deref() == Some(label))
                        {
                            return Err(vec![control_error(
                                "E-FORTH-LOOP-006",
                                format!("invalid or duplicate active loop label '{label}'"),
                                origin,
                            )]);
                        }
                        Some(label.clone())
                    } else {
                        None
                    };
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
                        label,
                        header,
                        exit: None,
                        stack: stack.clone(),
                        origin,
                    });
                    control.push(ControlKind::Loop);
                    token_index += if word == "begin:" { 2 } else { 1 };
                    continue;
                }
                "break" | "continue" => {
                    let Some(Token {
                        value: TokenValue::Word(label),
                        ..
                    }) = tokens.get(token_index + 1)
                    else {
                        return Err(vec![control_error(
                            "E-FORTH-LOOP-007",
                            format!("{word} requires a named loop label"),
                            origin,
                        )]);
                    };
                    let Some(frame) = loops
                        .iter()
                        .rev()
                        .find(|frame| frame.label.as_deref() == Some(label.as_str()))
                    else {
                        return Err(vec![control_error(
                            "E-FORTH-LOOP-007",
                            format!("{word} target '{label}' is not an active named loop"),
                            origin,
                        )]);
                    };
                    if stack != frame.stack {
                        let mut diagnostic = control_error(
                            "E-STACK-006",
                            format!(
                                "{word} target '{label}' requires the loop's declared stack shape"
                            ),
                            origin,
                        );
                        diagnostic.expected_types = frame.stack.clone();
                        diagnostic.found_types = stack.clone();
                        return Err(vec![diagnostic]);
                    }
                    let target = if word == "break" {
                        let Some(exit) = frame.exit else {
                            return Err(vec![control_error(
                                "E-FORTH-LOOP-008",
                                format!(
                                    "break target '{label}' has no exit yet; place it after that loop's while"
                                ),
                                origin,
                            )]);
                        };
                        exit
                    } else {
                        frame.header
                    };
                    emit(&mut blocks, current, Instruction::Jump { target }, origin);
                    token_index += 2;
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
                    token_index += 1;
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
                    token_index += 1;
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
                    token_index += 1;
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
                if let Some(symbol) = word.strip_prefix('\'').filter(|symbol| !symbol.is_empty()) {
                    stack.push(Type::Symbol);
                    Instruction::Constant {
                        value: TypedValue::Symbol(symbol.to_string()),
                    }
                } else if let Ok(value) = word.parse::<i64>() {
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
                } else if word == "defer-cpu" {
                    let closure = stack.pop().ok_or_else(|| {
                        vec![VmDiagnostic::error(
                            "E-FIBER-003",
                            DiagnosticPhase::TypeInference,
                            "defer-cpu requires a typed closure",
                            Some(origin.clone()),
                        )]
                    })?;
                    let Type::Function {
                        arguments,
                        result,
                        effects: closure_effects,
                    } = closure
                    else {
                        return Err(vec![VmDiagnostic::error(
                            "E-FIBER-003",
                            DiagnosticPhase::TypeInference,
                            "defer-cpu requires a typed closure",
                            Some(origin),
                        )]);
                    };
                    if !arguments.is_empty() {
                        return Err(vec![VmDiagnostic::error(
                            "E-FIBER-004",
                            DiagnosticPhase::TypeInference,
                            "defer-cpu requires a zero-argument closure; capture its arguments first",
                            Some(origin),
                        )]);
                    }
                    if !closure_effects.is_pure() {
                        return Err(vec![VmDiagnostic::error(
                            "E-FIBER-005",
                            DiagnosticPhase::TypeInference,
                            "defer-cpu requires a pure closure",
                            Some(origin),
                        )]);
                    }
                    stack.push(Type::Task(result));
                    Instruction::DeferCpu
                } else if word == "task-poll" {
                    let task = stack.pop().ok_or_else(|| {
                        vec![VmDiagnostic::error(
                            "E-FIBER-009",
                            DiagnosticPhase::TypeInference,
                            "task-poll requires task<T>",
                            Some(origin.clone()),
                        )]
                    })?;
                    let Type::Task(result) = task else {
                        return Err(vec![VmDiagnostic::error(
                            "E-FIBER-009",
                            DiagnosticPhase::TypeInference,
                            "task-poll requires task<T>",
                            Some(origin),
                        )]);
                    };
                    stack.push(Type::Option(result));
                    Instruction::PollCpuFiber
                } else if word == "task-join" {
                    let task = stack.pop().ok_or_else(|| {
                        vec![VmDiagnostic::error(
                            "E-FIBER-010",
                            DiagnosticPhase::TypeInference,
                            "task-join requires task<T>",
                            Some(origin.clone()),
                        )]
                    })?;
                    let Type::Task(result) = task else {
                        return Err(vec![VmDiagnostic::error(
                            "E-FIBER-010",
                            DiagnosticPhase::TypeInference,
                            "task-join requires task<T>",
                            Some(origin),
                        )]);
                    };
                    stack.push(*result);
                    Instruction::JoinCpuFiber
                } else if word == "task-cancel" {
                    let task = stack.pop().ok_or_else(|| {
                        vec![VmDiagnostic::error(
                            "E-FIBER-020",
                            DiagnosticPhase::TypeInference,
                            "task-cancel requires task<T>",
                            Some(origin.clone()),
                        )]
                    })?;
                    if !matches!(task, Type::Task(_)) {
                        return Err(vec![VmDiagnostic::error(
                            "E-FIBER-020",
                            DiagnosticPhase::TypeInference,
                            "task-cancel requires task<T>",
                            Some(origin),
                        )]);
                    }
                    stack.push(Type::Unit);
                    Instruction::CancelCpuFiber
                } else if let Some((index, ty)) = local_indexes.get(word.as_str()) {
                    stack.push(ty.clone());
                    Instruction::LocalGet { index: *index }
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
                    if word == "yield" {
                        Instruction::Yield
                    } else if word == "output-open" {
                        Instruction::OutputOpen
                    } else if let Some(operation) = output_operation(&word) {
                        Instruction::UiEffect {
                            operation,
                            input: signature.input.values.clone(),
                            output: signature.output.values.clone(),
                        }
                    } else if signature.effects.0.len() == 1 {
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
        token_index += 1;
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
    if let Some(frame) = cases.last() {
        return Err(vec![control_error(
            "E-FORTH-CASE-011",
            "unterminated case",
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
        documentation: None,
        signature: StackSignature {
            type_parameters: Vec::new(),
            input: StackRow::closed(initial_stack),
            output: StackRow::closed(stack),
            effects,
            control: ControlEffect::Returns,
        },
        locals: locals.iter().map(|local| local.ty.clone()).collect(),
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

fn output_operation(word: &str) -> Option<UiOperation> {
    match word {
        "output-append" => Some(UiOperation::Append),
        "output-replace" => Some(UiOperation::Replace),
        "output-status" => Some(UiOperation::Status),
        "output-progress" => Some(UiOperation::Progress),
        "output-complete" => Some(UiOperation::Complete),
        "output-fail" => Some(UiOperation::Fail),
        _ => None,
    }
}

struct ParsedDefinition {
    name: String,
    documentation: Option<String>,
    signature: StackSignature,
    declares_pure: bool,
    locals: Vec<LocalBinding>,
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
        let (locals, body_start) = parse_definition_locals(
            source_id,
            source,
            &tokens[body_start_index..end],
            body_start,
            &signature.input.values,
        )?;
        definitions.push(ParsedDefinition {
            name: name.clone(),
            documentation: forth_definition_documentation(source, definition_start),
            signature,
            declares_pure,
            locals,
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

/// Read a single public documentation line immediately preceding a typed
/// definition. `\\ finch-doc:` is deliberately source-only metadata: the
/// tokenizer discards the comment and the string never reaches the operand
/// stack. A blank or non-comment line breaks the association so a doc cannot
/// accidentally attach across an unrelated form.
fn forth_definition_documentation(source: &str, definition_start: usize) -> Option<String> {
    let prefix = &source[..definition_start];
    let line = prefix.lines().rev().find(|line| !line.trim().is_empty())?;
    line.trim()
        .strip_prefix("\\ finch-doc:")
        .map(str::trim)
        .filter(|documentation| !documentation.is_empty())
        .map(str::to_owned)
}

/// Parse the direct Co-Forth spelling for frame-local parameter names:
///
/// ```forth
/// : area ( S int int -- S int ! {} ) locals| width height | width height * ;
/// ```
///
/// This is intentionally not a macro. It lowers to one `LocalSet` per
/// declared input at function entry, followed by `LocalGet` at each use.
/// The declaration must name every typed input, in its bottom-to-top stack
/// order, so it cannot conceal a stack rearrangement from the IR viewer.
fn parse_definition_locals(
    source_id: &str,
    source: &str,
    body_tokens: &[Token],
    default_body_start: usize,
    input_types: &[Type],
) -> Result<(Vec<LocalBinding>, usize), Vec<VmDiagnostic>> {
    let Some(first) = body_tokens.first() else {
        return Ok((Vec::new(), default_body_start));
    };
    if !token_is(first, "locals|") {
        return Ok((Vec::new(), default_body_start));
    }
    let close = body_tokens[1..]
        .iter()
        .position(|token| token_is(token, "|"))
        .map(|index| index + 1)
        .ok_or_else(|| {
            vec![definition_error(
                source_id,
                source,
                first,
                "unterminated locals declaration; expected '|'",
            )]
        })?;
    let names = &body_tokens[1..close];
    if names.len() != input_types.len() {
        return Err(vec![definition_error(
            source_id,
            source,
            first,
            format!(
                "locals declaration names {} values but word signature has {} inputs",
                names.len(),
                input_types.len()
            ),
        )]);
    }
    let mut locals = Vec::with_capacity(names.len());
    for (token, ty) in names.iter().zip(input_types) {
        let TokenValue::Word(name) = &token.value else {
            return Err(vec![definition_error(
                source_id,
                source,
                token,
                "local name must be an identifier",
            )]);
        };
        if name == "|"
            || name == "locals|"
            || locals
                .iter()
                .any(|local: &LocalBinding| local.name == *name)
        {
            return Err(vec![definition_error(
                source_id,
                source,
                token,
                format!("invalid or duplicate local name '{name}'"),
            )]);
        }
        locals.push(LocalBinding {
            name: name.clone(),
            ty: ty.clone(),
            origin: origin(source_id, source, token.start, token.end),
        });
    }
    let body_start = body_tokens
        .get(close + 1)
        .map_or(default_body_start, |token| token.start);
    Ok((locals, body_start))
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
            super::lisp::parse_type_name(name).map_err(|_| {
                vec![definition_error(
                    source_id,
                    source,
                    token,
                    format!("unknown stack type '{name}'"),
                )]
            })
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
        // Raw prose literal.  It deliberately comes before `s"` so the
        // triple-quote opener is not mistaken for an empty escaped string.
        // The contents are verbatim (including newlines and ordinary quotes)
        // until the next `"""`; use it for model/user prose that would make
        // ordinary escaping needlessly fragile.
        if source[start..].starts_with("s\"\"\"") {
            cursor += 4;
            let (value, end) = read_raw_string(
                source_id,
                source,
                cursor,
                start,
                "E-READ-004",
                "unterminated Co-Forth raw string literal",
            )?;
            tokens.push(Token {
                value: TokenValue::String(value),
                start,
                end,
            });
            cursor = end;
            continue;
        }
        // Standard Forth output literal. In typed Co-Forth it is syntax sugar
        // for `s\"...\" say`, so it remains familiar to Forth authors while
        // preserving the same typed SessionEmit side effect as `say`.
        if source[start..].starts_with(".\"") {
            cursor += 2;
            if cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            let (value, end) = read_string(
                source_id,
                source,
                cursor,
                start,
                "E-READ-005",
                "unterminated Co-Forth output string literal",
            )?;
            tokens.push(Token {
                value: TokenValue::String(value),
                start,
                end,
            });
            // Attribute the implicit effect to the source literal, rather
            // than inventing an unlocatable synthetic `say` token.
            tokens.push(Token {
                value: TokenValue::Word("say".to_string()),
                start,
                end,
            });
            cursor = end;
            continue;
        }
        if source[start..].starts_with("s\"") {
            cursor += 2;
            if cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            let (value, end) = read_string(
                source_id,
                source,
                cursor,
                start,
                "E-READ-001",
                "unterminated Co-Forth string literal",
            )?;
            tokens.push(Token {
                value: TokenValue::String(value),
                start,
                end,
            });
            cursor = end;
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

fn read_string(
    source_id: &str,
    source: &str,
    mut cursor: usize,
    start: usize,
    code: &str,
    message: &str,
) -> Result<(String, usize), Vec<VmDiagnostic>> {
    let mut value = String::new();
    while cursor < source.len() {
        let mut chars = source[cursor..].chars();
        let character = chars.next().expect("cursor remains on a UTF-8 boundary");
        match character {
            '"' => return Ok((value, cursor + character.len_utf8())),
            '\\' => {
                let escape_start = cursor + character.len_utf8();
                let Some(escaped) = source[escape_start..].chars().next() else {
                    break;
                };
                cursor = escape_start;
                value.push(match escaped {
                    '"' => '"',
                    '\\' => '\\',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    other => other,
                });
                cursor += escaped.len_utf8();
                continue;
            }
            other => value.push(other),
        }
        cursor += character.len_utf8();
    }
    Err(vec![VmDiagnostic::error(
        code,
        DiagnosticPhase::Reader,
        message,
        Some(origin(source_id, source, start, source.len())),
    )])
}

fn read_raw_string(
    source_id: &str,
    source: &str,
    cursor: usize,
    start: usize,
    code: &str,
    message: &str,
) -> Result<(String, usize), Vec<VmDiagnostic>> {
    let remainder = &source[cursor..];
    if let Some(close) = remainder.find("\"\"\"") {
        let end = cursor + close;
        return Ok((source[cursor..end].to_string(), end + 3));
    }
    Err(vec![VmDiagnostic::error(
        code,
        DiagnosticPhase::Reader,
        message,
        Some(origin(source_id, source, start, source.len())),
    )])
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
    use crate::vm::interpreter::{
        CapabilityHandler, DenyCapabilities, Interpreter, InterpreterConfig,
    };
    use crate::vm::{
        core_vocabulary, CapabilityKind, CapabilityRequirement, ResourceSelector, TypedValue,
    };

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
    fn string_literals_are_streamable_through_explicit_say() {
        let module = compile_forth(
            "input.forth",
            "s\"Hello \\\"世界\\\"\" say 3 5 + int-to-string say s\"! \" say",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        #[derive(Default)]
        struct EmitHandler(String);
        impl crate::vm::interpreter::CapabilityHandler for EmitHandler {
            fn request(
                &mut self,
                requirement: &CapabilityRequirement,
                arguments: Vec<TypedValue>,
                _origin: &SourceOrigin,
            ) -> Result<Vec<TypedValue>, VmDiagnostic> {
                assert_eq!(requirement.capability, CapabilityKind::SessionEmit);
                let [TypedValue::String(text)] = arguments.as_slice() else {
                    panic!("expected string emission");
                };
                self.0.push_str(text);
                Ok(vec![TypedValue::Unit])
            }
            fn output(&self) -> String {
                self.0.clone()
            }
        }
        let mut handler = EmitHandler::default();
        Interpreter::new(
            &module,
            &mut handler,
            InterpreterConfig {
                fuel: 100_000,
                grants: EffectSet::from_requirement(CapabilityRequirement {
                    capability: CapabilityKind::SessionEmit,
                    selector: ResourceSelector::None,
                }),
            },
        )
        .execute(&mut stack)
        .unwrap();
        assert_eq!(handler.output(), "Hello \"世界\"8! ");
    }

    #[test]
    fn typed_frontend_lowers_standard_output_literal_to_say() {
        let module = compile_forth(
            "input.forth",
            ".\" legacy output\"",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("typed Co-Forth accepts standard output literal");
        #[derive(Default)]
        struct EmitHandler(String);
        impl crate::vm::interpreter::CapabilityHandler for EmitHandler {
            fn request(
                &mut self,
                requirement: &CapabilityRequirement,
                arguments: Vec<TypedValue>,
                _origin: &SourceOrigin,
            ) -> Result<Vec<TypedValue>, VmDiagnostic> {
                assert_eq!(requirement.capability, CapabilityKind::SessionEmit);
                let [TypedValue::String(text)] = arguments.as_slice() else {
                    panic!("expected string emission");
                };
                self.0.push_str(text);
                Ok(vec![TypedValue::Unit])
            }
            fn output(&self) -> String {
                self.0.clone()
            }
        }
        let mut stack = Vec::new();
        let mut handler = EmitHandler::default();
        Interpreter::new(
            &module,
            &mut handler,
            InterpreterConfig {
                fuel: 100_000,
                grants: EffectSet::from_requirement(CapabilityRequirement {
                    capability: CapabilityKind::SessionEmit,
                    selector: ResourceSelector::None,
                }),
            },
        )
        .execute(&mut stack)
        .unwrap();
        assert_eq!(handler.output(), "legacy output");
    }

    #[test]
    fn named_break_and_continue_lower_to_typed_loop_edges() {
        let break_module = compile_forth(
            "break.forth",
            "0 begin: search dup 3 < while 1 + dup 2 = if break search then repeat",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut break_stack = Vec::new();
        Interpreter::new(
            &break_module,
            DenyCapabilities,
            InterpreterConfig::default(),
        )
        .execute(&mut break_stack)
        .unwrap();
        assert_eq!(break_stack, vec![TypedValue::Int(2)]);

        let continue_module = compile_forth(
            "continue.forth",
            "0 begin: count dup 3 < while 1 + dup 2 = if continue count then repeat",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut continue_stack = Vec::new();
        Interpreter::new(
            &continue_module,
            DenyCapabilities,
            InterpreterConfig::default(),
        )
        .execute(&mut continue_stack)
        .unwrap();
        assert_eq!(continue_stack, vec![TypedValue::Int(3)]);
    }

    #[test]
    fn if_ok_binds_each_result_payload_on_its_selected_edge() {
        let ok_module = compile_forth(
            "ok.forth",
            "5 ok if-ok drop else drop then",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let err_module = compile_forth(
            "err.forth",
            "s\"bad\" err if-ok drop else drop then",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        for module in [ok_module, err_module] {
            let mut stack = Vec::new();
            Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
                .execute(&mut stack)
                .unwrap();
            assert!(stack.is_empty());
        }
    }

    #[test]
    fn typed_integer_case_has_no_fallthrough_and_requires_compatible_arms() {
        let selected = compile_forth(
            "case-selected.forth",
            "2 case 1 of 10 endof 2 of 20 endof otherwise 30 endcase",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("integer case should compile");
        let defaulted = compile_forth(
            "case-default.forth",
            "3 case 1 of 10 endof 2 of 20 endof otherwise 30 endcase",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("integer case with otherwise should compile");
        for (module, expected) in [(selected, 20), (defaulted, 30)] {
            let mut stack = Vec::new();
            Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
                .execute(&mut stack)
                .unwrap();
            assert_eq!(stack, vec![TypedValue::Int(expected)]);
        }

        let effect_only = compile_forth(
            "case-effect.forth",
            "1 case 1 of endof endcase",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("a case without otherwise may leave no values on every path");
        let mut stack = Vec::new();
        Interpreter::new(&effect_only, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert!(stack.is_empty());

        let mismatch = compile_forth(
            "case-mismatch.forth",
            "1 case 1 of 10 endof otherwise endcase",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect_err("every case path must leave the same stack row");
        assert_eq!(mismatch[0].code, "E-STACK-004");

        let non_integer = compile_forth(
            "case-string.forth",
            "s\" selector\" case 1 of 10 endof otherwise 20 endcase",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect_err("case selectors are intentionally integer-only in version 1");
        assert_eq!(non_integer[0].code, "E-TYPE-002");
    }

    #[test]
    fn result_error_projects_the_error_of_a_heterogeneous_result() {
        let module = compile_forth(
            "result-error.forth",
            "s\"bad\" err result-error",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::String("bad".into())]);
    }

    #[test]
    fn named_loop_exit_requires_an_active_label_and_preserved_stack() {
        let missing = compile_forth(
            "missing.forth",
            "0 begin: outer true while break absent repeat",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect_err("break must name an active loop");
        assert_eq!(missing[0].code, "E-FORTH-LOOP-007");

        let mismatch = compile_forth(
            "mismatch.forth",
            "0 begin: outer true while drop break outer repeat",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect_err("break must preserve its target stack row");
        assert_eq!(mismatch[0].code, "E-STACK-006");
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
    fn typed_forth_locals_lower_to_explicit_frame_operations() {
        let module = compile_forth(
            "locals.forth",
            ": area ( S int int -- S int ! {} ) locals| width height | width height * ; 4 3 area",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let area = &module.module.functions["area"];
        assert_eq!(area.locals, vec![Type::Int, Type::Int]);
        let instructions = &area.blocks[&area.entry].instructions;
        assert!(matches!(
            instructions[0].instruction,
            Instruction::LocalSet { index: 1 }
        ));
        assert!(matches!(
            instructions[1].instruction,
            Instruction::LocalSet { index: 0 }
        ));
        assert!(matches!(
            instructions[2].instruction,
            Instruction::LocalGet { index: 0 }
        ));
        assert!(matches!(
            instructions[3].instruction,
            Instruction::LocalGet { index: 1 }
        ));

        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(12)]);
    }

    #[test]
    fn retains_finch_doc_comment_on_typed_definition() {
        let module = compile_forth(
            "documented.forth",
            "\\ finch-doc: Double an integer.\n: double ( S int -- S int ! {} ) 2 * ; 21 double",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();

        assert_eq!(
            module.module.functions["double"].documentation.as_deref(),
            Some("Double an integer.")
        );
    }

    #[test]
    fn locals_must_name_every_declared_input() {
        let errors = compile_forth(
            "locals.forth",
            ": area ( S int int -- S int ! {} ) locals| width | width ;",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert!(errors.iter().any(|error| error.code == "E-FORTH-SIG-001"));
    }

    #[test]
    fn accepts_parameterized_stack_signature_types() {
        let module = compile_forth(
            "generic.forth",
            ": list-id ( S list<int> -- S list<int> ! {} ) ;",
            vec![Type::list(Type::Int)],
            &core_vocabulary(),
        )
        .unwrap();
        assert_eq!(
            module.module.functions["list-id"].signature.input.values,
            vec![Type::list(Type::Int)]
        );
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

    #[test]
    fn quoted_word_produces_a_typed_symbol_value() {
        let module = compile_forth("input.forth", "'bash", Vec::new(), &core_vocabulary()).unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Symbol("bash".into())]);
    }

    #[test]
    fn raw_string_literal_preserves_quotes_and_newlines_without_escaping() {
        let module = compile_forth(
            "input.forth",
            "s\"\"\"The user said \"hello\".\nSecond line.\"\"\"",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(
            stack,
            vec![TypedValue::String(
                "The user said \"hello\".\nSecond line.".into()
            )]
        );
    }

    #[test]
    fn raw_string_literal_reports_an_unclosed_delimiter() {
        let errors = compile_forth(
            "input.forth",
            "s\"\"\"unterminated",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert_eq!(errors[0].code, "E-READ-004");
    }

    #[test]
    fn reads_json_object_fields_through_the_shared_typed_vocabulary() {
        let module = compile_forth(
            "input.forth",
            "s\" {\\\"answer\\\":42}\" json-parse result-unwrap s\" answer\" json-get unwrap json-as-int unwrap",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("typed Co-Forth compiles JSON field access");
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("typed Co-Forth executes JSON field access");
        assert_eq!(stack, vec![TypedValue::Int(42)]);

        let float = compile_forth(
            "input.forth",
            "s\" 3.5\" json-parse result-unwrap json-as-float unwrap",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("typed Co-Forth compiles JSON float access");
        let mut stack = Vec::new();
        Interpreter::new(&float, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .expect("typed Co-Forth executes JSON float access");
        assert_eq!(stack, vec![TypedValue::Float(3.5)]);
    }
}
