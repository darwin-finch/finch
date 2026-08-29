use super::diagnostic::{DiagnosticPhase, SourceOrigin, VmDiagnostic};
use super::effects::{CapabilityRequirement, EffectSet};
use super::ir::{BlockId, Function, Instruction, Module};
use super::signature::{StackRow, StackSignature, SuspensionSignature};
use super::types::Type;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

pub type Vocabulary = BTreeMap<String, StackSignature>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifiedFunction {
    pub name: String,
    pub inferred_effects: EffectSet,
    pub inferred_suspension: Option<SuspensionSignature>,
    pub entry_stack: Vec<Type>,
    pub block_stacks: BTreeMap<BlockId, Vec<Type>>,
}

/// A verified module is immutable execution data. Keeping the verifier's
/// facts with a suspended continuation lets the daemon resume exactly the
/// program that was authorized rather than recompiling submitted source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifiedModule {
    pub module: Module,
    pub functions: BTreeMap<String, VerifiedFunction>,
}

pub struct Verifier<'a> {
    vocabulary: &'a Vocabulary,
}

impl<'a> Verifier<'a> {
    pub fn new(vocabulary: &'a Vocabulary) -> Self {
        Self { vocabulary }
    }

    pub fn verify(&self, module: Module) -> Result<VerifiedModule, Vec<VmDiagnostic>> {
        let mut diagnostics = Vec::new();
        if module.version != super::VM_TYPE_SYSTEM_VERSION {
            diagnostics.push(VmDiagnostic::error(
                "E-IR-001",
                DiagnosticPhase::Linking,
                format!(
                    "unsupported Finch IR version {}; expected {}",
                    module.version,
                    super::VM_TYPE_SYSTEM_VERSION
                ),
                None,
            ));
        }
        if !module.functions.contains_key(&module.entry) {
            diagnostics.push(VmDiagnostic::error(
                "E-LINK-001",
                DiagnosticPhase::Linking,
                format!("entry function '{}' does not exist", module.entry),
                None,
            ));
        }

        let mut functions = BTreeMap::new();
        for function in module.functions.values() {
            match self.verify_function(function, &module.functions) {
                Ok(verified) => {
                    functions.insert(function.name.clone(), verified);
                }
                Err(mut errors) => diagnostics.append(&mut errors),
            }
        }
        if diagnostics.is_empty() {
            Ok(VerifiedModule { module, functions })
        } else {
            Err(diagnostics)
        }
    }

    fn verify_function(
        &self,
        function: &Function,
        module_functions: &BTreeMap<String, Function>,
    ) -> Result<VerifiedFunction, Vec<VmDiagnostic>> {
        let Some(_) = function.blocks.get(&function.entry) else {
            return Err(vec![VmDiagnostic::error(
                "E-IR-002",
                DiagnosticPhase::Verification,
                format!("function '{}' has no entry block", function.name),
                None,
            )]);
        };
        let entry_stack = function.signature.input.values.clone();
        let mut block_stacks = BTreeMap::from([(function.entry, entry_stack.clone())]);
        let mut queue = VecDeque::from([function.entry]);
        let mut inferred_effects = EffectSet::pure();
        let mut inferred_suspension = None;
        let mut diagnostics = Vec::new();

        while let Some(block_id) = queue.pop_front() {
            let Some(block) = function.blocks.get(&block_id) else {
                diagnostics.push(VmDiagnostic::error(
                    "E-IR-003",
                    DiagnosticPhase::Verification,
                    format!("missing block {block_id}"),
                    None,
                ));
                continue;
            };
            let mut stack = block_stacks[&block_id].clone();
            let mut terminated = false;
            for located in &block.instructions {
                if terminated {
                    diagnostics.push(VmDiagnostic::error(
                        "E-IR-004",
                        DiagnosticPhase::Verification,
                        "instruction appears after a block terminator",
                        Some(located.origin.clone()),
                    ));
                    break;
                }
                let result = self.apply_instruction(
                    &located.instruction,
                    &located.origin,
                    &mut stack,
                    function,
                    module_functions,
                    &mut inferred_effects,
                    &mut inferred_suspension,
                );
                match result {
                    Ok(successors) => {
                        for successor in successors {
                            merge_stack(
                                successor,
                                &stack,
                                &mut block_stacks,
                                &mut queue,
                                &located.origin,
                                &mut diagnostics,
                            );
                        }
                    }
                    Err(error) => diagnostics.push(error),
                }
                terminated = located.instruction.is_terminator();
            }
            if !terminated {
                diagnostics.push(VmDiagnostic::error(
                    "E-IR-005",
                    DiagnosticPhase::Verification,
                    format!("block {block_id} does not end with a terminator"),
                    block.instructions.last().map(|item| item.origin.clone()),
                ));
            }
        }

        if !function.signature.effects.grants(&inferred_effects) {
            let mut diagnostic = VmDiagnostic::error(
                "E-CAP-001",
                DiagnosticPhase::Verification,
                format!(
                    "function '{}' declares effects {} but requires {}",
                    function.name, function.signature.effects, inferred_effects
                ),
                None,
            );
            diagnostic.expected_effects = function.signature.effects.clone();
            diagnostic.found_effects = inferred_effects.clone();
            diagnostics.push(diagnostic);
        }

        if function.signature.suspension != inferred_suspension {
            diagnostics.push(VmDiagnostic::error(
                "E-YIELD-005",
                DiagnosticPhase::Verification,
                format!(
                    "function '{}' declares suspension {:?}, but its body requires {:?}",
                    function.name, function.signature.suspension, inferred_suspension
                ),
                None,
            ));
        }

        if diagnostics.is_empty() {
            Ok(VerifiedFunction {
                name: function.name.clone(),
                inferred_effects,
                inferred_suspension,
                entry_stack,
                block_stacks,
            })
        } else {
            Err(diagnostics)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_instruction(
        &self,
        instruction: &Instruction,
        origin: &SourceOrigin,
        stack: &mut Vec<Type>,
        function: &Function,
        module_functions: &BTreeMap<String, Function>,
        inferred_effects: &mut EffectSet,
        inferred_suspension: &mut Option<SuspensionSignature>,
    ) -> Result<Vec<BlockId>, VmDiagnostic> {
        match instruction {
            Instruction::Constant { value } => stack.push(value.value_type()),
            Instruction::MakeList {
                element_type,
                count,
            } => {
                let count = *count as usize;
                if stack.len() < count {
                    return Err(underflow(origin, count, stack.len()));
                }
                let start = stack.len() - count;
                for found in &stack[start..] {
                    if !element_type.accepts(found) {
                        return Err(VmDiagnostic::type_mismatch(
                            element_type.clone(),
                            found.clone(),
                            Some(origin.clone()),
                        ));
                    }
                }
                stack.truncate(start);
                stack.push(Type::list(element_type.clone()));
            }
            Instruction::MakeMap {
                key_type,
                value_type,
                count,
            } => {
                let value_count = (*count as usize).saturating_mul(2);
                if stack.len() < value_count {
                    return Err(underflow(origin, value_count, stack.len()));
                }
                let start = stack.len() - value_count;
                for pair in stack[start..].chunks_exact(2) {
                    if !key_type.accepts(&pair[0]) {
                        return Err(VmDiagnostic::type_mismatch(
                            key_type.clone(),
                            pair[0].clone(),
                            Some(origin.clone()),
                        ));
                    }
                    if !value_type.accepts(&pair[1]) {
                        return Err(VmDiagnostic::type_mismatch(
                            value_type.clone(),
                            pair[1].clone(),
                            Some(origin.clone()),
                        ));
                    }
                }
                stack.truncate(start);
                stack.push(Type::Map(
                    Box::new(key_type.clone()),
                    Box::new(value_type.clone()),
                ));
            }
            Instruction::MakeRecord { fields } => {
                let count = fields.len();
                if stack.len() < count {
                    return Err(underflow(origin, count, stack.len()));
                }
                let start = stack.len() - count;
                for ((_, expected), found) in fields.iter().zip(&stack[start..]) {
                    if !expected.accepts(found) {
                        return Err(VmDiagnostic::type_mismatch(
                            expected.clone(),
                            found.clone(),
                            Some(origin.clone()),
                        ));
                    }
                }
                stack.truncate(start);
                stack.push(Type::Record(fields.clone()));
            }
            Instruction::MakeVariant {
                variants,
                tag,
                payload_type,
            } => {
                let Some((_, expected_payload)) =
                    variants.iter().find(|(candidate, _)| candidate == tag)
                else {
                    return Err(VmDiagnostic::error(
                        "E-VARIANT-002",
                        DiagnosticPhase::Verification,
                        format!("variant has no tag '{tag}'"),
                        Some(origin.clone()),
                    ));
                };
                if expected_payload != payload_type {
                    return Err(VmDiagnostic::error(
                        "E-VARIANT-003",
                        DiagnosticPhase::Verification,
                        format!("variant tag '{tag}' has an inconsistent IR payload type"),
                        Some(origin.clone()),
                    ));
                }
                if let Some(expected) = payload_type {
                    let found = stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
                    if !expected.accepts(&found) {
                        return Err(VmDiagnostic::type_mismatch(
                            expected.clone(),
                            found,
                            Some(origin.clone()),
                        ));
                    }
                }
                stack.push(Type::Variant(variants.clone()));
            }
            Instruction::VariantGet {
                variants,
                tag,
                payload_type,
            } => {
                let found = stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
                let expected = Type::Variant(variants.clone());
                if found != expected {
                    return Err(VmDiagnostic::type_mismatch(
                        expected,
                        found,
                        Some(origin.clone()),
                    ));
                }
                let Some((_, expected_payload)) =
                    variants.iter().find(|(candidate, _)| candidate == tag)
                else {
                    return Err(VmDiagnostic::error(
                        "E-VARIANT-002",
                        DiagnosticPhase::Verification,
                        format!("variant has no tag '{tag}'"),
                        Some(origin.clone()),
                    ));
                };
                if expected_payload != payload_type {
                    return Err(VmDiagnostic::error(
                        "E-VARIANT-003",
                        DiagnosticPhase::Verification,
                        format!("variant tag '{tag}' has an inconsistent IR payload type"),
                        Some(origin.clone()),
                    ));
                }
                stack.push(Type::Option(Box::new(
                    payload_type.clone().unwrap_or(Type::Unit),
                )));
            }
            Instruction::RecordGet { field, value_type } => {
                let field_name = stack.pop().ok_or_else(|| underflow(origin, 2, 0))?;
                if field_name != Type::String {
                    return Err(VmDiagnostic::type_mismatch(
                        Type::String,
                        field_name,
                        Some(origin.clone()),
                    ));
                }
                let record = stack.pop().ok_or_else(|| underflow(origin, 2, 1))?;
                let Type::Record(fields) = record else {
                    return Err(VmDiagnostic::error(
                        "E-RECORD-004",
                        DiagnosticPhase::Verification,
                        "record field projection requires a typed record",
                        Some(origin.clone()),
                    ));
                };
                let Some((_, found)) = fields.iter().find(|(name, _)| name == field) else {
                    return Err(VmDiagnostic::error(
                        "E-RECORD-005",
                        DiagnosticPhase::Verification,
                        format!("record has no field '{field}'"),
                        Some(origin.clone()),
                    ));
                };
                if found != value_type {
                    return Err(VmDiagnostic::error(
                        "E-RECORD-006",
                        DiagnosticPhase::Verification,
                        format!("record field '{field}' has an inconsistent IR type"),
                        Some(origin.clone()),
                    ));
                }
                stack.push(Type::Option(Box::new(value_type.clone())));
            }
            Instruction::RecordSet {
                field,
                value_type,
                record_type,
            } => {
                let field_name = stack.pop().ok_or_else(|| underflow(origin, 3, 0))?;
                if field_name != Type::String {
                    return Err(VmDiagnostic::type_mismatch(
                        Type::String,
                        field_name,
                        Some(origin.clone()),
                    ));
                }
                let value = stack.pop().ok_or_else(|| underflow(origin, 3, 1))?;
                if !value_type.accepts(&value) {
                    return Err(VmDiagnostic::type_mismatch(
                        value_type.clone(),
                        value,
                        Some(origin.clone()),
                    ));
                }
                let record = stack.pop().ok_or_else(|| underflow(origin, 3, 2))?;
                let Type::Record(fields) = record else {
                    return Err(VmDiagnostic::error(
                        "E-RECORD-008",
                        DiagnosticPhase::Verification,
                        "record field update requires a typed record and replacement value",
                        Some(origin.clone()),
                    ));
                };
                if &fields != record_type {
                    return Err(VmDiagnostic::error(
                        "E-RECORD-009",
                        DiagnosticPhase::Verification,
                        "record update has an inconsistent IR record type",
                        Some(origin.clone()),
                    ));
                }
                let Some((_, expected)) = fields.iter().find(|(name, _)| name == field) else {
                    return Err(VmDiagnostic::error(
                        "E-RECORD-005",
                        DiagnosticPhase::Verification,
                        format!("record has no field '{field}'"),
                        Some(origin.clone()),
                    ));
                };
                if expected != value_type {
                    return Err(VmDiagnostic::error(
                        "E-RECORD-006",
                        DiagnosticPhase::Verification,
                        format!("record field '{field}' has an inconsistent IR type"),
                        Some(origin.clone()),
                    ));
                }
                stack.push(Type::Record(record_type.clone()));
            }
            Instruction::Dup => {
                let value = stack
                    .last()
                    .cloned()
                    .ok_or_else(|| underflow(origin, 1, 0))?;
                stack.push(value);
            }
            Instruction::Drop => {
                stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
            }
            Instruction::Swap => {
                if stack.len() < 2 {
                    return Err(underflow(origin, 2, stack.len()));
                }
                let len = stack.len();
                stack.swap(len - 1, len - 2);
            }
            Instruction::LocalGet { index } => {
                let value = function
                    .locals
                    .get(*index as usize)
                    .cloned()
                    .ok_or_else(|| {
                        VmDiagnostic::error(
                            "E-IR-006",
                            DiagnosticPhase::Verification,
                            format!("local index {index} is out of bounds"),
                            Some(origin.clone()),
                        )
                    })?;
                stack.push(value);
            }
            Instruction::LocalSet { index } => {
                let expected = function
                    .locals
                    .get(*index as usize)
                    .cloned()
                    .ok_or_else(|| {
                        VmDiagnostic::error(
                            "E-IR-006",
                            DiagnosticPhase::Verification,
                            format!("local index {index} is out of bounds"),
                            Some(origin.clone()),
                        )
                    })?;
                let found = stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
                if !expected.accepts(&found) {
                    return Err(VmDiagnostic::type_mismatch(
                        expected,
                        found,
                        Some(origin.clone()),
                    ));
                }
            }
            Instruction::CaptureGet { index } => {
                let value = function
                    .captures
                    .get(*index as usize)
                    .cloned()
                    .ok_or_else(|| {
                        VmDiagnostic::error(
                            "E-IR-007",
                            DiagnosticPhase::Verification,
                            format!("capture index {index} is out of bounds"),
                            Some(origin.clone()),
                        )
                    })?;
                stack.push(value);
            }
            Instruction::MakeClosure {
                function: closure_function,
                capture_count,
                signature,
            } => {
                let closure = module_functions.get(closure_function).ok_or_else(|| {
                    VmDiagnostic::error(
                        "E-LINK-003",
                        DiagnosticPhase::Linking,
                        format!("unknown closure function '{closure_function}'"),
                        Some(origin.clone()),
                    )
                })?;
                if &closure.signature != signature {
                    return Err(VmDiagnostic::error(
                        "E-TYPE-003",
                        DiagnosticPhase::Verification,
                        format!("closure signature does not match '{closure_function}'"),
                        Some(origin.clone()),
                    ));
                }
                if signature.output.values.len() != 1 {
                    return Err(VmDiagnostic::error(
                        "E-CLOSURE-001",
                        DiagnosticPhase::Verification,
                        "first-class closures must declare exactly one result; package multiple values in a record or list",
                        Some(origin.clone()),
                    ));
                }
                let count = *capture_count as usize;
                if stack.len() < count {
                    return Err(underflow(origin, count, stack.len()));
                }
                let actual = &stack[stack.len() - count..];
                if actual != closure.captures.as_slice() {
                    let mut diagnostic = VmDiagnostic::error(
                        "E-TYPE-004",
                        DiagnosticPhase::Verification,
                        "closure capture types do not match its environment",
                        Some(origin.clone()),
                    );
                    diagnostic.expected_types = closure.captures.clone();
                    diagnostic.found_types = actual.to_vec();
                    return Err(diagnostic);
                }
                stack.truncate(stack.len() - count);
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
                    suspension: signature.suspension.clone(),
                });
            }
            Instruction::Call { function: callee } => {
                let signature = module_functions
                    .get(callee)
                    .map(|function| &function.signature)
                    .or_else(|| self.vocabulary.get(callee))
                    .ok_or_else(|| {
                        VmDiagnostic::error(
                            "E-LINK-002",
                            DiagnosticPhase::Linking,
                            format!("unknown word or function '{callee}'"),
                            Some(origin.clone()),
                        )
                    })?;
                let concrete = instantiate_signature_types(signature, stack, origin)?;
                apply_signature(signature, stack, origin)?;
                *inferred_effects = inferred_effects.union(&signature.effects);
                merge_suspension(inferred_suspension, concrete.suspension.as_ref(), origin)?;
            }
            Instruction::CallClosure { signature } => {
                let closure = stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
                let expected_closure = Type::Function {
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
                    suspension: signature.suspension.clone(),
                };
                if closure != expected_closure && closure != Type::Dynamic {
                    return Err(VmDiagnostic::type_mismatch(
                        expected_closure,
                        closure,
                        Some(origin.clone()),
                    ));
                }
                apply_signature(signature, stack, origin)?;
                *inferred_effects = inferred_effects.union(&signature.effects);
                merge_suspension(inferred_suspension, signature.suspension.as_ref(), origin)?;
            }
            Instruction::OutputOpen => {
                let found = stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
                if !Type::String.accepts(&found) {
                    return Err(VmDiagnostic::type_mismatch(
                        Type::String,
                        found,
                        Some(origin.clone()),
                    ));
                }
                stack.push(Type::Resource("output-handle".into()));
                *inferred_effects =
                    inferred_effects.union(&EffectSet::from_requirement(CapabilityRequirement {
                        capability: super::effects::CapabilityKind::SessionEmit,
                        selector: super::effects::ResourceSelector::None,
                    }));
            }
            Instruction::UiEffect { input, output, .. } => {
                let signature = StackSignature::pure(
                    StackRow::polymorphic("S", input.clone()),
                    StackRow::polymorphic("S", output.clone()),
                );
                apply_signature(&signature, stack, origin)?;
                *inferred_effects =
                    inferred_effects.union(&EffectSet::from_requirement(CapabilityRequirement {
                        capability: super::effects::CapabilityKind::SessionEmit,
                        selector: super::effects::ResourceSelector::None,
                    }));
            }
            Instruction::CapabilityRequest {
                requirement,
                input,
                output,
            } => {
                apply_stack_types(input, output, stack, origin)?;
                *inferred_effects =
                    inferred_effects.union(&EffectSet::from_requirement(requirement.clone()));
            }
            Instruction::Yield { value_type } => {
                let found = stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
                if !value_type.accepts(&found) {
                    return Err(VmDiagnostic::type_mismatch(
                        value_type.clone(),
                        found,
                        Some(origin.clone()),
                    ));
                }
                merge_suspension(
                    inferred_suspension,
                    Some(&SuspensionSignature::one_way(value_type.clone())),
                    origin,
                )?;
            }
            Instruction::DeferFiber => {
                let closure = stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
                let Type::Function {
                    arguments,
                    result,
                    effects,
                    suspension: Some(suspension),
                } = closure
                else {
                    return Err(VmDiagnostic::error(
                        "E-FIBER-021",
                        DiagnosticPhase::Verification,
                        "defer requires a typed yielding closure",
                        Some(origin.clone()),
                    ));
                };
                if !arguments.is_empty() || !effects.is_pure() {
                    return Err(VmDiagnostic::error(
                        "E-FIBER-022",
                        DiagnosticPhase::Verification,
                        "cooperative defer requires a pure zero-argument closure",
                        Some(origin.clone()),
                    ));
                }
                if *suspension.resume_type != Type::Unit {
                    return Err(VmDiagnostic::error(
                        "E-FIBER-023",
                        DiagnosticPhase::Verification,
                        "this runtime version supports only unit-resumed producer fibers",
                        Some(origin.clone()),
                    ));
                }
                stack.push(Type::Fiber(suspension.yield_type, result));
            }
            Instruction::NextFiber => {
                let fiber = stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
                let Type::Fiber(yield_type, result_type) = fiber else {
                    return Err(VmDiagnostic::error(
                        "E-FIBER-024",
                        DiagnosticPhase::Verification,
                        "fiber-next requires fiber<Y,R>",
                        Some(origin.clone()),
                    ));
                };
                stack.push(Type::fiber_step(*yield_type, *result_type));
            }
            Instruction::JoinFiber => {
                let fiber = stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
                let Type::Fiber(_, result_type) = fiber else {
                    return Err(VmDiagnostic::error(
                        "E-FIBER-025",
                        DiagnosticPhase::Verification,
                        "fiber-join requires fiber<Y,R>",
                        Some(origin.clone()),
                    ));
                };
                stack.push(*result_type);
            }
            Instruction::CancelFiber => {
                let fiber = stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
                if !matches!(fiber, Type::Fiber(_, _)) {
                    return Err(VmDiagnostic::error(
                        "E-FIBER-026",
                        DiagnosticPhase::Verification,
                        "fiber-cancel requires fiber<Y,R>",
                        Some(origin.clone()),
                    ));
                }
                stack.push(Type::Unit);
            }
            Instruction::DeferCpu => {
                let closure = stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
                let Type::Function {
                    arguments,
                    result,
                    effects,
                    suspension,
                } = closure
                else {
                    return Err(VmDiagnostic::error(
                        "E-FIBER-003",
                        DiagnosticPhase::Verification,
                        "defer-cpu requires a typed closure",
                        Some(origin.clone()),
                    ));
                };
                if !arguments.is_empty() {
                    return Err(VmDiagnostic::error(
                        "E-FIBER-004",
                        DiagnosticPhase::Verification,
                        "defer-cpu requires a zero-argument closure; capture its arguments first",
                        Some(origin.clone()),
                    ));
                }
                if !effects.is_pure() {
                    return Err(VmDiagnostic::error(
                        "E-FIBER-005",
                        DiagnosticPhase::Verification,
                        "defer-cpu requires a pure closure",
                        Some(origin.clone()),
                    ));
                }
                if let Some(suspension) = suspension {
                    if *suspension.yield_type != Type::Unit || *suspension.resume_type != Type::Unit
                    {
                        return Err(VmDiagnostic::error(
                            "E-YIELD-003",
                            DiagnosticPhase::Verification,
                            "CPU tasks may yield only unit timeslices; use defer for produced values",
                            Some(origin.clone()),
                        ));
                    }
                }
                stack.push(Type::Task(result));
            }
            Instruction::PollCpuFiber => {
                let task = stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
                let Type::Task(result) = task else {
                    return Err(VmDiagnostic::error(
                        "E-FIBER-009",
                        DiagnosticPhase::Verification,
                        "task-poll requires task<T>",
                        Some(origin.clone()),
                    ));
                };
                stack.push(Type::task_poll(*result));
            }
            Instruction::JoinCpuFiber => {
                let task = stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
                let Type::Task(result) = task else {
                    return Err(VmDiagnostic::error(
                        "E-FIBER-010",
                        DiagnosticPhase::Verification,
                        "task-join requires task<T>",
                        Some(origin.clone()),
                    ));
                };
                stack.push(*result);
            }
            Instruction::CancelCpuFiber => {
                let task = stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
                if !matches!(task, Type::Task(_)) {
                    return Err(VmDiagnostic::error(
                        "E-FIBER-020",
                        DiagnosticPhase::Verification,
                        "task-cancel requires task<T>",
                        Some(origin.clone()),
                    ));
                }
                stack.push(Type::Unit);
            }
            Instruction::PropagateResult {
                return_ok_type,
                error_type,
            } => {
                let result = stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
                let Type::Result(ok_type, found_error) = result else {
                    return Err(VmDiagnostic::error(
                        "E-RESULT-TRY-001",
                        DiagnosticPhase::Verification,
                        "result propagation requires result<T,E>",
                        Some(origin.clone()),
                    ));
                };
                let [Type::Result(declared_ok, declared_error)] =
                    function.signature.output.values.as_slice()
                else {
                    return Err(VmDiagnostic::error(
                        "E-RESULT-TRY-002",
                        DiagnosticPhase::Verification,
                        "result propagation is valid only in a function returning one result<T,E>",
                        Some(origin.clone()),
                    ));
                };
                unify(declared_ok, return_ok_type, &mut BTreeMap::new(), origin)?;
                unify(declared_error, error_type, &mut BTreeMap::new(), origin)?;
                unify(error_type, &found_error, &mut BTreeMap::new(), origin)?;
                stack.push(*ok_type);
            }
            Instruction::Jump { target } => return Ok(vec![*target]),
            Instruction::Branch {
                then_block,
                else_block,
            } => {
                let found = stack.pop().ok_or_else(|| underflow(origin, 1, 0))?;
                if !Type::Bool.accepts(&found) {
                    return Err(VmDiagnostic::type_mismatch(
                        Type::Bool,
                        found,
                        Some(origin.clone()),
                    ));
                }
                return Ok(vec![*then_block, *else_block]);
            }
            Instruction::Return => {
                verify_return(&function.signature.output, stack, origin)?;
            }
            Instruction::Trap { .. } => {}
        }
        Ok(Vec::new())
    }
}

fn apply_signature(
    signature: &StackSignature,
    stack: &mut Vec<Type>,
    origin: &SourceOrigin,
) -> Result<(), VmDiagnostic> {
    let required = signature.input.values.len();
    if stack.len() < required {
        return Err(underflow(origin, required, stack.len()));
    }
    if signature.input.tail.is_none() && stack.len() != required {
        return Err(VmDiagnostic::error(
            "E-STACK-002",
            DiagnosticPhase::Verification,
            format!(
                "closed signature requires exactly {required} values, found {}",
                stack.len()
            ),
            Some(origin.clone()),
        ));
    }
    let prefix_len = stack.len() - required;
    let mut substitutions = BTreeMap::<String, Type>::new();
    for (expected, found) in signature
        .input
        .values
        .iter()
        .zip(stack[prefix_len..].iter())
    {
        unify(expected, found, &mut substitutions, origin)?;
    }
    stack.truncate(prefix_len);
    if signature.output.tail.is_none() {
        stack.clear();
    }
    stack.extend(
        signature
            .output
            .values
            .iter()
            .map(|ty| substitute(ty, &substitutions)),
    );
    Ok(())
}

/// Apply a word signature to a concrete virtual stack. Frontends use the same
/// unification logic as the verifier while deriving a submitted program's
/// output signature.
pub(crate) fn apply_signature_types(
    signature: &StackSignature,
    stack: &mut Vec<Type>,
    origin: &SourceOrigin,
) -> Result<(), VmDiagnostic> {
    apply_signature(signature, stack, origin)
}

/// Instantiate a polymorphic signature against the current concrete suffix
/// without mutating the frontend's virtual stack. Awaited host effects carry
/// this concrete row in their portable ABI, so a host result such as
/// `option<string>` is not incorrectly compared with `option<A>` on resume.
pub(crate) fn instantiate_signature_types(
    signature: &StackSignature,
    stack: &[Type],
    origin: &SourceOrigin,
) -> Result<StackSignature, VmDiagnostic> {
    let required = signature.input.values.len();
    if stack.len() < required {
        return Err(underflow(origin, required, stack.len()));
    }
    if signature.input.tail.is_none() && stack.len() != required {
        return Err(VmDiagnostic::error(
            "E-STACK-002",
            DiagnosticPhase::Verification,
            format!(
                "closed signature requires exactly {required} values, found {}",
                stack.len()
            ),
            Some(origin.clone()),
        ));
    }
    let prefix_len = stack.len() - required;
    let mut substitutions = BTreeMap::<String, Type>::new();
    for (expected, found) in signature
        .input
        .values
        .iter()
        .zip(stack[prefix_len..].iter())
    {
        unify(expected, found, &mut substitutions, origin)?;
    }
    Ok(StackSignature {
        type_parameters: signature.type_parameters.clone(),
        input: StackRow {
            tail: signature.input.tail.clone(),
            values: signature
                .input
                .values
                .iter()
                .map(|ty| substitute(ty, &substitutions))
                .collect(),
        },
        output: StackRow {
            tail: signature.output.tail.clone(),
            values: signature
                .output
                .values
                .iter()
                .map(|ty| substitute(ty, &substitutions))
                .collect(),
        },
        effects: signature.effects.clone(),
        control: signature.control.clone(),
        suspension: signature
            .suspension
            .as_ref()
            .map(|suspension| SuspensionSignature {
                yield_type: Box::new(substitute(&suspension.yield_type, &substitutions)),
                resume_type: Box::new(substitute(&suspension.resume_type, &substitutions)),
            }),
    })
}

fn apply_stack_types(
    input: &[Type],
    output: &[Type],
    stack: &mut Vec<Type>,
    origin: &SourceOrigin,
) -> Result<(), VmDiagnostic> {
    let signature = StackSignature::pure(
        StackRow::polymorphic("S", input.to_vec()),
        StackRow::polymorphic("S", output.to_vec()),
    );
    apply_signature(&signature, stack, origin)
}

fn unify(
    expected: &Type,
    found: &Type,
    substitutions: &mut BTreeMap<String, Type>,
    origin: &SourceOrigin,
) -> Result<(), VmDiagnostic> {
    if let Type::Variable(name) = expected {
        if let Some(bound) = substitutions.get(name) {
            if bound != found {
                return Err(VmDiagnostic::type_mismatch(
                    bound.clone(),
                    found.clone(),
                    Some(origin.clone()),
                ));
            }
        } else {
            substitutions.insert(name.clone(), found.clone());
        }
        return Ok(());
    }
    match (expected, found) {
        (Type::List(expected), Type::List(found))
        | (Type::Option(expected), Type::Option(found))
        | (Type::Task(expected), Type::Task(found))
        | (Type::Stream(expected), Type::Stream(found)) => {
            unify(expected, found, substitutions, origin)
        }
        (Type::Fiber(expected_yield, expected_result), Type::Fiber(found_yield, found_result)) => {
            unify(expected_yield, found_yield, substitutions, origin)?;
            unify(expected_result, found_result, substitutions, origin)
        }
        (
            Type::Function {
                arguments: expected_arguments,
                result: expected_result,
                effects: expected_effects,
                suspension: expected_suspension,
            },
            Type::Function {
                arguments: found_arguments,
                result: found_result,
                effects: found_effects,
                suspension: found_suspension,
            },
        ) => {
            let non_suspending_subtype = matches!(
                (expected_suspension, found_suspension),
                (Some(expected), None)
                    if *expected.yield_type == Type::Unit
                        && *expected.resume_type == Type::Unit
            );
            if expected_arguments.len() != found_arguments.len()
                || expected_effects != found_effects
                || (expected_suspension.is_some() != found_suspension.is_some()
                    && !non_suspending_subtype)
            {
                return Err(VmDiagnostic::type_mismatch(
                    expected.clone(),
                    found.clone(),
                    Some(origin.clone()),
                ));
            }
            for (expected, found) in expected_arguments.iter().zip(found_arguments) {
                unify(expected, found, substitutions, origin)?;
            }
            unify(expected_result, found_result, substitutions, origin)?;
            if let (Some(expected), Some(found)) = (expected_suspension, found_suspension) {
                unify(
                    &expected.yield_type,
                    &found.yield_type,
                    substitutions,
                    origin,
                )?;
                unify(
                    &expected.resume_type,
                    &found.resume_type,
                    substitutions,
                    origin,
                )?;
            }
            Ok(())
        }
        (Type::Map(expected_key, expected_value), Type::Map(found_key, found_value)) => {
            unify(expected_key, found_key, substitutions, origin)?;
            unify(expected_value, found_value, substitutions, origin)
        }
        (Type::Result(expected_ok, expected_err), Type::Result(found_ok, found_err)) => {
            unify(expected_ok, found_ok, substitutions, origin)?;
            unify(expected_err, found_err, substitutions, origin)
        }
        (Type::Record(expected_fields), Type::Record(found_fields)) => {
            if expected_fields.len() != found_fields.len()
                || expected_fields
                    .iter()
                    .zip(found_fields)
                    .any(|((expected_name, _), (found_name, _))| expected_name != found_name)
            {
                return Err(VmDiagnostic::type_mismatch(
                    expected.clone(),
                    found.clone(),
                    Some(origin.clone()),
                ));
            }
            for ((_, expected_type), (_, found_type)) in expected_fields.iter().zip(found_fields) {
                unify(expected_type, found_type, substitutions, origin)?;
            }
            Ok(())
        }
        (Type::Variant(expected_variants), Type::Variant(found_variants)) => {
            if expected_variants.len() != found_variants.len()
                || expected_variants
                    .iter()
                    .zip(found_variants)
                    .any(|((expected_name, _), (found_name, _))| expected_name != found_name)
            {
                return Err(VmDiagnostic::type_mismatch(
                    expected.clone(),
                    found.clone(),
                    Some(origin.clone()),
                ));
            }
            for ((_, expected_payload), (_, found_payload)) in
                expected_variants.iter().zip(found_variants)
            {
                match (expected_payload, found_payload) {
                    (Some(expected_type), Some(found_type)) => {
                        unify(expected_type, found_type, substitutions, origin)?;
                    }
                    (None, None) => {}
                    _ => {
                        return Err(VmDiagnostic::type_mismatch(
                            expected.clone(),
                            found.clone(),
                            Some(origin.clone()),
                        ));
                    }
                }
            }
            Ok(())
        }
        _ if expected.accepts(found) => Ok(()),
        _ => Err(VmDiagnostic::type_mismatch(
            expected.clone(),
            found.clone(),
            Some(origin.clone()),
        )),
    }
}

fn substitute(ty: &Type, substitutions: &BTreeMap<String, Type>) -> Type {
    match ty {
        Type::Variable(name) => substitutions
            .get(name)
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        Type::List(inner) => Type::List(Box::new(substitute(inner, substitutions))),
        Type::Option(inner) => Type::Option(Box::new(substitute(inner, substitutions))),
        Type::Result(ok, error) => Type::Result(
            Box::new(substitute(ok, substitutions)),
            Box::new(substitute(error, substitutions)),
        ),
        Type::Map(key, value) => Type::Map(
            Box::new(substitute(key, substitutions)),
            Box::new(substitute(value, substitutions)),
        ),
        Type::Record(fields) => Type::Record(
            fields
                .iter()
                .map(|(name, ty)| (name.clone(), substitute(ty, substitutions)))
                .collect(),
        ),
        Type::Variant(variants) => Type::Variant(
            variants
                .iter()
                .map(|(name, payload)| {
                    (
                        name.clone(),
                        payload
                            .as_ref()
                            .map(|payload| substitute(payload, substitutions)),
                    )
                })
                .collect(),
        ),
        Type::Task(inner) => Type::Task(Box::new(substitute(inner, substitutions))),
        Type::Fiber(yield_type, result_type) => Type::Fiber(
            Box::new(substitute(yield_type, substitutions)),
            Box::new(substitute(result_type, substitutions)),
        ),
        Type::Stream(inner) => Type::Stream(Box::new(substitute(inner, substitutions))),
        Type::Function {
            arguments,
            result,
            effects,
            suspension,
        } => Type::Function {
            arguments: arguments
                .iter()
                .map(|argument| substitute(argument, substitutions))
                .collect(),
            result: Box::new(substitute(result, substitutions)),
            effects: effects.clone(),
            suspension: suspension.as_ref().map(|suspension| SuspensionSignature {
                yield_type: Box::new(substitute(&suspension.yield_type, substitutions)),
                resume_type: Box::new(substitute(&suspension.resume_type, substitutions)),
            }),
        },
        _ => ty.clone(),
    }
}

fn verify_return(
    expected: &StackRow,
    found: &[Type],
    origin: &SourceOrigin,
) -> Result<(), VmDiagnostic> {
    if found.len() < expected.values.len() {
        return Err(underflow(origin, expected.values.len(), found.len()));
    }
    let suffix = &found[found.len() - expected.values.len()..];
    if expected.tail.is_none() && suffix.len() != found.len() {
        return Err(VmDiagnostic::error(
            "E-STACK-003",
            DiagnosticPhase::Verification,
            "return leaves unexpected values on a closed stack",
            Some(origin.clone()),
        ));
    }
    let mut substitutions = BTreeMap::new();
    for (expected, found) in expected.values.iter().zip(suffix.iter()) {
        // Return annotations are real structured contracts. In particular,
        // `ok(value)` initially leaves `result<T,dynamic>` until an enclosing
        // declared `result<T,E>` return fixes its otherwise unknown error
        // component. Use the same recursive unification as normal calls
        // rather than comparing only the outer type constructor.
        unify(expected, found, &mut substitutions, origin)?;
    }
    Ok(())
}

fn merge_stack(
    target: BlockId,
    incoming: &[Type],
    block_stacks: &mut BTreeMap<BlockId, Vec<Type>>,
    queue: &mut VecDeque<BlockId>,
    origin: &SourceOrigin,
    diagnostics: &mut Vec<VmDiagnostic>,
) {
    match block_stacks.get(&target) {
        None => {
            block_stacks.insert(target, incoming.to_vec());
            queue.push_back(target);
        }
        Some(existing) if existing == incoming => {}
        Some(existing) => {
            let mut diagnostic = VmDiagnostic::error(
                "E-STACK-004",
                DiagnosticPhase::Verification,
                format!("incompatible stacks merge at block {target}"),
                Some(origin.clone()),
            );
            diagnostic.expected_types = existing.clone();
            diagnostic.found_types = incoming.to_vec();
            diagnostics.push(diagnostic);
        }
    }
}

fn merge_suspension(
    current: &mut Option<SuspensionSignature>,
    incoming: Option<&SuspensionSignature>,
    origin: &SourceOrigin,
) -> Result<(), VmDiagnostic> {
    let Some(incoming) = incoming else {
        return Ok(());
    };
    if let Some(current) = current {
        if current != incoming {
            return Err(VmDiagnostic::error(
                "E-YIELD-004",
                DiagnosticPhase::Verification,
                format!(
                    "one callable cannot yield incompatible types {} and {}",
                    current.yield_type, incoming.yield_type
                ),
                Some(origin.clone()),
            ));
        }
    } else {
        *current = Some(incoming.clone());
    }
    Ok(())
}

fn underflow(origin: &SourceOrigin, expected: usize, found: usize) -> VmDiagnostic {
    VmDiagnostic::error(
        "E-STACK-001",
        DiagnosticPhase::Verification,
        format!("stack underflow: instruction needs {expected} values, found {found}"),
        Some(origin.clone()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::effects::{CapabilityRequirement, FileOperation, FileSelector};
    use crate::vm::ir::{BasicBlock, LocatedInstruction};
    use crate::vm::signature::ControlEffect;
    use crate::vm::types::TypedValue;

    fn core_vocabulary() -> Vocabulary {
        let stack = |input, output| {
            StackSignature::pure(
                StackRow::polymorphic("S", input),
                StackRow::polymorphic("S", output),
            )
        };
        BTreeMap::from([
            (
                "+".into(),
                stack(vec![Type::Int, Type::Int], vec![Type::Int]),
            ),
            (
                "dup".into(),
                stack(
                    vec![Type::Variable("A".into())],
                    vec![Type::Variable("A".into()), Type::Variable("A".into())],
                ),
            ),
        ])
    }

    fn function(signature: StackSignature, instructions: Vec<Instruction>) -> Function {
        Function {
            name: "main".into(),
            documentation: None,
            signature,
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
        }
    }

    #[test]
    fn verifies_typed_arithmetic_and_generic_dup() {
        let signature = StackSignature::pure(
            StackRow::closed(Vec::new()),
            StackRow::closed(vec![Type::Int]),
        );
        let module = Module::single(function(
            signature,
            vec![
                Instruction::Constant {
                    value: TypedValue::Int(3),
                },
                Instruction::Dup,
                Instruction::Call {
                    function: "+".into(),
                },
                Instruction::Return,
            ],
        ));
        assert!(Verifier::new(&core_vocabulary()).verify(module).is_ok());
    }

    #[test]
    fn rejects_wrong_argument_type_with_stable_diagnostic() {
        let signature = StackSignature::pure(
            StackRow::closed(Vec::new()),
            StackRow::closed(vec![Type::Int]),
        );
        let module = Module::single(function(
            signature,
            vec![
                Instruction::Constant {
                    value: TypedValue::String("x".into()),
                },
                Instruction::Constant {
                    value: TypedValue::Int(2),
                },
                Instruction::Call {
                    function: "+".into(),
                },
                Instruction::Return,
            ],
        ));
        let errors = Verifier::new(&core_vocabulary())
            .verify(module)
            .unwrap_err();
        assert!(errors.iter().any(|error| error.code == "E-TYPE-002"));
    }

    #[test]
    fn rejects_malformed_variant_construction_ir() {
        let variants = vec![("none".into(), None), ("some".into(), Some(Type::Int))];
        let signature = StackSignature::pure(
            StackRow::closed(Vec::new()),
            StackRow::closed(vec![Type::Variant(variants.clone())]),
        );
        let unknown_tag = Module::single(function(
            signature.clone(),
            vec![
                Instruction::MakeVariant {
                    variants: variants.clone(),
                    tag: "missing".into(),
                    payload_type: None,
                },
                Instruction::Return,
            ],
        ));
        let errors = Verifier::new(&core_vocabulary())
            .verify(unknown_tag)
            .expect_err("unknown variant tags must not enter verified IR");
        assert!(errors.iter().any(|error| error.code == "E-VARIANT-002"));

        let inconsistent_payload = Module::single(function(
            signature,
            vec![
                Instruction::MakeVariant {
                    variants,
                    tag: "some".into(),
                    payload_type: Some(Type::String),
                },
                Instruction::Return,
            ],
        ));
        let errors = Verifier::new(&core_vocabulary())
            .verify(inconsistent_payload)
            .expect_err("variant payload metadata must match the closed type");
        assert!(errors.iter().any(|error| error.code == "E-VARIANT-003"));

        let variants = vec![("none".into(), None), ("some".into(), Some(Type::Int))];
        let projection = Module::single(function(
            StackSignature::pure(
                StackRow::closed(vec![Type::Variant(variants.clone())]),
                StackRow::closed(vec![Type::Option(Box::new(Type::Unit))]),
            ),
            vec![
                Instruction::VariantGet {
                    variants,
                    tag: "missing".into(),
                    payload_type: None,
                },
                Instruction::Return,
            ],
        ));
        let errors = Verifier::new(&core_vocabulary())
            .verify(projection)
            .expect_err("unknown projection tags must not enter verified IR");
        assert!(errors.iter().any(|error| error.code == "E-VARIANT-002"));
    }

    #[test]
    fn rejects_a_yield_hidden_from_the_callable_contract() {
        let signature = StackSignature::pure(
            StackRow::closed(Vec::new()),
            StackRow::closed(vec![Type::Int]),
        );
        let module = Module::single(function(
            signature,
            vec![
                Instruction::Constant {
                    value: TypedValue::Int(1),
                },
                Instruction::Yield {
                    value_type: Type::Int,
                },
                Instruction::Constant {
                    value: TypedValue::Int(2),
                },
                Instruction::Return,
            ],
        ));
        let errors = Verifier::new(&core_vocabulary())
            .verify(module)
            .expect_err("verified IR cannot hide a suspension boundary");
        assert!(errors.iter().any(|error| error.code == "E-YIELD-005"));
    }

    #[test]
    fn rejects_undeclared_capability_effect() {
        let signature = StackSignature {
            type_parameters: Vec::new(),
            input: StackRow::closed(vec![Type::String]),
            output: StackRow::closed(vec![Type::Unit]),
            effects: EffectSet::pure(),
            control: ControlEffect::Returns,
            suspension: None,
        };
        let requirement = CapabilityRequirement::file(
            FileOperation::Write,
            FileSelector::parse("./generated/**").unwrap(),
        );
        let module = Module::single(function(
            signature,
            vec![
                Instruction::CapabilityRequest {
                    requirement,
                    input: vec![Type::String],
                    output: vec![Type::Unit],
                },
                Instruction::Return,
            ],
        ));
        let errors = Verifier::new(&core_vocabulary())
            .verify(module)
            .unwrap_err();
        assert!(errors.iter().any(|error| error.code == "E-CAP-001"));
    }

    #[test]
    fn rejects_incompatible_branch_stacks() {
        let signature = StackSignature::pure(
            StackRow::closed(Vec::new()),
            StackRow::closed(vec![Type::Int]),
        );
        let mut main = function(
            signature,
            vec![
                Instruction::Constant {
                    value: TypedValue::Bool(true),
                },
                Instruction::Branch {
                    then_block: 1,
                    else_block: 2,
                },
            ],
        );
        main.blocks.insert(
            1,
            BasicBlock {
                id: 1,
                instructions: vec![
                    LocatedInstruction::generated(
                        Instruction::Constant {
                            value: TypedValue::Int(1),
                        },
                        "then",
                    ),
                    LocatedInstruction::generated(Instruction::Jump { target: 3 }, "then"),
                ],
            },
        );
        main.blocks.insert(
            2,
            BasicBlock {
                id: 2,
                instructions: vec![
                    LocatedInstruction::generated(
                        Instruction::Constant {
                            value: TypedValue::String("x".into()),
                        },
                        "else",
                    ),
                    LocatedInstruction::generated(Instruction::Jump { target: 3 }, "else"),
                ],
            },
        );
        main.blocks.insert(
            3,
            BasicBlock {
                id: 3,
                instructions: vec![LocatedInstruction::generated(Instruction::Return, "return")],
            },
        );
        let errors = Verifier::new(&core_vocabulary())
            .verify(Module::single(main))
            .unwrap_err();
        assert!(errors.iter().any(|error| error.code == "E-STACK-004"));
    }

    #[test]
    fn caller_inherits_callee_capabilities() {
        let requirement = CapabilityRequirement::file(
            FileOperation::Write,
            FileSelector::parse("./reports/**").unwrap(),
        );
        let effects = EffectSet::from_requirement(requirement.clone());
        let callee = Function {
            name: "save".into(),
            documentation: None,
            signature: StackSignature {
                type_parameters: Vec::new(),
                input: StackRow::closed(vec![Type::String]),
                output: StackRow::closed(vec![Type::Unit]),
                effects: effects.clone(),
                control: ControlEffect::MaySuspend,
                suspension: None,
            },
            locals: Vec::new(),
            captures: Vec::new(),
            entry: 0,
            blocks: BTreeMap::from([(
                0,
                BasicBlock {
                    id: 0,
                    instructions: vec![
                        LocatedInstruction::generated(
                            Instruction::CapabilityRequest {
                                requirement,
                                input: vec![Type::String],
                                output: vec![Type::Unit],
                            },
                            "file.write",
                        ),
                        LocatedInstruction::generated(Instruction::Return, "return"),
                    ],
                },
            )]),
        };
        let caller = Function {
            name: "main".into(),
            documentation: None,
            signature: StackSignature {
                type_parameters: Vec::new(),
                input: StackRow::closed(Vec::new()),
                output: StackRow::closed(vec![Type::Unit]),
                effects: effects.clone(),
                control: ControlEffect::MaySuspend,
                suspension: None,
            },
            locals: Vec::new(),
            captures: Vec::new(),
            entry: 0,
            blocks: BTreeMap::from([(
                0,
                BasicBlock {
                    id: 0,
                    instructions: vec![
                        LocatedInstruction::generated(
                            Instruction::Constant {
                                value: TypedValue::String("report".into()),
                            },
                            "literal",
                        ),
                        LocatedInstruction::generated(
                            Instruction::Call {
                                function: "save".into(),
                            },
                            "save",
                        ),
                        LocatedInstruction::generated(Instruction::Return, "return"),
                    ],
                },
            )]),
        };
        let module = Module {
            version: crate::vm::VM_TYPE_SYSTEM_VERSION,
            name: "composition".into(),
            entry: "main".into(),
            functions: BTreeMap::from([("main".into(), caller), ("save".into(), callee)]),
        };
        let verified = Verifier::new(&core_vocabulary()).verify(module).unwrap();
        assert_eq!(verified.functions["main"].inferred_effects, effects);
    }
}
