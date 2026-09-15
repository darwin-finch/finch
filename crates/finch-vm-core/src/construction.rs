//! Syntax-neutral semantic construction and opaque compiler phase types.
//!
//! Frontends parse source into their own AST, then call [`SemanticBuilder`]
//! instead of manufacturing verified types, effect certificates, or executable
//! modules. Only [`ModuleVerified`] may be submitted to execution.

use super::diagnostic::{DiagnosticPhase, SourceOrigin, VmDiagnostic};
use super::effects::EffectSet;
use super::ir::{BasicBlock, BlockId, Function, Instruction, LocatedInstruction, Module};
use super::signature::{ControlEffect, StackRow, StackSignature, SuspensionSignature};
use super::types::Type;
use super::verifier::{VerifiedFunction, VerifiedModule, Verifier, Vocabulary};
use super::VM_TYPE_SYSTEM_VERSION;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Deref;

/// Version of the frontend-facing semantic-construction protocol.
///
/// This is independent of [`VM_TYPE_SYSTEM_VERSION`]: protocol additions are
/// new builder operations, not a silent reinterpretation of typed IR.
pub const SEMANTIC_CONSTRUCTION_VERSION: u32 = 1;

/// A frontend syntax tree that has not yet entered semantic construction.
///
/// `Ast` remains frontend-private. The wrapper only records that parsing
/// finished before any builder call or IR emission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed<Ast> {
    source_id: String,
    ast: Ast,
}

impl<Ast> Parsed<Ast> {
    /// Wrap a frontend AST as the parse-complete phase.
    pub fn from_frontend(source_id: impl Into<String>, ast: Ast) -> Self {
        Self {
            source_id: source_id.into(),
            ast,
        }
    }

    /// Source identity retained from the parse boundary.
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    /// Borrow the frontend syntax tree.
    pub fn ast(&self) -> &Ast {
        &self.ast
    }

    /// Consume the wrapper and return the frontend syntax tree.
    pub fn into_ast(self) -> Ast {
        self.ast
    }
}

/// Syntax-neutral module under construction. Functions may still be added.
#[derive(Debug, Clone, PartialEq)]
pub struct Elaborated {
    name: String,
    entry: String,
    functions: BTreeMap<String, Function>,
}

impl Elaborated {
    /// Start an elaborated module with no functions.
    pub fn new(name: impl Into<String>, entry: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            entry: entry.into(),
            functions: BTreeMap::new(),
        }
    }

    /// Record a locally certified function. The function body is quarantined
    /// until the module is sealed and independently verified.
    pub fn add_function(&mut self, certified: FunctionCertified) {
        let function = certified.into_function();
        self.functions.insert(function.name.clone(), function);
    }

    /// Record an already-lowered dependency that this module may call.
    pub fn add_linked_function(&mut self, function: Function) {
        self.functions.insert(function.name.clone(), function);
    }

    /// Freeze declarations, exports, and content identity.
    pub fn seal(self) -> ModuleSealed {
        ModuleSealed {
            module: Module {
                version: VM_TYPE_SYSTEM_VERSION,
                name: self.name,
                entry: self.entry,
                functions: self.functions,
            },
        }
    }
}

/// A function whose local structural, type, stack, and dependency checks passed.
///
/// This permits quarantined downstream compilation and never execution.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionCertified {
    function: Function,
    facts: VerifiedFunction,
}

impl FunctionCertified {
    /// Local verification of one function against the functions it may call.
    pub fn certify(
        function: Function,
        vocabulary: &Vocabulary,
        module_functions: &BTreeMap<String, Function>,
    ) -> Result<Self, Vec<VmDiagnostic>> {
        let facts = Verifier::new(vocabulary).certify_function(&function, module_functions)?;
        Ok(Self { function, facts })
    }

    /// The certified function IR.
    pub fn function(&self) -> &Function {
        &self.function
    }

    /// Verifier facts for this function, never a module certificate.
    pub fn facts(&self) -> &VerifiedFunction {
        &self.facts
    }

    fn into_function(self) -> Function {
        self.function
    }
}

/// A closed declaration graph with frozen exports. Not executable.
#[derive(Debug, Clone, PartialEq)]
pub struct ModuleSealed {
    module: Module,
}

impl ModuleSealed {
    /// Independent composition and security verification.
    pub fn verify(self, vocabulary: &Vocabulary) -> Result<ModuleVerified, Vec<VmDiagnostic>> {
        let verified = Verifier::new(vocabulary).verify(self.module)?;
        Ok(ModuleVerified { inner: verified })
    }

    /// The sealed but unverified module IR.
    pub fn module(&self) -> &Module {
        &self.module
    }
}

/// The only compiler phase permitted to reach execution.
///
/// Construction is private: a value of this type is a verifier certificate,
/// not a flag on a raw module.
#[derive(Debug, Clone, PartialEq)]
pub struct ModuleVerified {
    inner: VerifiedModule,
}

impl ModuleVerified {
    /// The independently verified module retained for interpretation.
    pub fn as_verified(&self) -> &VerifiedModule {
        &self.inner
    }

    /// Unwrap the serializable verified module used by checkpoints.
    pub fn into_verified(self) -> VerifiedModule {
        self.inner
    }
}

impl Deref for ModuleVerified {
    type Target = VerifiedModule;

    fn deref(&self) -> &VerifiedModule {
        &self.inner
    }
}

/// A lexical binding introduced during semantic construction.
#[derive(Debug, Clone)]
pub enum SemanticBinding {
    /// Function-local slot.
    Local { index: u32, ty: Type },
    /// Closure capture slot.
    Capture { index: u32, ty: Type },
}

impl SemanticBinding {
    /// Type of the bound value.
    pub fn ty(&self) -> &Type {
        match self {
            Self::Local { ty, .. } | Self::Capture { ty, .. } => ty,
        }
    }
}

/// A lexically active structured loop. Compiler metadata only: emitted IR
/// still contains ordinary typed blocks and explicit jump edges.
#[derive(Debug, Clone)]
pub struct LoopBinding {
    /// Optional source label used by named break/continue.
    pub label: Option<String>,
    /// Loop header block.
    pub header: BlockId,
    /// Loop exit block.
    pub exit: BlockId,
    /// Stack row live at the loop header.
    pub stack: Vec<Type>,
}

/// Then/else/merge blocks created for a boolean branch.
#[derive(Debug, Clone)]
pub struct BoolBranch {
    /// Taken when the condition is true.
    pub then_block: BlockId,
    /// Taken when the condition is false.
    pub else_block: BlockId,
    /// Join of the two live alternatives.
    pub merge_block: BlockId,
    /// Stack row supplied to both branches after consuming the boolean.
    pub entry_stack: Vec<Type>,
}

/// Syntax-neutral IR constructor used by every frontend.
///
/// Frontends may retain their own name-resolution tables, but they emit
/// instructions, blocks, and stack rows only through this type.
pub struct SemanticBuilder {
    /// Function name being constructed.
    pub name: String,
    /// Basic blocks in construction order.
    pub blocks: BTreeMap<BlockId, BasicBlock>,
    /// Block currently receiving instructions.
    pub current: BlockId,
    next_block: BlockId,
    /// Local slot types.
    pub locals: Vec<Type>,
    /// Capture slot types.
    pub captures: Vec<Type>,
    /// Nested lexical scopes.
    pub scopes: Vec<HashMap<String, SemanticBinding>>,
    /// Active structured loops.
    pub loops: Vec<LoopBinding>,
    /// Live stack row.
    pub stack: Vec<Type>,
    input: Vec<Type>,
    /// Accumulated effect requirements. Grants and approval policy are not
    /// decided here.
    pub effects: EffectSet,
    /// Yield contract, when this callable suspends.
    pub suspension: Option<SuspensionSignature>,
    /// Enclosing definition result contract for early-result propagation.
    pub return_result: Option<(Type, Type)>,
}

impl SemanticBuilder {
    /// Start a function body with `input` already on the stack.
    pub fn new(name: impl Into<String>, input: Vec<Type>) -> Self {
        Self {
            name: name.into(),
            blocks: BTreeMap::from([(
                0,
                BasicBlock {
                    id: 0,
                    instructions: Vec::new(),
                },
            )]),
            current: 0,
            next_block: 1,
            locals: Vec::new(),
            captures: Vec::new(),
            scopes: vec![HashMap::new()],
            loops: Vec::new(),
            stack: input.clone(),
            input,
            effects: EffectSet::pure(),
            suspension: None,
            return_result: None,
        }
    }

    /// Append an instruction unless the current block already terminated.
    pub fn emit(&mut self, instruction: Instruction, origin: SourceOrigin) {
        let block = self
            .blocks
            .get_mut(&self.current)
            .expect("current block exists");
        // Structured loop exits terminate their current edge. Continue
        // lowering only to close surrounding forms and type-check their live
        // alternatives; never append a synthetic merge instruction after a
        // terminator.
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
    }

    /// Merge an incoming yield contract into this callable.
    pub fn merge_suspension(
        &mut self,
        incoming: Option<&SuspensionSignature>,
        origin: &SourceOrigin,
    ) -> Result<(), Vec<VmDiagnostic>> {
        let Some(incoming) = incoming else {
            return Ok(());
        };
        if let Some(current) = &self.suspension {
            if current != incoming {
                return Err(vec![VmDiagnostic::error(
                    "E-YIELD-004",
                    DiagnosticPhase::TypeInference,
                    format!(
                        "one callable cannot yield incompatible types {} and {}",
                        current.yield_type, incoming.yield_type
                    ),
                    Some(origin.clone()),
                )]);
            }
        } else {
            self.suspension = Some(incoming.clone());
        }
        Ok(())
    }

    /// Allocate a fresh basic block.
    pub fn new_block(&mut self) -> BlockId {
        let id = self.next_block;
        self.next_block += 1;
        self.blocks.insert(
            id,
            BasicBlock {
                id,
                instructions: Vec::new(),
            },
        );
        id
    }

    /// Continue lowering in `block` with `stack` as the live row.
    pub fn switch_to(&mut self, block: BlockId, stack: Vec<Type>) {
        self.current = block;
        self.stack = stack;
    }

    /// Allocate a local slot of `ty`.
    pub fn allocate_local(&mut self, ty: Type) -> u32 {
        let index = self.locals.len() as u32;
        self.locals.push(ty);
        index
    }

    /// Resolve a name through nested scopes, innermost first.
    pub fn resolve(&self, name: &str) -> Option<SemanticBinding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
    }

    /// Visible bindings, innermost first then sorted by name within a scope.
    pub fn visible_bindings(&self) -> Vec<(String, SemanticBinding)> {
        let mut seen = HashSet::new();
        let mut bindings = Vec::new();
        for scope in self.scopes.iter().rev() {
            for (name, binding) in scope {
                if seen.insert(name.clone()) {
                    bindings.push((name.clone(), binding.clone()));
                }
            }
        }
        bindings.sort_by(|left, right| left.0.cmp(&right.0));
        bindings
    }

    /// Consume a live `bool` and emit a then/else/merge branch skeleton.
    pub fn start_bool_branch(
        &mut self,
        origin: SourceOrigin,
    ) -> Result<BoolBranch, Vec<VmDiagnostic>> {
        let condition = self.stack.pop().ok_or_else(|| {
            vec![VmDiagnostic::error(
                "E-STACK-001",
                DiagnosticPhase::TypeInference,
                "boolean branch requires a condition",
                Some(origin.clone()),
            )]
        })?;
        if condition != Type::Bool {
            return Err(vec![VmDiagnostic::type_mismatch(
                Type::Bool,
                condition,
                Some(origin),
            )]);
        }
        let entry_stack = self.stack.clone();
        let then_block = self.new_block();
        let else_block = self.new_block();
        let merge_block = self.new_block();
        self.emit(
            Instruction::Branch {
                then_block,
                else_block,
            },
            origin,
        );
        Ok(BoolBranch {
            then_block,
            else_block,
            merge_block,
            entry_stack,
        })
    }

    /// Jump from the current alternative to `branch.merge_block`.
    pub fn jump_to_merge(&mut self, branch: &BoolBranch, origin: SourceOrigin) {
        self.emit(
            Instruction::Jump {
                target: branch.merge_block,
            },
            origin,
        );
    }

    /// Finish a function with the live output row.
    pub fn finish(self, output: Vec<Type>) -> Function {
        Function {
            name: self.name,
            documentation: None,
            signature: StackSignature {
                type_parameters: Vec::new(),
                input: StackRow::polymorphic("S", self.input),
                output: StackRow::polymorphic("S", output),
                effects: self.effects,
                control: if self.suspension.is_some() {
                    ControlEffect::MaySuspend
                } else {
                    ControlEffect::Returns
                },
                suspension: self.suspension,
            },
            locals: self.locals,
            captures: self.captures,
            entry: 0,
            blocks: self.blocks,
        }
    }

    /// Finish with an explicit closed stack signature, as Co-Forth definitions do.
    pub fn finish_closed(
        self,
        input: Vec<Type>,
        output: Vec<Type>,
        documentation: Option<String>,
    ) -> Function {
        Function {
            name: self.name,
            documentation,
            signature: StackSignature {
                type_parameters: Vec::new(),
                input: StackRow::closed(input),
                output: StackRow::closed(output),
                effects: self.effects,
                control: if self.suspension.is_some() {
                    ControlEffect::MaySuspend
                } else {
                    ControlEffect::Returns
                },
                suspension: self.suspension,
            },
            locals: self.locals,
            captures: self.captures,
            entry: 0,
            blocks: self.blocks,
        }
    }
}

/// Seal a complete function map and independently verify it.
pub fn certify_module(
    name: impl Into<String>,
    entry: impl Into<String>,
    functions: BTreeMap<String, Function>,
    vocabulary: &Vocabulary,
) -> Result<ModuleVerified, Vec<VmDiagnostic>> {
    let mut elaborated = Elaborated::new(name, entry);
    for function in functions.into_values() {
        elaborated.add_linked_function(function);
    }
    elaborated.seal().verify(vocabulary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core_vocabulary, Instruction, TypedValue};

    fn generated(word: &str) -> SourceOrigin {
        SourceOrigin::generated(word)
    }

    #[test]
    fn parsed_phase_preserves_frontend_ast_and_source_identity() {
        let parsed = Parsed::from_frontend("example.lisp", vec!["(say \"hi\")"]);
        assert_eq!(parsed.source_id(), "example.lisp");
        assert_eq!(parsed.ast(), &vec!["(say \"hi\")"]);
        assert_eq!(parsed.into_ast(), vec!["(say \"hi\")"]);
    }

    #[test]
    fn bool_branch_is_one_shared_semantic_node() {
        let mut builder = SemanticBuilder::new("main", Vec::new());
        builder.stack.push(Type::Bool);
        builder.emit(
            Instruction::Constant {
                value: TypedValue::Bool(true),
            },
            generated("true"),
        );
        let branch = builder.start_bool_branch(generated("if")).unwrap();
        builder.switch_to(branch.then_block, branch.entry_stack.clone());
        builder.stack.push(Type::Int);
        builder.emit(
            Instruction::Constant {
                value: TypedValue::Int(1),
            },
            generated("then"),
        );
        let then_stack = builder.stack.clone();
        builder.jump_to_merge(&branch, generated("then"));
        builder.switch_to(branch.else_block, branch.entry_stack.clone());
        builder.stack.push(Type::Int);
        builder.emit(
            Instruction::Constant {
                value: TypedValue::Int(0),
            },
            generated("else"),
        );
        builder.jump_to_merge(&branch, generated("else"));
        builder.switch_to(branch.merge_block, then_stack);
        builder.emit(Instruction::Return, generated("return"));
        let function = builder.finish(vec![Type::Int]);
        let mut functions = BTreeMap::new();
        functions.insert(function.name.clone(), function);
        let verified = certify_module("branch", "main", functions, &core_vocabulary()).unwrap();
        assert_eq!(verified.as_verified().module.entry, "main");
        assert!(verified.functions.contains_key("main"));
    }

    #[test]
    fn unverified_ir_cannot_become_module_verified_without_certify() {
        let function = SemanticBuilder::new("main", Vec::new()).finish(Vec::new());
        let mut functions: BTreeMap<String, Function> = BTreeMap::new();
        functions.insert("main".into(), function);
        // Missing Return terminator: sealing succeeds, verification must not.
        let sealed = {
            let mut elaborated = Elaborated::new("broken", "main");
            for function in functions.into_values() {
                elaborated.add_linked_function(function);
            }
            elaborated.seal()
        };
        let errors = sealed.verify(&core_vocabulary()).expect_err(
            "ModuleVerified must not be constructible from IR that failed independent verification",
        );
        assert!(
            errors.iter().any(|error| error.code.starts_with("E-")),
            "unverified IR must produce a diagnostic rather than a ModuleVerified certificate: {errors:?}"
        );
    }

    #[test]
    fn function_certified_is_not_a_module_certificate() {
        let mut builder = SemanticBuilder::new("id", vec![Type::Int]);
        builder.emit(Instruction::Return, generated("return"));
        let function = builder.finish(vec![Type::Int]);
        let certified =
            FunctionCertified::certify(function.clone(), &core_vocabulary(), &BTreeMap::new())
                .expect("identity function should certify locally");
        assert_eq!(certified.function().name, "id");
        assert_eq!(certified.facts().name, "id");
        let mut elaborated = Elaborated::new("id-mod", "id");
        elaborated.add_function(certified);
        let verified = elaborated.seal().verify(&core_vocabulary()).unwrap();
        assert_eq!(verified.as_verified().module.entry, "id");
    }
}
