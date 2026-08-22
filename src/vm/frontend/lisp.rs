use crate::lisp::Val;
use crate::vm::diagnostic::{
    DiagnosticPhase, SourceLanguage, SourceOrigin, SourceSpan, VmDiagnostic,
};
use crate::vm::effects::EffectSet;
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
            stack: input.clone(),
            input,
            effects: EffectSet::pure(),
        }
    }

    fn emit(&mut self, instruction: Instruction, origin: SourceOrigin) {
        self.blocks
            .get_mut(&self.current)
            .expect("current block exists")
            .instructions
            .push(LocatedInstruction {
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
    let expressions = crate::lisp::reader::parse_str(source).map_err(|error| {
        vec![VmDiagnostic::error(
            "E-READ-002",
            DiagnosticPhase::Reader,
            error.to_string(),
            Some(source_origin(source_id, source, "<reader>")),
        )]
    })?;
    let mut compiler = Compiler {
        source_id,
        source,
        vocabulary: vocabulary.clone(),
        functions: BTreeMap::new(),
        next_lambda: 0,
    };
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
        let Val::List(items) = expression else {
            unreachable!("definition predicate only accepts lists");
        };
        if items.len() < 3 {
            return Err(vec![self.error(
                "E-LISP-DEF-001",
                "define requires a function header and body",
            )]);
        }
        let Val::List(header) = &items[1] else {
            return Err(vec![self.error(
                "E-LISP-DEF-002",
                "typed define must use (define (name (arg : type) ...) body...)",
            )]);
        };
        let Some(Val::Symbol(name)) = header.first() else {
            return Err(vec![self.error(
                "E-LISP-DEF-003",
                "function definition requires a symbol name",
            )]);
        };
        if name == "main" || self.vocabulary.contains_key(name) || self.functions.contains_key(name)
        {
            return Err(vec![self.error(
                "E-LISP-DEF-004",
                format!("function '{name}' is already defined"),
            )]);
        }
        let parameters = header[1..]
            .iter()
            .map(parse_parameter)
            .collect::<Result<Vec<_>, _>>()?;
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
        let result_type = self.compile_begin(&items[2..], &mut child)?;
        child.stack.push(result_type);
        child.emit(Instruction::Return, self.origin("define-return"));
        let output = child.stack.clone();
        let function = child.finish(output);
        self.vocabulary
            .insert(name.clone(), function.signature.clone());
        self.functions.insert(name.clone(), function);
        Ok(())
    }

    fn compile_list(
        &mut self,
        items: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if items.len() == 2 && items[0] == Val::Symbol("quote".into()) {
            let Val::Symbol(name) = &items[1] else {
                return Err(vec![self.error(
                    "E-TYPE-011",
                    "quote currently accepts only a symbol",
                )]);
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
            "while" => self.compile_while(&items[1..], builder),
            "lambda" => self.compile_lambda(&items[1..], builder),
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

    fn compile_while(
        &mut self,
        expressions: &[Val],
        builder: &mut FunctionBuilder,
    ) -> Result<Type, Vec<VmDiagnostic>> {
        if expressions.len() < 2 {
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
        let condition = self.compile_expression(&expressions[0], builder)?;
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
        self.compile_begin(&expressions[1..], builder)?;
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
        let Val::List(parameters) = &expressions[0] else {
            return Err(vec![
                self.error("E-LISP-007", "lambda parameters must be a list")
            ]);
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
        let instruction = if signature.effects.0.len() == 1 {
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

fn is_definition(expression: &Val) -> bool {
    matches!(
        expression,
        Val::List(items) if matches!(items.first(), Some(Val::Symbol(name)) if name == "define")
    )
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
    match name.as_str() {
        "unit" | "nil" => Ok(Type::Unit),
        "bool" => Ok(Type::Bool),
        "int" => Ok(Type::Int),
        "uint" => Ok(Type::UInt),
        "float" => Ok(Type::Float),
        "char" => Ok(Type::Char),
        "string" | "str" => Ok(Type::String),
        "bytes" => Ok(Type::Bytes),
        "dynamic" | "any" => Ok(Type::Dynamic),
        _ => Err(vec![VmDiagnostic::error(
            "E-TYPE-009",
            DiagnosticPhase::TypeInference,
            format!("unknown type '{name}'"),
            None,
        )]),
    }
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
        assert_eq!(run("(quote bash)").unwrap(), vec![TypedValue::Symbol("bash".into())]);
        assert_eq!(run("'bash").unwrap(), vec![TypedValue::Symbol("bash".into())]);
    }
}
