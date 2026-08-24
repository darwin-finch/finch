use crate::lisp::reader::SpannedVal;
use crate::lisp::Val;
use crate::vm::diagnostic::{
    DiagnosticPhase, SourceLanguage, SourceOrigin, SourceSpan, VmDiagnostic,
};
use crate::vm::effects::{CapabilityKind, CapabilityRequirement, EffectSet, ResourceSelector};
use crate::vm::interpreter::UiOperation;
use crate::vm::ir::{BasicBlock, BlockId, Function, Instruction, LocatedInstruction, Module};
use crate::vm::signature::{ControlEffect, StackRow, StackSignature, SuspensionSignature};
use crate::vm::types::{Type, TypedValue};
use crate::vm::verifier::{
    apply_signature_types, instantiate_signature_types, VerifiedModule, Verifier, Vocabulary,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;

/// A parsed top-level Lisp form paired with its structural source tree.
/// Macro-expanded forms preserve caller-owned argument spans inside that tree
/// and link generated syntax to its definition through `expansion`.
#[derive(Debug, Clone)]
struct SourceForm {
    value: Val,
    span: Range<usize>,
    source: SpannedVal,
    expansion: Option<SourceOrigin>,
}

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
    suspension: Option<SuspensionSignature>,
    /// The enclosing definition's result contract, when it opted into
    /// early-result propagation. Top-level forms and closures intentionally
    /// have none: `try` must not manufacture a hidden dynamic return path.
    return_result: Option<(Type, Type)>,
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
            suspension: None,
            return_result: None,
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

    fn merge_suspension(
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
    current_span: Range<usize>,
    current_source: Option<SpannedVal>,
    current_expansion: Option<SourceOrigin>,
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
    let parsed_forms = crate::lisp::reader::parse_str_spanned(source).map_err(|error| {
        vec![VmDiagnostic::error(
            "E-READ-002",
            DiagnosticPhase::Reader,
            error.to_string(),
            Some(source_origin(source_id, source, "<reader>")),
        )]
    })?;
    let macros = collect_template_macros(source_id, source, &parsed_forms)?;
    let mut expansion_budget = 128;
    let expressions = parsed_forms
        .iter()
        .filter(|form| !is_macro_definition(&form.value))
        .map(|form| {
            expand_template_macros(source_id, source, form, &macros, &mut expansion_budget)
                .map(|expanded| {
                    SourceForm {
                        value: expanded.value,
                        span: form.span.clone(),
                        source: expanded.source,
                        expansion: expanded.expansion,
                    }
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let expressions = flatten_top_level_begins(expressions);
    let mut compiler = Compiler {
        source_id,
        source,
        vocabulary: vocabulary.clone(),
        predeclared: BTreeMap::new(),
        functions: BTreeMap::new(),
        next_lambda: 0,
        current_span: 0..source.len(),
        current_source: None,
        current_expansion: None,
    };
    for expression in &expressions {
        compiler.current_span = expression.span.clone();
        compiler.current_source = Some(expression.source.clone());
        compiler.current_expansion = expression.expansion.clone();
        if is_definition(&expression.value) {
            compiler.predeclare_definition(&expression.value)?;
        }
    }
    let mut executable = Vec::new();
    for expression in &expressions {
        compiler.current_span = expression.span.clone();
        compiler.current_source = Some(expression.source.clone());
        compiler.current_expansion = expression.expansion.clone();
        if is_definition(&expression.value) {
            compiler.compile_definition(&expression.value)?;
        } else {
            executable.push(expression.clone());
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
            compiler.current_span = expression.span.clone();
            compiler.current_source = Some(expression.source.clone());
            compiler.current_expansion = expression.expansion.clone();
            compiler.compile_expression(&expression.value, &mut builder)?;
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

/// Scheme-style top-level `begin` is a declaration container, not a runtime
/// lexical scope. Splice its children before the definition predeclaration
/// pass so a model can naturally submit one `(begin (define ...) (use ...))`
/// response while definitions nested inside functions and other expressions
/// remain invalid. Reader-owned child spans preserve diagnostics for every
/// spliced form.
fn flatten_top_level_begins(forms: Vec<SourceForm>) -> Vec<SourceForm> {
    fn append(form: SourceForm, flattened: &mut Vec<SourceForm>) {
        let Val::List(items) = &form.value else {
            flattened.push(form);
            return;
        };
        if !matches!(items.first(), Some(Val::Symbol(name)) if name == "begin") {
            flattened.push(form);
            return;
        }
        let Val::List(source_items) = &form.source.value else {
            flattened.push(form);
            return;
        };
        if source_items != items || form.source.children.len() != items.len() {
            flattened.push(form);
            return;
        }

        for (value, source) in items
            .iter()
            .skip(1)
            .cloned()
            .zip(form.source.children.iter().skip(1).cloned())
        {
            append(
                SourceForm {
                    value,
                    span: source.span.clone(),
                    source,
                    expansion: form.expansion.clone(),
                },
                flattened,
            );
        }
    }

    let mut flattened = Vec::new();
    for form in forms {
        append(form, &mut flattened);
    }
    flattened
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
    definition_origin: SourceOrigin,
}

#[derive(Debug, Clone)]
struct ExpandedForm {
    value: Val,
    source: SpannedVal,
    /// The innermost macro definition responsible for generated syntax. The
    /// call-site itself remains the primary SourceForm span, giving
    /// diagnostics an explicit and truthful expansion ancestry chain.
    expansion: Option<SourceOrigin>,
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
    forms: &[SpannedVal],
) -> Result<HashMap<String, TemplateMacro>, Vec<VmDiagnostic>> {
    let mut macros = HashMap::new();
    for form in forms {
        let expression = &form.value;
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
                    definition_origin: source_origin_in_range(
                        source_id,
                        source,
                        form.children
                            .get(2)
                            .map(|template| template.span.clone())
                            .unwrap_or_else(|| form.span.clone()),
                        format!("macro {name}"),
                    ),
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
            if matches!(items.first(), Some(Val::Symbol(name)) if matches!(name.as_str(), "let" | "lambda" | "define" | "define-syntax"))
            {
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
    expression: &SpannedVal,
    macros: &HashMap<String, TemplateMacro>,
    budget: &mut u16,
) -> Result<ExpandedForm, Vec<VmDiagnostic>> {
    let Val::List(items) = &expression.value else {
        return Ok(ExpandedForm {
            value: expression.value.clone(),
            source: expression.clone(),
            expansion: None,
        });
    };
    // Data is not syntax.  In particular, a quoted list that happens to
    // start with a macro name must remain a literal symbol/list value.
    if matches!(items.first(), Some(Val::Symbol(name)) if name == "quote") {
        return Ok(ExpandedForm {
            value: expression.value.clone(),
            source: expression.clone(),
            expansion: None,
        });
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
                .zip(expression.children.iter().skip(1).cloned())
                .collect::<HashMap<_, _>>();
            let expanded = substitute_macro_template(
                &definition.template,
                &bindings,
                expression.span.clone(),
            );
            let mut expanded = expand_template_macros(source_id, source, &expanded, macros, budget)?;
            let mut origin = definition.definition_origin.clone();
            origin.expansion = expanded.expansion.map(Box::new);
            expanded.expansion = Some(origin);
            return Ok(expanded);
        }
    }
    let values = expression
        .children
        .iter()
        .map(|item| expand_template_macros(source_id, source, item, macros, budget))
        .collect::<Result<Vec<_>, _>>()?;
    let expansion = values.iter().find_map(|item| item.expansion.clone());
    Ok(ExpandedForm {
        value: Val::List(values.iter().map(|item| item.value.clone()).collect()),
        source: SpannedVal {
            value: Val::List(values.iter().map(|item| item.value.clone()).collect()),
            span: expression.span.clone(),
            children: values.into_iter().map(|item| item.source).collect(),
        },
        expansion,
    })
}

fn substitute_macro_template(
    template: &Val,
    bindings: &HashMap<String, SpannedVal>,
    generated_span: Range<usize>,
) -> SpannedVal {
    match template {
        Val::Symbol(name) => bindings
            .get(name)
            .cloned()
            .unwrap_or_else(|| SpannedVal {
                value: template.clone(),
                span: generated_span,
                children: Vec::new(),
            }),
        Val::List(items) => {
            let children = items
                .iter()
                .map(|item| substitute_macro_template(item, bindings, generated_span.clone()))
                .collect::<Vec<_>>();
            SpannedVal {
                value: Val::List(children.iter().map(|child| child.value.clone()).collect()),
                span: generated_span,
                children,
            }
        }
        _ => SpannedVal {
            value: template.clone(),
            span: generated_span,
            children: Vec::new(),
        },
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
        let mut origin =
            source_origin_in_range(self.source_id, self.source, self.current_span.clone(), word);
        origin.expansion = self.current_expansion.clone().map(Box::new);
        origin
    }

    fn compile_expression_at(
        &mut self,
        expression: &Val,
        source: Option<&SpannedVal>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let Some(source) = source else {
            return self.compile_expression(expression, builder);
        };
        let previous_span = std::mem::replace(&mut self.current_span, source.span.clone());
        let previous_source = self.current_source.replace(source.clone());
        let result = self.compile_expression(expression, builder);
        self.current_span = previous_span;
        self.current_source = previous_source;
        result
    }

    fn current_list_children(&self, items: &[Val]) -> Option<&[SpannedVal]> {
        Self::source_list_children(self.current_source.as_ref()?, items)
    }

    fn source_list_children<'source>(
        source: &'source SpannedVal,
        items: &[Val],
    ) -> Option<&'source [SpannedVal]> {
        let Val::List(source_items) = &source.value else {
            return None;
        };
        (source_items.as_slice() == items).then_some(source.children.as_slice())
    }

    /// Return reader-owned span metadata for a structural suffix of the
    /// current form. Definition bodies are such a suffix after optional type,
    /// effect, and docstring metadata, so this avoids source-text searching.
    fn current_form_suffix_sources(&self, suffix: &[Val]) -> Option<Vec<SpannedVal>> {
        let source = self.current_source.as_ref()?;
        let Val::List(items) = &source.value else {
            return None;
        };
        let start = items.len().checked_sub(suffix.len())?;
        (items[start..] == *suffix).then(|| source.children[start..].to_vec())
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
        let declared_return = definition.return_type.clone();
        let parameters = definition.parameters;
        let arguments = parameters
            .iter()
            .map(|(_, ty)| ty.clone())
            .collect::<Vec<_>>();
        let mut child = FunctionBuilder::new(name, arguments);
        child.return_result = declared_return.as_ref().and_then(|ty| match ty {
            Type::Result(ok, error) => Some(((**ok).clone(), (**error).clone())),
            _ => None,
        });
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
        let body_sources = self.current_form_suffix_sources(definition.body);
        let result_type = self.compile_begin(definition.body, body_sources.as_deref(), &mut child)?;
        if let Some(expected) = &declared_return {
            if !declared_type_accepts(expected, &result_type) {
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
        if declared_return.is_some() {
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
        if let Some(return_type) = declared_return {
            // The annotation is a public call contract, not merely a check
            // against this body's initially inferred `dynamic` component.
            // This preserves the exact `result<T,E>` error type for callers
            // and recursive definitions while the verifier still validates
            // the body's concrete return against it.
            function.signature.output = StackRow::polymorphic("S", vec![return_type]);
        }
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
            suspension: None,
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
        // Source children are owned by the reader tree, not reconstructed by
        // text matching. Clone this small metadata vector before recursive
        // lowering so mutable compiler operations can temporarily enter each
        // argument's exact source context.
        let source_children = self.current_list_children(items).map(ToOwned::to_owned);
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
            _ => {
                return self.compile_closure_call(
                    &items[0],
                    &items[1..],
                    source_children.as_deref().and_then(|children| children.first()),
                    source_children.as_deref().and_then(|children| children.get(1..)),
                    builder,
                )
            }
        };
        match operator {
            "begin" => self.compile_begin(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
            ),
            "let" => self.compile_let(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
            ),
            "if" => self.compile_if(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
            ),
            "match" => self.compile_match(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
            ),
            "match-option" => self.compile_match_option(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
            ),
            "match-result" => self.compile_match_result(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
            ),
            "try" => self.compile_try(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
            ),
            "while" => self.compile_while(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
            ),
            "break" => self.compile_loop_exit("break", &items[1..], builder),
            "continue" => self.compile_loop_exit("continue", &items[1..], builder),
            "lambda" => self.compile_lambda(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
            ),
            "defer" => self.compile_defer(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
            ),
            "defer-cpu" => self.compile_defer_cpu(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
            ),
            "task-poll" => self.compile_cpu_task_operation(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
                false,
            ),
            "task-join" => self.compile_cpu_task_operation(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
                true,
            ),
            "task-cancel" => self.compile_cpu_task_cancel(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
            ),
            "fiber-next" => self.compile_fiber_operation(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
                "fiber-next",
            ),
            "fiber-join" => self.compile_fiber_operation(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
                "fiber-join",
            ),
            "fiber-cancel" => self.compile_fiber_operation(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
                "fiber-cancel",
            ),
            "list" => self.compile_list_value(&items[1..], builder),
            "empty-list" => self.compile_empty_list(&items[1..], builder),
            "map" => self.compile_map_value(&items[1..], builder),
            "empty-map" => self.compile_empty_map(&items[1..], builder),
            "finch-record-literal" => self.compile_record_value(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
            ),
            "record-get" => self.compile_record_get(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
            ),
            "record-set" => self.compile_record_set(
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                builder,
            ),
            _ if builder.resolve(operator).is_some() => {
                self.compile_closure_call(
                    &items[0],
                    &items[1..],
                    source_children.as_deref().and_then(|children| children.first()),
                    source_children.as_deref().and_then(|children| children.get(1..)),
                    builder,
                )
            }
            _ => self.compile_named_call(
                operator,
                &items[1..],
                source_children.as_deref().and_then(|children| children.get(1..)),
                source_children.as_deref().and_then(|children| children.first()),
                builder,
            ),
        }
    }

    fn compile_begin(
        &mut self,
        expressions: &[Val],
        expression_sources: Option<&[SpannedVal]>,
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
        for (index, expression) in expressions[..expressions.len() - 1].iter().enumerate() {
            self.compile_expression_at(
                expression,
                expression_sources.and_then(|sources| sources.get(index)),
                builder,
            )?;
            builder.stack.pop();
            builder.emit(Instruction::Drop, self.origin("begin"));
        }
        let last_index = expressions.len() - 1;
        self.compile_expression_at(
            &expressions[last_index],
            expression_sources.and_then(|sources| sources.get(last_index)),
            builder,
        )?;
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
        expression_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if expressions.len() != 1 {
            return Err(vec![self.error(
                "E-FIBER-003",
                "defer-cpu requires exactly one zero-argument closure",
            )]);
        }
        let closure_type = self.compile_expression_at(
            &expressions[0],
            expression_sources.and_then(|sources| sources.first()),
            builder,
        )?;
        let Type::Function {
            arguments,
            result,
            effects,
            suspension,
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
        if let Some(suspension) = suspension {
            if *suspension.yield_type != Type::Unit
                || *suspension.resume_type != Type::Unit
            {
                return Err(vec![self.error(
                    "E-YIELD-003",
                    "CPU tasks may yield only unit timeslices; use defer for produced values",
                )]);
            }
        }
        builder.stack.pop();
        builder.emit(Instruction::DeferCpu, self.origin("defer-cpu"));
        Ok(Type::Task(result))
    }

    fn compile_defer_fiber(
        &mut self,
        expressions: &[Val],
        expression_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if expressions.len() != 1 {
            return Err(vec![self.error(
                "E-FIBER-021",
                "defer requires exactly one zero-argument yielding closure",
            )]);
        }
        let closure_type = self.compile_expression_at(
            &expressions[0],
            expression_sources.and_then(|sources| sources.first()),
            builder,
        )?;
        let Type::Function {
            arguments,
            result,
            effects,
            suspension: Some(suspension),
        } = closure_type
        else {
            return Err(vec![self.error(
                "E-FIBER-021",
                "defer requires a typed yielding closure; use defer :cpu for terminal CPU work",
            )]);
        };
        if !arguments.is_empty() || !effects.is_pure() {
            return Err(vec![self.error(
                "E-FIBER-022",
                "cooperative defer requires a pure zero-argument closure",
            )]);
        }
        if *suspension.resume_type != Type::Unit {
            return Err(vec![self.error(
                "E-FIBER-023",
                "this runtime version supports only unit-resumed producer fibers",
            )]);
        }
        builder.stack.pop();
        builder.emit(Instruction::DeferFiber, self.origin("defer"));
        Ok(Type::Fiber(suspension.yield_type, result))
    }

    /// User-facing deferred-work form. Omitting a mode creates a cooperative
    /// producer; `:cpu` explicitly requests the bounded native worker pool.
    fn compile_defer(
        &mut self,
        expressions: &[Val],
        expression_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if expressions.len() == 1 {
            return self.compile_defer_fiber(expressions, expression_sources, builder);
        }
        if expressions.len() != 2 {
            return Err(vec![self.error(
                "E-FIBER-003",
                "defer requires one closure, optionally preceded by :fiber or :cpu",
            )]);
        }
        match &expressions[0] {
            Val::Symbol(mode) if mode == ":cpu" => {
                self.compile_defer_cpu(
                    &expressions[1..],
                    expression_sources.and_then(|sources| sources.get(1..)),
                    builder,
                )
            }
            Val::Symbol(mode) if mode == ":fiber" => self.compile_defer_fiber(
                &expressions[1..],
                expression_sources.and_then(|sources| sources.get(1..)),
                builder,
            ),
            Val::Symbol(mode) => Err(vec![self.error(
                "E-FIBER-019",
                format!("unsupported defer mode '{mode}'; supported modes: :fiber, :cpu"),
            )]),
            _ => Err(vec![self.error(
                "E-FIBER-019",
                "defer mode must be a symbol such as :fiber or :cpu",
            )]),
        }
    }

    fn compile_fiber_operation(
        &mut self,
        expressions: &[Val],
        expression_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
        operation: &str,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if expressions.len() != 1 {
            return Err(vec![self.error(
                "E-FIBER-024",
                format!("{operation} requires exactly one fiber<Y,R>"),
            )]);
        }
        let fiber_type = self.compile_expression_at(
            &expressions[0],
            expression_sources.and_then(|sources| sources.first()),
            builder,
        )?;
        let Type::Fiber(yield_type, result_type) = fiber_type else {
            return Err(vec![self.error(
                "E-FIBER-024",
                format!("{operation} requires fiber<Y,R>"),
            )]);
        };
        builder.stack.pop();
        let (instruction, result) = match operation {
            "fiber-next" => (
                Instruction::NextFiber,
                Type::fiber_step((*yield_type).clone(), (*result_type).clone()),
            ),
            "fiber-join" => (Instruction::JoinFiber, *result_type),
            "fiber-cancel" => (Instruction::CancelFiber, Type::Unit),
            _ => unreachable!("known fiber operation"),
        };
        builder.emit(instruction, self.origin(operation));
        Ok(result)
    }

    fn compile_cpu_task_operation(
        &mut self,
        expressions: &[Val],
        expression_sources: Option<&[SpannedVal]>,
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
        let task_type = self.compile_expression_at(
            &expressions[0],
            expression_sources.and_then(|sources| sources.first()),
            builder,
        )?;
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
        expression_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if expressions.len() != 1 {
            return Err(vec![
                self.error("E-FIBER-020", "task-cancel requires exactly one task<T>")
            ]);
        }
        let task_type = self.compile_expression_at(
            &expressions[0],
            expression_sources.and_then(|sources| sources.first()),
            builder,
        )?;
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

    fn compile_empty_list(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let [Val::Symbol(element)] = expressions else {
            return Err(vec![self.error(
                "E-LIST-005",
                "empty-list requires one element type name, for example (empty-list string)",
            )]);
        };
        let element_type = parse_type_name(element).map_err(|_| {
            vec![self.error(
                "E-LIST-005",
                format!("unknown list element type '{element}'"),
            )]
        })?;
        builder.emit(
            Instruction::MakeList {
                element_type: element_type.clone(),
                count: 0,
            },
            self.origin("empty-list"),
        );
        Ok(Type::list(element_type))
    }

    fn compile_map_value(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if expressions.is_empty() || expressions.len() % 2 != 0 {
            return Err(vec![
                self.error("E-MAP-001", "map requires one or more key/value pairs")
            ]);
        }
        let key_type = self.compile_expression(&expressions[0], builder)?;
        let value_type = self.compile_expression(&expressions[1], builder)?;
        for pair in expressions[2..].chunks_exact(2) {
            let found_key = self.compile_expression(&pair[0], builder)?;
            if !key_type.accepts(&found_key) {
                return Err(vec![VmDiagnostic::type_mismatch(
                    key_type.clone(),
                    found_key,
                    Some(self.origin("map")),
                )]);
            }
            let found_value = self.compile_expression(&pair[1], builder)?;
            if !value_type.accepts(&found_value) {
                return Err(vec![VmDiagnostic::type_mismatch(
                    value_type.clone(),
                    found_value,
                    Some(self.origin("map")),
                )]);
            }
        }
        for _ in expressions {
            builder.stack.pop();
        }
        builder.emit(
            Instruction::MakeMap {
                key_type: key_type.clone(),
                value_type: value_type.clone(),
                count: (expressions.len() / 2) as u32,
            },
            self.origin("map"),
        );
        Ok(Type::Map(Box::new(key_type), Box::new(value_type)))
    }

    fn compile_empty_map(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let [Val::Symbol(key), Val::Symbol(value)] = expressions else {
            return Err(vec![self.error(
                "E-MAP-005",
                "empty-map requires key and value type names, for example (empty-map string int)",
            )]);
        };
        let key_type = parse_type_name(key)
            .map_err(|_| vec![self.error("E-MAP-005", format!("unknown map key type '{key}'"))])?;
        let value_type = parse_type_name(value).map_err(|_| {
            vec![self.error("E-MAP-005", format!("unknown map value type '{value}'"))]
        })?;
        builder.emit(
            Instruction::MakeMap {
                key_type: key_type.clone(),
                value_type: value_type.clone(),
                count: 0,
            },
            self.origin("empty-map"),
        );
        Ok(Type::Map(Box::new(key_type), Box::new(value_type)))
    }

    /// Construct a heterogeneous immutable product. Field labels are syntax,
    /// not runtime strings, so the generated record type retains exact field
    /// names and each field's independent type.
    fn compile_record_value(
        &mut self,
        fields: &[Val],
        field_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let mut record_fields = Vec::with_capacity(fields.len());
        for (index, field) in fields.iter().enumerate() {
            let Val::List(parts) = field else {
                return Err(vec![self.error(
                    "E-RECORD-001",
                    "record fields must be (name value) pairs",
                )]);
            };
            let [Val::Symbol(name), value] = parts.as_slice() else {
                return Err(vec![self.error(
                    "E-RECORD-001",
                    "record fields must be (name value) pairs",
                )]);
            };
            if record_fields.iter().any(|(existing, _)| existing == name) {
                return Err(vec![self.error(
                    "E-RECORD-001",
                    format!("record field '{name}' is declared more than once"),
                )]);
            }
            let value_source = field_sources
                .and_then(|sources| sources.get(index))
                .and_then(|source| source.children.get(1));
            let value_type = self.compile_expression_at(value, value_source, builder)?;
            record_fields.push((name.clone(), value_type));
        }
        for _ in fields {
            builder.stack.pop();
        }
        builder.emit(
            Instruction::MakeRecord {
                fields: record_fields.clone(),
            },
            self.origin("record"),
        );
        Ok(Type::Record(record_fields))
    }

    /// Project a statically named field. Dynamic lookup belongs to the managed
    /// JSON boundary; a typed record stays a product with a known field set.
    fn compile_record_get(
        &mut self,
        expressions: &[Val],
        expression_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let [record, Val::Str(field)] = expressions else {
            return Err(vec![self.error(
                "E-RECORD-004",
                "record-get requires a record expression and a literal field name string",
            )]);
        };
        let Type::Record(fields) = self.compile_expression_at(
            record,
            expression_sources.and_then(|sources| sources.first()),
            builder,
        )? else {
            return Err(vec![self.error(
                "E-RECORD-004",
                "record-get requires a typed record",
            )]);
        };
        let field_type = self.compile_expression_at(
            &expressions[1],
            expression_sources.and_then(|sources| sources.get(1)),
            builder,
        )?;
        if field_type != Type::String {
            return Err(vec![self.error(
                "E-RECORD-004", "record-get field name must be a literal string",
            )]);
        }
        let Some((_, value_type)) = fields.iter().find(|(name, _)| name == field) else {
            return Err(vec![self.error(
                "E-RECORD-005",
                format!("record has no field '{field}'"),
            )]);
        };
        let value_type = value_type.clone();
        builder.stack.pop();
        builder.stack.pop();
        builder.emit(
            Instruction::RecordGet {
                field: field.clone(),
                value_type: value_type.clone(),
            },
            self.origin("record-get"),
        );
        Ok(Type::Option(Box::new(value_type)))
    }

    /// Replace a statically named field by constructing a new typed record.
    /// The original value is not mutated, so aliases and captured values retain
    /// ordinary persistent-value semantics.
    fn compile_record_set(
        &mut self,
        expressions: &[Val],
        expression_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let [record, Val::Str(field), value] = expressions else {
            return Err(vec![self.error(
                "E-RECORD-007",
                "record-set requires a record expression, literal field name string, and replacement value",
            )]);
        };
        let Type::Record(fields) = self.compile_expression_at(
            record,
            expression_sources.and_then(|sources| sources.first()),
            builder,
        )? else {
            return Err(vec![self.error(
                "E-RECORD-004", "record-set requires a typed record",
            )]);
        };
        let value_type = self.compile_expression_at(
            value,
            expression_sources.and_then(|sources| sources.get(2)),
            builder,
        )?;
        // Co-Forth's public stack spelling is `record value "field"
        // record-set`. Keep the ordinary Lisp argument order while lowering
        // its compile-time-known field literal after the replacement value.
        let field_type = self.compile_expression_at(
            &expressions[1],
            expression_sources.and_then(|sources| sources.get(1)),
            builder,
        )?;
        if field_type != Type::String {
            return Err(vec![self.error(
                "E-RECORD-007", "record-set field name must be a literal string",
            )]);
        }
        let Some((_, expected)) = fields.iter().find(|(name, _)| name == field) else {
            return Err(vec![self.error(
                "E-RECORD-005", format!("record has no field '{field}'"),
            )]);
        };
        if expected != &value_type {
            return Err(vec![VmDiagnostic::type_mismatch(
                expected.clone(), value_type, Some(self.origin("record-set")),
            )]);
        }
        builder.stack.pop();
        builder.stack.pop();
        builder.stack.pop();
        builder.emit(
            Instruction::RecordSet {
                field: field.clone(),
                value_type: expected.clone(),
                record_type: fields.clone(),
            },
            self.origin("record-set"),
        );
        Ok(Type::Record(fields))
    }

    fn compile_let(
        &mut self,
        expressions: &[Val],
        expression_sources: Option<&[SpannedVal]>,
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
        let result = self.compile_begin(
            &expressions[1..],
            expression_sources.and_then(|sources| sources.get(1..)),
            builder,
        );
        builder.scopes.pop();
        result
    }

    fn compile_if(
        &mut self,
        expressions: &[Val],
        expression_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if expressions.len() != 3 {
            return Err(vec![
                self.error("E-LISP-005", "if requires condition, then, and else")
            ]);
        }
        let condition = self.compile_expression_at(
            &expressions[0],
            expression_sources.and_then(|sources| sources.first()),
            builder,
        )?;
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
        let then_type = self.compile_expression_at(
            &expressions[1],
            expression_sources.and_then(|sources| sources.get(1)),
            builder,
        )?;
        let then_stack = builder.stack.clone();
        builder.emit(
            Instruction::Jump {
                target: merge_block,
            },
            self.origin("if/then"),
        );

        builder.switch_to(else_block, branch_stack);
        let else_type = self.compile_expression_at(
            &expressions[2],
            expression_sources.and_then(|sources| sources.get(2)),
            builder,
        )?;
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
        expression_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let [option_expression, some_arm, none_arm] = expressions else {
            return Err(vec![self.error(
                "E-LISP-MATCH-001",
                "match-option requires an option expression, (some name body...), and (none body...)",
            )]);
        };
        let Type::Option(inner) = self.compile_expression_at(
            option_expression,
            expression_sources.and_then(|sources| sources.first()),
            builder,
        )? else {
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
        let some_sources = expression_sources
            .and_then(|sources| sources.get(1))
            .and_then(|source| Self::source_list_children(source, some_items))
            .and_then(|sources| sources.get(2..));
        let some_type = self.compile_begin(some_body, some_sources, builder)?;
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
        let none_sources = expression_sources
            .and_then(|sources| sources.get(2))
            .and_then(|source| Self::source_list_children(source, none_items))
            .and_then(|sources| sources.get(1..));
        let none_type = self.compile_begin(none_body, none_sources, builder)?;
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
        expression_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if expressions.len() < 2 {
            return Err(vec![self.error(
                "E-LISP-MATCH-009",
                "match requires a value and exhaustive arms",
            )]);
        }
        let arm_marker = |arm: &Val| match arm {
            Val::List(items) => match items.first() {
                Some(Val::Symbol(marker)) => Some(marker.clone()),
                _ => None,
            },
            _ => None,
        };
        if expressions.len() >= 3 {
            let first_arm = &expressions[1];
            let second_arm = &expressions[2];
            match (arm_marker(first_arm), arm_marker(second_arm)) {
                (Some(first), Some(second)) if first == "some" && second == "none" => {
                    return self.compile_match_option(expressions, expression_sources, builder);
                }
                (Some(first), Some(second)) if first == "ok" && second == "err" => {
                    return self.compile_match_result(expressions, expression_sources, builder);
                }
                _ => {}
            }
        }
        self.compile_literal_match(expressions, expression_sources, builder)
    }

    /// Compile the non-dynamic literal subset of `match`.
    ///
    /// Booleans require exactly `(true body...)` and `(false body...)` arms.
    /// Integers use zero or more literal arms followed by one required final
    /// `(_ body...)` arm, so every branch remains total and the same typed
    /// merge rules as `if` apply.  This is deliberately not a general runtime
    /// pattern dispatcher: strings, records, lists, and JSON retain their
    /// explicit typed operations until their structural patterns have a
    /// verified lowering design.
    fn compile_literal_match(
        &mut self,
        expressions: &[Val],
        expression_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let Some((value, arms)) = expressions.split_first() else {
            return Err(vec![self.error(
                "E-LISP-MATCH-009",
                "match requires a value and exhaustive arms",
            )]);
        };
        let value_type = self.compile_expression_at(
            value,
            expression_sources.and_then(|sources| sources.first()),
            builder,
        )?;
        match value_type {
            Type::Bool => self.compile_boolean_match(arms, expression_sources, builder),
            Type::Int => self.compile_integer_match(arms, expression_sources, builder),
            found => Err(vec![VmDiagnostic::type_mismatch(
                Type::Bool,
                found,
                Some(self.origin("match")),
            )]),
        }
    }

    fn compile_boolean_match(
        &mut self,
        arms: &[Val],
        expression_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if arms.len() != 2 {
            return Err(vec![self.error(
                "E-LISP-MATCH-010",
                "boolean match requires exactly (true body...) and (false body...) arms",
            )]);
        }
        let parse_arm = |arm: &Val,
                         expected: bool|
         -> Result<(Vec<Val>, Vec<Val>), Vec<VmDiagnostic>> {
            let Val::List(items) = arm else {
                return Err(vec![self.error(
                    "E-LISP-MATCH-010",
                    "boolean match arms must be (true body...) and (false body...)",
                )]);
            };
            let [Val::Bool(found), body @ ..] = items.as_slice() else {
                return Err(vec![self.error(
                    "E-LISP-MATCH-010",
                    "boolean match arms must be (true body...) and (false body...)",
                )]);
            };
            if *found != expected || body.is_empty() {
                return Err(vec![self.error(
                    "E-LISP-MATCH-010",
                    "boolean match arms must be (true body...) and (false body...)",
                )]);
            }
            Ok((items.clone(), body.to_vec()))
        };

        let (true_items, true_body) = parse_arm(&arms[0], true)?;
        let (false_items, false_body) = parse_arm(&arms[1], false)?;
        builder.stack.pop();
        let branch_stack = builder.stack.clone();
        let true_block = builder.new_block();
        let false_block = builder.new_block();
        let merge_block = builder.new_block();
        builder.emit(
            Instruction::Branch {
                then_block: true_block,
                else_block: false_block,
            },
            self.origin("match/bool-test"),
        );

        builder.switch_to(true_block, branch_stack.clone());
        let true_sources = expression_sources
            .and_then(|sources| sources.get(1))
            .and_then(|source| Self::source_list_children(source, &true_items))
            .and_then(|sources| sources.get(1..));
        let true_type = self.compile_begin(&true_body, true_sources, builder)?;
        builder.stack.push(true_type.clone());
        let true_stack = builder.stack.clone();
        builder.emit(
            Instruction::Jump { target: merge_block },
            self.origin("match/true-end"),
        );

        builder.switch_to(false_block, branch_stack);
        let false_sources = expression_sources
            .and_then(|sources| sources.get(2))
            .and_then(|source| Self::source_list_children(source, &false_items))
            .and_then(|sources| sources.get(1..));
        let false_type = self.compile_begin(&false_body, false_sources, builder)?;
        builder.stack.push(false_type.clone());
        if true_type != false_type || builder.stack != true_stack {
            return Err(vec![VmDiagnostic::type_mismatch(
                true_type,
                false_type,
                Some(self.origin("match")),
            )]);
        }
        builder.emit(
            Instruction::Jump { target: merge_block },
            self.origin("match/false-end"),
        );
        builder.switch_to(merge_block, true_stack);
        Ok(builder.stack.pop().expect("boolean match leaves a value"))
    }

    fn compile_integer_match(
        &mut self,
        arms: &[Val],
        expression_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if arms.len() < 2 {
            return Err(vec![self.error(
                "E-LISP-MATCH-011",
                "integer match requires literal arms and a final (_ body...) arm",
            )]);
        }

        let mut parsed = Vec::with_capacity(arms.len());
        let mut seen = HashSet::new();
        for (index, arm) in arms.iter().enumerate() {
            let Val::List(items) = arm else {
                return Err(vec![self.error(
                    "E-LISP-MATCH-011",
                    "integer match arms must be (integer body...) followed by (_ body...)",
                )]);
            };
            let Some((pattern, body)) = items.split_first() else {
                return Err(vec![self.error(
                    "E-LISP-MATCH-011",
                    "integer match arms must have a pattern and at least one body expression",
                )]);
            };
            if body.is_empty() {
                return Err(vec![self.error(
                    "E-LISP-MATCH-011",
                    "integer match arms must have a pattern and at least one body expression",
                )]);
            }
            let pattern = match pattern {
                Val::Int(value) if index + 1 < arms.len() => Some(*value),
                Val::Symbol(name) if name == "_" && index + 1 == arms.len() => None,
                _ => {
                    return Err(vec![self.error(
                        "E-LISP-MATCH-011",
                        "integer match requires unique integer literal arms followed by one final (_ body...) arm",
                    )]);
                }
            };
            if let Some(value) = pattern {
                if !seen.insert(value) {
                    return Err(vec![self.error(
                        "E-LISP-MATCH-011",
                        "integer match literal arms must be unique",
                    )]);
                }
            }
            parsed.push((pattern, items.clone(), body.to_vec()));
        }

        // Store the selector once. Each comparison reloads a typed immutable
        // local, so branch compilation cannot accidentally consume it.
        let selector = builder.allocate_local(Type::Int);
        builder.stack.pop();
        builder.emit(
            Instruction::LocalSet { index: selector },
            self.origin("match/int-selector"),
        );
        let branch_stack = builder.stack.clone();
        let merge_block = builder.new_block();
        let mut result: Option<(Type, Vec<Type>)> = None;

        for (arm_index, (pattern, items, body)) in parsed.into_iter().enumerate() {
            if let Some(pattern) = pattern {
                let arm_block = builder.new_block();
                let next_block = builder.new_block();
                builder.emit(
                    Instruction::LocalGet { index: selector },
                    self.origin("match/int-selector"),
                );
                builder.stack.push(Type::Int);
                builder.emit(
                    Instruction::Constant {
                        value: TypedValue::Int(pattern),
                    },
                    self.origin("match/int-literal"),
                );
                builder.stack.push(Type::Int);
                builder.emit(
                    Instruction::Call {
                        function: "=".into(),
                    },
                    self.origin("match/int-test"),
                );
                builder.stack.pop();
                builder.stack.pop();
                builder.stack.push(Type::Bool);
                builder.stack.pop();
                builder.emit(
                    Instruction::Branch {
                        then_block: arm_block,
                        else_block: next_block,
                    },
                    self.origin("match/int-test"),
                );

                builder.switch_to(arm_block, branch_stack.clone());
                let body_sources = expression_sources
                    .and_then(|sources| sources.get(arm_index + 1))
                    .and_then(|source| Self::source_list_children(source, &items))
                    .and_then(|sources| sources.get(1..));
                let ty = self.compile_begin(&body, body_sources, builder)?;
                builder.stack.push(ty.clone());
                let stack = builder.stack.clone();
                if let Some((expected, expected_stack)) = &result {
                    if *expected != ty || *expected_stack != stack {
                        return Err(vec![VmDiagnostic::type_mismatch(
                            expected.clone(),
                            ty,
                            Some(self.origin("match")),
                        )]);
                    }
                } else {
                    result = Some((ty, stack));
                }
                builder.emit(
                    Instruction::Jump { target: merge_block },
                    self.origin("match/int-arm-end"),
                );
                builder.switch_to(next_block, branch_stack.clone());
            } else {
                let body_sources = expression_sources
                    .and_then(|sources| sources.get(arm_index + 1))
                    .and_then(|source| Self::source_list_children(source, &items))
                    .and_then(|sources| sources.get(1..));
                let ty = self.compile_begin(&body, body_sources, builder)?;
                builder.stack.push(ty.clone());
                let stack = builder.stack.clone();
                if let Some((expected, expected_stack)) = &result {
                    if *expected != ty || *expected_stack != stack {
                        return Err(vec![VmDiagnostic::type_mismatch(
                            expected.clone(),
                            ty,
                            Some(self.origin("match")),
                        )]);
                    }
                } else {
                    result = Some((ty, stack));
                }
                builder.emit(
                    Instruction::Jump { target: merge_block },
                    self.origin("match/default-end"),
                );
            }
        }

        let Some((_result_type, result_stack)) = result else {
            unreachable!("a total integer match always has a default arm");
        };
        builder.switch_to(merge_block, result_stack);
        Ok(builder.stack.pop().expect("integer match leaves a value"))
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
        expression_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let [result_expression, ok_arm, err_arm] = expressions else {
            return Err(vec![self.error(
                "E-LISP-MATCH-005",
                "match-result requires a result expression, (ok name body...), and (err name body...)",
            )]);
        };
        let Type::Result(ok_type, err_type) = self.compile_expression_at(
            result_expression,
            expression_sources.and_then(|sources| sources.first()),
            builder,
        )?
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
        let ok_sources = expression_sources
            .and_then(|sources| sources.get(1))
            .and_then(|source| match ok_arm {
                Val::List(items) => Self::source_list_children(source, items),
                _ => None,
            })
            .and_then(|sources| sources.get(2..));
        let ok_result = self.compile_begin(&ok_body, ok_sources, builder)?;
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
        let err_sources = expression_sources
            .and_then(|sources| sources.get(2))
            .and_then(|source| match err_arm {
                Val::List(items) => Self::source_list_children(source, items),
                _ => None,
            })
            .and_then(|sources| sources.get(2..));
        let err_result = self.compile_begin(&err_body, err_sources, builder)?;
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

    /// Lower `(try expression)` to the shared result-propagation instruction.
    /// The success value remains an ordinary expression value; the error edge
    /// returns directly from the enclosing typed `result<R,E>` definition.
    fn compile_try(
        &mut self,
        expressions: &[Val],
        expression_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let [expression] = expressions else {
            return Err(vec![self.error(
                "E-RESULT-TRY-001",
                "try requires exactly one result<T,E> expression",
            )]);
        };
        let (return_ok_type, return_error_type) = builder.return_result.clone().ok_or_else(|| {
            vec![self.error(
                "E-RESULT-TRY-002",
                "try is valid only inside a typed definition returning result<T,E>",
            )]
        })?;
        let result_type = self.compile_expression_at(
            expression,
            expression_sources.and_then(|sources| sources.first()),
            builder,
        )?;
        let Type::Result(ok_type, error_type) = result_type else {
            return Err(vec![self.error(
                "E-RESULT-TRY-001",
                "try requires a result<T,E> expression",
            )]);
        };
        if !return_error_type.accepts(&error_type) {
            return Err(vec![VmDiagnostic::type_mismatch(
                return_error_type,
                *error_type,
                Some(self.origin("try")),
            )]);
        }
        builder.stack.pop();
        builder.emit(
            Instruction::PropagateResult {
                return_ok_type,
                error_type: (*error_type).clone(),
            },
            self.origin("try"),
        );
        Ok(*ok_type)
    }

    fn compile_while(
        &mut self,
        expressions: &[Val],
        expression_sources: Option<&[SpannedVal]>,
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
        let condition = self.compile_expression_at(
            &expressions[condition_index],
            expression_sources.and_then(|sources| sources.get(condition_index)),
            builder,
        )?;
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
        let body = self.compile_begin(
            &expressions[condition_index + 1..],
            expression_sources.and_then(|sources| sources.get(condition_index + 1..)),
            builder,
        );
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
        expression_sources: Option<&[SpannedVal]>,
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
        let result_type = self.compile_begin(
            &expressions[1..],
            expression_sources.and_then(|sources| sources.get(1..)),
            &mut child,
        )?;
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
            suspension: signature.suspension,
        })
    }

    fn compile_named_call(
        &mut self,
        operator: &str,
        arguments: &[Val],
        argument_sources: Option<&[SpannedVal]>,
        operator_source: Option<&SpannedVal>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        let word = match operator {
            "string-append" => "str-cat",
            other => other,
        };
        let origin = operator_source.map_or_else(
            || self.origin(operator),
            |source| {
                let mut origin = source_origin_in_range(
                    self.source_id,
                    self.source,
                    source.span.clone(),
                    operator,
                );
                origin.expansion = self.current_expansion.clone().map(Box::new);
                origin
            },
        );
        if word == "yield" && arguments.is_empty() {
            // `(yield)` is only source sugar for yielding the ordinary unit
            // value. The IR and scheduler still see the same typed payload
            // as `(yield nil)` and Co-Forth `unit yield`.
            builder.emit(
                Instruction::Constant {
                    value: TypedValue::Unit,
                },
                origin.clone(),
            );
            builder.emit(
                Instruction::Yield {
                    value_type: Type::Unit,
                },
                origin.clone(),
            );
            builder.merge_suspension(
                Some(&SuspensionSignature::one_way(Type::Unit)),
                &origin,
            )?;
            builder.emit(
                Instruction::Constant {
                    value: TypedValue::Unit,
                },
                origin,
            );
            return Ok(Type::Unit);
        }
        let Some(signature) = self.vocabulary.get(word).cloned() else {
            return Err(vec![VmDiagnostic::error(
                "E-LINK-002",
                DiagnosticPhase::Linking,
                format!("unknown Lisp function '{operator}'"),
                Some(origin),
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
        for (index, argument) in arguments.iter().enumerate() {
            self.compile_expression_at(
                argument,
                argument_sources.and_then(|sources| sources.get(index)),
                builder,
            )?;
        }
        let concrete_signature = instantiate_signature_types(&signature, &builder.stack, &origin)
            .map_err(|diagnostic| vec![diagnostic])?;
        apply_signature_types(&signature, &mut builder.stack, &origin)
            .map_err(|diagnostic| vec![diagnostic])?;
        builder.effects = builder.effects.union(&signature.effects);
        builder.merge_suspension(concrete_signature.suspension.as_ref(), &origin)?;
        if word == "yield" {
            // The argument is consumed as the typed yielded payload. Lisp
            // remains expression-oriented, so resuming the one-way form
            // evaluates to unit.
            let value_type = concrete_signature
                .input
                .values
                .last()
                .cloned()
                .expect("yield has one typed input");
            builder.emit(Instruction::Yield { value_type }, origin.clone());
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
                input: concrete_signature.input.values.clone(),
                output: concrete_signature.output.values.clone(),
            }
        } else if signature.effects.0.len() == 1 {
            Instruction::CapabilityRequest {
                requirement: signature.effects.0.iter().next().unwrap().clone(),
                input: concrete_signature.input.values.clone(),
                output: concrete_signature.output.values.clone(),
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
        target_source: Option<&SpannedVal>,
        argument_sources: Option<&[SpannedVal]>,
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        for (index, argument) in arguments.iter().enumerate() {
            self.compile_expression_at(
                argument,
                argument_sources.and_then(|sources| sources.get(index)),
                builder,
            )?;
        }
        let closure_type = self.compile_expression_at(target, target_source, builder)?;
        let Type::Function {
            arguments: expected,
            result,
            effects,
            suspension,
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
            control: if suspension.is_some() {
                ControlEffect::MaySuspend
            } else {
                ControlEffect::Returns
            },
            suspension: suspension.clone(),
        };
        builder.stack.pop();
        apply_signature_types(&signature, &mut builder.stack, &self.origin("call"))
            .map_err(|diagnostic| vec![diagnostic])?;
        builder.effects = builder.effects.union(&effects);
        builder.merge_suspension(suspension.as_ref(), &self.origin("call"))?;
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

/// Match the verifier's permissive treatment of an unresolved `dynamic`
/// component inside a structured value.  Core `ok`/`err` constructors infer
/// only the component they receive, so a definition's explicit
/// `result<T,E>` contract is the contextual information that closes the
/// other component without discarding the structure of the check.
fn declared_type_accepts(expected: &Type, actual: &Type) -> bool {
    if expected.accepts(actual) {
        return true;
    }
    match (expected, actual) {
        (Type::List(expected), Type::List(actual))
        | (Type::Option(expected), Type::Option(actual))
        | (Type::Task(expected), Type::Task(actual))
        | (Type::Stream(expected), Type::Stream(actual)) => declared_type_accepts(expected, actual),
        (Type::Map(expected_key, expected_value), Type::Map(actual_key, actual_value))
        | (Type::Result(expected_key, expected_value), Type::Result(actual_key, actual_value)) => {
            declared_type_accepts(expected_key, actual_key)
                && declared_type_accepts(expected_value, actual_value)
        }
        (Type::Record(expected_fields), Type::Record(actual_fields)) => {
            expected_fields.len() == actual_fields.len()
                && expected_fields.iter().zip(actual_fields).all(
                    |((expected_name, expected_type), (actual_name, actual_type))| {
                        expected_name == actual_name
                            && declared_type_accepts(expected_type, actual_type)
                    },
                )
        }
        _ => false,
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
        "json" => Ok(Type::Json),
        "dynamic" | "any" => Ok(Type::Dynamic),
        _ => parse_record_type(name).or_else(|| parse_generic_type(name)).ok_or_else(|| {
            vec![VmDiagnostic::error(
                "E-TYPE-009",
                DiagnosticPhase::TypeInference,
                format!("unknown type '{name}'"),
                None,
            )]
        }),
    }
}

/// Parse a fixed, named product type. Records deliberately use braces rather
/// than angle brackets so `record{name:string,age:int}` remains visually and
/// semantically distinct from an open `map<string,string>`.
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
        parsed.push((field_name.to_string(), parse_type_name(field_type.trim()).ok()?));
    }
    Some(Type::Record(parsed))
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
        // `fn<R>` is a pure zero-argument closure and `fn<A,B,R>` is a
        // pure closure from A,B to R. Effectful/suspending function types are
        // inferred from quotation bodies and deliberately have no lossy
        // compact annotation yet.
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

fn source_origin(source_id: &str, source: &str, word: impl Into<String>) -> SourceOrigin {
    source_origin_in_range(source_id, source, 0..source.len(), word)
}

fn source_origin_in_range(
    source_id: &str,
    source: &str,
    range: Range<usize>,
    word: impl Into<String>,
) -> SourceOrigin {
    let range = range.start.min(source.len())
        ..range
            .end
            .min(source.len())
            .max(range.start.min(source.len()));
    let (start_line, start_column) = source_position(source, range.start);
    let (end_line, end_column) = source_position(source, range.end);
    SourceOrigin {
        language: SourceLanguage::Lisp,
        span: Some(SourceSpan {
            source_id: source_id.to_string(),
            start_byte: range.start,
            end_byte: range.end,
            start_line,
            start_column,
            end_line,
            end_column,
        }),
        word: Some(word.into()),
        expansion: None,
    }
}

fn source_position(source: &str, byte: usize) -> (usize, usize) {
    let prefix = &source[..byte];
    let line = prefix
        .bytes()
        .filter(|character| *character == b'\n')
        .count()
        + 1;
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
    fn macro_expansion_origins_link_call_sites_to_template_definitions() {
        let source = "(define-syntax (announce text) (say text))\n(announce \"hello\")";
        let module = compile_lisp("macros.lisp", source, Vec::new(), &core_vocabulary()).unwrap();
        let main = module.module.functions.get("main").expect("main function");
        let emitted = main
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find(|located| {
                matches!(
                    located.instruction,
                    Instruction::CapabilityRequest { ref requirement, .. }
                        if requirement.capability == CapabilityKind::SessionEmit
                )
            })
            .expect("expanded say effect");
        let span = emitted.origin.span.as_ref().expect("macro call-site span");
        assert_eq!(&source[span.start_byte..span.end_byte], "(announce \"hello\")");
        let expansion = emitted.origin.expansion.as_ref().expect("macro definition ancestry");
        assert_eq!(expansion.word.as_deref(), Some("macro announce"));
        let template = expansion.span.as_ref().expect("template span");
        assert_eq!(&source[template.start_byte..template.end_byte], "(say text)");
    }

    #[test]
    fn macro_substituted_argument_keeps_its_exact_caller_span() {
        let source = "(define-syntax (increment value) (+ value 1))\n(increment 41)";
        let module = compile_lisp("macros.lisp", source, Vec::new(), &core_vocabulary()).unwrap();
        let main = module.module.functions.get("main").expect("main function");
        let argument = main
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find(|located| {
                matches!(located.instruction, Instruction::Constant { value: TypedValue::Int(41) })
            })
            .expect("macro-substituted argument constant");
        let span = argument.origin.span.as_ref().expect("caller argument span");
        assert_eq!(&source[span.start_byte..span.end_byte], "41");
        assert_eq!(
            argument
                .origin
                .expansion
                .as_ref()
                .and_then(|origin| origin.word.as_deref()),
            Some("macro increment")
        );
    }

    #[test]
    fn nested_macro_expansion_origins_preserve_the_full_definition_chain() {
        let source = "(define-syntax (inc value) (+ value 1))\n\
                      (define-syntax (twice value) (inc (inc value)))\n\
                      (twice 40)";
        let module = compile_lisp("nested-macros.lisp", source, Vec::new(), &core_vocabulary())
            .expect("nested macro program compiles");
        let main = &module.module.functions["main"];
        let call = main
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find(|located| {
                matches!(located.instruction, Instruction::Call { ref function } if function == "+")
            })
            .expect("expanded arithmetic call");
        let twice = call.origin.expansion.as_ref().expect("outer macro ancestry");
        assert_eq!(twice.word.as_deref(), Some("macro twice"));
        let inc = twice.expansion.as_ref().expect("nested macro ancestry");
        assert_eq!(inc.word.as_deref(), Some("macro inc"));
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
    fn top_level_begin_can_group_a_definition_and_its_first_use() {
        assert_eq!(
            run("(begin
                    (define (factorial (n : int)) : int
                      (if (<= n 1) 1 (* n (factorial (- n 1)))))
                    (factorial 6))")
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
        assert!(announce
            .inferred_effects
            .grants(&EffectSet::from_requirement(CapabilityRequirement {
                capability: CapabilityKind::SessionEmit,
                selector: ResourceSelector::None,
            },)));
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
    fn try_returns_an_err_from_the_enclosing_typed_definition() {
        let stack = run(
            "(define (fail-fast) : result<dynamic,string> \
                (begin (try (err \"no\")) (err \"unreachable\"))) \
             (fail-fast)",
        )
        .expect("try must compile as typed result propagation");
        assert_eq!(
            stack,
            vec![TypedValue::Result {
                ok_type: Type::Dynamic,
                error_type: Type::String,
                is_ok: false,
                value: Box::new(TypedValue::String("no".into())),
            }]
        );
    }

    #[test]
    fn try_continues_with_an_ok_payload() {
        let source = "(define (keep-going) : result<int,string> \
                      (begin (try (ok 7)) (ok 8))) \
                      (keep-going)";
        let module = compile_lisp("try-ok.lisp", source, Vec::new(), &core_vocabulary())
            .expect("successful try must leave the unwrapped payload for the next expression");
        assert_eq!(
            module.module.functions["keep-going"].signature.output.values,
            vec![Type::result(Type::Int, Type::String)],
            "the declared result contract remains visible to callers"
        );
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert!(matches!(
            stack.as_slice(),
            [TypedValue::Result { is_ok: true, value, .. }] if **value == TypedValue::Int(8)
        ));
    }

    #[test]
    fn rejects_try_outside_a_typed_result_definition() {
        let errors = compile_lisp(
            "try.lisp",
            "(try (ok 7))",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect_err("top-level try has no typed result return target");
        assert!(errors.iter().any(|error| error.code == "E-RESULT-TRY-002"));
    }

    #[test]
    fn constructs_and_uses_immutable_typed_maps() {
        assert_eq!(
            run("(unwrap (map-get (map \"answer\" 42 \"other\" 7) \"answer\"))").unwrap(),
            vec![TypedValue::Int(42)]
        );
        assert_eq!(
            run("(unwrap (map-get (map-set (map \"answer\" 42) \"answer\" 99) \"answer\"))")
                .unwrap(),
            vec![TypedValue::Int(99)]
        );
        assert_eq!(
            run("(map-length (map \"a\" 1 \"a\" 2))").unwrap(),
            vec![TypedValue::Int(1)]
        );
        assert_eq!(
            run("(unwrap (map-get (map-set (empty-map string int) \"answer\" 42) \"answer\"))")
                .unwrap(),
            vec![TypedValue::Int(42)]
        );
    }

    #[test]
    fn constructs_and_projects_heterogeneous_typed_records() {
        assert_eq!(
            run("(unwrap (record-get { :name \"Ada\" :age 37 } \"age\"))").unwrap(),
            vec![TypedValue::Int(37)]
        );
        let missing = compile_lisp(
            "record.lisp",
            "(record-get { :name \"Ada\" } \"age\")",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect_err("record fields are statically known");
        assert_eq!(missing[0].code, "E-RECORD-005");
        assert_eq!(
            run("(unwrap (record-get (record-set { :name \"Ada\" :age 37 } \"age\" 38) \"age\"))")
                .unwrap(),
            vec![TypedValue::Int(38)]
        );
        assert_eq!(
            run(
                "(let ((object { :run (lambda ((x : int)) (+ x 1)) })) ((unwrap (record-get object \"run\")) 41))",
            )
            .unwrap(),
            vec![TypedValue::Int(42)]
        );
    }

    #[test]
    fn reads_json_object_fields_through_typed_option_boundaries() {
        assert_eq!(
            run(
                "(unwrap (json-as-int (unwrap (json-get (result-unwrap (json-parse \"{\\\"answer\\\":42}\")) \"answer\"))))"
            )
            .unwrap(),
            vec![TypedValue::Int(42)]
        );
        assert_eq!(
            run("(is-some (json-get (result-unwrap (json-parse \"{}\")) \"missing\"))").unwrap(),
            vec![TypedValue::Bool(false)]
        );
        assert_eq!(
            run(
                "(unwrap (json-as-string (unwrap (json-index (result-unwrap (json-parse \"[0,\\\"one\\\"]\")) 1))))"
            )
            .unwrap(),
            vec![TypedValue::String("one".into())]
        );
        assert_eq!(
            run("(unwrap (json-as-float (result-unwrap (json-parse \"3.5\"))))").unwrap(),
            vec![TypedValue::Float(3.5)]
        );
        assert_eq!(
            run("(list-length (json-keys (result-unwrap (json-parse \"{\\\"a\\\":1,\\\"b\\\":2}\"))))")
                .unwrap(),
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
    fn generic_match_supports_total_boolean_and_integer_literals() {
        assert_eq!(
            run("(match true (true 42) (false 0))").unwrap(),
            vec![TypedValue::Int(42)]
        );
        assert_eq!(
            run("(match 2 (0 100) (2 42) (_ 0))").unwrap(),
            vec![TypedValue::Int(42)]
        );
        assert_eq!(
            run("(match 9 (0 100) (2 42) (_ 0))").unwrap(),
            vec![TypedValue::Int(0)]
        );
    }

    #[test]
    fn generic_integer_match_requires_a_total_unique_literal_shape() {
        for source in [
            "(match 2 (2 42))",
            "(match 2 (_ 0) (2 42))",
            "(match 2 (2 42) (2 99) (_ 0))",
        ] {
            let errors = compile_lisp("match.lisp", source, Vec::new(), &core_vocabulary())
                .expect_err("invalid integer match shape must not compile");
            assert_eq!(errors[0].code, "E-LISP-MATCH-011");
        }
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
    fn accepts_fixed_record_return_annotations() {
        assert_eq!(
            run(
                "(define (person) : record{name:string,age:int} \
                 { :name \"Ada\" :age 37 }) \
                 (unwrap (record-get (person) \"age\"))"
            )
            .unwrap(),
            vec![TypedValue::Int(37)]
        );
    }

    #[test]
    fn parses_compact_pure_function_types_shared_with_forth() {
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
            Type::result(Type::Option(Box::new(Type::list(Type::Int))), Type::String,)
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
                    Type::Map(
                        Box::new(Type::String),
                        Box::new(Type::list(Type::Int)),
                    ),
                ),
            ])
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
    fn lambda_type_retains_its_typed_suspension_contract() {
        let module = compile_lisp(
            "producer.lisp",
            "(lambda () (begin (yield 1) 2))",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("yielding Lisp closure compiles");
        let lambda = module
            .module
            .functions
            .values()
            .find(|function| function.name.starts_with("lambda$"))
            .expect("lowered lambda function");
        assert_eq!(
            lambda.signature.suspension,
            Some(SuspensionSignature::one_way(Type::Int))
        );
        assert_eq!(lambda.signature.control, ControlEffect::MaySuspend);
        assert!(matches!(
            module.module.functions["main"].signature.output.values.as_slice(),
            [Type::Function {
                suspension: Some(SuspensionSignature { yield_type, resume_type }),
                ..
            }] if **yield_type == Type::Int && **resume_type == Type::Unit
        ));
    }

    #[test]
    fn callable_rejects_incompatible_yield_types() {
        let errors = compile_lisp(
            "mixed-yield.lisp",
            "(lambda () (begin (yield 1) (yield \"two\") 2))",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect_err("one producer must have one stable yield type");
        assert!(errors.iter().any(|error| error.code == "E-YIELD-004"));
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

    #[test]
    fn preserves_exact_operator_spans_on_lisp_ir() {
        let source = "; setup\n(say \"first\")\n(+ 2 3)";
        let module = compile_lisp("spans.lisp", source, Vec::new(), &core_vocabulary()).unwrap();
        let instructions = &module.module.functions["main"].blocks[&0].instructions;
        let say = instructions
            .iter()
            .find(|located| {
                matches!(
                    located.instruction,
                    Instruction::CapabilityRequest { ref requirement, .. }
                        if requirement.capability == CapabilityKind::SessionEmit
                )
            })
            .expect("say lowers to an awaited effect");
        let add = instructions
            .iter()
            .find(|located| matches!(located.instruction, Instruction::Call { ref function } if function == "+"))
            .expect("addition call");
        let say_span = say.origin.span.as_ref().expect("Lisp span");
        let add_span = add.origin.span.as_ref().expect("Lisp span");
        assert_eq!(say_span.source_id, "spans.lisp");
        assert_eq!(&source[say_span.start_byte..say_span.end_byte], "say");
        assert_eq!(&source[add_span.start_byte..add_span.end_byte], "+");
        assert_eq!((say_span.start_line, say_span.start_column), (2, 2));
        assert_eq!((add_span.start_line, add_span.start_column), (3, 2));
    }

    #[test]
    fn nested_named_call_diagnostics_point_to_the_exact_operator_or_argument() {
        let source = "(+ missing (unknown 2))";
        let errors = compile_lisp("nested.lisp", source, Vec::new(), &core_vocabulary())
            .expect_err("both the unbound argument and unknown nested call are invalid");
        let unbound = errors
            .iter()
            .find(|error| error.code == "E-NAME-001")
            .expect("unbound argument diagnostic");
        let span = unbound.primary.as_ref().and_then(|origin| origin.span.as_ref()).unwrap();
        assert_eq!(&source[span.start_byte..span.end_byte], "missing");

        let source = "(+ 1 (unknown 2))";
        let errors = compile_lisp("nested.lisp", source, Vec::new(), &core_vocabulary())
            .expect_err("unknown nested call is invalid");
        let unknown = errors
            .iter()
            .find(|error| error.code == "E-LINK-002")
            .expect("unknown nested call diagnostic");
        let span = unknown.primary.as_ref().and_then(|origin| origin.span.as_ref()).unwrap();
        assert_eq!(&source[span.start_byte..span.end_byte], "unknown");
    }

    #[test]
    fn sequence_and_branch_diagnostics_keep_nested_source_spans() {
        let source = "(begin 1 (if true 2 missing))";
        let errors = compile_lisp("control.lisp", source, Vec::new(), &core_vocabulary())
            .expect_err("the else branch has an unbound name");
        let missing = errors
            .iter()
            .find(|error| error.code == "E-NAME-001")
            .expect("unbound else-branch diagnostic");
        let span = missing
            .primary
            .as_ref()
            .and_then(|origin| origin.span.as_ref())
            .expect("exact source span");
        assert_eq!(&source[span.start_byte..span.end_byte], "missing");
    }

    #[test]
    fn lexical_body_diagnostics_keep_nested_source_spans() {
        for source in [
            "(define (broken (value : int)) : int missing)",
            "(let ((value 1)) missing)",
            "((lambda ((value : int)) missing) 1)",
        ] {
            let errors = compile_lisp("lexical.lisp", source, Vec::new(), &core_vocabulary())
                .expect_err("body contains an unbound name");
            let missing = errors
                .iter()
                .find(|error| error.code == "E-NAME-001")
                .expect("unbound lexical-body diagnostic");
            let span = missing
                .primary
                .as_ref()
                .and_then(|origin| origin.span.as_ref())
                .expect("exact source span");
            assert_eq!(&source[span.start_byte..span.end_byte], "missing");
        }
    }

    #[test]
    fn match_and_loop_diagnostics_keep_nested_source_spans() {
        for source in [
            "(match-option (some 1) (some value missing) (none 0))",
            "(match-result (ok 1) (ok value missing) (err problem 0))",
            "(while true missing)",
        ] {
            let errors = compile_lisp("patterns.lisp", source, Vec::new(), &core_vocabulary())
                .expect_err("nested form contains an unbound name");
            let missing = errors
                .iter()
                .find(|error| error.code == "E-NAME-001")
                .expect("unbound nested-form diagnostic");
            let span = missing
                .primary
                .as_ref()
                .and_then(|origin| origin.span.as_ref())
                .expect("exact source span");
            assert_eq!(&source[span.start_byte..span.end_byte], "missing");
        }
    }

    #[test]
    fn deferred_closure_diagnostics_keep_nested_source_spans() {
        let source = "(defer :cpu (lambda () missing))";
        let errors = compile_lisp("defer.lisp", source, Vec::new(), &core_vocabulary())
            .expect_err("deferred closure contains an unbound name");
        let missing = errors
            .iter()
            .find(|error| error.code == "E-NAME-001")
            .expect("unbound deferred-closure diagnostic");
        let span = missing
            .primary
            .as_ref()
            .and_then(|origin| origin.span.as_ref())
            .expect("exact source span");
        assert_eq!(&source[span.start_byte..span.end_byte], "missing");
    }

    #[test]
    fn finds_top_level_forms_through_comments_quotes_and_json_literals() {
        let source = "; lead\n'(say \"quoted\") #| ignored ( ) |#\n[1, {\"x\": \"y\"}]\n$2*x$";
        let forms = crate::lisp::reader::parse_str_spanned(source)
            .expect("reader-compatible top-level forms")
            .iter()
            .map(|form| &source[form.span.clone()])
            .collect::<Vec<_>>();
        assert_eq!(
            forms,
            vec!["'(say \"quoted\")", "[1, {\"x\": \"y\"}]", "$2*x$"]
        );
        assert_eq!(
            crate::lisp::reader::parse_str(source).unwrap().len(),
            forms.len()
        );
    }
}
