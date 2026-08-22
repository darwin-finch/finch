use super::diagnostic::{DiagnosticPhase, SourceOrigin, VmDiagnostic};
use super::effects::{CapabilityKind, CapabilityRequirement, EffectSet};
use super::frontend::{forth::compile_forth_with_functions, lisp::compile_lisp_with_functions};
use super::interpreter::{CapabilityHandler, Interpreter, InterpreterConfig};
use super::ir::Function;
use super::types::TypedValue;
use super::{core_vocabulary, VerifiedModule, Vocabulary};
use crate::programs::ProgramLanguage;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Result of compiling, authorizing, and interpreting one source submission.
/// Authorization is decided before interpretation, so an approval request
/// never exposes a partially mutated VM stack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypedExecution {
    pub status: TypedExecutionStatus,
    pub values: Vec<TypedValue>,
    pub output: String,
    pub effects: EffectSet,
    pub diagnostics: Vec<VmDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TypedExecutionStatus {
    Completed,
    AuthorizationRequired {
        requirements: Vec<CapabilityRequirement>,
    },
    Failed,
}

/// Persistent typed stack shared by Finch Lisp and Co-Forth source.
pub struct TypedRuntime {
    stack: Vec<TypedValue>,
    vocabulary: Vocabulary,
    functions: BTreeMap<String, Function>,
    grants: EffectSet,
}

impl Default for TypedRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl TypedRuntime {
    pub fn new() -> Self {
        let response = CapabilityRequirement {
            capability: CapabilityKind::SessionEmit,
            selector: super::effects::ResourceSelector::None,
        };
        Self {
            stack: Vec::new(),
            vocabulary: core_vocabulary(),
            functions: BTreeMap::new(),
            // Producing the requested assistant response is part of the
            // session contract, not an ambient host permission.
            grants: EffectSet::from_requirement(response),
        }
    }

    pub fn stack(&self) -> &[TypedValue] {
        &self.stack
    }

    pub fn vocabulary(&self) -> &Vocabulary {
        &self.vocabulary
    }

    pub fn functions(&self) -> &BTreeMap<String, Function> {
        &self.functions
    }

    pub fn grants(&self) -> &EffectSet {
        &self.grants
    }

    pub fn set_grants(&mut self, grants: EffectSet) {
        self.grants = grants;
    }

    pub fn grant(&mut self, requirement: CapabilityRequirement) {
        self.grants = self.grants.union(&EffectSet::from_requirement(requirement));
    }

    pub fn execute(
        &mut self,
        language: ProgramLanguage,
        source_id: &str,
        source: &str,
        fuel: u64,
    ) -> TypedExecution {
        self.execute_with_declaration(language, source_id, source, fuel, None)
    }

    pub fn execute_with_declaration(
        &mut self,
        language: ProgramLanguage,
        source_id: &str,
        source: &str,
        fuel: u64,
        declared: Option<&EffectSet>,
    ) -> TypedExecution {
        let initial_types = self.stack.iter().map(TypedValue::value_type).collect();
        let compiled = match language {
            ProgramLanguage::Forth => compile_forth_with_functions(
                source_id,
                source,
                initial_types,
                &self.vocabulary,
                &self.functions,
            ),
            ProgramLanguage::Lisp => compile_lisp_with_functions(
                source_id,
                source,
                initial_types,
                &self.vocabulary,
                &self.functions,
            ),
        };
        let module = match compiled {
            Ok(module) => module,
            Err(diagnostics) => return TypedExecution::failed(diagnostics),
        };
        let effects = entry_effects(&module);
        if let Some(declared) = declared {
            if !declared.grants(&effects) {
                let mut diagnostic = VmDiagnostic::error(
                    "E-CAP-003",
                    DiagnosticPhase::Verification,
                    format!(
                        "declared capabilities {declared} do not cover inferred requirements {effects}"
                    ),
                    None,
                );
                diagnostic.expected_effects = effects.clone();
                diagnostic.found_effects = declared.clone();
                return TypedExecution {
                    status: TypedExecutionStatus::Failed,
                    values: Vec::new(),
                    output: String::new(),
                    effects,
                    diagnostics: vec![diagnostic],
                };
            }
        }
        let missing = effects
            .0
            .iter()
            .filter(|requirement| !self.grants.0.iter().any(|grant| grant.covers(requirement)))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return TypedExecution {
                status: TypedExecutionStatus::AuthorizationRequired {
                    requirements: missing,
                },
                values: Vec::new(),
                output: String::new(),
                effects,
                diagnostics: Vec::new(),
            };
        }

        let new_functions = module
            .module
            .functions
            .iter()
            .filter(|(name, _)| {
                *name != &module.module.entry && !self.functions.contains_key(*name)
            })
            .map(|(name, function)| (name.clone(), function.clone()))
            .collect::<Vec<_>>();
        let mut handler = RuntimeCapabilities::default();
        let result = Interpreter::new(
            &module,
            &mut handler,
            InterpreterConfig {
                fuel,
                grants: self.grants.clone(),
            },
        )
        .execute(&mut self.stack);
        match result {
            Ok(()) => {
                for (name, function) in new_functions {
                    if !name.starts_with("lambda$") {
                        self.vocabulary
                            .insert(name.clone(), function.signature.clone());
                    }
                    self.functions.insert(name, function);
                }
                TypedExecution {
                    status: TypedExecutionStatus::Completed,
                    // A stack program may replace existing cells without changing
                    // depth, so a length delta is not a meaningful result ABI.
                    // Return the committed stack snapshot.
                    values: self.stack.clone(),
                    output: handler.output,
                    effects,
                    diagnostics: Vec::new(),
                }
            }
            Err(diagnostic) => TypedExecution {
                status: TypedExecutionStatus::Failed,
                values: Vec::new(),
                output: String::new(),
                effects,
                diagnostics: vec![diagnostic],
            },
        }
    }
}

fn entry_effects(module: &VerifiedModule) -> EffectSet {
    module
        .functions
        .get(&module.module.entry)
        .map(|function| function.inferred_effects.clone())
        .unwrap_or_default()
}

#[derive(Default)]
struct RuntimeCapabilities {
    output: String,
}

impl CapabilityHandler for &mut RuntimeCapabilities {
    fn request(
        &mut self,
        requirement: &CapabilityRequirement,
        arguments: Vec<TypedValue>,
        origin: &SourceOrigin,
    ) -> Result<Vec<TypedValue>, VmDiagnostic> {
        match requirement.capability {
            CapabilityKind::SessionEmit => {
                let [TypedValue::String(text)] = arguments.as_slice() else {
                    return Err(VmDiagnostic::error(
                        "E-HOST-001",
                        DiagnosticPhase::HostCall,
                        "session.emit requires one string",
                        Some(origin.clone()),
                    ));
                };
                self.output.push_str(text);
                Ok(vec![TypedValue::Unit])
            }
            _ => {
                let mut diagnostic = VmDiagnostic::error(
                    "E-HOST-002",
                    DiagnosticPhase::HostCall,
                    format!(
                        "capability {:?} is authorized but has no host binding",
                        requirement.capability
                    ),
                    Some(origin.clone()),
                );
                diagnostic.capability = Some(requirement.clone());
                Err(diagnostic)
            }
        }
    }
}

impl TypedExecution {
    fn failed(diagnostics: Vec<VmDiagnostic>) -> Self {
        Self {
            status: TypedExecutionStatus::Failed,
            values: Vec::new(),
            output: String::new(),
            effects: EffectSet::pure(),
            diagnostics,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lisp_say_compiles_directly_and_emits() {
        let mut runtime = TypedRuntime::new();
        let result = runtime.execute(
            ProgramLanguage::Lisp,
            "model-response.lisp",
            "(say \"hello\")",
            1_000,
        );
        assert_eq!(result.status, TypedExecutionStatus::Completed);
        assert_eq!(result.output, "hello");
        assert_eq!(result.values, vec![TypedValue::Unit]);
    }

    #[test]
    fn lisp_and_forth_share_one_typed_stack() {
        let mut runtime = TypedRuntime::new();
        let lisp = runtime.execute(ProgramLanguage::Lisp, "a.lisp", "(+ 2 3)", 1_000);
        assert_eq!(lisp.values, vec![TypedValue::Int(5)]);
        let forth = runtime.execute(ProgramLanguage::Forth, "b.forth", "2 *", 1_000);
        assert_eq!(forth.values, vec![TypedValue::Int(10)]);
        assert_eq!(runtime.stack(), &[TypedValue::Int(10)]);
    }

    #[test]
    fn forth_definition_persists_and_is_callable_from_lisp() {
        let mut runtime = TypedRuntime::new();
        let definition = runtime.execute(
            ProgramLanguage::Forth,
            "words.forth",
            ": square ( S int -- S int ! {} ) dup * ;",
            1_000,
        );
        assert_eq!(definition.status, TypedExecutionStatus::Completed);
        assert!(runtime.functions().contains_key("square"));
        let call = runtime.execute(ProgramLanguage::Lisp, "call.lisp", "(square 9)", 1_000);
        assert_eq!(call.status, TypedExecutionStatus::Completed);
        assert_eq!(call.values, vec![TypedValue::Int(81)]);
    }

    #[test]
    fn rejected_definition_does_not_enter_dictionary() {
        let mut runtime = TypedRuntime::new();
        let definition = runtime.execute(
            ProgramLanguage::Forth,
            "words.forth",
            ": dishonest ( S string -- S unit ! {} ) say ;",
            1_000,
        );
        assert_eq!(definition.status, TypedExecutionStatus::Failed);
        assert_eq!(definition.diagnostics[0].code, "E-CAP-001");
        assert!(!runtime.functions().contains_key("dishonest"));
    }

    #[test]
    fn lisp_definition_persists_and_is_callable_from_forth() {
        let mut runtime = TypedRuntime::new();
        let definition = runtime.execute(
            ProgramLanguage::Lisp,
            "words.lisp",
            "(define (triple (x : int)) (* x 3))",
            1_000,
        );
        assert_eq!(definition.status, TypedExecutionStatus::Completed);
        assert!(runtime.functions().contains_key("triple"));
        let call = runtime.execute(ProgramLanguage::Forth, "call.forth", "7 triple", 1_000);
        assert_eq!(call.status, TypedExecutionStatus::Completed);
        assert_eq!(call.values, vec![TypedValue::Int(21)]);
    }

    #[test]
    fn missing_capability_suspends_before_stack_mutation() {
        let mut runtime = TypedRuntime::new();
        let before = runtime.stack().to_vec();
        let result = runtime.execute(
            ProgramLanguage::Lisp,
            "memory.lisp",
            "(mem-store \"remember this\")",
            1_000,
        );
        assert!(matches!(
            result.status,
            TypedExecutionStatus::AuthorizationRequired { .. }
        ));
        assert_eq!(runtime.stack(), before);
    }

    #[test]
    fn runtime_failure_rolls_back_stack() {
        let mut runtime = TypedRuntime::new();
        runtime.execute(ProgramLanguage::Forth, "a.forth", "7", 1_000);
        let before = runtime.stack().to_vec();
        let result = runtime.execute(ProgramLanguage::Forth, "b.forth", "0 /", 1_000);
        assert_eq!(result.status, TypedExecutionStatus::Failed);
        assert_eq!(runtime.stack(), before);
    }

    #[test]
    fn closure_capabilities_compose_into_the_caller() {
        let mut runtime = TypedRuntime::new();
        let result = runtime.execute(
            ProgramLanguage::Lisp,
            "closure.lisp",
            "(let ((store (lambda ((text : string)) (mem-store text)))) (store \"memo\"))",
            1_000,
        );
        let TypedExecutionStatus::AuthorizationRequired { requirements } = result.status else {
            panic!("expected authorization request: {:?}", result.diagnostics);
        };
        assert_eq!(requirements.len(), 1);
        assert_eq!(requirements[0].capability, CapabilityKind::MemoryWrite);
        assert!(runtime.stack().is_empty());
    }

    #[test]
    fn declaration_cannot_hide_an_inferred_capability() {
        let mut runtime = TypedRuntime::new();
        let result = runtime.execute_with_declaration(
            ProgramLanguage::Lisp,
            "response.lisp",
            "(say \"hidden effect\")",
            1_000,
            Some(&EffectSet::pure()),
        );
        assert_eq!(result.status, TypedExecutionStatus::Failed);
        assert_eq!(result.diagnostics[0].code, "E-CAP-003");
        assert!(runtime.stack().is_empty());
    }

    #[test]
    fn lisp_while_is_metered_and_transactional() {
        let mut runtime = TypedRuntime::new();
        runtime.execute(ProgramLanguage::Lisp, "seed.lisp", "7", 1_000);
        let before = runtime.stack().to_vec();
        let result = runtime.execute(ProgramLanguage::Lisp, "loop.lisp", "(while true 1)", 20);
        assert_eq!(result.status, TypedExecutionStatus::Failed);
        assert_eq!(result.diagnostics[0].code, "E-LIMIT-001");
        assert_eq!(runtime.stack(), before);
    }

    #[test]
    fn lisp_while_composes_body_capabilities_even_when_condition_is_false() {
        let mut runtime = TypedRuntime::new();
        let result = runtime.execute(
            ProgramLanguage::Lisp,
            "loop.lisp",
            "(while false (mem-store \"never runs\"))",
            1_000,
        );
        let TypedExecutionStatus::AuthorizationRequired { requirements } = result.status else {
            panic!("expected authorization request");
        };
        assert_eq!(requirements[0].capability, CapabilityKind::MemoryWrite);
    }
}
