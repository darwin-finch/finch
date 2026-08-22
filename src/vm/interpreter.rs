use super::diagnostic::{DiagnosticPhase, SourceOrigin, VmDiagnostic};
use super::effects::{CapabilityRequirement, EffectSet};
use super::ir::Instruction;
use super::types::TypedValue;
use super::verifier::VerifiedModule;

pub trait CapabilityHandler {
    fn request(
        &mut self,
        requirement: &CapabilityRequirement,
        arguments: Vec<TypedValue>,
        origin: &SourceOrigin,
    ) -> Result<Vec<TypedValue>, VmDiagnostic>;

    /// Optional user-facing output accumulated by a host binding. Capability
    /// handlers that do not produce text can keep the default implementation.
    fn output(&self) -> String {
        String::new()
    }

    /// Receive each user-visible output chunk as it is produced. The default
    /// keeps non-streaming handlers source-compatible.
    fn emit(&mut self, _chunk: &str) {}

    fn output_chunks(&self) -> Vec<String> {
        Vec::new()
    }
}

impl<T: CapabilityHandler + ?Sized> CapabilityHandler for &mut T {
    fn request(
        &mut self,
        requirement: &CapabilityRequirement,
        arguments: Vec<TypedValue>,
        origin: &SourceOrigin,
    ) -> Result<Vec<TypedValue>, VmDiagnostic> {
        (**self).request(requirement, arguments, origin)
    }

    fn output(&self) -> String {
        (**self).output()
    }

    fn emit(&mut self, chunk: &str) {
        (**self).emit(chunk)
    }

    fn output_chunks(&self) -> Vec<String> {
        (**self).output_chunks()
    }
}

pub struct DenyCapabilities;

impl CapabilityHandler for DenyCapabilities {
    fn request(
        &mut self,
        requirement: &CapabilityRequirement,
        _arguments: Vec<TypedValue>,
        origin: &SourceOrigin,
    ) -> Result<Vec<TypedValue>, VmDiagnostic> {
        let mut diagnostic = VmDiagnostic::error(
            "E-CAP-002",
            DiagnosticPhase::Authorization,
            format!("capability {:?} was not granted", requirement.capability),
            Some(origin.clone()),
        );
        diagnostic.capability = Some(requirement.clone());
        Err(diagnostic)
    }
}

#[derive(Debug, Clone)]
pub struct InterpreterConfig {
    pub fuel: u64,
    pub grants: EffectSet,
}

impl Default for InterpreterConfig {
    fn default() -> Self {
        Self {
            fuel: 100_000,
            grants: EffectSet::pure(),
        }
    }
}

pub struct Interpreter<'a, H> {
    module: &'a VerifiedModule,
    handler: H,
    config: InterpreterConfig,
    fuel: u64,
    trace: Vec<String>,
}

impl<'a, H: CapabilityHandler> Interpreter<'a, H> {
    pub fn new(module: &'a VerifiedModule, handler: H, config: InterpreterConfig) -> Self {
        let fuel = config.fuel;
        Self {
            module,
            handler,
            config,
            fuel,
            trace: Vec::new(),
        }
    }

    /// Execute transactionally against an owned stack. The caller's stack is
    /// changed only after the entry function returns successfully.
    pub fn execute(&mut self, stack: &mut Vec<TypedValue>) -> Result<(), VmDiagnostic> {
        let mut pending = stack.clone();
        let entry = self.module.module.entry.clone();
        self.call(&entry, &mut pending, Vec::new())?;
        *stack = pending;
        Ok(())
    }

    fn call(
        &mut self,
        name: &str,
        stack: &mut Vec<TypedValue>,
        captures: Vec<TypedValue>,
    ) -> Result<(), VmDiagnostic> {
        if self.module.module.functions.contains_key(name) {
            return self.call_function(name, stack, captures);
        }
        execute_core(name, stack).map_err(|mut diagnostic| {
            diagnostic.trace = self.trace.clone();
            diagnostic
        })
    }

    fn call_function(
        &mut self,
        name: &str,
        stack: &mut Vec<TypedValue>,
        captures: Vec<TypedValue>,
    ) -> Result<(), VmDiagnostic> {
        let function = &self.module.module.functions[name];
        let mut locals = vec![TypedValue::Unit; function.locals.len()];
        self.trace.push(name.to_string());
        let result = (|| {
            let mut block_id = function.entry;
            loop {
                let block = &function.blocks[&block_id];
                let mut next_block = None;
                for located in &block.instructions {
                    self.consume_fuel(&located.origin)?;
                    match &located.instruction {
                        Instruction::Constant { value } => stack.push(value.clone()),
                        Instruction::MakeList {
                            element_type,
                            count,
                        } => {
                            let start = stack.len() - *count as usize;
                            let values = stack.drain(start..).collect();
                            stack.push(TypedValue::List {
                                element_type: element_type.clone(),
                                values,
                            });
                        }
                        Instruction::Dup => {
                            let value = stack.last().cloned().ok_or_else(|| {
                                runtime_underflow(&located.origin, self.trace.clone())
                            })?;
                            stack.push(value);
                        }
                        Instruction::Drop => {
                            stack.pop().ok_or_else(|| {
                                runtime_underflow(&located.origin, self.trace.clone())
                            })?;
                        }
                        Instruction::Swap => {
                            let len = stack.len();
                            if len < 2 {
                                return Err(runtime_underflow(&located.origin, self.trace.clone()));
                            }
                            stack.swap(len - 1, len - 2);
                        }
                        Instruction::LocalGet { index } => {
                            stack.push(locals[*index as usize].clone());
                        }
                        Instruction::LocalSet { index } => {
                            locals[*index as usize] = stack.pop().ok_or_else(|| {
                                runtime_underflow(&located.origin, self.trace.clone())
                            })?;
                        }
                        Instruction::CaptureGet { index } => {
                            stack.push(captures[*index as usize].clone());
                        }
                        Instruction::MakeClosure {
                            function,
                            capture_count,
                            signature,
                        } => {
                            let start = stack.len() - *capture_count as usize;
                            let captures = stack.drain(start..).collect();
                            stack.push(TypedValue::Closure {
                                function: function.clone(),
                                captures,
                                signature: signature.clone(),
                            });
                        }
                        Instruction::Call { function } => self.call(function, stack, Vec::new())?,
                        Instruction::CallClosure { .. } => {
                            let closure = stack.pop().ok_or_else(|| {
                                runtime_underflow(&located.origin, self.trace.clone())
                            })?;
                            let TypedValue::Closure {
                                function, captures, ..
                            } = closure
                            else {
                                return Err(VmDiagnostic::error(
                                    "E-RUNTIME-002",
                                    DiagnosticPhase::Interpretation,
                                    "attempted to call a non-closure value",
                                    Some(located.origin.clone()),
                                ));
                            };
                            self.call(&function, stack, captures)?;
                        }
                        Instruction::CapabilityRequest {
                            requirement, input, ..
                        } => {
                            let requested = EffectSet::from_requirement(requirement.clone());
                            if !self.config.grants.grants(&requested) {
                                let mut diagnostic = VmDiagnostic::error(
                                    "E-CAP-002",
                                    DiagnosticPhase::Authorization,
                                    format!(
                                        "capability {:?} is outside this execution's grants",
                                        requirement.capability
                                    ),
                                    Some(located.origin.clone()),
                                );
                                diagnostic.capability = Some(requirement.clone());
                                return Err(diagnostic);
                            }
                            let start = stack.len() - input.len();
                            let arguments = stack.drain(start..).collect();
                            let values =
                                self.handler
                                    .request(requirement, arguments, &located.origin)?;
                            stack.extend(values);
                        }
                        Instruction::Jump { target } => {
                            next_block = Some(*target);
                            break;
                        }
                        Instruction::Branch {
                            then_block,
                            else_block,
                        } => {
                            let condition = stack.pop().ok_or_else(|| {
                                runtime_underflow(&located.origin, self.trace.clone())
                            })?;
                            let TypedValue::Bool(condition) = condition else {
                                return Err(VmDiagnostic::error(
                                    "E-RUNTIME-003",
                                    DiagnosticPhase::Interpretation,
                                    "branch condition is not boolean",
                                    Some(located.origin.clone()),
                                ));
                            };
                            next_block = Some(if condition { *then_block } else { *else_block });
                            break;
                        }
                        Instruction::Return => return Ok(()),
                        Instruction::Trap { code } => {
                            return Err(VmDiagnostic::error(
                                code,
                                DiagnosticPhase::Interpretation,
                                "program raised a trap",
                                Some(located.origin.clone()),
                            ));
                        }
                    }
                }
                block_id = next_block.ok_or_else(|| {
                    VmDiagnostic::error(
                        "E-RUNTIME-004",
                        DiagnosticPhase::Interpretation,
                        format!("block {block_id} ended without transferring control"),
                        None,
                    )
                })?;
            }
        })();
        self.trace.pop();
        result.map_err(|mut diagnostic| {
            if diagnostic.trace.is_empty() {
                diagnostic.trace = self.trace.clone();
                diagnostic.trace.push(name.to_string());
            }
            diagnostic
        })
    }

    fn consume_fuel(&mut self, origin: &SourceOrigin) -> Result<(), VmDiagnostic> {
        if self.fuel == 0 {
            let mut diagnostic = VmDiagnostic::error(
                "E-LIMIT-001",
                DiagnosticPhase::ResourceLimit,
                "typed VM fuel exhausted",
                Some(origin.clone()),
            );
            diagnostic.trace = self.trace.clone();
            return Err(diagnostic);
        }
        self.fuel -= 1;
        Ok(())
    }
}

fn execute_core(name: &str, stack: &mut Vec<TypedValue>) -> Result<(), VmDiagnostic> {
    let origin = SourceOrigin::generated(name);
    let pop = |stack: &mut Vec<TypedValue>| {
        stack
            .pop()
            .ok_or_else(|| runtime_underflow(&origin, Vec::new()))
    };
    match name {
        "+" | "-" | "*" | "/" | "mod" => {
            let right = pop(stack)?;
            let left = pop(stack)?;
            let (TypedValue::Int(left), TypedValue::Int(right)) = (left, right) else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-005",
                    DiagnosticPhase::Interpretation,
                    format!("{name} requires integer operands"),
                    Some(origin),
                ));
            };
            let value = match name {
                "+" => left.checked_add(right),
                "-" => left.checked_sub(right),
                "*" => left.checked_mul(right),
                "/" if right != 0 => left.checked_div(right),
                "mod" if right != 0 => left.checked_rem(right),
                _ => None,
            }
            .ok_or_else(|| {
                VmDiagnostic::error(
                    if right == 0 && matches!(name, "/" | "mod") {
                        "E-NUM-001"
                    } else {
                        "E-NUM-002"
                    },
                    DiagnosticPhase::Interpretation,
                    if right == 0 && matches!(name, "/" | "mod") {
                        "division by zero"
                    } else {
                        "integer overflow"
                    },
                    Some(SourceOrigin::generated(name)),
                )
            })?;
            stack.push(TypedValue::Int(value));
        }
        "=" | "<" | ">" | "<=" | ">=" => {
            let right = pop(stack)?;
            let left = pop(stack)?;
            let (TypedValue::Int(left), TypedValue::Int(right)) = (left, right) else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-005",
                    DiagnosticPhase::Interpretation,
                    format!("{name} requires integer operands"),
                    Some(origin),
                ));
            };
            stack.push(TypedValue::Bool(match name {
                "=" => left == right,
                "<" => left < right,
                ">" => left > right,
                "<=" => left <= right,
                ">=" => left >= right,
                _ => unreachable!(),
            }));
        }
        "negate" | "abs" => {
            let value = pop(stack)?;
            let TypedValue::Int(value) = value else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-005",
                    DiagnosticPhase::Interpretation,
                    format!("{name} requires an integer operand"),
                    Some(origin),
                ));
            };
            let value = if name == "negate" {
                value.checked_neg()
            } else {
                value.checked_abs()
            }
            .ok_or_else(|| {
                VmDiagnostic::error(
                    "E-NUM-002",
                    DiagnosticPhase::Interpretation,
                    "integer overflow",
                    Some(SourceOrigin::generated(name)),
                )
            })?;
            stack.push(TypedValue::Int(value));
        }
        "not" => {
            let value = pop(stack)?;
            let TypedValue::Bool(value) = value else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-006",
                    DiagnosticPhase::Interpretation,
                    "not requires a boolean operand",
                    Some(origin),
                ));
            };
            stack.push(TypedValue::Bool(!value));
        }
        "dup" => {
            let value = stack
                .last()
                .cloned()
                .ok_or_else(|| runtime_underflow(&origin, Vec::new()))?;
            stack.push(value);
        }
        "drop" => {
            pop(stack)?;
        }
        "swap" => {
            if stack.len() < 2 {
                return Err(runtime_underflow(&origin, Vec::new()));
            }
            let len = stack.len();
            stack.swap(len - 1, len - 2);
        }
        "str-cat" => {
            let right = pop(stack)?;
            let left = pop(stack)?;
            let (TypedValue::String(left), TypedValue::String(right)) = (left, right) else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-007",
                    DiagnosticPhase::Interpretation,
                    "str-cat requires string operands",
                    Some(origin),
                ));
            };
            let mut result = String::with_capacity(left.len() + right.len());
            result.push_str(&left);
            result.push_str(&right);
            stack.push(TypedValue::String(result));
        }
        "bytes" => {
            let value = pop(stack)?;
            let TypedValue::String(value) = value else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-011",
                    DiagnosticPhase::Interpretation,
                    "bytes requires a string",
                    Some(origin),
                ));
            };
            stack.push(TypedValue::Bytes(value.into_bytes()));
        }
        "path" => {
            let value = pop(stack)?;
            let TypedValue::String(relative) = value else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-010",
                    DiagnosticPhase::Interpretation,
                    "path requires a string",
                    Some(origin),
                ));
            };
            let selector = super::effects::FileSelector::parse("./**").map_err(|error| {
                VmDiagnostic::error(
                    "E-PATH-001",
                    DiagnosticPhase::Interpretation,
                    error.to_string(),
                    Some(SourceOrigin::generated(name)),
                )
            })?;
            if !selector.matches(&relative) || relative.contains(['*', '?']) {
                return Err(VmDiagnostic::error(
                    "E-PATH-002",
                    DiagnosticPhase::Interpretation,
                    "path is not a normalized relative workspace path",
                    Some(SourceOrigin::generated(name)),
                ));
            }
            stack.push(TypedValue::Path { selector, relative });
        }
        "list-length" => {
            let value = pop(stack)?;
            let TypedValue::List { values, .. } = value else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-008",
                    DiagnosticPhase::Interpretation,
                    "list-length requires a list",
                    Some(origin),
                ));
            };
            stack.push(TypedValue::Int(values.len() as i64));
        }
        "list-get" => {
            let index = pop(stack)?;
            let list = pop(stack)?;
            let (TypedValue::List { values, .. }, TypedValue::Int(index)) = (list, index) else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-009",
                    DiagnosticPhase::Interpretation,
                    "list-get requires a list and integer index",
                    Some(origin),
                ));
            };
            let index = usize::try_from(index).ok();
            let value = index
                .and_then(|index| values.get(index).cloned())
                .ok_or_else(|| {
                    VmDiagnostic::error(
                        "E-INDEX-001",
                        DiagnosticPhase::Interpretation,
                        "list index is out of bounds",
                        Some(SourceOrigin::generated(name)),
                    )
                })?;
            stack.push(value);
        }
        _ => {
            return Err(VmDiagnostic::error(
                "E-LINK-004",
                DiagnosticPhase::Linking,
                format!("core word '{name}' has no interpreter implementation"),
                Some(origin),
            ));
        }
    }
    Ok(())
}

fn runtime_underflow(origin: &SourceOrigin, trace: Vec<String>) -> VmDiagnostic {
    let mut diagnostic = VmDiagnostic::error(
        "E-STACK-005",
        DiagnosticPhase::Interpretation,
        "runtime stack underflow",
        Some(origin.clone()),
    );
    diagnostic.trace = trace;
    diagnostic
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::ir::{BasicBlock, Function, LocatedInstruction, Module};
    use crate::vm::signature::{StackRow, StackSignature};
    use crate::vm::types::Type;
    use crate::vm::{core_vocabulary, Verifier};
    use std::collections::BTreeMap;

    fn arithmetic_module(instructions: Vec<Instruction>) -> VerifiedModule {
        let function = Function {
            name: "main".into(),
            signature: StackSignature::pure(
                StackRow::closed(Vec::new()),
                StackRow::closed(vec![Type::Int]),
            ),
            locals: Vec::new(),
            captures: Vec::new(),
            entry: 0,
            blocks: BTreeMap::from([(
                0,
                BasicBlock {
                    id: 0,
                    instructions: instructions
                        .into_iter()
                        .map(|instruction| LocatedInstruction::generated(instruction, "test"))
                        .collect(),
                },
            )]),
        };
        Verifier::new(&core_vocabulary())
            .verify(Module::single(function))
            .unwrap()
    }

    #[test]
    fn executes_verified_arithmetic_transactionally() {
        let module = arithmetic_module(vec![
            Instruction::Constant {
                value: TypedValue::Int(3),
            },
            Instruction::Constant {
                value: TypedValue::Int(4),
            },
            Instruction::Call {
                function: "+".into(),
            },
            Instruction::Return,
        ]);
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(7)]);
    }

    #[test]
    fn runtime_failure_rolls_back_stack() {
        let module = arithmetic_module(vec![
            Instruction::Constant {
                value: TypedValue::Int(1),
            },
            Instruction::Constant {
                value: TypedValue::Int(0),
            },
            Instruction::Call {
                function: "/".into(),
            },
            Instruction::Return,
        ]);
        let mut stack = vec![TypedValue::String("existing".into())];
        let error = Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap_err();
        assert_eq!(error.code, "E-NUM-001");
        assert_eq!(stack, vec![TypedValue::String("existing".into())]);
    }
}
