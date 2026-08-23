use crate::lisp::Val;
use crate::vm::diagnostic::{
    DiagnosticPhase, SourceLanguage, SourceOrigin, SourceSpan, VmDiagnostic,
};
use crate::vm::effects::{CapabilityKind, CapabilityRequirement, EffectSet, ResourceSelector};
use crate::vm::interpreter::UiOperation;
use crate::vm::ir::{BasicBlock, BlockId, Function, Instruction, LocatedInstruction, Module};
use crate::vm::signature::{ControlEffect, StackRow, StackSignature};
use crate::vm::types::{Type, TypedValue};
use crate::vm::verifier::{apply_signature_types, VerifiedModule, Verifier, Vocabulary};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};

#[derive(Debug, Clone)]
enum Binding {
    Local { index: u32, ty: Type },
    Capture { index: u32, ty: Type },
}

/// A lexically active structured loop.  It is compiler metadata only: the
/// emitted IR still contains ordinary typed blocks and explicit jump edges.
#[derive(Debug, Clone)]
struct LoopBinding {
    label: Option<String>,
    header: BlockId,
    exit: BlockId,
    stack: Vec<Type>,
}

impl Binding {
    fn ty(&self) -> &Type {
        match self {
            Self::Local { ty, .. } | Self::Capture { ty, .. } => ty,
        }
    }
}

struct FunctionBuilder {
    name: String,
    blocks: BTreeMap<BlockId, BasicBlock>,
    current: BlockId,
    next_block: BlockId,
    locals: Vec<Type>,
    captures: Vec<Type>,
    scopes: Vec<HashMap<String, Binding>>,
    loops: Vec<LoopBinding>,
    stack: Vec<Type>,
    input: Vec<Type>,
    effects: EffectSet,
}

impl FunctionBuilder {
    fn new(name: impl Into<String>, input: Vec<Type>) -> Self {
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
        }
    }

    fn emit(&mut self, instruction: Instruction, origin: SourceOrigin) {
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

    fn new_block(&mut self) -> BlockId {
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

    fn switch_to(&mut self, block: BlockId, stack: Vec<Type>) {
        self.current = block;
        self.stack = stack;
    }

    fn allocate_local(&mut self, ty: Type) -> u32 {
        let index = self.locals.len() as u32;
        self.locals.push(ty);
        index
    }

    fn resolve(&self, name: &str) -> Option<Binding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
    }

    fn visible_bindings(&self) -> Vec<(String, Binding)> {
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

    fn finish(self, output: Vec<Type>) -> Function {
        Function {
            name: self.name,
            documentation: None,
            signature: StackSignature {
                type_parameters: Vec::new(),
                input: StackRow::polymorphic("S", self.input),
                output: StackRow::polymorphic("S", output),
                effects: self.effects,
                control: ControlEffect::Returns,
            },
            locals: self.locals,
            captures: self.captures,
            entry: 0,
            blocks: self.blocks,
        }
    }
}

struct Compiler<'a> {
    source_id: &'a str,
    source: &'a str,
    vocabulary: Vocabulary,
    /// Explicitly typed definitions are registered before their bodies compile.
    /// This makes recursive and mutually-recursive calls visible without
    /// introducing an untyped placeholder into the shared vocabulary.
    predeclared: BTreeMap<String, StackSignature>,
    functions: BTreeMap<String, Function>,
    next_lambda: u64,
}

/// Parse and compile Finch Lisp directly into the common typed stack IR. No
/// Forth source text is generated or reparsed.
pub fn compile_lisp(
    source_id: &str,
    source: &str,
    initial_stack: Vec<Type>,
    vocabulary: &Vocabulary,
) -> Result<VerifiedModule, Vec<VmDiagnostic>> {
    compile_lisp_with_functions(
        source_id,
        source,
        initial_stack,
        vocabulary,
        &BTreeMap::new(),
    )
}

pub fn compile_lisp_with_functions(
    source_id: &str,
    source: &str,
    initial_stack: Vec<Type>,
    vocabulary: &Vocabulary,
    linked_functions: &BTreeMap<String, Function>,
) -> Result<VerifiedModule, Vec<VmDiagnostic>> {
    let parsed_expressions = crate::lisp::reader::parse_str(source).map_err(|error| {
        vec![VmDiagnostic::error(
            "E-READ-002",
            DiagnosticPhase::Reader,
            error.to_string(),
            Some(source_origin(source_id, source, "<reader>")),
        )]
    })?;
    let macros = collect_template_macros(source_id, source, &parsed_expressions)?;
    let mut expansion_budget = 128;
    let expressions = parsed_expressions
        .iter()
        .filter(|expression| !is_macro_definition(expression))
        .map(|expression| {
            expand_template_macros(source_id, source, expression, &macros, &mut expansion_budget)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut compiler = Compiler {
        source_id,
        source,
        vocabulary: vocabulary.clone(),
        predeclared: BTreeMap::new(),
        functions: BTreeMap::new(),
        next_lambda: 0,
    };
    for expression in &expressions {
        if is_definition(expression) {
            compiler.predeclare_definition(expression)?;
        }
    }
    let mut executable = Vec::new();
    for expression in &expressions {
        if is_definition(expression) {
            compiler.compile_definition(expression)?;
        } else {
            executable.push(expression);
        }
    }
    let mut builder = FunctionBuilder::new("main", initial_stack);
    if executable.is_empty() {
        if expressions.is_empty() {
            builder.stack.push(Type::Unit);
            builder.emit(
                Instruction::Constant {
                    value: TypedValue::Unit,
                },
                compiler.origin("nil"),
            );
        }
    } else {
        for (index, expression) in executable.iter().enumerate() {
            compiler.compile_expression(expression, &mut builder)?;
            if index + 1 != executable.len() {
                builder.stack.pop();
                builder.emit(Instruction::Drop, compiler.origin("begin"));
            }
        }
    }
    // Co-Forth effects and loops are stack-neutral. Lisp retains an internal
    // `unit` expression so `(begin (say "a") ...)` remains well-formed, but
    // a top-level unit must not become a synthetic persistent stack value.
    // Drop it at the program boundary to preserve one shared runtime stack
    // semantics across the two source forms.
    if builder.stack.last() == Some(&Type::Unit) {
        builder.stack.pop();
        builder.emit(Instruction::Drop, compiler.origin("top-level-unit"));
    }
    let output = builder.stack.clone();
    builder.emit(Instruction::Return, compiler.origin("<return>"));
    let main = builder.finish(output);
    compiler.functions.insert("main".into(), main);
    let module = Module {
        version: crate::vm::VM_TYPE_SYSTEM_VERSION,
        name: source_id.to_string(),
        entry: "main".into(),
        functions: {
            let mut functions = linked_functions.clone();
            functions.extend(compiler.functions);
            functions
        },
    };
    Verifier::new(vocabulary).verify(module)
}

/// A deliberately restricted source macro.  It is syntax-only: expansion
/// happens before type checking, cannot run code, cannot request a capability,
/// and is bounded globally for one submission.  Templates are capture-free
/// because they are forbidden from introducing a binding form; substituted
/// caller syntax keeps its original lexical bindings.
#[derive(Debug, Clone)]
struct TemplateMacro {
    parameters: Vec<String>,
    template: Val,
}

fn is_macro_definition(expression: &Val) -> bool {
    matches!(
        expression,
        Val::List(items) if matches!(items.first(), Some(Val::Symbol(name)) if name == "define-syntax")
    )
}

fn collect_template_macros(
    source_id: &str,
    source: &str,
    expressions: &[Val],
) -> Result<HashMap<String, TemplateMacro>, Vec<VmDiagnostic>> {
    let mut macros = HashMap::new();
    for expression in expressions {
        if !is_macro_definition(expression) {
            continue;
        }
        let Val::List(items) = expression else {
            unreachable!("macro predicate only accepts lists");
        };
        if items.len() != 3 {
            return Err(vec![macro_error(
                source_id,
                source,
                "E-LISP-MACRO-001",
                "define-syntax requires exactly a header and one template",
            )]);
        }
        let Val::List(header) = &items[1] else {
            return Err(vec![macro_error(
                source_id,
                source,
                "E-LISP-MACRO-002",
                "define-syntax header must be (name parameter ...)",
            )]);
        };
        let Some(Val::Symbol(name)) = header.first() else {
            return Err(vec![macro_error(
                source_id,
                source,
                "E-LISP-MACRO-002",
                "define-syntax requires a macro name",
            )]);
        };
        let mut parameters = Vec::with_capacity(header.len().saturating_sub(1));
        for parameter in &header[1..] {
            let Val::Symbol(parameter) = parameter else {
                return Err(vec![macro_error(
                    source_id,
                    source,
                    "E-LISP-MACRO-002",
                    "define-syntax parameters must be symbols",
                )]);
            };
            if parameters.iter().any(|existing| existing == parameter) {
                return Err(vec![macro_error(
                    source_id,
                    source,
                    "E-LISP-MACRO-002",
                    format!("duplicate macro parameter '{parameter}'"),
                )]);
            }
            parameters.push(parameter.clone());
        }
        reject_template_binding_forms(source_id, source, &items[2])?;
        if macros
            .insert(
                name.clone(),
                TemplateMacro {
                    parameters,
                    template: items[2].clone(),
                },
            )
            .is_some()
        {
            return Err(vec![macro_error(
                source_id,
                source,
                "E-LISP-MACRO-003",
                format!("macro '{name}' is already defined"),
            )]);
        }
    }
    Ok(macros)
}

fn reject_template_binding_forms(
    source_id: &str,
    source: &str,
    template: &Val,
) -> Result<(), Vec<VmDiagnostic>> {
    match template {
        Val::List(items) => {
            if matches!(items.first(), Some(Val::Symbol(name)) if matches!(name.as_str(), "let" | "lambda" | "define" | "define-syntax")) {
                return Err(vec![macro_error(
                    source_id,
                    source,
                    "E-LISP-MACRO-004",
                    "bounded syntax templates cannot introduce lexical bindings; use a function or compose an existing binding form in caller syntax",
                )]);
            }
            for item in items {
                reject_template_binding_forms(source_id, source, item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn expand_template_macros(
    source_id: &str,
    source: &str,
    expression: &Val,
    macros: &HashMap<String, TemplateMacro>,
    budget: &mut u16,
) -> Result<Val, Vec<VmDiagnostic>> {
    let Val::List(items) = expression else {
        return Ok(expression.clone());
    };
    // Data is not syntax.  In particular, a quoted list that happens to
    // start with a macro name must remain a literal symbol/list value.
    if matches!(items.first(), Some(Val::Symbol(name)) if name == "quote") {
        return Ok(expression.clone());
    }
    if let Some(Val::Symbol(name)) = items.first() {
        if let Some(definition) = macros.get(name) {
            if *budget == 0 {
                return Err(vec![macro_error(
                    source_id,
                    source,
                    "E-LISP-MACRO-005",
                    "macro expansion exceeded the per-submission limit of 128 forms",
                )]);
            }
            let arguments = &items[1..];
            if arguments.len() != definition.parameters.len() {
                return Err(vec![macro_error(
                    source_id,
                    source,
                    "E-LISP-MACRO-006",
                    format!(
                        "macro '{name}' expects {} arguments, received {}",
                        definition.parameters.len(),
                        arguments.len()
                    ),
                )]);
            }
            *budget -= 1;
            let bindings = definition
                .parameters
                .iter()
                .cloned()
                .zip(arguments.iter().cloned())
                .collect::<HashMap<_, _>>();
            let expanded = substitute_macro_template(&definition.template, &bindings);
            return expand_template_macros(source_id, source, &expanded, macros, budget);
        }
    }
    items
        .iter()
        .map(|item| expand_template_macros(source_id, source, item, macros, budget))
        .collect::<Result<Vec<_>, _>>()
        .map(Val::List)
}

fn substitute_macro_template(template: &Val, bindings: &HashMap<String, Val>) -> Val {
    match template {
        Val::Symbol(name) => bindings
            .get(name)
            .cloned()
            .unwrap_or_else(|| template.clone()),
        Val::List(items) => Val::List(
            items
                .iter()
                .map(|item| substitute_macro_template(item, bindings))
                .collect(),
        ),
        _ => template.clone(),
    }
}

fn macro_error(
    source_id: &str,
    source: &str,
    code: &'static str,
    message: impl Into<String>,
) -> VmDiagnostic {
    VmDiagnostic::error(
        code,
        DiagnosticPhase::MacroExpansion,
        message,
        Some(source_origin(source_id, source, "define-syntax")),
    )
}

impl Compiler<'_> {
    fn origin(&self, word: impl Into<String>) -> SourceOrigin {
        source_origin(self.source_id, self.source, word)
    }

    fn compile_expression(
        &mut self,
        expression: &Val,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let origin = self.origin(expression.to_string());
        let ty = match expression {
            Val::Nil => {
                builder.emit(
                    Instruction::Constant {
                        value: TypedValue::Unit,
                    },
                    origin,
                );
                Type::Unit
            }
            Val::Bool(value) => {
                builder.emit(
                    Instruction::Constant {
                        value: TypedValue::Bool(*value),
                    },
                    origin,
                );
                Type::Bool
            }
            Val::Int(value) => {
                builder.emit(
                    Instruction::Constant {
                        value: TypedValue::Int(*value),
                    },
                    origin,
                );
                Type::Int
            }
            Val::Float(value) => {
                builder.emit(
                    Instruction::Constant {
                        value: TypedValue::Float(*value),
                    },
                    origin,
                );
                Type::Float
            }
            Val::Str(value) => {
                builder.emit(
                    Instruction::Constant {
                        value: TypedValue::String(value.clone()),
                    },
                    origin,
                );
                Type::String
            }
            Val::Symbol(name) => {
                let binding = builder.resolve(name).ok_or_else(|| {
                    vec![VmDiagnostic::error(
                        "E-NAME-001",
                        DiagnosticPhase::NameResolution,
                        format!("unbound Lisp name '{name}'"),
                        Some(origin.clone()),
                    )]
                })?;
                let instruction = match binding.clone() {
                    Binding::Local { index, .. } => Instruction::LocalGet { index },
                    Binding::Capture { index, .. } => Instruction::CaptureGet { index },
                };
                builder.emit(instruction, origin);
                binding.ty().clone()
            }
            Val::List(items) if items.is_empty() => {
                builder.emit(
                    Instruction::Constant {
                        value: TypedValue::Unit,
                    },
                    origin,
                );
                Type::Unit
            }
            Val::List(items) => self.compile_list(items, builder)?,
            _ => {
                return Err(vec![VmDiagnostic::error(
                    "E-TYPE-005",
                    DiagnosticPhase::TypeInference,
                    format!(
                        "{} is not supported by typed Finch Lisp",
                        expression.type_name()
                    ),
                    Some(origin),
                )]);
            }
        };
        builder.stack.push(ty.clone());
        Ok(ty)
    }

    fn compile_definition(&mut self, expression: &Val) -> Result<(), Vec<VmDiagnostic>> {
        let definition = parse_definition(expression)?;
        let name = definition.name;
        if name == "main"
            || self.functions.contains_key(name)
            || (self.vocabulary.contains_key(name) && !self.predeclared.contains_key(name))
        {
            return Err(vec![self.error(
                "E-LISP-DEF-004",
                format!("function '{name}' is already defined"),
            )]);
        }
        let parameters = definition.parameters;
        let arguments = parameters
            .iter()
            .map(|(_, ty)| ty.clone())
            .collect::<Vec<_>>();
        let mut child = FunctionBuilder::new(name, arguments);
        for (parameter_name, ty) in &parameters {
            let index = child.allocate_local(ty.clone());
            child.scopes[0].insert(
                parameter_name.clone(),
                Binding::Local {
                    index,
                    ty: ty.clone(),
                },
            );
        }
        for parameter_index in (0..parameters.len()).rev() {
            child.stack.pop();
            child.emit(
                Instruction::LocalSet {
                    index: parameter_index as u32,
                },
                self.origin("define-parameter"),
            );
        }
        let result_type = self.compile_begin(definition.body, &mut child)?;
        if let Some(expected) = &definition.return_type {
            if result_type != *expected {
                return Err(vec![self.error(
                    "E-LISP-DEF-005",
                    format!(
                        "function '{name}' declares return type {expected}, but its body returns {result_type}"
                    ),
                )]);
            }
        }
        // A return annotation is also the marker that this definition was
        // predeclared for recursive calls.  An omitted `! (...)` therefore
        // means an explicit pure bound, preserving the old safe default.
        if definition.return_type.is_some() {
            let declared_effects = definition
                .declared_effects
                .clone()
                .unwrap_or_else(EffectSet::pure);
            if !declared_effects.grants(&child.effects) {
                return Err(vec![self.error(
                    "E-LISP-DEF-007",
                    format!(
                        "function '{name}' declares effects {declared_effects}, but requires {}",
                        child.effects
                    ),
                )]);
            }
        }
        child.stack.push(result_type);
        child.emit(Instruction::Return, self.origin("define-return"));
        let output = child.stack.clone();
        let mut function = child.finish(output);
        function.documentation = definition.documentation.map(str::to_owned);
        self.vocabulary
            .insert(name.to_string(), function.signature.clone());
        self.functions.insert(name.to_string(), function);
        Ok(())
    }

    fn predeclare_definition(&mut self, expression: &Val) -> Result<(), Vec<VmDiagnostic>> {
        let definition = parse_definition(expression)?;
        let Some(result) = definition.return_type else {
            // A recursive definition needs an input/output contract before we
            // compile its body.  Without it the eventual failure looks like
            // an unrelated unknown-word error, which is especially unhelpful
            // to a provider repairing a wire program.
            if definition
                .body
                .iter()
                .any(|body| expression_mentions_symbol(body, definition.name))
            {
                return Err(vec![self.error(
                    "E-LISP-DEF-008",
                    format!(
                        "recursive function '{}' requires a return type: \
                         (define ({} (arg : type) ...) : result-type body...)",
                        definition.name, definition.name
                    ),
                )]);
            }
            return Ok(());
        };
        if definition.name == "main"
            || self.vocabulary.contains_key(definition.name)
            || self.predeclared.contains_key(definition.name)
        {
            return Err(vec![self.error(
                "E-LISP-DEF-004",
                format!("function '{}' is already defined", definition.name),
            )]);
        }
        let signature = StackSignature {
            type_parameters: Vec::new(),
            input: StackRow::polymorphic(
                "S",
                definition
                    .parameters
                    .iter()
                    .map(|(_, ty)| ty.clone())
                    .collect(),
            ),
            output: StackRow::polymorphic("S", vec![result]),
            effects: definition
                .declared_effects
                .clone()
                .unwrap_or_else(EffectSet::pure),
            control: ControlEffect::Returns,
        };
        self.vocabulary
            .insert(definition.name.to_string(), signature.clone());
        self.predeclared
            .insert(definition.name.to_string(), signature);
        Ok(())
    }

    fn compile_list(
        &mut self,
        items: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if items.len() == 2 && items[0] == Val::Symbol("quote".into()) {
            let Val::Symbol(name) = &items[1] else {
                return Err(vec![
                    self.error("E-TYPE-011", "quote currently accepts only a symbol")
                ]);
            };
            builder.emit(
                Instruction::Constant {
                    value: TypedValue::Symbol(name.clone()),
                },
                self.origin("quote"),
            );
            return Ok(Type::Symbol);
        }
        let operator = match &items[0] {
            Val::Symbol(operator) => operator.as_str(),
            _ => return self.compile_closure_call(&items[0], &items[1..], builder),
        };
        match operator {
            "begin" => self.compile_begin(&items[1..], builder),
            "let" => self.compile_let(&items[1..], builder),
            "if" => self.compile_if(&items[1..], builder),
            "match" => self.compile_match(&items[1..], builder),
            "match-option" => self.compile_match_option(&items[1..], builder),
            "match-result" => self.compile_match_result(&items[1..], builder),
            "while" => self.compile_while(&items[1..], builder),
            "break" => self.compile_loop_exit("break", &items[1..], builder),
            "continue" => self.compile_loop_exit("continue", &items[1..], builder),
            "lambda" => self.compile_lambda(&items[1..], builder),
            "defer" => self.compile_defer(&items[1..], builder),
            "defer-cpu" => self.compile_defer_cpu(&items[1..], builder),
            "task-poll" => self.compile_cpu_task_operation(&items[1..], builder, false),
            "task-join" => self.compile_cpu_task_operation(&items[1..], builder, true),
            "task-cancel" => self.compile_cpu_task_cancel(&items[1..], builder),
            "list" => self.compile_list_value(&items[1..], builder),
            _ if builder.resolve(operator).is_some() => {
                self.compile_closure_call(&items[0], &items[1..], builder)
            }
            _ => self.compile_named_call(operator, &items[1..], builder),
        }
    }

    fn compile_begin(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if expressions.is_empty() {
            builder.emit(
                Instruction::Constant {
                    value: TypedValue::Unit,
                },
                self.origin("begin"),
            );
            return Ok(Type::Unit);
        }
        for expression in &expressions[..expressions.len() - 1] {
            self.compile_expression(expression, builder)?;
            builder.stack.pop();
            builder.emit(Instruction::Drop, self.origin("begin"));
        }
        self.compile_expression(&expressions[expressions.len() - 1], builder)?;
        Ok(builder
            .stack
            .pop()
            .expect("compiled expression leaves value"))
    }

    /// Lower a pure zero-argument closure directly to the shared IR's
    /// `DeferCpu` instruction. The closure's captures have already been
    /// materialized as typed values, so the worker can never retain a parent
    /// stack frame or local binding.
    fn compile_defer_cpu(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if expressions.len() != 1 {
            return Err(vec![self.error(
                "E-FIBER-003",
                "defer-cpu requires exactly one zero-argument closure",
            )]);
        }
        let closure_type = self.compile_expression(&expressions[0], builder)?;
        let Type::Function {
            arguments,
            result,
            effects,
        } = closure_type
        else {
            return Err(vec![
                self.error("E-FIBER-003", "defer-cpu requires a typed closure")
            ]);
        };
        if !arguments.is_empty() {
            return Err(vec![self.error(
                "E-FIBER-004",
                "defer-cpu requires a zero-argument closure; capture its arguments first",
            )]);
        }
        if !effects.is_pure() {
            return Err(vec![
                self.error("E-FIBER-005", "defer-cpu requires a pure closure")
            ]);
        }
        builder.stack.pop();
        builder.emit(Instruction::DeferCpu, self.origin("defer-cpu"));
        Ok(Type::Task(result))
    }

    /// User-facing deferred-work form. Modes deliberately remain explicit so
    /// an LLM chooses CPU work rather than accidentally treating an I/O wait
    /// as a native thread. `:cpu` is the first supported mode; cooperative
    /// and agent modes will lower to different scheduler instructions.
    fn compile_defer(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if expressions.len() != 2 {
            return Err(vec![self.error(
                "E-FIBER-003",
                "defer requires a mode and exactly one closure",
            )]);
        }
        match &expressions[0] {
            Val::Symbol(mode) if mode == ":cpu" => {
                self.compile_defer_cpu(&expressions[1..], builder)
            }
            Val::Symbol(mode) => Err(vec![self.error(
                "E-FIBER-019",
                format!("unsupported defer mode '{mode}'; supported modes: :cpu"),
            )]),
            _ => Err(vec![self.error(
                "E-FIBER-019",
                "defer mode must be a symbol such as :cpu",
            )]),
        }
    }

    fn compile_cpu_task_operation(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
        join: bool,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let name = if join { "task-join" } else { "task-poll" };
        if expressions.len() != 1 {
            return Err(vec![self.error(
                if join { "E-FIBER-010" } else { "E-FIBER-009" },
                format!("{name} requires exactly one task<T>"),
            )]);
        }
        let task_type = self.compile_expression(&expressions[0], builder)?;
        let Type::Task(result) = task_type else {
            return Err(vec![self.error(
                if join { "E-FIBER-010" } else { "E-FIBER-009" },
                format!("{name} requires task<T>"),
            )]);
        };
        builder.stack.pop();
        builder.emit(
            if join {
                Instruction::JoinCpuFiber
            } else {
                Instruction::PollCpuFiber
            },
            self.origin(name),
        );
        Ok(if join { *result } else { Type::Option(result) })
    }

    fn compile_cpu_task_cancel(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if expressions.len() != 1 {
            return Err(vec![
                self.error("E-FIBER-020", "task-cancel requires exactly one task<T>")
            ]);
        }
        let task_type = self.compile_expression(&expressions[0], builder)?;
        if !matches!(task_type, Type::Task(_)) {
            return Err(vec![
                self.error("E-FIBER-020", "task-cancel requires task<T>")
            ]);
        }
        builder.stack.pop();
        builder.emit(Instruction::CancelCpuFiber, self.origin("task-cancel"));
        Ok(Type::Unit)
    }

    fn compile_list_value(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let Some(first) = expressions.first() else {
            return Err(vec![self.error(
                "E-TYPE-010",
                "an empty list needs an explicit element type",
            )]);
        };
        let element_type = self.compile_expression(first, builder)?;
        for expression in &expressions[1..] {
            let found = self.compile_expression(expression, builder)?;
            if !element_type.accepts(&found) {
                return Err(vec![VmDiagnostic::type_mismatch(
                    element_type.clone(),
                    found,
                    Some(self.origin("list")),
                )]);
            }
        }
        for _ in expressions {
            builder.stack.pop();
        }
        builder.emit(
            Instruction::MakeList {
                element_type: element_type.clone(),
                count: expressions.len() as u32,
            },
            self.origin("list"),
        );
        Ok(Type::list(element_type))
    }

    fn compile_let(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if expressions.len() < 2 {
            return Err(vec![
                self.error("E-LISP-001", "let requires bindings and a body")
            ]);
        }
        let Val::List(bindings) = &expressions[0] else {
            return Err(vec![self.error("E-LISP-002", "let bindings must be a list")]);
        };
        let mut compiled = Vec::new();
        for binding in bindings {
            let Val::List(pair) = binding else {
                return Err(vec![
                    self.error("E-LISP-003", "let binding must be (name value)")
                ]);
            };
            if pair.len() != 2 {
                return Err(vec![
                    self.error("E-LISP-003", "let binding must be (name value)")
                ]);
            }
            let Val::Symbol(name) = &pair[0] else {
                return Err(vec![
                    self.error("E-LISP-004", "let binding name must be a symbol")
                ]);
            };
            let ty = self.compile_expression(&pair[1], builder)?;
            let index = builder.allocate_local(ty.clone());
            compiled.push((name.clone(), index, ty));
        }
        let mut scope = HashMap::new();
        for (name, index, ty) in compiled.iter().rev() {
            builder.stack.pop();
            builder.emit(
                Instruction::LocalSet { index: *index },
                self.origin(name.clone()),
            );
            scope.insert(
                name.clone(),
                Binding::Local {
                    index: *index,
                    ty: ty.clone(),
                },
            );
        }
        builder.scopes.push(scope);
        let result = self.compile_begin(&expressions[1..], builder);
        builder.scopes.pop();
        result
    }

    fn compile_if(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if expressions.len() != 3 {
            return Err(vec![
                self.error("E-LISP-005", "if requires condition, then, and else")
            ]);
        }
        let condition = self.compile_expression(&expressions[0], builder)?;
        if condition != Type::Bool {
            return Err(vec![VmDiagnostic::type_mismatch(
                Type::Bool,
                condition,
                Some(self.origin("if")),
            )]);
        }
        builder.stack.pop();
        let branch_stack = builder.stack.clone();
        let then_block = builder.new_block();
        let else_block = builder.new_block();
        let merge_block = builder.new_block();
        builder.emit(
            Instruction::Branch {
                then_block,
                else_block,
            },
            self.origin("if"),
        );

        builder.switch_to(then_block, branch_stack.clone());
        let then_type = self.compile_expression(&expressions[1], builder)?;
        let then_stack = builder.stack.clone();
        builder.emit(
            Instruction::Jump {
                target: merge_block,
            },
            self.origin("if/then"),
        );

        builder.switch_to(else_block, branch_stack);
        let else_type = self.compile_expression(&expressions[2], builder)?;
        if then_type != else_type || builder.stack != then_stack {
            return Err(vec![VmDiagnostic::type_mismatch(
                then_type,
                else_type,
                Some(self.origin("if")),
            )]);
        }
        builder.emit(
            Instruction::Jump {
                target: merge_block,
            },
            self.origin("if/else"),
        );
        builder.switch_to(merge_block, then_stack);
        Ok(builder.stack.pop().expect("if expression leaves value"))
    }

    /// Compile a total option match without introducing dynamic values. The
    /// `some` arm receives the unwrapped value through a lexical local; the
    /// `none` arm receives no synthetic value. Both arms must therefore agree
    /// on their ordinary expression type and stack row.
    ///
    /// ```lisp
    /// (match-option next-line
    ///   (some line (say line))
    ///   (none (say "EOF")))
    /// ```
    fn compile_match_option(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let [option_expression, some_arm, none_arm] = expressions else {
            return Err(vec![self.error(
                "E-LISP-MATCH-001",
                "match-option requires an option expression, (some name body...), and (none body...)",
            )]);
        };
        let Type::Option(inner) = self.compile_expression(option_expression, builder)? else {
            return Err(vec![self.error(
                "E-LISP-MATCH-002",
                "match-option requires an option<T> expression",
            )]);
        };
        builder.stack.pop();
        let branch_stack = builder.stack.clone();

        let Val::List(some_items) = some_arm else {
            return Err(vec![self.error(
                "E-LISP-MATCH-003",
                "match-option's first arm must be (some name body...)",
            )]);
        };
        let [Val::Symbol(marker), Val::Symbol(binding), some_body @ ..] = some_items.as_slice()
        else {
            return Err(vec![self.error(
                "E-LISP-MATCH-003",
                "match-option's first arm must be (some name body...)",
            )]);
        };
        if marker != "some" || some_body.is_empty() {
            return Err(vec![self.error(
                "E-LISP-MATCH-003",
                "match-option's first arm must be (some name body...)",
            )]);
        }
        let Val::List(none_items) = none_arm else {
            return Err(vec![self.error(
                "E-LISP-MATCH-004",
                "match-option's second arm must be (none body...)",
            )]);
        };
        let [Val::Symbol(marker), none_body @ ..] = none_items.as_slice() else {
            return Err(vec![self.error(
                "E-LISP-MATCH-004",
                "match-option's second arm must be (none body...)",
            )]);
        };
        if marker != "none" || none_body.is_empty() {
            return Err(vec![self.error(
                "E-LISP-MATCH-004",
                "match-option's second arm must be (none body...)",
            )]);
        }

        // The runtime starts with the option on the stack. Keep one copy for
        // the selected branch and branch on a second copy.
        builder.emit(Instruction::Dup, self.origin("match-option/test"));
        builder.emit(
            Instruction::Call {
                function: "is-some".into(),
            },
            self.origin("match-option/test"),
        );
        let then_block = builder.new_block();
        let else_block = builder.new_block();
        let merge_block = builder.new_block();
        builder.emit(
            Instruction::Branch {
                then_block,
                else_block,
            },
            self.origin("match-option/test"),
        );

        builder.switch_to(then_block, branch_stack.clone());
        builder.emit(
            Instruction::Call {
                function: "unwrap".into(),
            },
            self.origin("match-option/some"),
        );
        builder.stack.push((*inner).clone());
        let index = builder.allocate_local((*inner).clone());
        builder.stack.pop();
        builder.emit(
            Instruction::LocalSet { index },
            self.origin(binding.clone()),
        );
        builder.scopes.push(HashMap::from([(
            binding.clone(),
            Binding::Local {
                index,
                ty: (*inner).clone(),
            },
        )]));
        let some_type = self.compile_begin(some_body, builder)?;
        builder.scopes.pop();
        builder.stack.push(some_type.clone());
        let some_stack = builder.stack.clone();
        builder.emit(
            Instruction::Jump {
                target: merge_block,
            },
            self.origin("match-option/some-end"),
        );

        builder.switch_to(else_block, branch_stack);
        builder.emit(Instruction::Drop, self.origin("match-option/none"));
        let none_type = self.compile_begin(none_body, builder)?;
        builder.stack.push(none_type.clone());
        let none_stack = builder.stack.clone();
        if some_type != none_type || some_stack != none_stack {
            return Err(vec![VmDiagnostic::type_mismatch(
                some_type,
                none_type,
                Some(self.origin("match-option")),
            )]);
        }
        builder.emit(
            Instruction::Jump {
                target: merge_block,
            },
            self.origin("match-option/none-end"),
        );
        builder.switch_to(merge_block, none_stack);
        Ok(builder.stack.pop().expect("match-option leaves a value"))
    }

    /// Type-directed surface form for the finite tagged values supported by
    /// Finch Lisp version 1. This is deliberately only syntax selection: the
    /// chosen lowering remains `match-option` or `match-result`, so both
    /// frontends still share the same typed branch IR and verifier rules.
    fn compile_match(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let [_, first_arm, second_arm] = expressions else {
            return Err(vec![self.error(
                "E-LISP-MATCH-009",
                "match requires a value and exactly two exhaustive tagged arms",
            )]);
        };
        let arm_marker = |arm: &Val| match arm {
            Val::List(items) => match items.first() {
                Some(Val::Symbol(marker)) => Some(marker.clone()),
                _ => None,
            },
            _ => None,
        };
        match (arm_marker(first_arm), arm_marker(second_arm)) {
            (Some(first), Some(second)) if first == "some" && second == "none" => {
                self.compile_match_option(expressions, builder)
            }
            (Some(first), Some(second)) if first == "ok" && second == "err" => {
                self.compile_match_result(expressions, builder)
            }
            _ => Err(vec![self.error(
                "E-LISP-MATCH-009",
                "match supports exhaustive (some name ...)/(none ...) or (ok name ...)/(err name ...) arms",
            )]),
        }
    }

    /// Compile a total result match. Both edges consume the result and bind
    /// exactly the selected payload, so ordinary error handling never needs a
    /// dynamic value or a potentially trapping `result-unwrap`.
    ///
    /// ```lisp
    /// (match-result operation
    ///   (ok value (use value))
    ///   (err problem (recover problem)))
    /// ```
    fn compile_match_result(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let [result_expression, ok_arm, err_arm] = expressions else {
            return Err(vec![self.error(
                "E-LISP-MATCH-005",
                "match-result requires a result expression, (ok name body...), and (err name body...)",
            )]);
        };
        let Type::Result(ok_type, err_type) =
            self.compile_expression(result_expression, builder)?
        else {
            return Err(vec![self.error(
                "E-LISP-MATCH-006",
                "match-result requires a result<T,E> expression",
            )]);
        };
        builder.stack.pop();
        let branch_stack = builder.stack.clone();

        let parse_arm = |arm: &Val,
                         marker: &str,
                         code: &str|
         -> Result<(String, Vec<Val>), Vec<VmDiagnostic>> {
            let Val::List(items) = arm else {
                return Err(vec![self.error(
                    code,
                    format!("match-result's {marker} arm must be ({marker} name body...)"),
                )]);
            };
            let [Val::Symbol(found), Val::Symbol(binding), body @ ..] = items.as_slice() else {
                return Err(vec![self.error(
                    code,
                    format!("match-result's {marker} arm must be ({marker} name body...)"),
                )]);
            };
            if found != marker || body.is_empty() {
                return Err(vec![self.error(
                    code,
                    format!("match-result's {marker} arm must be ({marker} name body...)"),
                )]);
            }
            Ok((binding.clone(), body.to_vec()))
        };
        let (ok_binding, ok_body) = parse_arm(ok_arm, "ok", "E-LISP-MATCH-007")?;
        let (err_binding, err_body) = parse_arm(err_arm, "err", "E-LISP-MATCH-008")?;

        builder.emit(Instruction::Dup, self.origin("match-result/test"));
        builder.emit(
            Instruction::Call {
                function: "is-ok".into(),
            },
            self.origin("match-result/test"),
        );
        let ok_block = builder.new_block();
        let err_block = builder.new_block();
        let merge_block = builder.new_block();
        builder.emit(
            Instruction::Branch {
                then_block: ok_block,
                else_block: err_block,
            },
            self.origin("match-result/test"),
        );

        builder.switch_to(ok_block, branch_stack.clone());
        builder.emit(
            Instruction::Call {
                function: "result-unwrap".into(),
            },
            self.origin("match-result/ok"),
        );
        builder.stack.push((*ok_type).clone());
        let index = builder.allocate_local((*ok_type).clone());
        builder.stack.pop();
        builder.emit(
            Instruction::LocalSet { index },
            self.origin(ok_binding.clone()),
        );
        builder.scopes.push(HashMap::from([(
            ok_binding,
            Binding::Local {
                index,
                ty: (*ok_type).clone(),
            },
        )]));
        let ok_result = self.compile_begin(&ok_body, builder)?;
        builder.scopes.pop();
        builder.stack.push(ok_result.clone());
        let ok_stack = builder.stack.clone();
        builder.emit(
            Instruction::Jump {
                target: merge_block,
            },
            self.origin("match-result/ok-end"),
        );

        builder.switch_to(err_block, branch_stack);
        builder.emit(
            Instruction::Call {
                function: "result-error".into(),
            },
            self.origin("match-result/err"),
        );
        builder.stack.push((*err_type).clone());
        let index = builder.allocate_local((*err_type).clone());
        builder.stack.pop();
        builder.emit(
            Instruction::LocalSet { index },
            self.origin(err_binding.clone()),
        );
        builder.scopes.push(HashMap::from([(
            err_binding,
            Binding::Local {
                index,
                ty: (*err_type).clone(),
            },
        )]));
        let err_result = self.compile_begin(&err_body, builder)?;
        builder.scopes.pop();
        builder.stack.push(err_result.clone());
        let err_stack = builder.stack.clone();
        if ok_result != err_result || ok_stack != err_stack {
            return Err(vec![VmDiagnostic::type_mismatch(
                ok_result,
                err_result,
                Some(self.origin("match-result")),
            )]);
        }
        builder.emit(
            Instruction::Jump {
                target: merge_block,
            },
            self.origin("match-result/err-end"),
        );
        builder.switch_to(merge_block, err_stack);
        Ok(builder.stack.pop().expect("match-result leaves a value"))
    }

    fn compile_while(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let (label, condition_index) = match expressions {
            [Val::Symbol(marker), Val::Symbol(label), rest @ ..] if marker == ":label" => {
                if label.is_empty() || rest.len() < 2 {
                    return Err(vec![self.error(
                        "E-LISP-011",
                        "while :label requires a name, condition, and at least one body expression",
                    )]);
                }
                (Some(label.clone()), 2)
            }
            _ => (None, 0),
        };
        if expressions.len() < condition_index + 2 {
            return Err(vec![self.error(
                "E-LISP-010",
                "while requires a condition and at least one body expression",
            )]);
        }
        let loop_stack = builder.stack.clone();
        let condition_block = builder.new_block();
        let body_block = builder.new_block();
        let exit_block = builder.new_block();
        builder.emit(
            Instruction::Jump {
                target: condition_block,
            },
            self.origin("while/enter"),
        );

        builder.switch_to(condition_block, loop_stack.clone());
        let condition = self.compile_expression(&expressions[condition_index], builder)?;
        if condition != Type::Bool {
            return Err(vec![VmDiagnostic::type_mismatch(
                Type::Bool,
                condition,
                Some(self.origin("while")),
            )]);
        }
        builder.stack.pop();
        if builder.stack != loop_stack {
            return Err(vec![self.error(
                "E-STACK-005",
                "while condition must preserve the loop stack",
            )]);
        }
        builder.emit(
            Instruction::Branch {
                then_block: body_block,
                else_block: exit_block,
            },
            self.origin("while/test"),
        );

        builder.switch_to(body_block, loop_stack.clone());
        builder.loops.push(LoopBinding {
            label,
            header: condition_block,
            exit: exit_block,
            stack: loop_stack.clone(),
        });
        let body = self.compile_begin(&expressions[condition_index + 1..], builder);
        builder.loops.pop();
        body?;
        builder.emit(Instruction::Drop, self.origin("while/body-result"));
        if builder.stack != loop_stack {
            return Err(vec![
                self.error("E-STACK-005", "while body must preserve the loop stack")
            ]);
        }
        builder.emit(
            Instruction::Jump {
                target: condition_block,
            },
            self.origin("while/repeat"),
        );

        builder.switch_to(exit_block, loop_stack);
        builder.emit(
            Instruction::Constant {
                value: TypedValue::Unit,
            },
            self.origin("while/result"),
        );
        Ok(Type::Unit)
    }

    fn compile_loop_exit(
        &mut self,
        kind: &str,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let [Val::Symbol(label)] = expressions else {
            return Err(vec![self.error(
                "E-LISP-012",
                format!("{kind} requires exactly one active loop label"),
            )]);
        };
        let Some(loop_binding) = builder
            .loops
            .iter()
            .rev()
            .find(|loop_binding| loop_binding.label.as_deref() == Some(label.as_str()))
            .cloned()
        else {
            return Err(vec![self.error(
                "E-LISP-012",
                format!("{kind} target '{label}' is not an active named loop"),
            )]);
        };
        if builder.stack != loop_binding.stack {
            let mut diagnostic = self.error(
                "E-STACK-006",
                format!("{kind} target '{label}' requires the loop's declared stack shape"),
            );
            diagnostic.expected_types = loop_binding.stack;
            diagnostic.found_types = builder.stack.clone();
            return Err(vec![diagnostic]);
        }
        let target = if kind == "break" {
            loop_binding.exit
        } else {
            loop_binding.header
        };
        builder.emit(Instruction::Jump { target }, self.origin(kind));
        // Lisp expressions must have a value even though the actual edge has
        // already left this block.  The synthetic unit is compiler-only
        // unreachable code; it lets enclosing `begin`/`if` type-check the
        // alternate live path without changing the target loop row.
        builder.emit(
            Instruction::Constant {
                value: TypedValue::Unit,
            },
            self.origin(format!("{kind}/unreachable")),
        );
        Ok(Type::Unit)
    }

    fn compile_lambda(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if expressions.len() < 2 {
            return Err(vec![
                self.error("E-LISP-006", "lambda requires parameters and a body")
            ]);
        }
        // The reader represents `()` as `nil`, so accept it as the natural
        // zero-argument parameter list. Zero-argument closures are important
        // for `defer`: captures supply their immutable environment and the
        // worker receives no parent stack values.
        let parameters: &[Val] = match &expressions[0] {
            Val::List(parameters) => parameters,
            Val::Nil => &[],
            _ => {
                return Err(vec![
                    self.error("E-LISP-007", "lambda parameters must be a list")
                ]);
            }
        };
        let parameters = parameters
            .iter()
            .map(parse_parameter)
            .collect::<Result<Vec<_>, _>>()?;
        let visible = builder.visible_bindings();
        for (_, binding) in &visible {
            self.emit_binding_load(builder, binding.clone());
        }

        let mut hasher = DefaultHasher::new();
        self.source_id.hash(&mut hasher);
        self.source.hash(&mut hasher);
        let name = format!("lambda${:016x}${}", hasher.finish(), self.next_lambda);
        self.next_lambda += 1;
        let arguments = parameters
            .iter()
            .map(|(_, ty)| ty.clone())
            .collect::<Vec<_>>();
        let mut child = FunctionBuilder::new(&name, arguments.clone());
        child.captures = visible
            .iter()
            .map(|(_, binding)| binding.ty().clone())
            .collect();
        for (index, (capture_name, binding)) in visible.iter().enumerate() {
            child.scopes[0].insert(
                capture_name.clone(),
                Binding::Capture {
                    index: index as u32,
                    ty: binding.ty().clone(),
                },
            );
        }
        for (name, ty) in &parameters {
            let index = child.allocate_local(ty.clone());
            child.scopes[0].insert(
                name.clone(),
                Binding::Local {
                    index,
                    ty: ty.clone(),
                },
            );
        }
        for (parameter_index, _) in (0..parameters.len()).rev().zip(parameters.iter().rev()) {
            child.stack.pop();
            child.emit(
                Instruction::LocalSet {
                    index: parameter_index as u32,
                },
                self.origin("lambda-parameter"),
            );
        }
        let result_type = self.compile_begin(&expressions[1..], &mut child)?;
        child.stack.push(result_type.clone());
        child.emit(Instruction::Return, self.origin("lambda-return"));
        let child_function = child.finish(vec![result_type.clone()]);
        let signature = child_function.signature.clone();
        self.functions.insert(name.clone(), child_function);
        builder.emit(
            Instruction::MakeClosure {
                function: name,
                capture_count: visible.len() as u32,
                signature: signature.clone(),
            },
            self.origin("lambda"),
        );
        for _ in &visible {
            builder.stack.pop();
        }
        Ok(Type::Function {
            arguments,
            result: Box::new(result_type),
            effects: signature.effects,
        })
    }

    fn compile_named_call(
        &mut self,
        operator: &str,
        arguments: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let word = match operator {
            "string-append" => "str-cat",
            other => other,
        };
        let Some(signature) = self.vocabulary.get(word).cloned() else {
            return Err(vec![self.error(
                "E-LINK-002",
                format!("unknown Lisp function '{operator}'"),
            )]);
        };
        if arguments.len() != signature.input.values.len() {
            return Err(vec![self.error(
                "E-TYPE-006",
                format!(
                    "{operator} expects {} arguments, received {}",
                    signature.input.values.len(),
                    arguments.len()
                ),
            )]);
        }
        for argument in arguments {
            self.compile_expression(argument, builder)?;
        }
        let origin = self.origin(operator);
        apply_signature_types(&signature, &mut builder.stack, &origin)
            .map_err(|diagnostic| vec![diagnostic])?;
        builder.effects = builder.effects.union(&signature.effects);
        if word == "yield" {
            // Lisp expressions always produce a value. The underlying
            // control instruction has no stack effect, so expose it as unit
            // while keeping Co-Forth's `yield` stack-neutral.
            builder.emit(Instruction::Yield, origin.clone());
            builder.emit(
                Instruction::Constant {
                    value: TypedValue::Unit,
                },
                origin,
            );
            return Ok(Type::Unit);
        }
        let instruction = if word == "output-open" {
            Instruction::OutputOpen
        } else if let Some(operation) = output_operation(word) {
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
            Instruction::Call {
                function: word.to_string(),
            }
        };
        builder.emit(instruction, origin);
        if signature.output.values.is_empty() {
            // Finch Lisp is expression-oriented even when its shared stack
            // primitive is an effect with no value result. Keep the Lisp
            // surface composable without leaking synthetic units into
            // Co-Forth's operand stack.
            builder.emit(
                Instruction::Constant {
                    value: TypedValue::Unit,
                },
                self.origin("effect-unit"),
            );
            return Ok(Type::Unit);
        }
        Ok(builder.stack.pop().expect("call signature leaves result"))
    }

    fn compile_closure_call(
        &mut self,
        target: &Val,
        arguments: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        for argument in arguments {
            self.compile_expression(argument, builder)?;
        }
        let closure_type = self.compile_expression(target, builder)?;
        let Type::Function {
            arguments: expected,
            result,
            effects,
        } = closure_type
        else {
            return Err(vec![
                self.error("E-TYPE-007", "call target is not a function")
            ]);
        };
        if expected.len() != arguments.len() {
            return Err(vec![self.error(
                "E-TYPE-006",
                format!(
                    "closure expects {} arguments, received {}",
                    expected.len(),
                    arguments.len()
                ),
            )]);
        }
        let signature = StackSignature {
            type_parameters: Vec::new(),
            input: StackRow::polymorphic("S", expected),
            output: StackRow::polymorphic("S", vec![(*result).clone()]),
            effects: effects.clone(),
            control: ControlEffect::Returns,
        };
        builder.stack.pop();
        apply_signature_types(&signature, &mut builder.stack, &self.origin("call"))
            .map_err(|diagnostic| vec![diagnostic])?;
        builder.effects = builder.effects.union(&effects);
        builder.emit(
            Instruction::CallClosure {
                signature: signature.clone(),
            },
            self.origin("call"),
        );
        Ok(builder.stack.pop().expect("closure leaves result"))
    }

    fn emit_binding_load(&self, builder: &mut FunctionBuilder, binding: Binding) {
        let (instruction, ty) = match binding {
            Binding::Local { index, ty } => (Instruction::LocalGet { index }, ty),
            Binding::Capture { index, ty } => (Instruction::CaptureGet { index }, ty),
        };
        builder.emit(instruction, self.origin("capture"));
        builder.stack.push(ty);
    }

    fn error(&self, code: &str, message: impl Into<String>) -> VmDiagnostic {
        VmDiagnostic::error(
            code,
            DiagnosticPhase::TypeInference,
            message,
            Some(self.origin("lisp")),
        )
    }
}

/// Conservative structural check used only to improve the diagnostic for an
/// unannotated self-recursive definition.  The reader has no separate call
/// node: a list whose first item is the definition name is a named call.
fn expression_mentions_symbol(expression: &Val, name: &str) -> bool {
    match expression {
        Val::List(items) => items.iter().any(|item| match item {
            Val::Symbol(symbol) => symbol == name,
            nested => expression_mentions_symbol(nested, name),
        }),
        _ => false,
    }
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

fn is_definition(expression: &Val) -> bool {
    matches!(
        expression,
        Val::List(items) if matches!(items.first(), Some(Val::Symbol(name)) if name == "define")
    )
}

/// Parsed typed function definition. An explicit `: return-type` after the
/// header makes the signature available before the body is compiled, which is
/// required for self- and mutually-recursive calls.
struct Definition<'a> {
    name: &'a str,
    parameters: Vec<(String, Type)>,
    return_type: Option<Type>,
    /// An explicit upper bound for effects used by a return-annotated
    /// definition.  This makes recursive calls checkable before the body has
    /// been compiled: inferred effects must be covered by this declaration.
    /// Version 1 deliberately accepts only unscoped named capabilities here;
    /// parameterized selectors need their own typed syntax rather than an
    /// ambiguous string convention.
    declared_effects: Option<EffectSet>,
    /// The first body string is a Python-style docstring. It is protocol
    /// metadata, never a runtime stack value or an instruction in the typed
    /// function body.
    documentation: Option<&'a str>,
    body: &'a [Val],
}

fn parse_definition(expression: &Val) -> Result<Definition<'_>, Vec<VmDiagnostic>> {
    let Val::List(items) = expression else {
        unreachable!("definition predicate only accepts lists");
    };
    if items.len() < 3 {
        return Err(vec![VmDiagnostic::error(
            "E-LISP-DEF-001",
            DiagnosticPhase::TypeInference,
            "define requires a function header and body",
            None,
        )]);
    }
    let Val::List(header) = &items[1] else {
        return Err(vec![VmDiagnostic::error(
            "E-LISP-DEF-002",
            DiagnosticPhase::TypeInference,
            "typed define must use (define (name (arg : type) ...) [: return-type] body...)",
            None,
        )]);
    };
    let Some(Val::Symbol(name)) = header.first() else {
        return Err(vec![VmDiagnostic::error(
            "E-LISP-DEF-003",
            DiagnosticPhase::TypeInference,
            "function definition requires a symbol name",
            None,
        )]);
    };
    let parameters = header[1..]
        .iter()
        .map(parse_parameter)
        .collect::<Result<Vec<_>, _>>()?;
    let (return_type, mut body) = if items.get(2) == Some(&Val::Symbol(":".into())) {
        let Some(annotation) = items.get(3) else {
            return Err(vec![VmDiagnostic::error(
                "E-LISP-DEF-006",
                DiagnosticPhase::TypeInference,
                "function return annotation requires a type after ':'",
                None,
            )]);
        };
        (Some(parse_type(annotation)?), &items[4..])
    } else {
        (None, &items[2..])
    };
    let declared_effects = if body.first() == Some(&Val::Symbol("!".into())) {
        let Some(annotation) = body.get(1) else {
            return Err(vec![VmDiagnostic::error(
                "E-LISP-DEF-009",
                DiagnosticPhase::TypeInference,
                "function effect annotation requires a capability list after '!'",
                None,
            )]);
        };
        body = &body[2..];
        Some(parse_definition_effects(annotation)?)
    } else {
        None
    };
    let (documentation, body) = match body.split_first() {
        Some((Val::Str(documentation), body)) => (Some(documentation.as_str()), body),
        _ => (None, body),
    };
    if body.is_empty() {
        return Err(vec![VmDiagnostic::error(
            "E-LISP-DEF-001",
            DiagnosticPhase::TypeInference,
            "define requires a function body",
            None,
        )]);
    }
    Ok(Definition {
        name,
        parameters,
        return_type,
        declared_effects,
        documentation,
        body,
    })
}

/// Parse a definition-level effect bound such as `! (session.emit memory.read)`.
/// These are capability identities, not authority grants.  Parameterized file,
/// process, network, and agent selectors still come from calls and must be
/// inferred precisely by the shared typed IR.
fn parse_definition_effects(value: &Val) -> Result<EffectSet, Vec<VmDiagnostic>> {
    let Val::List(capabilities) = value else {
        return Err(vec![VmDiagnostic::error(
            "E-LISP-DEF-009",
            DiagnosticPhase::TypeInference,
            "function effect annotation must be a list such as ! (session.emit)",
            None,
        )]);
    };
    let mut effects = EffectSet::pure();
    for capability in capabilities {
        let Val::Symbol(name) = capability else {
            return Err(vec![VmDiagnostic::error(
                "E-LISP-DEF-009",
                DiagnosticPhase::TypeInference,
                "function effect annotations must contain capability names",
                None,
            )]);
        };
        let Some(capability) = parse_unscoped_capability(name) else {
            return Err(vec![VmDiagnostic::error(
                "E-LISP-DEF-009",
                DiagnosticPhase::TypeInference,
                format!("unknown or parameterized definition effect '{name}'"),
                None,
            )]);
        };
        effects = effects.union(&EffectSet::from_requirement(CapabilityRequirement {
            capability,
            selector: ResourceSelector::None,
        }));
    }
    Ok(effects)
}

fn parse_unscoped_capability(name: &str) -> Option<CapabilityKind> {
    let normalized = name.replace(['.', '-'], "_");
    Some(match normalized.as_str() {
        "vm_read" => CapabilityKind::VmRead,
        "vm_write" => CapabilityKind::VmWrite,
        "file_read" => CapabilityKind::FileRead,
        "file_write" => CapabilityKind::FileWrite,
        "network_connect" => CapabilityKind::NetworkConnect,
        "automation_inspect" => CapabilityKind::AutomationInspect,
        "automation_write" => CapabilityKind::AutomationWrite,
        "agent_spawn" => CapabilityKind::AgentSpawn,
        "agent_await" => CapabilityKind::AgentAwait,
        "agent_poll" => CapabilityKind::AgentPoll,
        "agent_cancel" => CapabilityKind::AgentCancel,
        "process_run" => CapabilityKind::ProcessRun,
        "session_emit" => CapabilityKind::SessionEmit,
        "memory_read" => CapabilityKind::MemoryRead,
        "memory_write" => CapabilityKind::MemoryWrite,
        "memory_consolidate" => CapabilityKind::MemoryConsolidate,
        "schedule_create" => CapabilityKind::ScheduleCreate,
        "schedule_read" => CapabilityKind::ScheduleRead,
        "schedule_manage" => CapabilityKind::ScheduleManage,
        "program_invoke" => CapabilityKind::ProgramInvoke,
        "unsafe_memory" => CapabilityKind::UnsafeMemory,
        _ => return None,
    })
}

fn parse_parameter(parameter: &Val) -> Result<(String, Type), Vec<VmDiagnostic>> {
    let (name, type_value) = match parameter {
        Val::List(parts) if parts.len() == 3 && parts[1] == Val::Symbol(":".into()) => {
            (&parts[0], &parts[2])
        }
        Val::List(parts) if parts.len() == 2 => (&parts[0], &parts[1]),
        _ => {
            return Err(vec![VmDiagnostic::error(
                "E-LISP-008",
                DiagnosticPhase::TypeInference,
                "lambda parameter must be (name : type)",
                None,
            )]);
        }
    };
    let Val::Symbol(name) = name else {
        return Err(vec![VmDiagnostic::error(
            "E-LISP-009",
            DiagnosticPhase::TypeInference,
            "lambda parameter name must be a symbol",
            None,
        )]);
    };
    Ok((name.clone(), parse_type(type_value)?))
}

fn parse_type(value: &Val) -> Result<Type, Vec<VmDiagnostic>> {
    let Val::Symbol(name) = value else {
        return Err(vec![VmDiagnostic::error(
            "E-TYPE-008",
            DiagnosticPhase::TypeInference,
            "type annotation must be a type name",
            None,
        )]);
    };
    parse_type_name(name)
}

/// Parse the compact type spelling shared by Lisp annotations and Co-Forth
/// stack signatures. The reader keeps `list<int>` as one symbol, so this
/// parser deliberately handles nested angle brackets without adding a second
/// syntax tree for type expressions.
pub(crate) fn parse_type_name(name: &str) -> Result<Type, Vec<VmDiagnostic>> {
    match name {
        "unit" | "nil" => Ok(Type::Unit),
        "bool" => Ok(Type::Bool),
        "int" => Ok(Type::Int),
        "uint" => Ok(Type::UInt),
        "float" => Ok(Type::Float),
        "char" => Ok(Type::Char),
        "string" | "str" => Ok(Type::String),
        "bytes" => Ok(Type::Bytes),
        "dynamic" | "any" => Ok(Type::Dynamic),
        _ => parse_generic_type(name).ok_or_else(|| {
            vec![VmDiagnostic::error(
                "E-TYPE-009",
                DiagnosticPhase::TypeInference,
                format!("unknown type '{name}'"),
                None,
            )]
        }),
    }
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
        _ => None,
    }
}

fn split_type_arguments(source: &str) -> Option<Vec<&str>> {
    let mut arguments = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (index, character) in source.char_indices() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
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
    if depth != 0 {
        return None;
    }
    let argument = source[start..].trim();
    if argument.is_empty() {
        return None;
    }
    arguments.push(argument);
    Some(arguments)
}

fn source_origin(source_id: &str, source: &str, word: impl Into<String>) -> SourceOrigin {
    let line_count = source.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let final_column = source
        .rsplit_once('\n')
        .map_or(source.chars().count() + 1, |(_, tail)| {
            tail.chars().count() + 1
        });
    SourceOrigin {
        language: SourceLanguage::Lisp,
        span: Some(SourceSpan {
            source_id: source_id.to_string(),
            start_byte: 0,
            end_byte: source.len(),
            start_line: 1,
            start_column: 1,
            end_line: line_count,
            end_column: final_column,
        }),
        word: Some(word.into()),
        expansion: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::interpreter::{DenyCapabilities, Interpreter, InterpreterConfig};
    use crate::vm::{core_vocabulary, TypedValue};

    fn run(source: &str) -> Result<Vec<TypedValue>, Vec<VmDiagnostic>> {
        let module = compile_lisp("input.lisp", source, Vec::new(), &core_vocabulary())?;
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .map_err(|error| vec![error])?;
        Ok(stack)
    }

    #[test]
    fn lowers_nested_lisp_without_generating_forth_text() {
        assert_eq!(run("(+ 3 (* 4 2))").unwrap(), vec![TypedValue::Int(11)]);
    }

    #[test]
    fn lowers_lexical_let_to_typed_locals() {
        assert_eq!(
            run("(let ((a 10) (b 5)) (- a b))").unwrap(),
            vec![TypedValue::Int(5)]
        );
    }

    #[test]
    fn lowers_typed_closure_with_captured_environment() {
        assert_eq!(
            run("(let ((n 10)) ((lambda ((x : int)) (+ x n)) 5))").unwrap(),
            vec![TypedValue::Int(15)]
        );
    }

    #[test]
    fn expands_bounded_capture_free_syntax_templates_before_type_checking() {
        assert_eq!(
            run("(define-syntax (when test body) (if test body 0)) (when true 42)").unwrap(),
            vec![TypedValue::Int(42)]
        );
    }

    #[test]
    fn syntax_templates_compose_with_a_bounded_expansion_budget() {
        assert_eq!(
            run("(define-syntax (inc value) (+ value 1)) \
                 (define-syntax (twice value) (inc (inc value))) \
                 (twice 40)")
            .unwrap(),
            vec![TypedValue::Int(42)]
        );
    }

    #[test]
    fn syntax_templates_do_not_hide_expanded_capabilities() {
        let module = compile_lisp(
            "input.lisp",
            "(define-syntax (announce text) (say text)) (announce \"hello\")",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let main = module.functions.get("main").unwrap();
        assert!(main.inferred_effects.grants(&EffectSet::from_requirement(
            CapabilityRequirement {
                capability: CapabilityKind::SessionEmit,
                selector: ResourceSelector::None,
            },
        )));
    }

    #[test]
    fn rejects_template_macros_that_introduce_a_binding_form() {
        let errors = compile_lisp(
            "input.lisp",
            "(define-syntax (bad value) (let ((temporary value)) temporary)) (bad 1)",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert!(errors.iter().any(|error| error.code == "E-LISP-MACRO-004"));
    }

    #[test]
    fn rejects_unbounded_recursive_syntax_expansion() {
        let errors = compile_lisp(
            "input.lisp",
            "(define-syntax (forever value) (forever value)) (forever 1)",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert!(errors.iter().any(|error| error.code == "E-LISP-MACRO-005"));
    }

    #[test]
    fn binds_lisp_parameters_from_the_shared_stack_in_reverse_pop_order() {
        let module = compile_lisp(
            "input.lisp",
            "(define (subtract (left : int) (right : int)) (- left right))",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let function = &module.module.functions["subtract"];
        assert_eq!(function.signature.input.values, vec![Type::Int, Type::Int]);
        let instructions = &function.blocks[&function.entry].instructions;
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
    }

    #[test]
    fn compiles_a_recursively_typed_function() {
        assert_eq!(
            run("(define (factorial (n : int)) : int \
                   (if (<= n 1) 1 (* n (factorial (- n 1))))) \
                 (factorial 6)")
            .unwrap(),
            vec![TypedValue::Int(720)]
        );
    }

    #[test]
    fn retains_lisp_definition_docstrings_as_non_executable_ir_metadata() {
        let module = compile_lisp(
            "input.lisp",
            "(define (double (n : int)) : int \"Return twice n.\" (* n 2)) (double 21)",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        assert_eq!(
            module.module.functions["double"].documentation.as_deref(),
            Some("Return twice n.")
        );

        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(42)]);
    }

    #[test]
    fn explains_that_unannotated_recursion_needs_a_return_type() {
        let errors = compile_lisp(
            "input.lisp",
            "(define (factorial (n : int)) \
                (if (<= n 1) 1 (* n (factorial (- n 1)))))",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert!(errors.iter().any(|error| {
            error.code == "E-LISP-DEF-008" && error.message.contains("return type")
        }));
    }

    #[test]
    fn rejects_a_definition_whose_return_annotation_disagrees_with_its_body() {
        let errors = compile_lisp(
            "input.lisp",
            "(define (wrong (n : int)) : string (+ n 1))",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert!(errors.iter().any(|error| error.code == "E-LISP-DEF-005"));
    }

    #[test]
    fn rejects_an_effectful_predeclared_definition_without_an_effect_bound() {
        let errors = compile_lisp(
            "input.lisp",
            "(define (announce (text : string)) : unit (say text))",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert!(errors.iter().any(|error| error.code == "E-LISP-DEF-007"));
    }

    #[test]
    fn predeclares_recursive_effectful_definitions_from_their_effect_bound() {
        let module = compile_lisp(
            "input.lisp",
            "(define (announce (n : int)) : unit ! (session.emit) \
                (if (<= n 0) (say \"done\") \
                    (begin (say \"tick\") (announce (- n 1)))))",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();

        let announce = module.functions.get("announce").unwrap();
        assert!(announce.inferred_effects.grants(&EffectSet::from_requirement(
            CapabilityRequirement {
                capability: CapabilityKind::SessionEmit,
                selector: ResourceSelector::None,
            },
        )));
    }

    #[test]
    fn predeclares_mutually_recursive_effectful_definitions_from_effect_bounds() {
        let module = compile_lisp(
            "input.lisp",
            "(define (even-report (n : int)) : unit ! (session.emit) \
                (if (<= n 0) (say \"even\") (odd-report (- n 1)))) \
             (define (odd-report (n : int)) : unit ! (session.emit) \
                (if (<= n 0) (say \"odd\") (even-report (- n 1))))",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();

        assert!(module.functions.contains_key("even-report"));
        assert!(module.functions.contains_key("odd-report"));
    }

    #[test]
    fn rejects_unknown_definition_effects() {
        let errors = compile_lisp(
            "input.lisp",
            "(define (announce (text : string)) : unit ! (filesystem.everything) (say text))",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert!(errors.iter().any(|error| error.code == "E-LISP-DEF-009"));
    }

    #[test]
    fn verifies_both_if_branches_before_execution() {
        let errors = compile_lisp(
            "input.lisp",
            "(if true 1 \"wrong\")",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert!(errors.iter().any(|error| error.code == "E-TYPE-002"));
    }

    #[test]
    fn constructs_and_uses_homogeneous_typed_lists() {
        assert_eq!(
            run("(list-get (list 4 8 15 16) 2)").unwrap(),
            vec![TypedValue::Int(15)]
        );
        assert_eq!(
            run("(list-length (list \"a\" \"b\"))").unwrap(),
            vec![TypedValue::Int(2)]
        );
    }

    #[test]
    fn matches_typed_result_payloads_without_unsafe_projection() {
        assert_eq!(
            run("(match-result (ok 5) (ok value (+ value 1)) (err problem (begin problem 0)))")
                .unwrap(),
            vec![TypedValue::Int(6)]
        );
        assert_eq!(
            run("(match-result (err \"bad\") (ok value (begin value 0)) (err problem (begin problem 3)))").unwrap(),
            vec![TypedValue::Int(3)]
        );
    }

    #[test]
    fn generic_match_selects_the_existing_typed_tagged_lowering() {
        assert_eq!(
            run("(match (some 5) (some value (+ value 1)) (none 0))").unwrap(),
            vec![TypedValue::Int(6)]
        );
        assert_eq!(
            run("(match (err \"bad\") (ok value (begin value 0)) (err problem (begin problem 3)))")
                .unwrap(),
            vec![TypedValue::Int(3)]
        );
    }

    #[test]
    fn accepts_parameterized_return_annotations() {
        assert_eq!(
            run(
                "(define (singleton (value : int)) : list<int> (list value)) \
                 (list-get (singleton 7) 0)"
            )
            .unwrap(),
            vec![TypedValue::Int(7)]
        );
    }

    #[test]
    fn parses_nested_parameterized_type_annotations() {
        assert_eq!(
            parse_type_name("result<option<list<int>>,string>").unwrap(),
            Type::result(Type::Option(Box::new(Type::list(Type::Int))), Type::String,)
        );
    }

    #[test]
    fn rejects_heterogeneous_list_without_dynamic_boundary() {
        let errors = compile_lisp(
            "input.lisp",
            "(list 1 \"two\")",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert_eq!(errors[0].code, "E-TYPE-002");
    }

    #[test]
    fn quote_produces_a_typed_symbol_value() {
        assert_eq!(
            run("(quote bash)").unwrap(),
            vec![TypedValue::Symbol("bash".into())]
        );
        assert_eq!(
            run("'bash").unwrap(),
            vec![TypedValue::Symbol("bash".into())]
        );
    }

    #[test]
    fn lowers_explicit_cpu_defer_mode_to_the_shared_instruction() {
        let module = compile_lisp(
            "input.lisp",
            "(defer :cpu (lambda () 42))",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();

        let main = &module.module.functions["main"];
        assert!(main.blocks[&main.entry]
            .instructions
            .iter()
            .any(|operation| matches!(operation.instruction, Instruction::DeferCpu)));
    }

    #[test]
    fn named_break_and_continue_lower_to_typed_loop_edges() {
        assert_eq!(
            run("(while :label outer true (break outer))").unwrap(),
            Vec::<TypedValue>::new()
        );
        let module = compile_lisp(
            "continue.lisp",
            "(while :label outer false (continue outer))",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        assert!(module.module.functions["main"]
            .blocks
            .values()
            .any(|block| {
                block
                    .instructions
                    .iter()
                    .any(|located| matches!(located.instruction, Instruction::Jump { .. }))
            }));
    }

    #[test]
    fn named_lisp_loop_exits_require_an_active_label() {
        let errors = compile_lisp(
            "missing.lisp",
            "(while :label outer true (break absent))",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect_err("break must name an active loop");
        assert_eq!(errors[0].code, "E-LISP-012");
    }
}
