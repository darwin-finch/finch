use finch_vm_core::{
    apply_signature_types, certify_module, instantiate_signature_types, nearest_names,
    parse_type_name, BasicBlock, ControlEffect, DiagnosticPhase, EffectSet, Function, Instruction,
    LocatedInstruction, Module, ModuleVerified, Parsed, SourceLanguage, SourceOrigin, SourceSpan,
    StackRow, StackSignature, SuspensionSignature, Type, TypedValue, UiOperation, VmDiagnostic,
    Vocabulary,
};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};

#[cfg(test)]
thread_local! {
    static PARSER_TOKEN_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn record_parser_token_visit() {
    #[cfg(test)]
    PARSER_TOKEN_VISITS.with(|visits| visits.set(visits.get() + 1));
}

#[derive(Debug, Clone)]
struct LocalBinding<'source> {
    name: &'source str,
    ty: Type,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone)]
struct Token<'source> {
    value: TokenValue<'source>,
    start: usize,
    end: usize,
}

#[allow(clippy::type_complexity)]
fn parse_variant_constructor_name(
    name: &str,
) -> Option<(Vec<(String, Option<Type>)>, String, Option<Type>)> {
    let inner = name.strip_prefix("variant<")?.strip_suffix('>')?;
    let (variant_type, tag) = split_variant_constructor_arguments(inner)?;
    let Type::Variant(variants) = parse_type_name(variant_type).ok()? else {
        return None;
    };
    let tag = tag.trim();
    let (_, payload_type) = variants.iter().find(|(name, _)| name == tag)?;
    Some((variants.clone(), tag.to_string(), payload_type.clone()))
}

fn split_variant_constructor_arguments(source: &str) -> Option<(&str, &str)> {
    let mut angle_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut separator = None;
    for (index, character) in source.char_indices() {
        match character {
            '<' => angle_depth += 1,
            '>' => angle_depth = angle_depth.checked_sub(1)?,
            '{' => brace_depth += 1,
            '}' => brace_depth = brace_depth.checked_sub(1)?,
            ',' if angle_depth == 0 && brace_depth == 0 => match separator {
                None => separator = Some(index),
                Some(_) => return None,
            },
            _ => {}
        }
    }
    if angle_depth != 0 || brace_depth != 0 {
        return None;
    }
    let separator = separator?;
    let variant_type = source[..separator].trim();
    let tag = source[separator + 1..].trim();
    (!variant_type.is_empty() && !tag.is_empty()).then_some((variant_type, tag))
}

/// The source-preserving Co-Forth module syntax tree. This is deliberately a
/// frontend representation rather than typed IR: definition boundaries and
/// body nodes are retained with their original byte spans, and lowering never
/// serializes or reparses a definition body. Source spellings borrow the
/// immutable input; decoded literals and semantic type information own data.
#[derive(Debug, Clone)]
struct ForthModuleAst<'source> {
    definitions: Vec<ForthDefinitionAst<'source>>,
    body: ForthBodyAst<'source>,
}

#[derive(Debug, Clone)]
struct ForthBodyAst<'source> {
    nodes: Vec<ForthBodyNode<'source>>,
}

#[derive(Debug, Clone)]
enum ForthBodyNode<'source> {
    Primitive(ForthOperationAst<'source>),
    Control(ForthOperationAst<'source>),
    Collection(ForthOperationAst<'source>),
    RecordReference(ForthOperationAst<'source>),
    QuotationReference(ForthOperationAst<'source>),
    LocalReference(ForthBindingReferenceAst<'source>),
    CaptureReference(ForthBindingReferenceAst<'source>),
    Call(ForthCallAst<'source>),
    Literal(ForthLiteralAst<'source>),
    Quotation(ForthQuotationAst<'source>),
    Group(ForthBodyAst<'source>),
}

impl ForthBodyNode<'_> {
    fn leading_token(&self) -> &Token<'_> {
        match self {
            Self::Primitive(operation)
            | Self::Control(operation)
            | Self::Collection(operation)
            | Self::RecordReference(operation)
            | Self::QuotationReference(operation) => &operation.token,
            Self::LocalReference(binding) | Self::CaptureReference(binding) => &binding.token,
            Self::Call(call) => &call.token,
            Self::Literal(literal) => &literal.token,
            Self::Quotation(quotation) => &quotation.open,
            Self::Group(body) => body.nodes[0].leading_token(),
        }
    }
}

#[derive(Debug, Clone)]
struct ForthOperationAst<'source> {
    token: Token<'source>,
    name: &'source str,
    parameter: Option<&'source str>,
    type_argument: Option<Result<Type, ()>>,
    variant: Option<ForthVariantAst>,
    syntax: ForthSyntax,
    operand: Option<ForthReferenceAst<'source>>,
    field: Option<Cow<'source, str>>,
    field_valid: bool,
    control_valid: bool,
    next_is_terminal: bool,
}

#[derive(Debug, Clone)]
struct ForthReferenceAst<'source> {
    /// The target's source span remains independent of its operator's span.
    #[allow(dead_code)]
    token: Token<'source>,
    name: Option<&'source str>,
    valid_loop_label: bool,
}

#[derive(Debug, Clone)]
struct ForthVariantAst {
    variants: Vec<(String, Option<Type>)>,
    tag: String,
    payload_type: Option<Type>,
}

#[derive(Debug, Clone)]
struct ForthBindingReferenceAst<'source> {
    token: Token<'source>,
    index: u32,
    ty: Type,
}

#[derive(Debug, Clone)]
struct ForthCallAst<'source> {
    token: Token<'source>,
    name: &'source str,
    kind: ForthCallKind,
}

#[derive(Debug, Clone)]
enum ForthCallKind {
    Function,
    Yield,
    OutputOpen,
    Ui(UiOperation),
}

#[derive(Debug, Clone)]
struct ForthCaptureAst<'source> {
    binding: LocalBinding<'source>,
    index: u32,
    from_local: bool,
}

#[derive(Debug, Clone)]
struct ForthLiteralAst<'source> {
    token: Token<'source>,
    value: TypedValue,
}

#[derive(Debug, Clone)]
struct ForthQuotationAst<'source> {
    open: Token<'source>,
    signature: Result<(StackSignature, bool), Vec<VmDiagnostic>>,
    captures: Vec<ForthCaptureAst<'source>>,
    body: ForthBodyAst<'source>,
    end: usize,
}

#[derive(Debug, Clone)]
enum TokenValue<'source> {
    Word(&'source str),
    String(String),
    /// A pasted JSON object literal. It is deliberately retained as managed
    /// JSON rather than guessed to be a typed record or map.
    Json(serde_json::Value),
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

/// A typed `map{ key value ... }map` literal records the pre-literal stack
/// depth while ordinary Forth tokens compile its pair expressions.  The close
/// delimiter lowers the resulting typed suffix to the shared `MakeMap` IR.
#[derive(Debug, Clone)]
struct MapLiteralFrame {
    stack_start: usize,
    origin: SourceOrigin,
}

/// A typed `[ value ... ]` literal records the pre-literal stack
/// depth while ordinary Forth tokens compile its element expressions. The
/// close delimiter lowers that homogeneous suffix to the shared `MakeList`
/// IR, matching Lisp's `(list value ...)` source form.
#[derive(Debug, Clone)]
struct ListLiteralFrame {
    stack_start: usize,
    origin: SourceOrigin,
}

/// A typed `{ name: value ... }` literal records its field
/// labels outside the value stack. That keeps heterogeneous products explicit
/// in source while lowering to the same record IR used by Lisp.
#[derive(Debug, Clone)]
struct RecordLiteralFrame {
    stack_start: usize,
    fields: Vec<String>,
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

/// Syntax is classified once by the parser. Group children retain the exact
/// source order, including malformed closers, so lowering reports the original
/// first diagnostic without searching for delimiters or adjacent operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForthSyntax {
    Call,
    Execute,
    EmptyMap,
    EmptyList,
    Variant,
    VariantGet,
    Dup,
    Drop,
    Swap,
    Defer,
    DeferCpu,
    TaskPoll,
    TaskJoin,
    TaskCancel,
    FiberNext,
    FiberJoin,
    FiberCancel,
    PropagateResult,
    QuoteTarget,
    RecordField,
    RecordGetNamed,
    RecordGet,
    RecordSet,
    ListOpen,
    ListClose,
    MapOpen,
    MapClose,
    RecordOpen,
    RecordClose,
    Case,
    Of,
    EndOf,
    Otherwise,
    EndCase,
    If,
    MatchOption,
    MatchResult,
    Else,
    Then,
    Begin,
    NamedBegin,
    Break,
    Continue,
    While,
    Repeat,
    Until,
}

impl ForthSyntax {
    fn parse(word: &str) -> Self {
        match word {
            "execute" => Self::Execute,
            "dup" => Self::Dup,
            "drop" => Self::Drop,
            "swap" => Self::Swap,
            "defer" => Self::Defer,
            "defer-cpu" => Self::DeferCpu,
            "task-poll" => Self::TaskPoll,
            "task-join" => Self::TaskJoin,
            "task-cancel" => Self::TaskCancel,
            "fiber-next" => Self::FiberNext,
            "fiber-join" => Self::FiberJoin,
            "fiber-cancel" => Self::FiberCancel,
            "?" => Self::PropagateResult,
            "[']" => Self::QuoteTarget,
            "record-get" => Self::RecordGet,
            "record-set" => Self::RecordSet,
            "[" | "list{" => Self::ListOpen,
            "]" | "}list" => Self::ListClose,
            "map{" => Self::MapOpen,
            "}map" => Self::MapClose,
            "{" | "record{" => Self::RecordOpen,
            "}" | "}record" => Self::RecordClose,
            "case" => Self::Case,
            "of" => Self::Of,
            "endof" => Self::EndOf,
            "otherwise" => Self::Otherwise,
            "endcase" => Self::EndCase,
            "if" => Self::If,
            "if-some" => Self::MatchOption,
            "if-ok" => Self::MatchResult,
            "else" => Self::Else,
            "then" => Self::Then,
            "begin" => Self::Begin,
            "begin:" => Self::NamedBegin,
            "break" => Self::Break,
            "continue" => Self::Continue,
            "while" => Self::While,
            "repeat" => Self::Repeat,
            "until" => Self::Until,
            _ => Self::Call,
        }
    }
}

impl<'source> ForthBodyAst<'source> {
    fn visit(
        &self,
        lower: &mut impl FnMut(&ForthBodyNode<'source>) -> Result<(), Vec<VmDiagnostic>>,
    ) -> Result<(), Vec<VmDiagnostic>> {
        for node in &self.nodes {
            match node {
                ForthBodyNode::Group(body) => body.visit(lower)?,
                _ => lower(node)?,
            }
        }
        Ok(())
    }
}

/// The delimiter index is built once, without inspecting a nested slice again.
/// The cursor subsequently consumes every token exactly once, even when a
/// quotation is an operand whose body is not semantically elaborated.
struct ForthParser<'tokens, 'source> {
    atoms: &'tokens [Token<'source>],
    quotations: Vec<Option<(usize, usize)>>,
    cursor: usize,
}

impl<'tokens, 'source> ForthParser<'tokens, 'source> {
    fn new(atoms: &'tokens [Token<'source>]) -> Self {
        let mut quotations = vec![None; atoms.len()];
        let mut brackets: Vec<(usize, Option<usize>, bool)> = Vec::new();
        for (index, token) in atoms.iter().enumerate() {
            record_parser_token_visit();
            match token.value {
                TokenValue::Word("[") => brackets.push((index, None, false)),
                TokenValue::Word("]") => {
                    if let Some((open, Some(pipe), true)) = brackets.pop() {
                        quotations[open] = Some((pipe, index));
                    }
                }
                TokenValue::Word("--") => {
                    if let Some((_, _, arrow)) = brackets.last_mut() {
                        *arrow = true;
                    }
                }
                TokenValue::Word("|") => {
                    if let Some((_, pipe, _)) = brackets.last_mut() {
                        pipe.get_or_insert(index);
                    }
                }
                _ => {}
            }
        }
        Self {
            atoms,
            quotations,
            cursor: 0,
        }
    }

    fn next(&mut self) -> Option<(usize, Token<'source>)> {
        let index = self.cursor;
        let token = self.atoms.get(index)?.clone();
        self.cursor += 1;
        record_parser_token_visit();
        Some((index, token))
    }

    fn advance_to(&mut self, end: usize) {
        while self.cursor < end {
            self.next()
                .expect("parser boundary is inside its token stream");
        }
    }
}

fn parse_quotation_signature(
    source_id: &str,
    source: &str,
    header: &[Token],
    origin: &SourceOrigin,
) -> Result<(StackSignature, bool), Vec<VmDiagnostic>> {
    let Some(separator) = header
        .iter()
        .position(|token| matches!(&token.value, TokenValue::Word(word) if *word == "--"))
    else {
        return Err(vec![control_error(
            "E-FORTH-QUOTE-003",
            "anonymous quotation signature requires --",
            origin.clone(),
        )]);
    };
    let effect = header
        .iter()
        .position(|token| matches!(&token.value, TokenValue::Word(word) if *word == "!"))
        .unwrap_or(header.len());
    if effect < separator {
        return Err(vec![control_error(
            "E-FORTH-QUOTE-003",
            "anonymous quotation ! effect follows its output types",
            origin.clone(),
        )]);
    }
    let input = parse_stack_types(source_id, source, &header[..separator])?;
    let output = parse_stack_types(source_id, source, &header[separator + 1..effect])?;
    if output.len() != 1 {
        return Err(vec![control_error(
            "E-CLOSURE-001",
            "anonymous quotations return exactly one value; use a record or list for multiple values",
            origin.clone(),
        )]);
    }
    let declares_pure = if effect == header.len() {
        false
    } else {
        let annotation = &header[effect + 1..];
        if annotation.len() == 1
            && matches!(&annotation[0].value, TokenValue::Word(word) if *word == "pure")
        {
            true
        } else if annotation.len() == 1
            && matches!(&annotation[0].value, TokenValue::Word(word) if *word == "infer")
        {
            false
        } else {
            return Err(vec![control_error(
                "E-FORTH-QUOTE-003",
                "anonymous quotation effect is ! pure or ! infer",
                origin.clone(),
            )]);
        }
    };
    Ok((
        StackSignature {
            type_parameters: Vec::new(),
            input: StackRow::polymorphic("S", input),
            output: StackRow::polymorphic("S", output),
            effects: EffectSet::pure(),
            control: ControlEffect::Returns,
            suspension: None,
        },
        declares_pure,
    ))
}

/// Compile user/model-entered Co-Forth source text directly into Finch typed
/// stack IR and run the common verifier. `initial_stack` is the actual typed VM
/// stack against which the program was composed.
pub fn compile_forth(
    source_id: &str,
    source: &str,
    initial_stack: Vec<Type>,
    vocabulary: &Vocabulary,
) -> Result<ModuleVerified, Vec<VmDiagnostic>> {
    compile_forth_with_functions(
        source_id,
        source,
        initial_stack,
        vocabulary,
        &BTreeMap::new(),
    )
}

/// Compile Co-Forth source with additional already-lowered functions available
/// for definition calls, then verify the complete typed module.
pub fn compile_forth_with_functions(
    source_id: &str,
    source: &str,
    initial_stack: Vec<Type>,
    vocabulary: &Vocabulary,
    linked_functions: &BTreeMap<String, Function>,
) -> Result<ModuleVerified, Vec<VmDiagnostic>> {
    let ast = parse_forth_module(source_id, source)?;
    let parsed = Parsed::from_frontend(source_id, ast);
    let ast = parsed.into_ast();
    let definitions = ast.definitions;
    if definitions.is_empty() {
        return compile_forth_ast_body_with_functions(
            source_id,
            source,
            &ast.body,
            initial_stack,
            vocabulary,
            linked_functions,
        );
    }

    let mut functions = linked_functions.clone();
    let mut local_vocabulary = vocabulary.clone();
    let mut definition_names = BTreeSet::new();
    for definition in &definitions {
        if vocabulary.contains_key(definition.name)
            || functions.contains_key(definition.name)
            || definition.name == "main"
            || !definition_names.insert(definition.name.to_owned())
        {
            return Err(vec![control_error(
                "E-FORTH-DEF-001",
                format!("word '{}' is already defined", definition.name),
                origin(source_id, source, definition.start, definition.end),
            )]);
        }
        // A declared-pure signature is complete authority information, so it
        // can safely be made visible before compiling bodies. This supports
        // pure mutually-recursive words without an untyped forward reference.
        // `! infer` words intentionally remain sequential: their effects are
        // learned from their body and must not be guessed for a sibling call.
        if definition.declares_pure {
            local_vocabulary.insert(definition.name.to_owned(), definition.signature.clone());
        }
    }
    for definition in definitions {
        local_vocabulary.insert(definition.name.to_owned(), definition.signature.clone());
        let compiled = lower_forth_ast_body_with_locals(
            source_id,
            source,
            &definition.body,
            definition.signature.input.values.clone(),
            &local_vocabulary,
            &functions,
            &definition.locals,
            &[],
            Some(&definition.signature.output.values),
        )?;
        let verified = &compiled.functions[&compiled.module.entry];
        let mut function = compiled.module.functions[&compiled.module.entry].clone();
        for (nested_name, nested_function) in &compiled.module.functions {
            if nested_name != &compiled.module.entry {
                functions.insert(nested_name.clone(), nested_function.clone());
                local_vocabulary.insert(nested_name.clone(), nested_function.signature.clone());
            }
        }
        let actual_output = function.signature.output.values.clone();
        if actual_output != definition.signature.output.values {
            let mut diagnostic = control_error(
                "E-FORTH-DEF-002",
                format!(
                    "word '{}' declares output {:?} but its body leaves {:?}",
                    definition.name, definition.signature.output.values, actual_output
                ),
                origin(source_id, source, definition.start, definition.end),
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
                origin(source_id, source, definition.start, definition.end),
            );
            diagnostic.found_effects = verified.inferred_effects.clone();
            return Err(vec![diagnostic]);
        }
        function.name = definition.name.to_owned();
        function.documentation = definition.documentation.map(str::to_owned);
        function.signature = definition.signature;
        function.signature.effects = verified.inferred_effects.clone();
        function.signature.suspension = verified.inferred_suspension.clone();
        function.signature.control = if function.signature.suspension.is_some() {
            ControlEffect::MaySuspend
        } else {
            ControlEffect::Returns
        };
        local_vocabulary.insert(definition.name.to_owned(), function.signature.clone());
        functions.insert(definition.name.to_owned(), function);
    }

    compile_forth_ast_body_with_functions(
        source_id,
        source,
        &ast.body,
        initial_stack,
        &local_vocabulary,
        &functions,
    )
}

fn compile_forth_ast_body_with_functions(
    source_id: &str,
    source: &str,
    body: &ForthBodyAst,
    initial_stack: Vec<Type>,
    vocabulary: &Vocabulary,
    linked_functions: &BTreeMap<String, Function>,
) -> Result<ModuleVerified, Vec<VmDiagnostic>> {
    lower_forth_ast_body_with_locals(
        source_id,
        source,
        body,
        initial_stack,
        vocabulary,
        linked_functions,
        &[],
        &[],
        None,
    )
}

// This is the inherited recursive lowering seam; grouping it is separate from the crate move.
#[allow(clippy::too_many_arguments)]
fn lower_forth_ast_body_with_locals(
    source_id: &str,
    source: &str,
    body: &ForthBodyAst,
    initial_stack: Vec<Type>,
    vocabulary: &Vocabulary,
    linked_functions: &BTreeMap<String, Function>,
    locals: &[LocalBinding],
    captures: &[LocalBinding],
    expected_return: Option<&[Type]>,
) -> Result<ModuleVerified, Vec<VmDiagnostic>> {
    let mut stack = initial_stack.clone();
    let mut effects = EffectSet::pure();
    let mut suspension: Option<SuspensionSignature> = None;
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
    let mut map_literals: Vec<MapLiteralFrame> = Vec::new();
    let mut list_literals: Vec<ListLiteralFrame> = Vec::new();
    let mut record_literals: Vec<RecordLiteralFrame> = Vec::new();

    let mut available_functions = linked_functions.clone();

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

    // Signature names store declared inputs in bottom-to-top order. Store the
    // top value first, exactly as a Lisp parameter prologue does, so the body
    // begins with a clean shared stack and values live in this activation.
    for (index, local) in locals.iter().enumerate().rev() {
        let found = stack.pop().ok_or_else(|| {
            vec![control_error(
                "E-FORTH-LOCAL-003",
                "named signature input requires a corresponding typed value",
                origin(source_id, source, local.start, local.end),
            )]
        })?;
        if found != local.ty {
            return Err(vec![type_mismatch_with_stack(
                &stack,
                local.ty.clone(),
                found,
                Some(origin(source_id, source, local.start, local.end)),
            )]);
        }
        emit(
            &mut blocks,
            current,
            Instruction::LocalSet {
                index: index as u32,
            },
            origin(source_id, source, local.start, local.end),
        );
    }
    body.visit(&mut |node| {
        let token = node.leading_token().clone();
        if let ForthBodyNode::Quotation(quotation) = node {
            let origin = origin(source_id, source, token.start, quotation.end);
            let (declared_signature, declares_pure) =
                quotation.signature.clone()?;

            let visible = quotation.captures.iter().map(|capture| capture.binding.clone()).collect::<Vec<_>>();
            for capture in &quotation.captures {
                stack.push(capture.binding.ty.clone());
                let instruction = if capture.from_local {
                    Instruction::LocalGet { index: capture.index }
                } else {
                    Instruction::CaptureGet { index: capture.index }
                };
                emit(&mut blocks, current, instruction, origin.clone());
            }

            let compiled = lower_forth_ast_body_with_locals(
                source_id,
                source,
                &quotation.body,
                declared_signature.input.values.clone(),
                vocabulary,
                &available_functions,
                &[],
                &visible,
                Some(&declared_signature.output.values),
            )?;
            let mut quote_function = compiled.module.functions[&compiled.module.entry].clone();
            if declares_pure
                && !compiled.functions[&compiled.module.entry]
                    .inferred_effects
                    .is_pure()
            {
                return Err(vec![control_error(
                    "E-CAP-001",
                    "anonymous quotation declares pure but its body requires capabilities",
                    origin,
                )]);
            }
            let mut hasher = DefaultHasher::new();
            source_id.hash(&mut hasher);
            token.start.hash(&mut hasher);
            quotation.end.hash(&mut hasher);
            let quote_name = format!("quote${:016x}", hasher.finish());
            quote_function.name = quote_name.clone();
            quote_function.signature.effects = compiled.functions[&compiled.module.entry]
                .inferred_effects
                .clone();
            quote_function.signature.suspension = compiled.functions[&compiled.module.entry]
                .inferred_suspension
                .clone();
            quote_function.signature.control = if quote_function.signature.suspension.is_some() {
                ControlEffect::MaySuspend
            } else {
                ControlEffect::Returns
            };
            for (name, function) in compiled.module.functions.clone() {
                if name != compiled.module.entry {
                    available_functions.insert(name, function);
                }
            }
            let signature = quote_function.signature.clone();
            available_functions.insert(quote_name.clone(), quote_function);
            stack.truncate(stack.len() - visible.len());
            stack.push(Type::Function {
                arguments: signature.input.values.clone(),
                result: Box::new(signature.output.values[0].clone()),
                effects: signature.effects.clone(),
                suspension: signature.suspension.clone(),
            });
            emit(
                &mut blocks,
                current,
                Instruction::MakeClosure {
                    function: quote_name,
                    capture_count: visible.len() as u32,
                    signature,
                },
                origin,
            );
            return Ok(());
        }
        if let ForthBodyNode::Literal(literal) = node {
            let origin = origin(source_id, source, literal.token.start, literal.token.end);
            stack.push(literal.value.value_type());
            emit(
                &mut blocks,
                current,
                Instruction::Constant {
                    value: literal.value.clone(),
                },
                origin,
            );
            return Ok(());
        }
        let origin = origin(source_id, source, token.start, token.end);
        if let ForthBodyNode::LocalReference(binding) | ForthBodyNode::CaptureReference(binding) = node {
            stack.push(binding.ty.clone());
            let instruction = if matches!(node, ForthBodyNode::LocalReference(_)) {
                Instruction::LocalGet { index: binding.index }
            } else {
                Instruction::CaptureGet { index: binding.index }
            };
            emit(&mut blocks, current, instruction, origin);
            return Ok(());
        }
        if let ForthBodyNode::Primitive(word_node)
            | ForthBodyNode::Control(word_node)
            | ForthBodyNode::Collection(word_node)
            | ForthBodyNode::RecordReference(word_node)
            | ForthBodyNode::QuotationReference(word_node) = node {
            let word = word_node.name;
            if word_node.syntax == ForthSyntax::QuoteTarget {
                let Some(target) = word_node.operand.as_ref() else {
                    return Err(vec![control_error(
                        "E-FORTH-QUOTE-001",
                        "['] requires a persistent typed word name",
                        origin.clone(),
                    )]);
                };
                let Some(target_name) = target.name else {
                    return Err(vec![control_error(
                        "E-FORTH-QUOTE-001",
                        "quotation target must be a word name",
                        origin,
                    )]);
                };
                let Some(function) = available_functions.get(target_name) else {
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
                    suspension: signature.suspension.clone(),
                });
                emit(
                    &mut blocks,
                    current,
                    Instruction::MakeClosure {
                        function: target_name.to_owned(),
                        capture_count: 0,
                        signature,
                    },
                    origin,
                );
                return Ok(());
            }
            if word_node.syntax == ForthSyntax::Execute {
                let Type::Function {
                    arguments,
                    result,
                    effects: closure_effects,
                    suspension: closure_suspension,
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
                    control: if closure_suspension.is_some() {
                        ControlEffect::MaySuspend
                    } else {
                        ControlEffect::Returns
                    },
                    suspension: closure_suspension.clone(),
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
                return Ok(());
            }
            if word_node.syntax == ForthSyntax::EmptyMap {
                let Type::Map(key_type, value_type) =
                    word_node.type_argument.clone().expect("parser elaborates map type arguments").map_err(|_| {
                        vec![control_error(
                            "E-MAP-005",
                            "empty-map requires two valid type arguments, for example empty-map<string,int>",
                            origin.clone(),
                        )]
                    })?
                else {
                    unreachable!("map spelling always parses as a map type");
                };
                stack.push(Type::Map(key_type.clone(), value_type.clone()));
                emit(
                    &mut blocks,
                    current,
                    Instruction::MakeMap {
                        key_type: *key_type,
                        value_type: *value_type,
                        count: 0,
                    },
                    origin,
                );
                return Ok(());
            }
            if word_node.syntax == ForthSyntax::EmptyList {
                let element_type = word_node.type_argument.clone().expect("parser elaborates list type argument").map_err(|_| {
                    vec![control_error(
                        "E-LIST-005",
                        "empty-list requires one valid type argument, for example empty-list<string>",
                        origin.clone(),
                    )]
                })?;
                stack.push(Type::list(element_type.clone()));
                emit(
                    &mut blocks,
                    current,
                    Instruction::MakeList {
                        element_type,
                        count: 0,
                    },
                    origin,
                );
                return Ok(());
            }
            if word_node.syntax == ForthSyntax::Variant {
                let Some(ForthVariantAst { variants, tag, payload_type }) = word_node.variant.clone() else {
                    return Err(vec![control_error(
                        "E-VARIANT-001",
                        "variant constructor must be variant<variant{tag|tag(type)},selected-tag>",
                        origin,
                    )]);
                };
                if let Some(expected) = &payload_type {
                    let found = stack.pop().ok_or_else(|| {
                        vec![control_error(
                            "E-STACK-001",
                            format!("variant tag '{tag}' requires one {expected} payload"),
                            origin.clone(),
                        )]
                    })?;
                    if !expected.accepts(&found) {
                        return Err(vec![type_mismatch_with_stack(
                            &stack,
                            expected.clone(),
                            found,
                            Some(origin),
                        )]);
                    }
                }
                stack.push(Type::Variant(variants.clone()));
                emit(
                    &mut blocks,
                    current,
                    Instruction::MakeVariant {
                        variants,
                        tag,
                        payload_type,
                    },
                    origin,
                );
                return Ok(());
            }
            if word_node.syntax == ForthSyntax::VariantGet {
                let tag = word_node.parameter.expect("parser retains variant tag");
                let found = stack.pop().ok_or_else(|| {
                    vec![control_error(
                        "E-STACK-001",
                        "variant-get requires one variant value",
                        origin.clone(),
                    )]
                })?;
                let Type::Variant(variants) = found else {
                    return Err(vec![type_mismatch_with_stack(
                        &stack,
                        Type::Variant(Vec::new()),
                        found,
                        Some(origin),
                    )]);
                };
                let Some((_, payload_type)) = variants.iter().find(|(name, _)| name == tag) else {
                    return Err(vec![control_error(
                        "E-VARIANT-002",
                        format!("variant has no tag '{tag}'"),
                        origin,
                    )]);
                };
                let payload_type = payload_type.clone();
                stack.push(Type::Option(Box::new(
                    payload_type.clone().unwrap_or(Type::Unit),
                )));
                emit(
                    &mut blocks,
                    current,
                    Instruction::VariantGet {
                        variants,
                        tag: tag.to_string(),
                        payload_type,
                    },
                    origin,
                );
                return Ok(());
            }
            if word_node.syntax == ForthSyntax::RecordField {
                let field = word_node.field.as_deref().expect("parsed record field");
                if !word_node.field_valid {
                    return Err(vec![control_error(
                        "E-RECORD-001",
                        "record fields use name: with an ASCII letter, digit, '_' or '-' name",
                        origin,
                    )]);
                }
                let Some(frame) = record_literals.last_mut() else {
                    return Err(vec![control_error(
                        "E-RECORD-001",
                        "name: is valid only inside { ... }",
                        origin,
                    )]);
                };
                if stack.len() != frame.stack_start + frame.fields.len() {
                    return Err(vec![control_error(
                        "E-RECORD-003",
                        "each record field: label must be followed by exactly one value",
                        origin,
                    )]);
                }
                if frame.fields.iter().any(|existing| existing == field) {
                    return Err(vec![control_error(
                        "E-RECORD-001",
                        format!("record field '{field}' is declared more than once"),
                        origin,
                    )]);
                }
                frame.fields.push(field.to_owned());
                return Ok(());
            }
            if word_node.syntax == ForthSyntax::RecordGetNamed {
                let field = word_node.field.as_deref().expect("parsed record accessor");
                let Some(Type::Record(fields)) = stack.last() else {
                    return Err(vec![control_error(
                        "E-RECORD-004",
                        "record-get:<field> requires a typed record on top of the stack",
                        origin,
                    )]);
                };
                let Some((_, value_type)) = fields.iter().find(|(name, _)| name == field) else {
                    return Err(vec![control_error(
                        "E-RECORD-005",
                        format!("record has no field '{field}'"),
                        origin,
                    )]);
                };
                let value_type = value_type.clone();
                stack.pop();
                stack.push(Type::String);
                emit(
                    &mut blocks,
                    current,
                    Instruction::Constant {
                        value: TypedValue::String(field.to_owned()),
                    },
                    origin.clone(),
                );
                stack.pop();
                stack.push(Type::Option(Box::new(value_type.clone())));
                emit(
                    &mut blocks,
                    current,
                    Instruction::RecordGet {
                        field: field.to_owned(),
                        value_type,
                    },
                    origin,
                );
                return Ok(());
            }
            if word_node.syntax == ForthSyntax::RecordGet {
                let Some(field) = word_node.field.as_deref()
                else {
                    return Err(vec![control_error(
                        "E-RECORD-004",
                        "record-get requires a literal string field name immediately before it",
                        origin,
                    )]);
                };
                if stack.len() < 2 {
                    return Err(vec![control_error(
                        "E-RECORD-004",
                        "record-get requires a typed record followed by a literal field name",
                        origin,
                    )]);
                }
                let record_index = stack.len() - 2;
                let Type::Record(fields) = &stack[record_index] else {
                    return Err(vec![control_error(
                        "E-RECORD-004",
                        "record-get requires a typed record below the field name",
                        origin,
                    )]);
                };
                let Some((_, value_type)) = fields.iter().find(|(name, _)| name == field) else {
                    return Err(vec![control_error(
                        "E-RECORD-005",
                        format!("record has no field '{field}'"),
                        origin,
                    )]);
                };
                let value_type = value_type.clone();
                stack.pop();
                stack.pop();
                stack.push(Type::Option(Box::new(value_type.clone())));
                emit(
                    &mut blocks,
                    current,
                    Instruction::RecordGet {
                        field: field.to_owned(),
                        value_type,
                    },
                    origin,
                );
                return Ok(());
            }
            if word_node.syntax == ForthSyntax::RecordSet {
                let Some(field) = word_node.field.as_deref()
                else {
                    return Err(vec![control_error(
                        "E-RECORD-007",
                        "record-set requires a literal string field name immediately before it",
                        origin,
                    )]);
                };
                if stack.len() < 3 {
                    return Err(vec![control_error(
                        "E-RECORD-007",
                        "record-set requires a typed record, replacement value, and literal field name",
                        origin,
                    )]);
                }
                let record_index = stack.len() - 3;
                let Type::Record(fields) = &stack[record_index] else {
                    return Err(vec![control_error(
                        "E-RECORD-004",
                        "record-set requires a typed record below the replacement value and field name",
                        origin,
                    )]);
                };
                let Some((_, expected)) = fields.iter().find(|(name, _)| name == field) else {
                    return Err(vec![control_error(
                        "E-RECORD-005",
                        format!("record has no field '{field}'"),
                        origin,
                    )]);
                };
                let replacement = &stack[stack.len() - 2];
                if expected != replacement {
                    return Err(vec![type_mismatch_with_stack(
                        &stack,
                        expected.clone(),
                        replacement.clone(),
                        Some(origin),
                    )]);
                }
                let record_type = fields.clone();
                let value_type = expected.clone();
                stack.pop();
                stack.pop();
                stack.pop();
                stack.push(Type::Record(record_type.clone()));
                emit(
                    &mut blocks,
                    current,
                    Instruction::RecordSet {
                        field: field.to_owned(),
                        value_type,
                        record_type,
                    },
                    origin,
                );
                return Ok(());
            }
            match word_node.syntax {
                ForthSyntax::ListOpen => {
                    list_literals.push(ListLiteralFrame {
                        stack_start: stack.len(),
                        origin,
                    });
                    return Ok(());
                }
                ForthSyntax::ListClose => {
                    let Some(frame) = list_literals.pop() else {
                        return Err(vec![control_error(
                            "E-LIST-002",
                            "list close delimiter has no matching [",
                            origin,
                        )]);
                    };
                    let values = &stack[frame.stack_start..];
                    let Some(element_type) = values.first().cloned() else {
                        return Err(vec![control_error(
                            "E-LIST-001",
                            "[ ... ] requires one or more values; use empty-list<T> for an explicitly typed empty list",
                            frame.origin,
                        )]);
                    };
                    if values
                        .iter()
                        .any(|value_type| !element_type.accepts(value_type))
                    {
                        return Err(vec![control_error(
                            "E-LIST-003",
                            "every list literal value must have one consistent type",
                            origin,
                        )]);
                    }
                    let count = values.len() as u32;
                    stack.truncate(frame.stack_start);
                    stack.push(Type::list(element_type.clone()));
                    emit(
                        &mut blocks,
                        current,
                        Instruction::MakeList {
                            element_type,
                            count,
                        },
                        origin,
                    );
                    return Ok(());
                }
                ForthSyntax::MapOpen => {
                    map_literals.push(MapLiteralFrame {
                        stack_start: stack.len(),
                        origin,
                    });
                    return Ok(());
                }
                ForthSyntax::MapClose => {
                    let Some(frame) = map_literals.pop() else {
                        return Err(vec![control_error(
                            "E-MAP-002",
                            "}map has no matching map{",
                            origin,
                        )]);
                    };
                    let values = &stack[frame.stack_start..];
                    if values.is_empty() || !values.len().is_multiple_of(2) {
                        return Err(vec![control_error(
                            "E-MAP-001",
                            "map{ requires one or more key/value pairs",
                            frame.origin,
                        )]);
                    }
                    let key_type = values[0].clone();
                    let value_type = values[1].clone();
                    for pair in values.as_chunks::<2>().0 {
                        if !key_type.accepts(&pair[0]) || !value_type.accepts(&pair[1]) {
                            return Err(vec![control_error(
                                "E-MAP-003",
                                "every map literal key and value must have one consistent type",
                                origin,
                            )]);
                        }
                    }
                    let count = (values.len() / 2) as u32;
                    stack.truncate(frame.stack_start);
                    stack.push(Type::Map(
                        Box::new(key_type.clone()),
                        Box::new(value_type.clone()),
                    ));
                    emit(
                        &mut blocks,
                        current,
                        Instruction::MakeMap {
                            key_type,
                            value_type,
                            count,
                        },
                        origin,
                    );
                    return Ok(());
                }
                ForthSyntax::RecordOpen => {
                    record_literals.push(RecordLiteralFrame {
                        stack_start: stack.len(),
                        fields: Vec::new(),
                        origin,
                    });
                    return Ok(());
                }
                ForthSyntax::RecordClose => {
                    let Some(frame) = record_literals.pop() else {
                        return Err(vec![control_error(
                            "E-RECORD-002",
                            "} has no matching {",
                            origin,
                        )]);
                    };
                    let values = &stack[frame.stack_start..];
                    if values.len() != frame.fields.len() {
                        return Err(vec![control_error(
                            "E-RECORD-003",
                            "each record field: label must be followed by exactly one value",
                            frame.origin,
                        )]);
                    }
                    let fields = frame
                        .fields
                        .into_iter()
                        .zip(values.iter().cloned())
                        .collect::<Vec<_>>();
                    stack.truncate(frame.stack_start);
                    stack.push(Type::Record(fields.clone()));
                    emit(
                        &mut blocks,
                        current,
                        Instruction::MakeRecord { fields },
                        origin,
                    );
                    return Ok(());
                }
                ForthSyntax::Case => {
                    let selector = stack.last().cloned().ok_or_else(|| {
                        vec![control_error(
                            "E-FORTH-CASE-001",
                            "case requires an integer selector",
                            origin.clone(),
                        )]
                    })?;
                    if selector != Type::Int {
                        return Err(vec![type_mismatch_with_stack(
                            &stack,
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

                    return Ok(());
                }
                ForthSyntax::Of => {
                    if !word_node.control_valid {
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
                    return Ok(());
                }
                ForthSyntax::EndOf => {
                    if !word_node.control_valid {
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
                    if !word_node.next_is_terminal {
                        emit(&mut blocks, current, Instruction::Dup, origin.clone());
                        stack.push(Type::Int);
                    }
                    return Ok(());
                }
                ForthSyntax::Otherwise => {
                    if !word_node.control_valid {
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
                    return Ok(());
                }
                ForthSyntax::EndCase => {
                    if !word_node.control_valid {
                        return Err(vec![control_error(
                            "E-FORTH-CASE-008",
                            "endcase has no matching case",
                            origin,
                        )]);
                    }
                    let Some(frame) = cases.pop() else {
                        unreachable!("case control has a case frame");
                    };

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
                    return Ok(());
                }
                ForthSyntax::If => {
                    let condition = stack.pop().ok_or_else(|| {
                        vec![control_error(
                            "E-STACK-001",
                            "if requires a boolean condition",
                            origin.clone(),
                        )]
                    })?;
                    if condition != Type::Bool {
                        return Err(vec![type_mismatch_with_stack(
                            &stack,
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

                    current = then_block;
                    return Ok(());
                }
                ForthSyntax::MatchOption | ForthSyntax::MatchResult => {
                    let option = stack.pop().ok_or_else(|| {
                        vec![control_error(
                            "E-STACK-001",
                            format!("{word} requires a tagged condition"),
                            origin.clone(),
                        )]
                    })?;
                    let (then_type, else_type, condition) = match option {
                        Type::Option(inner) if word_node.syntax == ForthSyntax::MatchOption => {
                            ((*inner).clone(), None, MatchCondition::Option)
                        }
                        Type::Result(ok, err) if word_node.syntax == ForthSyntax::MatchResult => {
                            ((*ok).clone(), Some((*err).clone()), MatchCondition::Result)
                        }
                        _ => {
                            return Err(vec![control_error(
                                "E-TYPE-012",
                                if word_node.syntax == ForthSyntax::MatchOption {
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
                    return Ok(());
                }
                ForthSyntax::Else => {
                    if !word_node.control_valid {
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
                    return Ok(());
                }
                ForthSyntax::Then => {
                    if !word_node.control_valid {
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
                    return Ok(());
                }
                ForthSyntax::Begin | ForthSyntax::NamedBegin => {
                    let label = if word_node.syntax == ForthSyntax::NamedBegin {
                        let Some(label) = word_node.operand.as_ref().and_then(|operand| operand.name)
                        else {
                            return Err(vec![control_error(
                                "E-FORTH-LOOP-006",
                                "begin: requires a loop label",
                                origin,
                            )]);
                        };
                        if !word_node.operand.as_ref().expect("parsed named loop operand").valid_loop_label
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
                        Some(label.to_owned())
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

                    return Ok(());
                }
                ForthSyntax::Break | ForthSyntax::Continue => {
                    let Some(label) = word_node.operand.as_ref().and_then(|operand| operand.name)
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
                        .find(|frame| frame.label.as_deref() == Some(label))
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
                    let target = if word_node.syntax == ForthSyntax::Break {
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
                    return Ok(());
                }
                ForthSyntax::While => {
                    if !word_node.control_valid {
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
                        return Err(vec![type_mismatch_with_stack(
                            &stack,
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
                    return Ok(());
                }
                ForthSyntax::Repeat => {
                    if !word_node.control_valid {
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
                    return Ok(());
                }
                ForthSyntax::Until => {
                    if !word_node.control_valid {
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
                        return Err(vec![type_mismatch_with_stack(
                            &stack,
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
                    return Ok(());
                }
                _ => {}
            }
        }
        let instruction = match node {
            ForthBodyNode::Primitive(word_node) => match word_node.syntax {
                ForthSyntax::Dup => {
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
                }
                ForthSyntax::Drop => {
                    stack.pop().ok_or_else(|| {
                        vec![VmDiagnostic::error(
                            "E-STACK-001",
                            DiagnosticPhase::TypeInference,
                            "drop requires one value",
                            Some(origin.clone()),
                        )]
                    })?;
                    Instruction::Drop
                }
                ForthSyntax::Swap => {
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
                }
                ForthSyntax::Defer => {
                    let closure = stack.pop().ok_or_else(|| {
                        vec![VmDiagnostic::error(
                            "E-FIBER-021",
                            DiagnosticPhase::TypeInference,
                            "defer requires a typed yielding quotation",
                            Some(origin.clone()),
                        )]
                    })?;
                    let Type::Function {
                        arguments,
                        result,
                        effects: closure_effects,
                        suspension: Some(closure_suspension),
                    } = closure
                    else {
                        return Err(vec![VmDiagnostic::error(
                            "E-FIBER-021",
                            DiagnosticPhase::TypeInference,
                            "defer requires a typed yielding quotation",
                            Some(origin),
                        )]);
                    };
                    if !arguments.is_empty() || !closure_effects.is_pure() {
                        return Err(vec![VmDiagnostic::error(
                            "E-FIBER-022",
                            DiagnosticPhase::TypeInference,
                            "cooperative defer requires a pure zero-argument quotation",
                            Some(origin),
                        )]);
                    }
                    if *closure_suspension.resume_type != Type::Unit {
                        return Err(vec![VmDiagnostic::error(
                            "E-FIBER-023",
                            DiagnosticPhase::TypeInference,
                            "this runtime version supports only unit-resumed producer fibers",
                            Some(origin),
                        )]);
                    }
                    stack.push(Type::Fiber(closure_suspension.yield_type, result));
                    Instruction::DeferFiber
                }
                ForthSyntax::DeferCpu => {
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
                        suspension: closure_suspension,
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
                    if let Some(closure_suspension) = closure_suspension {
                        if *closure_suspension.yield_type != Type::Unit
                            || *closure_suspension.resume_type != Type::Unit
                        {
                            return Err(vec![VmDiagnostic::error(
                                "E-YIELD-003",
                                DiagnosticPhase::TypeInference,
                                "CPU tasks may yield only unit timeslices; use defer for produced values",
                                Some(origin),
                            )]);
                        }
                    }
                    stack.push(Type::Task(result));
                    Instruction::DeferCpu
                }
                ForthSyntax::TaskPoll => {
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
                    stack.push(Type::task_poll(*result));
                    Instruction::PollCpuFiber
                }
                ForthSyntax::TaskJoin => {
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
                }
                ForthSyntax::TaskCancel => {
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
                }
                ForthSyntax::FiberNext => {
                    let fiber = stack.pop().ok_or_else(|| {
                        vec![VmDiagnostic::error(
                            "E-FIBER-024",
                            DiagnosticPhase::TypeInference,
                            "fiber-next requires fiber<Y,R>",
                            Some(origin.clone()),
                        )]
                    })?;
                    let Type::Fiber(yield_type, result_type) = fiber else {
                        return Err(vec![VmDiagnostic::error(
                            "E-FIBER-024",
                            DiagnosticPhase::TypeInference,
                            "fiber-next requires fiber<Y,R>",
                            Some(origin),
                        )]);
                    };
                    stack.push(Type::fiber_step(*yield_type, *result_type));
                    Instruction::NextFiber
                }
                ForthSyntax::FiberJoin => {
                    let fiber = stack.pop().ok_or_else(|| {
                        vec![VmDiagnostic::error(
                            "E-FIBER-025",
                            DiagnosticPhase::TypeInference,
                            "fiber-join requires fiber<Y,R>",
                            Some(origin.clone()),
                        )]
                    })?;
                    let Type::Fiber(_, result_type) = fiber else {
                        return Err(vec![VmDiagnostic::error(
                            "E-FIBER-025",
                            DiagnosticPhase::TypeInference,
                            "fiber-join requires fiber<Y,R>",
                            Some(origin),
                        )]);
                    };
                    stack.push(*result_type);
                    Instruction::JoinFiber
                }
                ForthSyntax::FiberCancel => {
                    let fiber = stack.pop().ok_or_else(|| {
                        vec![VmDiagnostic::error(
                            "E-FIBER-026",
                            DiagnosticPhase::TypeInference,
                            "fiber-cancel requires fiber<Y,R>",
                            Some(origin.clone()),
                        )]
                    })?;
                    if !matches!(fiber, Type::Fiber(_, _)) {
                        return Err(vec![VmDiagnostic::error(
                            "E-FIBER-026",
                            DiagnosticPhase::TypeInference,
                            "fiber-cancel requires fiber<Y,R>",
                            Some(origin),
                        )]);
                    }
                    stack.push(Type::Unit);
                    Instruction::CancelFiber
                }
                ForthSyntax::PropagateResult => {
                    let Some(expected_return) = expected_return else {
                        return Err(vec![VmDiagnostic::error(
                            "E-RESULT-TRY-002",
                            DiagnosticPhase::TypeInference,
                            "? is valid only inside a typed definition returning result<T,E>",
                            Some(origin),
                        )]);
                    };
                    let [Type::Result(return_ok_type, return_error_type)] = expected_return else {
                        return Err(vec![VmDiagnostic::error(
                            "E-RESULT-TRY-002",
                            DiagnosticPhase::TypeInference,
                            "? is valid only inside a typed definition returning one result<T,E>",
                            Some(origin),
                        )]);
                    };
                    let result = stack.pop().ok_or_else(|| {
                        vec![VmDiagnostic::error(
                            "E-RESULT-TRY-001",
                            DiagnosticPhase::TypeInference,
                            "? requires result<T,E>",
                            Some(origin.clone()),
                        )]
                    })?;
                    let Type::Result(ok_type, error_type) = result else {
                        return Err(vec![VmDiagnostic::error(
                            "E-RESULT-TRY-001",
                            DiagnosticPhase::TypeInference,
                            "? requires result<T,E>",
                            Some(origin),
                        )]);
                    };
                    if !return_error_type.accepts(&error_type) {
                        return Err(vec![type_mismatch_with_stack(
                            &stack,
                            (**return_error_type).clone(),
                            *error_type.clone(),
                            Some(origin),
                        )]);
                    }
                    stack.push(*ok_type);
                    Instruction::PropagateResult {
                        return_ok_type: (**return_ok_type).clone(),
                        error_type: (**return_error_type).clone(),
                    }

                }
                _ => unreachable!("primitive was handled before general instruction emission"),
            },
            ForthBodyNode::Call(call) => {
                let word = call.name;
                    let Some(signature) = vocabulary.get(word) else {
                        let mut diagnostic = VmDiagnostic::error(
                            "E-LINK-002",
                            DiagnosticPhase::NameResolution,
                            format!("unknown Co-Forth word '{word}'"),
                            Some(origin),
                        );
                        let nearest = nearest_names(word, vocabulary.keys().map(String::as_str));
                        if !nearest.is_empty() {
                            diagnostic.hints.push(format!(
                                "did you mean {}?",
                                nearest
                                    .iter()
                                    .map(|name| format!("`{name}`"))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));
                        }
                        return Err(vec![diagnostic]);
                    };
                    let concrete_signature =
                        instantiate_signature_types(signature, &stack, &origin)
                            .map_err(|diagnostic| vec![diagnostic])?;
                    apply_signature_types(signature, &mut stack, &origin)
                        .map_err(|diagnostic| vec![diagnostic])?;
                    effects = effects.union(&signature.effects);
                    merge_suspension_contract(
                        &mut suspension,
                        concrete_signature.suspension.as_ref(),
                        &origin,
                    )?;
                    match &call.kind {
                    ForthCallKind::Yield => {
                        let value_type = concrete_signature
                            .input
                            .values
                            .last()
                            .cloned()
                            .expect("yield has one typed input");
                        Instruction::Yield { value_type }
                    }
                    ForthCallKind::OutputOpen => {
                        Instruction::OutputOpen
                    }
                    ForthCallKind::Ui(operation) => {
                        Instruction::UiEffect {
                            operation: *operation,
                            input: concrete_signature.input.values.clone(),
                            output: concrete_signature.output.values.clone(),
                        }
                    }
                    ForthCallKind::Function if signature.effects.0.len() == 1 => {
                        Instruction::CapabilityRequest {
                            requirement: signature.effects.0.iter().next().unwrap().clone(),
                            input: concrete_signature.input.values.clone(),
                            output: concrete_signature.output.values.clone(),
                        }
                    }
                    ForthCallKind::Function => {
                        Instruction::Call { function: word.to_owned() }
                    }

                    }
            }
            _ => unreachable!("structured and reference nodes were emitted above"),
        };
        emit(&mut blocks, current, instruction, origin);
        Ok(())
    })?;
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
    if let Some(frame) = map_literals.last() {
        return Err(vec![control_error(
            "E-MAP-004",
            "unterminated map{ literal",
            frame.origin.clone(),
        )]);
    }
    if let Some(frame) = list_literals.last() {
        return Err(vec![control_error(
            "E-LIST-004",
            "unterminated [ list literal",
            frame.origin.clone(),
        )]);
    }
    if let Some(frame) = record_literals.last() {
        return Err(vec![control_error(
            "E-RECORD-007",
            "unterminated { record literal",
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
            output: StackRow::closed(
                expected_return
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| stack.clone()),
            ),
            effects,
            control: if suspension.is_some() {
                ControlEffect::MaySuspend
            } else {
                ControlEffect::Returns
            },
            suspension,
        },
        locals: locals.iter().map(|local| local.ty.clone()).collect(),
        captures: captures.iter().map(|capture| capture.ty.clone()).collect(),
        entry: 0,
        blocks,
    };
    let mut functions = Module::single(function).functions;
    for (name, function) in available_functions {
        functions.insert(name.clone(), function.clone());
    }
    certify_module(source_id.to_string(), "main", functions, vocabulary)
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

fn merge_suspension_contract(
    current: &mut Option<SuspensionSignature>,
    incoming: Option<&SuspensionSignature>,
    origin: &SourceOrigin,
) -> Result<(), Vec<VmDiagnostic>> {
    let Some(incoming) = incoming else {
        return Ok(());
    };
    if let Some(current) = current {
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
        *current = Some(incoming.clone());
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct ForthDefinitionAst<'source> {
    name: &'source str,
    documentation: Option<&'source str>,
    signature: StackSignature,
    declares_pure: bool,
    locals: Vec<LocalBinding<'source>>,
    body: ForthBodyAst<'source>,
    start: usize,
    end: usize,
}

fn parse_forth_module<'source>(
    source_id: &str,
    source: &'source str,
) -> Result<ForthModuleAst<'source>, Vec<VmDiagnostic>> {
    let tokens = tokenize(source_id, source)?;
    let mut definitions = Vec::new();
    let mut main_atoms = Vec::new();
    let mut cursor = 0;
    while cursor < tokens.len() {
        if !token_is(&tokens[cursor], ":") {
            main_atoms.push(tokens[cursor].clone());
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
        let (signature, declares_pure, locals) =
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
        let definition_end = tokens[end].end;
        definitions.push(ForthDefinitionAst {
            name,
            documentation: forth_definition_documentation(source, definition_start),
            signature,
            declares_pure,
            body: parse_forth_body(
                source_id,
                source,
                &tokens[body_start_index..end],
                &locals,
                &[],
            ),
            locals,
            start: definition_start,
            end: definition_end,
        });
        cursor = end + 1;
    }
    Ok(ForthModuleAst {
        definitions,
        body: parse_forth_body(source_id, source, &main_atoms, &[], &[]),
    })
}

/// Own control pairing, collection delimiters, and reference operands here.
/// Malformed structure retains its position in the tree: its diagnostic is
/// emitted when visited, preserving precedence over later semantic failures.
fn parse_forth_body<'source>(
    source_id: &str,
    source: &'source str,
    atoms: &[Token<'source>],
    locals: &[LocalBinding<'source>],
    captures: &[LocalBinding<'source>],
) -> ForthBodyAst<'source> {
    parse_forth_body_cursor(
        &mut ForthParser::new(atoms),
        source_id,
        source,
        atoms.len(),
        locals,
        captures,
    )
}

fn parse_forth_body_cursor<'source>(
    parser: &mut ForthParser<'_, 'source>,
    source_id: &str,
    source: &'source str,
    end: usize,
    locals: &[LocalBinding<'source>],
    captures: &[LocalBinding<'source>],
) -> ForthBodyAst<'source> {
    let mut nodes = Vec::new();
    let mut groups: Vec<(ForthSyntax, Vec<ForthBodyNode<'source>>)> = Vec::new();
    let mut controls = Vec::new();
    let mut record_depth = 0usize;
    let mut previous = None;
    while parser.cursor < end {
        let (index, token) = parser.next().expect("body ends inside its token stream");
        if let Some((pipe_index, close_index)) = parser.quotations[index] {
            let captured = captures
                .iter()
                .enumerate()
                .filter(|(_, capture)| !locals.iter().any(|local| local.name == capture.name))
                .map(|(index, capture)| ForthCaptureAst {
                    binding: capture.clone(),
                    index: index as u32,
                    from_local: false,
                })
                .chain(
                    locals
                        .iter()
                        .enumerate()
                        .map(|(index, local)| ForthCaptureAst {
                            binding: local.clone(),
                            index: index as u32,
                            from_local: true,
                        }),
                )
                .collect::<Vec<_>>();
            let visible = captured
                .iter()
                .map(|capture| capture.binding.clone())
                .collect::<Vec<_>>();
            let quote_end = parser.atoms[close_index].end;
            let signature = parse_quotation_signature(
                source_id,
                source,
                &parser.atoms[index + 1..pipe_index],
                &origin(source_id, source, token.start, quote_end),
            );
            parser.advance_to(pipe_index + 1);
            let body =
                parse_forth_body_cursor(parser, source_id, source, close_index, &[], &visible);
            parser
                .next()
                .expect("indexed quotation has a closing bracket");
            nodes.push(ForthBodyNode::Quotation(ForthQuotationAst {
                open: token,
                signature,
                captures: captured,
                body,
                end: quote_end,
            }));
            previous = Some(index);
            continue;
        }
        if let Some(value) = forth_literal_value(&token) {
            nodes.push(ForthBodyNode::Literal(ForthLiteralAst {
                token: token.clone(),
                value,
            }));
        } else {
            let TokenValue::Word(name) = token.value else {
                unreachable!("all non-word token values are parser-owned literals");
            };
            let mut syntax = ForthSyntax::parse(name);
            let parameter = if let Some(arguments) = name
                .strip_prefix("empty-map<")
                .and_then(|value| value.strip_suffix('>'))
            {
                syntax = ForthSyntax::EmptyMap;
                Some(arguments)
            } else if let Some(element) = name
                .strip_prefix("empty-list<")
                .and_then(|value| value.strip_suffix('>'))
            {
                syntax = ForthSyntax::EmptyList;
                Some(element)
            } else if name.starts_with("variant<") {
                syntax = ForthSyntax::Variant;
                Some(name)
            } else if let Some(tag) = name
                .strip_prefix("variant-get<")
                .and_then(|value| value.strip_suffix('>'))
            {
                syntax = ForthSyntax::VariantGet;
                Some(tag)
            } else {
                None
            };
            let named_field = name
                .strip_prefix("field:")
                .or_else(|| (record_depth > 0).then(|| name.strip_suffix(':')).flatten());
            let mut field = if let Some(field) = named_field {
                syntax = ForthSyntax::RecordField;
                Some(Cow::Borrowed(field))
            } else if let Some(field) = name.strip_prefix("record-get:") {
                syntax = ForthSyntax::RecordGetNamed;
                Some(Cow::Borrowed(field))
            } else {
                None
            };
            match syntax {
                ForthSyntax::RecordOpen => record_depth += 1,
                ForthSyntax::RecordClose => record_depth = record_depth.saturating_sub(1),
                _ => {}
            }
            let opens_control = match syntax {
                ForthSyntax::If | ForthSyntax::MatchOption | ForthSyntax::MatchResult => {
                    Some(ControlKind::If)
                }
                ForthSyntax::Begin | ForthSyntax::NamedBegin => Some(ControlKind::Loop),
                ForthSyntax::Case => Some(ControlKind::Case),
                _ => None,
            };
            let expected_control = match syntax {
                ForthSyntax::Else | ForthSyntax::Then => Some(ControlKind::If),
                ForthSyntax::While | ForthSyntax::Repeat | ForthSyntax::Until => {
                    Some(ControlKind::Loop)
                }
                ForthSyntax::Of
                | ForthSyntax::EndOf
                | ForthSyntax::Otherwise
                | ForthSyntax::EndCase => Some(ControlKind::Case),
                _ => None,
            };
            let control_valid = expected_control.is_none_or(|kind| controls.last() == Some(&kind));
            if let Some(kind) = opens_control {
                controls.push(kind);
            } else if control_valid
                && matches!(
                    syntax,
                    ForthSyntax::Then
                        | ForthSyntax::Repeat
                        | ForthSyntax::Until
                        | ForthSyntax::EndCase
                )
            {
                controls.pop();
            }
            let opens_group = match syntax {
                ForthSyntax::MatchOption | ForthSyntax::MatchResult => Some(ForthSyntax::If),
                ForthSyntax::NamedBegin => Some(ForthSyntax::Begin),
                ForthSyntax::If
                | ForthSyntax::Begin
                | ForthSyntax::Case
                | ForthSyntax::ListOpen
                | ForthSyntax::MapOpen
                | ForthSyntax::RecordOpen => Some(syntax),
                _ => None,
            };
            if let Some(kind) = opens_group {
                groups.push((kind, std::mem::take(&mut nodes)));
            }
            let consumes_operand = matches!(
                syntax,
                ForthSyntax::QuoteTarget
                    | ForthSyntax::NamedBegin
                    | ForthSyntax::Break
                    | ForthSyntax::Continue
            );
            if matches!(syntax, ForthSyntax::RecordGet | ForthSyntax::RecordSet) {
                field = previous.and_then(|index: usize| match &parser.atoms[index].value {
                    TokenValue::String(field) => Some(Cow::Owned(field.clone())),
                    _ => None,
                });
            }
            previous = Some(index);
            let operand = if consumes_operand && parser.cursor < end {
                let (operand_index, operand) = parser.next().expect("reference operand exists");
                previous = Some(operand_index);
                if let Some((_, close)) = parser.quotations[operand_index] {
                    parser.advance_to(close + 1);
                }
                let name = match operand.value {
                    TokenValue::Word(name) => Some(name),
                    _ => None,
                };
                let valid_loop_label = name.is_some_and(|name| {
                    !name.is_empty()
                        && !matches!(name, "if" | "else" | "then" | "while" | "repeat" | "until")
                });
                Some(ForthReferenceAst {
                    token: operand,
                    name,
                    valid_loop_label,
                })
            } else {
                None
            };
            let type_argument = match syntax {
                ForthSyntax::EmptyMap => Some(
                    parse_type_name(&format!("map<{}>", parameter.expect("map arguments")))
                        .map_err(|_| ()),
                ),
                ForthSyntax::EmptyList => {
                    Some(parse_type_name(parameter.expect("list type argument")).map_err(|_| ()))
                }
                _ => None,
            };
            let variant = (syntax == ForthSyntax::Variant)
                .then(|| parse_variant_constructor_name(name))
                .flatten()
                .map(|(variants, tag, payload_type)| ForthVariantAst {
                    variants,
                    tag,
                    payload_type,
                });
            nodes.push(
                ForthOperationAst {
                    token: token.clone(),
                    name,
                    parameter,
                    type_argument,
                    variant,
                    syntax,
                    operand,
                    field_valid: field.as_deref().is_some_and(|field| {
                        !field.is_empty()
                            && field.chars().all(|character| {
                                character.is_ascii_alphanumeric()
                                    || character == '_'
                                    || character == '-'
                            })
                    }),
                    field,
                    control_valid,
                    next_is_terminal: parser.atoms.get(parser.cursor).is_some_and(|token| {
                        token_is(token, "otherwise") || token_is(token, "endcase")
                    }),
                }
                .into_node(locals, captures),
            );
            let closes_group = match syntax {
                ForthSyntax::Then => Some(ForthSyntax::If),
                ForthSyntax::Repeat | ForthSyntax::Until => Some(ForthSyntax::Begin),
                ForthSyntax::EndCase => Some(ForthSyntax::Case),
                ForthSyntax::ListClose => Some(ForthSyntax::ListOpen),
                ForthSyntax::MapClose => Some(ForthSyntax::MapOpen),
                ForthSyntax::RecordClose => Some(ForthSyntax::RecordOpen),
                _ => None,
            };
            if let Some(index) =
                closes_group.and_then(|kind| groups.iter().rposition(|(open, _)| *open == kind))
            {
                while groups.len() > index {
                    let (_, mut parent) = groups.pop().expect("group index exists");
                    parent.push(ForthBodyNode::Group(ForthBodyAst { nodes }));
                    nodes = parent;
                }
            }
        }
        if matches!(nodes.last(), Some(ForthBodyNode::Literal(_))) {
            previous = Some(index);
        }
    }
    while let Some((_, mut parent)) = groups.pop() {
        parent.push(ForthBodyNode::Group(ForthBodyAst { nodes }));
        nodes = parent;
    }
    ForthBodyAst { nodes }
}

impl<'source> ForthOperationAst<'source> {
    fn into_node(
        self,
        locals: &[LocalBinding<'source>],
        captures: &[LocalBinding<'source>],
    ) -> ForthBodyNode<'source> {
        if self.syntax == ForthSyntax::Call {
            if let Some((index, local)) = locals
                .iter()
                .enumerate()
                .find(|(_, local)| local.name == self.name)
            {
                return ForthBodyNode::LocalReference(ForthBindingReferenceAst {
                    token: self.token,
                    index: index as u32,
                    ty: local.ty.clone(),
                });
            }
            if let Some((index, capture)) = captures
                .iter()
                .enumerate()
                .find(|(_, capture)| capture.name == self.name)
            {
                return ForthBodyNode::CaptureReference(ForthBindingReferenceAst {
                    token: self.token,
                    index: index as u32,
                    ty: capture.ty.clone(),
                });
            }
            let kind = match self.name {
                "yield" => ForthCallKind::Yield,
                "output-open" => ForthCallKind::OutputOpen,
                name => output_operation(name)
                    .map(ForthCallKind::Ui)
                    .unwrap_or(ForthCallKind::Function),
            };
            return ForthBodyNode::Call(ForthCallAst {
                token: self.token,
                name: self.name,
                kind,
            });
        }
        match self.syntax {
            ForthSyntax::QuoteTarget => ForthBodyNode::QuotationReference(self),
            ForthSyntax::RecordGet | ForthSyntax::RecordSet | ForthSyntax::RecordGetNamed => {
                ForthBodyNode::RecordReference(self)
            }
            ForthSyntax::ListOpen
            | ForthSyntax::ListClose
            | ForthSyntax::MapOpen
            | ForthSyntax::MapClose
            | ForthSyntax::RecordOpen
            | ForthSyntax::RecordClose
            | ForthSyntax::RecordField => ForthBodyNode::Collection(self),
            ForthSyntax::Case
            | ForthSyntax::Of
            | ForthSyntax::EndOf
            | ForthSyntax::Otherwise
            | ForthSyntax::EndCase
            | ForthSyntax::If
            | ForthSyntax::MatchOption
            | ForthSyntax::MatchResult
            | ForthSyntax::Else
            | ForthSyntax::Then
            | ForthSyntax::Begin
            | ForthSyntax::NamedBegin
            | ForthSyntax::Break
            | ForthSyntax::Continue
            | ForthSyntax::While
            | ForthSyntax::Repeat
            | ForthSyntax::Until => ForthBodyNode::Control(self),
            _ => ForthBodyNode::Primitive(self),
        }
    }
}

fn forth_literal_value(token: &Token) -> Option<TypedValue> {
    match &token.value {
        TokenValue::String(value) => Some(TypedValue::String(value.clone())),
        TokenValue::Json(value) => Some(TypedValue::Json(value.clone())),
        TokenValue::Word(word) => word
            .strip_prefix('\'')
            .filter(|symbol| !symbol.is_empty())
            .map(|symbol| TypedValue::Symbol(symbol.to_string()))
            .or_else(|| word.parse::<i64>().ok().map(TypedValue::Int))
            .or(match *word {
                "true" => Some(TypedValue::Bool(true)),
                "false" => Some(TypedValue::Bool(false)),
                _ => None,
            }),
    }
}

/// Read a single public documentation line immediately preceding a typed
/// definition. `\\ finch-doc:` is deliberately source-only metadata: the
/// tokenizer discards the comment and the string never reaches the operand
/// stack. A blank or non-comment line breaks the association so a doc cannot
/// accidentally attach across an unrelated form.
fn forth_definition_documentation(source: &str, definition_start: usize) -> Option<&str> {
    let prefix = &source[..definition_start];
    let line = prefix.lines().rev().find(|line| !line.trim().is_empty())?;
    line.trim()
        .strip_prefix("\\ finch-doc:")
        .map(str::trim)
        .filter(|documentation| !documentation.is_empty())
}

fn parse_definition_signature<'source>(
    source_id: &str,
    source: &str,
    tokens: &[Token<'source>],
    fallback: &Token,
) -> Result<(StackSignature, bool, Vec<LocalBinding<'source>>), Vec<VmDiagnostic>> {
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
    let input_tokens = &tokens[..separator];
    let output_tokens = &tokens[separator + 1..output_end];
    validate_preserved_stack_row(source_id, source, input_tokens, fallback, "input")?;
    validate_preserved_stack_row(source_id, source, output_tokens, fallback, "output")?;
    let (input, locals) = parse_input_stack_types(source_id, source, &input_tokens[1..])?;
    let output = parse_stack_types(source_id, source, &output_tokens[1..])?;
    let declares_pure = if let Some(effect) = effect {
        let annotation = &tokens[effect + 1..];
        if annotation.len() == 1 && token_is(&annotation[0], "pure") {
            true
        } else if annotation.len() == 1 && token_is(&annotation[0], "infer") {
            false
        } else {
            return Err(vec![definition_error(
                source_id,
                source,
                tokens.get(effect).unwrap_or(fallback),
                "effect annotation must currently be 'pure' or 'infer'",
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
            suspension: None,
        },
        declares_pure,
        locals,
    ))
}

/// Signature-local names are the sole lexical-local syntax for typed words.
/// `width:int` is both a public input contract and a lowering instruction to
/// move that input into the frame at entry.  Either name every input or name
/// none: partial naming makes the stack contract harder to read than it is
/// worth.
fn parse_input_stack_types<'source>(
    source_id: &str,
    source: &str,
    tokens: &[Token<'source>],
) -> Result<(Vec<Type>, Vec<LocalBinding<'source>>), Vec<VmDiagnostic>> {
    let mut types = Vec::with_capacity(tokens.len());
    let mut locals = Vec::new();
    let mut saw_named = false;
    let mut saw_unnamed = false;
    for token in tokens {
        let TokenValue::Word(spelling) = &token.value else {
            return Err(vec![definition_error(
                source_id,
                source,
                token,
                "stack type must be an identifier",
            )]);
        };
        let named = (!spelling.starts_with("record{"))
            .then(|| spelling.split_once(':'))
            .flatten();
        let (name, type_spelling) = match named {
            Some((name, type_spelling)) if !name.is_empty() && !type_spelling.is_empty() => {
                saw_named = true;
                (Some(name), type_spelling)
            }
            _ => {
                saw_unnamed = true;
                (None, *spelling)
            }
        };
        let ty = parse_type_name(type_spelling).map_err(|_| {
            vec![definition_error(
                source_id,
                source,
                token,
                format!("unknown stack type '{type_spelling}'"),
            )]
        })?;
        if let Some(name) = name {
            if !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                || locals.iter().any(|local: &LocalBinding| local.name == name)
            {
                return Err(vec![definition_error(
                    source_id,
                    source,
                    token,
                    format!("invalid or duplicate input name '{name}'"),
                )]);
            }
            locals.push(LocalBinding {
                name,
                ty: ty.clone(),
                start: token.start,
                end: token.end,
            });
        }
        types.push(ty);
    }
    if saw_named && saw_unnamed {
        return Err(vec![definition_error(
            source_id,
            source,
            tokens.first().expect("mixed names require an input token"),
            "typed word inputs must either all use name:type or all be unnamed types",
        )]);
    }
    Ok((types, locals))
}

fn parse_stack_types(
    source_id: &str,
    source: &str,
    tokens: &[Token],
) -> Result<Vec<Type>, Vec<VmDiagnostic>> {
    tokens
        .iter()
        .map(|token| {
            let TokenValue::Word(name) = &token.value else {
                return Err(vec![definition_error(
                    source_id,
                    source,
                    token,
                    "stack type must be an identifier",
                )]);
            };
            parse_type_name(name).map_err(|_| {
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

/// Every typed Co-Forth definition is row-polymorphic over the caller stack.
/// The written `S` is not decorative: requiring it on both sides makes the
/// source contract match the verifier's stack row and prevents a definition
/// from appearing closed while silently preserving arbitrary lower values.
fn validate_preserved_stack_row(
    source_id: &str,
    source: &str,
    tokens: &[Token],
    fallback: &Token,
    side: &str,
) -> Result<(), Vec<VmDiagnostic>> {
    let Some(first) = tokens.first() else {
        return Err(vec![definition_error(
            source_id,
            source,
            fallback,
            format!("typed word signature {side} must begin with preserved stack row 'S'"),
        )]);
    };
    if !token_is(first, "S") {
        return Err(vec![definition_error(
            source_id,
            source,
            first,
            format!("typed word signature {side} must begin with preserved stack row 'S'"),
        )]);
    }
    if let Some(extra) = tokens[1..].iter().find(|token| token_is(token, "S")) {
        return Err(vec![definition_error(
            source_id,
            source,
            extra,
            "preserved stack row 'S' may appear only once on each side of '--'",
        )]);
    }
    Ok(())
}

fn token_is(token: &Token, expected: &str) -> bool {
    matches!(&token.value, TokenValue::Word(value) if *value == expected)
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

/// A type mismatch, carrying the stack as the verifier saw it.
///
/// "expected int, found string" names the word that rejected the value and not the value itself.
/// A reader — or a model asked to repair the program — needs to know what was on the stack to find
/// where the wrong value came from, and the verifier is the only place that still knows.
fn type_mismatch_with_stack(
    stack: &[Type],
    expected: Type,
    found: Type,
    origin: Option<SourceOrigin>,
) -> VmDiagnostic {
    let mut diagnostic = VmDiagnostic::type_mismatch(expected, found, origin);
    diagnostic.hints.push(describe_stack(stack));
    diagnostic
}

/// The stack bottom-first, which is the order a Forth author writes and reads it in.
fn describe_stack(stack: &[Type]) -> String {
    if stack.is_empty() {
        return "the stack is empty here".to_string();
    }
    format!(
        "stack here, bottom first: {}",
        stack
            .iter()
            .map(|ty| ty.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn control_error(code: &str, message: impl Into<String>, origin: SourceOrigin) -> VmDiagnostic {
    VmDiagnostic::error(code, DiagnosticPhase::TypeInference, message, Some(origin))
}

fn tokenize<'source>(
    source_id: &str,
    source: &'source str,
) -> Result<Vec<Token<'source>>, Vec<VmDiagnostic>> {
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
        // Commas are optional collection separators.  They are ignored by
        // Co-Forth's ordinary stack syntax as well, so `[1, 2]` and
        // `[ 1 2 ]` share the same typed lowering.
        if bytes[cursor] == b',' {
            cursor += 1;
            continue;
        }
        if bytes[cursor] == b'\\' {
            while cursor < bytes.len() && bytes[cursor] != b'\n' {
                cursor += 1;
            }
            continue;
        }
        let start = cursor;
        // Preserve the conventional quotation token before treating `[` as a
        // typed-list delimiter.
        if source[start..].starts_with("[']") {
            cursor += 3;
            tokens.push(Token {
                value: TokenValue::Word("[']"),
                start,
                end: cursor,
            });
            continue;
        }
        // A compact record type is one signature token even though its field
        // syntax uses braces. Record literals use `{ field: value }`; keeping
        // this no-whitespace spelling distinct lets annotations remain a
        // direct representation of `Type::Record`.
        if let Some(end) = compact_braced_type_end(source, start) {
            tokens.push(Token {
                value: TokenValue::Word(&source[start..end]),
                start,
                end,
            });
            cursor = end;
            continue;
        }
        // Parameterized type signatures are single tokens even when their
        // type arguments use commas. Ordinary collection commas remain
        // optional separators, but `result<int,string>` must not become two
        // unrelated stack types during definition parsing.
        if let Some(end) = parameterized_type_end(source, start) {
            tokens.push(Token {
                value: TokenValue::Word(&source[start..end]),
                start,
                end,
            });
            cursor = end;
            continue;
        }
        // These existing multi-character forms take precedence over generic
        // brace punctuation. `map{`, `list{`, and `record{` retain their
        // literal spellings; purity is written explicitly as `! pure`.
        if let Some(word) = ["map{", "}map", "list{", "}list", "record{", "}record"]
            .into_iter()
            .find(|word| source[start..].starts_with(word))
        {
            cursor += word.len();
            tokens.push(Token {
                value: TokenValue::Word(word),
                start,
                end: cursor,
            });
            continue;
        }
        // Generic collection type applications are one source token even
        // though their closing `>` may be immediately followed by ordinary
        // collection punctuation.
        if source[start..].starts_with("empty-map<") || source[start..].starts_with("empty-list<") {
            let Some(close) = source[start..].find('>') else {
                return Err(vec![VmDiagnostic::error(
                    "E-READ-007",
                    DiagnosticPhase::Reader,
                    "unterminated typed empty collection",
                    Some(origin(source_id, source, start, source.len())),
                )]);
            };
            cursor = start + close + 1;
            tokens.push(Token {
                value: TokenValue::Word(&source[start..cursor]),
                start,
                end: cursor,
            });
            continue;
        }
        // A JSON object begins with a quoted key (or is `{}`).  Keep it as a
        // managed JSON value so ordinary pasted JSON does not need escaping
        // or conversion into a record/map source form. Bare identifier field
        // labels continue into the typed `{ field: value }` record grammar.
        if bytes[start] == b'{' && looks_like_json_object(source, start) {
            let (value, end) = read_json_object(source_id, source, start)?;
            tokens.push(Token {
                value: TokenValue::Json(value),
                start,
                end,
            });
            cursor = end;
            continue;
        }
        // Split collection/record delimiters even when pasted without
        // whitespace: `[1,2]` and `{name: \"Ada\"}` are valid source.
        if matches!(bytes[start], b'[' | b']' | b'{' | b'}') {
            cursor += 1;
            tokens.push(Token {
                value: TokenValue::Word(&source[start..cursor]),
                start,
                end: cursor,
            });
            continue;
        }
        // Raw prose literal.  It deliberately comes before `s"` so the
        // triple-quote opener is not mistaken for an empty escaped string.
        // The contents are verbatim (including newlines and ordinary quotes)
        // until the next `"""`; use it for model/user prose that would make
        // ordinary escaping needlessly fragile.
        if source[start..].starts_with("s\"\"\"") || source[start..].starts_with("\"\"\"") {
            cursor += if source[start..].starts_with("s\"\"\"") {
                4
            } else {
                3
            };
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
                value: TokenValue::Word("say"),
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
        // Finch also accepts a bare quoted string as the concise typed-string
        // spelling. `s"..."` remains the familiar Forth spelling (the `s`
        // means "string", not "say"), while `"..."` is unambiguously a
        // constant and avoids making models learn an unnecessary prefix.
        if source[start..].starts_with('"') {
            cursor += 1;
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
        while cursor < bytes.len()
            && !bytes[cursor].is_ascii_whitespace()
            && !matches!(bytes[cursor], b',' | b'[' | b']' | b'}')
        {
            cursor += 1;
        }
        // Keep the conventional Forth line-break word available without
        // changing `say`'s exact-chunk contract. It lowers to the same typed
        // session-emission path as `s\"\\n\" say`, so it is capability
        // checked, journaled, and streamable rather than a terminal escape.
        if &source[start..cursor] == "cr" {
            tokens.push(Token {
                value: TokenValue::String("\n".to_string()),
                start,
                end: cursor,
            });
            tokens.push(Token {
                value: TokenValue::Word("say"),
                start,
                end: cursor,
            });
            continue;
        }
        tokens.push(Token {
            value: TokenValue::Word(&source[start..cursor]),
            start,
            end: cursor,
        });
    }
    Ok(tokens)
}

fn looks_like_json_object(source: &str, start: usize) -> bool {
    let remainder = &source[start + 1..];
    matches!(remainder.trim_start().as_bytes().first(), Some(b'"' | b'}'))
}

fn compact_braced_type_end(source: &str, start: usize) -> Option<usize> {
    let remainder = source.get(start..)?;
    if !remainder.starts_with("record{") && !remainder.starts_with("variant{") {
        return None;
    }
    let mut depth = 0usize;
    for (offset, byte) in remainder.bytes().enumerate() {
        if byte.is_ascii_whitespace() || byte == b'"' {
            return None;
        }
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(start + offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn parameterized_type_end(source: &str, start: usize) -> Option<usize> {
    let remainder = source.get(start..)?;
    let known_prefix = [
        "empty-list<",
        "empty-map<",
        "list<",
        "map<",
        "option<",
        "result<",
        "fn<",
        "task<",
        "fiber<",
        "stream<",
        "resource<",
        "capability<",
        "variant<",
        "variant-get<",
    ]
    .into_iter()
    .any(|prefix| remainder.starts_with(prefix));
    if !known_prefix {
        return None;
    }
    let mut depth = 0usize;
    for (offset, byte) in remainder.bytes().enumerate() {
        if byte.is_ascii_whitespace() || byte == b'"' {
            return None;
        }
        match byte {
            b'<' => depth += 1,
            b'>' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(start + offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn read_json_object(
    source_id: &str,
    source: &str,
    start: usize,
) -> Result<(serde_json::Value, usize), Vec<VmDiagnostic>> {
    let mut cursor = start;
    let mut depth = 0_u32;
    let mut in_string = false;
    let mut escaped = false;
    while cursor < source.len() {
        let character = source[cursor..]
            .chars()
            .next()
            .expect("cursor remains on a UTF-8 boundary");
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
        } else {
            match character {
                '"' => in_string = true,
                '{' => depth += 1,
                '}' => {
                    depth = depth
                        .checked_sub(1)
                        .expect("JSON object starts at an opening brace");
                    if depth == 0 {
                        let end = cursor + character.len_utf8();
                        let value = serde_json::from_str(&source[start..end]).map_err(|error| {
                            vec![VmDiagnostic::error(
                                "E-READ-006",
                                DiagnosticPhase::Reader,
                                format!("invalid pasted JSON object: {error}"),
                                Some(origin(source_id, source, start, end)),
                            )]
                        })?;
                        return Ok((value, end));
                    }
                }
                _ => {}
            }
        }
        cursor += character.len_utf8();
    }
    Err(vec![VmDiagnostic::error(
        "E-READ-006",
        DiagnosticPhase::Reader,
        "unterminated pasted JSON object",
        Some(origin(source_id, source, start, source.len())),
    )])
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
    use finch_vm_core::{core_vocabulary, TypedValue};
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
    fn parser_owns_nested_quotation_structure_before_lowering() {
        let source = "[ -- fn<unit,int> ! pure | [ -- int ! pure | 42 ] ]";
        let ast = parse_forth_module("nested-quotes.forth", source)
            .expect("quotation syntax should parse before semantic lowering");
        assert_eq!(ast.body.nodes.len(), 1);
        let ForthBodyNode::Quotation(outer) = &ast.body.nodes[0] else {
            panic!("outer quotation should be a recursive body node");
        };
        assert_eq!(outer.body.nodes.len(), 1);
        let ForthBodyNode::Quotation(inner) = &outer.body.nodes[0] else {
            panic!("inner quotation should be a recursive body node");
        };
        let ForthBodyNode::Literal(value) = &inner.body.nodes[0] else {
            panic!("quotation body should own its literal node");
        };
        assert_eq!(&source[value.token.start..value.token.end], "42");
        assert_eq!(value.value, TypedValue::Int(42));
    }

    #[test]
    fn parser_represents_a_quotation_as_one_body_node() {
        let source = "[ -- int ! pure | 42 ] execute";
        let ast = parse_forth_module("quote-node.forth", source)
            .expect("quotation syntax should parse before semantic lowering");
        assert_eq!(ast.body.nodes.len(), 2);
        assert!(matches!(ast.body.nodes[0], ForthBodyNode::Quotation(_)));
        let ForthBodyNode::Primitive(execute) = &ast.body.nodes[1] else {
            panic!("the word after a quotation should remain the adjacent body node");
        };
        assert_eq!(&source[execute.token.start..execute.token.end], "execute");
        assert!(token_is(&execute.token, "execute"));
    }

    #[test]
    fn parser_classifies_literals_before_semantic_lowering() {
        let source = "42 true 'answer \"hello\" {\"ok\":true} dup";
        let ast = parse_forth_module("literal-nodes.forth", source)
            .expect("literal syntax should parse before semantic lowering");
        let values = ast
            .body
            .nodes
            .iter()
            .take(5)
            .map(|node| match node {
                ForthBodyNode::Literal(literal) => literal.value.clone(),
                _ => panic!("the parser should classify every literal node"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            vec![
                TypedValue::Int(42),
                TypedValue::Bool(true),
                TypedValue::Symbol("answer".into()),
                TypedValue::String("hello".into()),
                TypedValue::Json(serde_json::json!({"ok": true})),
            ]
        );
        let ForthBodyNode::Primitive(dup) = &ast.body.nodes[5] else {
            panic!("non-literal words should remain explicit unresolved word nodes");
        };
        assert!(token_is(&dup.token, "dup"));
    }

    #[test]
    fn anonymous_quotation_effect_contract_is_inferred_and_checked() {
        let rejected = compile_forth(
            "pure-output-quote.forth",
            "[ string -- unit ! pure | say unit ]",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert_eq!(rejected[0].code, "E-CAP-001");

        let source = "[ string -- unit ! infer | say unit ]";
        let inferred = compile_forth(
            "inferred-output-quote.forth",
            source,
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("! infer should retain the quotation's verified effect row");
        let quote = inferred
            .module
            .functions
            .values()
            .find(|function| function.name.starts_with("quote$"))
            .unwrap();
        assert!(!quote.signature.effects.is_pure());
        let say = quote
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .find(|located| matches!(&located.instruction, Instruction::CapabilityRequest { .. }))
            .expect("say lowers to a capability request");
        let span = say
            .origin
            .span
            .as_ref()
            .expect("quotation body source span");
        assert_eq!(&source[span.start_byte..span.end_byte], "say");
    }

    #[test]
    fn retains_finch_doc_comment_on_typed_definition() {
        let module = compile_forth(
            "documented.forth",
            "\\ finch-doc: Double an integer.\n: double ( S int -- S int ! pure ) 2 * ; 21 double",
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
    fn typed_input_names_must_cover_every_declared_input() {
        let errors = compile_forth(
            "named-signature.forth",
            ": area ( S width:int int -- S int ! pure ) width ;",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert!(errors.iter().any(|error| error.code == "E-FORTH-SIG-001"));
    }

    #[test]
    fn typed_definitions_must_spell_the_preserved_stack_row_on_both_sides() {
        for source in [
            ": bad ( int -- S int ! pure ) ;",
            ": bad ( S int -- int ! pure ) ;",
            ": bad ( S S int -- S int ! pure ) ;",
            ": bad ( S int -- S S int ! pure ) ;",
        ] {
            let errors = compile_forth("missing-row.forth", source, Vec::new(), &core_vocabulary())
                .expect_err("typed definitions must state their preserved stack row");
            assert_eq!(errors[0].code, "E-FORTH-SIG-001");
        }
    }

    #[test]
    fn accepts_parameterized_stack_signature_types() {
        let module = compile_forth(
            "generic.forth",
            ": list-id ( S list<int> -- S list<int> ! pure ) ;",
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
    fn accepts_fixed_record_types_in_stack_signatures() {
        let record = Type::Record(vec![
            ("name".into(), Type::String),
            ("age".into(), Type::Int),
        ]);
        let module = compile_forth(
            "record-signature.forth",
            ": identity-person ( S record{name:string,age:int} -- S record{name:string,age:int} ! pure ) ;",
            vec![record.clone()],
            &core_vocabulary(),
        )
        .expect("record type signature should compile");
        assert_eq!(
            module.module.functions["identity-person"]
                .signature
                .input
                .values,
            vec![record]
        );
    }

    #[test]
    fn accepts_closed_variant_types_in_stack_signatures() {
        let variant = Type::Variant(vec![
            ("none".into(), None),
            ("some".into(), Some(Type::Int)),
        ]);
        let module = compile_forth(
            "variant-signature.forth",
            ": identity-result ( S variant{none|some(int)} -- S variant{none|some(int)} ! pure ) ;",
            vec![variant.clone()],
            &core_vocabulary(),
        )
        .expect("variant type signature should compile");
        assert_eq!(
            module.module.functions["identity-result"]
                .signature
                .input
                .values,
            vec![variant]
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
    fn definition_diagnostics_retain_original_module_spans() {
        let source = "\\ module prelude\n\n: broken ( S -- S int ! pure )\n  \"oops\" 2 +\n;\n";
        let errors =
            compile_forth("module-span.forth", source, Vec::new(), &core_vocabulary()).unwrap_err();
        assert_eq!(errors[0].code, "E-TYPE-002");
        let span = errors[0]
            .primary
            .as_ref()
            .and_then(|origin| origin.span.as_ref())
            .expect("definition error retains its original module span");
        assert_eq!(span.start_line, 4);
        assert_eq!(&source[span.start_byte..span.end_byte], "+");
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
    fn rejects_mixed_or_unterminated_typed_list_literals() {
        let mixed = compile_forth(
            "input.forth",
            "[ 1 s\" two\" ]",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_err();
        assert!(mixed.iter().any(|error| error.code == "E-LIST-003"));

        let unclosed =
            compile_forth("input.forth", "[ 1", Vec::new(), &core_vocabulary()).unwrap_err();
        assert!(unclosed.iter().any(|error| error.code == "E-LIST-004"));
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
    fn published_word_retains_its_typed_suspension_contract() {
        let module = compile_forth(
            "producer.forth",
            ": producer ( S -- S int ! infer ) 1 yield 2 ; ['] producer",
            Vec::new(),
            &core_vocabulary(),
        )
        .expect("yielding Co-Forth definition compiles");
        let signature = &module.module.functions["producer"].signature;
        assert_eq!(
            signature.suspension,
            Some(SuspensionSignature::one_way(Type::Int))
        );
        assert_eq!(signature.control, ControlEffect::MaySuspend);
        assert!(matches!(
            module.module.functions["main"].signature.output.values.as_slice(),
            [Type::Function {
                suspension: Some(SuspensionSignature { yield_type, resume_type }),
                ..
            }] if **yield_type == Type::Int && **resume_type == Type::Unit
        ));
    }
    #[test]
    fn test_parser_owns_control_and_delimiter_pairing() {
        let source = "true if [ 1 2 ] else [ 3 ] then";
        let ast = parse_forth_module("groups.forth", source).unwrap();
        let ForthBodyNode::Group(branch) = &ast.body.nodes[1] else {
            panic!("if must own its complete nested control tree: {ast:?}");
        };
        assert_eq!(
            branch.nodes.len(),
            5,
            "if owns both list bodies and its branch markers: {branch:?}"
        );
        assert!(
            matches!(&branch.nodes[1], ForthBodyNode::Group(list) if list.nodes.len() == 4),
            "then list owns both delimiters and values: {branch:?}"
        );
        assert!(
            matches!(&branch.nodes[3], ForthBodyNode::Group(list) if list.nodes.len() == 3),
            "else list owns both delimiters and its value: {branch:?}"
        );
    }

    #[test]
    fn test_ast_repair_distinguishes_binding_call_and_control_nodes() {
        let ast = parse_forth_module("node-kinds.forth", ": identity ( S value:int -- S int ! pure ) value 1 + ; : capture ( S value:int -- S fn<unit,int> ! pure ) [ -- int | value ] ; true if 1 else 2 then").unwrap();
        let local = &ast.definitions[0].body.nodes[0];
        let call = &ast.definitions[0].body.nodes[2];
        let ForthBodyNode::Quotation(quote) = &ast.definitions[1].body.nodes[0] else {
            panic!("capture definition must retain its quotation: {ast:?}");
        };
        let capture = &quote.body.nodes[0];
        let ForthBodyNode::Group(branch) = &ast.body.nodes[1] else {
            panic!("conditional must retain its structured body: {ast:?}");
        };
        let nodes = [local, capture, call, &branch.nodes[0]];
        for (index, node) in nodes.iter().enumerate() {
            for other in &nodes[index + 1..] {
                assert_ne!(std::mem::discriminant(*node), std::mem::discriminant(*other), "parser must distinguish local, capture, call, and control nodes before lowering: {node:?} versus {other:?}");
            }
        }
    }

    #[test]
    fn test_parser_resolves_shadowed_calls_nested_captures_and_quotation_operands() {
        let source = ": identity ( S yield:int -- S int ! pure ) yield ; : capture ( S left:int right:int -- S fn<unit,fn<unit,int>> ! pure ) [ unit -- fn<unit,int> | drop [ unit -- int | drop left right + ] ] ;";
        let verified = compile_forth("bindings.forth", source, Vec::new(), &core_vocabulary())
            .unwrap_or_else(|diagnostics| {
                panic!(
                    "resolved lexical references must preserve verified closures: {diagnostics:?}"
                )
            });
        let identity = &verified.module.functions["identity"];
        assert!(
            identity
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
                .any(|located| matches!(located.instruction, Instruction::LocalGet { index: 0 })),
            "a named input shadows a vocabulary call such as yield: {identity:?}"
        );
        assert!(
            identity.signature.suspension.is_none(),
            "a lexical reference named yield must not acquire a suspension contract: {identity:?}"
        );
        let captures = verified
            .module
            .functions
            .values()
            .filter(|function| function.name.starts_with("quote$"))
            .map(|function| {
                function
                    .blocks
                    .values()
                    .flat_map(|block| &block.instructions)
                    .filter_map(|located| match located.instruction {
                        Instruction::CaptureGet { index } => Some(index),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(captures, vec![vec![0, 1], vec![0, 1]], "nested quotations must retain the parser's deterministic inherited capture slots: {verified:?}");
        let operand_source = "begin: [ -- int | 1 ] true while break [ -- int | 1 ] repeat";
        let operand = compile_forth(
            "operands.forth",
            operand_source,
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap_or_else(|diagnostics| {
            panic!(
                "a quotation used as a loop-label operand is consumed as one atom: {diagnostics:?}"
            )
        });
        assert_eq!(
            operand.module.functions.len(),
            1,
            "consumed reference operands must not create executable closures: {operand:?}"
        );
        assert!(
            operand.module.functions["main"]
                .signature
                .output
                .values
                .is_empty(),
            "reference operand bodies must not affect the operand stack: {operand:?}"
        );
    }

    #[test]
    fn test_ast_repair_nested_quotations_consume_tokens_linearly() {
        for prefix in ["", "['] "] {
            let source = format!("{prefix}{}1{}", "[ -- int | ".repeat(32), " ]".repeat(32));
            let tokens = tokenize("nested.forth", &source).unwrap().len();
            PARSER_TOKEN_VISITS.with(|visits| visits.set(0));
            let ast = parse_forth_module("nested.forth", &source).unwrap();
            let visits = PARSER_TOKEN_VISITS.with(|visits| visits.get());
            assert!(visits <= 2 * tokens, "nested quotation parsing may index each token once and consume it once, including quotation reference operands: {tokens} tokens, {visits} visits, {} root nodes", ast.body.nodes.len());
        }
    }

    #[test]
    fn test_parser_owns_reference_operands_and_borrows_source_spellings() {
        let source = String::from("\\ finch-doc: Identity.\n: identity ( S value:int -- S int ! pure ) value ; ['] identity begin: outer continue outer");
        let ast = parse_forth_module("references.forth", &source).unwrap();
        let definition = &ast.definitions[0];
        for spelling in [
            definition.name,
            definition.documentation.unwrap(),
            definition.locals[0].name,
        ] {
            assert!(
                spelling.as_ptr() >= source.as_ptr()
                    && spelling.as_ptr() < source.as_ptr().wrapping_add(source.len()),
                "AST spelling must borrow its immutable source buffer: {spelling:?}"
            );
        }
        let ForthBodyNode::QuotationReference(quote) = &ast.body.nodes[0] else {
            panic!("quotation target must be owned by its reference node: {ast:?}");
        };
        assert!(
            quote
                .operand
                .as_ref()
                .is_some_and(|token| token_is(&token.token, "identity")),
            "quotation target operand is retained: {quote:?}"
        );
        let ForthBodyNode::Group(body) = &ast.body.nodes[1] else {
            panic!("named loop must own its control body: {ast:?}");
        };
        assert_eq!(
            body.nodes.len(),
            2,
            "loop labels are operands, never executable sibling words: {body:?}"
        );
        for node in &body.nodes {
            assert!(
                matches!(node, ForthBodyNode::Control(word) if word.operand.as_ref().is_some_and(|token| token_is(&token.token, "outer"))),
                "begin and continue each retain their label: {node:?}"
            );
        }
    }

    #[test]
    fn test_structured_nodes_preserve_verified_control_and_reference_results() {
        for (source, output) in [
            ("true if [ 1 2 ] else [ 3 ] then", Type::list(Type::Int)),
            ("1 case 1 of 7 endof otherwise 8 endcase", Type::Int),
            (
                "0 begin: outer dup 2 < while true if 1 + else 1 + then repeat",
                Type::Int,
            ),
            (
                "0 begin: outer dup 2 < while 1 + continue outer repeat",
                Type::Int,
            ),
            ("0 begin: outer true while break outer repeat", Type::Int),
            (
                "{ name: \"Ada\" } \"Grace\" \"name\" record-set \"name\" record-get",
                Type::Option(Box::new(Type::String)),
            ),
            (
                "{ name: \"Ada\" } record-get:name",
                Type::Option(Box::new(Type::String)),
            ),
            (
                ": answer ( S -- S int ! pure ) 42 ; ['] answer execute",
                Type::Int,
            ),
        ] {
            let verified =
                compile_forth("structured.forth", source, Vec::new(), &core_vocabulary())
                    .unwrap_or_else(|diagnostics| {
                        panic!("structured source must still verify: {source:?}: {diagnostics:?}")
                    });
            assert_eq!(
                verified.module.functions["main"].signature.output.values,
                vec![output],
                "structured lowering must preserve its typed output: {source:?}: {verified:?}"
            );
        }
    }

    #[test]
    fn test_synthetic_output_retains_the_original_literal_provenance() {
        let source = ".\" hello\"";
        let verified = compile_forth("output.forth", source, Vec::new(), &core_vocabulary())
            .expect("output literal should lower through its synthetic say word");
        let effect = verified.module.functions["main"]
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .find(|located| matches!(located.instruction, Instruction::CapabilityRequest { .. }))
            .expect("synthetic say must retain its session output effect");
        let span = effect
            .origin
            .span
            .as_ref()
            .expect("synthetic effect has a source span");
        assert_eq!(
            (span.start_byte, span.end_byte),
            (0, source.len()),
            "synthetic say must point to the original literal: {effect:?}"
        );
        assert_eq!(
            effect.origin.word.as_deref(),
            Some(source),
            "synthetic say must preserve original spelling in diagnostic provenance: {effect:?}"
        );
    }

    #[test]
    fn test_structured_diagnostics_keep_exact_original_spans_and_precedence() {
        for (source, code, spelling) in [
            ("true if begin then", "E-FORTH-CONTROL-001", "then"),
            ("0 begin true if repeat", "E-FORTH-CONTROL-001", "repeat"),
            (
                "0 begin: outer true while break absent repeat",
                "E-FORTH-LOOP-007",
                "break",
            ),
            ("begin:", "E-FORTH-LOOP-006", "begin:"),
            ("[']", "E-FORTH-QUOTE-001", "[']"),
            ("['] \"name\"", "E-FORTH-QUOTE-001", "[']"),
            ("['] absent", "E-FORTH-QUOTE-002", "[']"),
            ("\"λ\" drop\nrecord-get", "E-RECORD-004", "record-get"),
            (
                "{ name: \"Ada\" } \"absent\" record-get",
                "E-RECORD-005",
                "record-get",
            ),
            ("unknown true if", "E-LINK-002", "unknown"),
            ("unknown empty-list<invalid>", "E-LINK-002", "unknown"),
            ("unknown [ -- invalid | 1 ]", "E-LINK-002", "unknown"),
        ] {
            let diagnostics = compile_forth("spans.forth", source, Vec::new(), &core_vocabulary())
                .expect_err("malformed structured source must report a diagnostic");
            let diagnostic = &diagnostics[0];
            assert_eq!(
                diagnostic.code, code,
                "first-error precedence must be preserved for {source:?}: {diagnostics:?}"
            );
            let span = diagnostic
                .primary
                .as_ref()
                .and_then(|origin| origin.span.as_ref())
                .expect("diagnostic must retain its original source span");
            let start = source.find(spelling).unwrap();
            assert_eq!(
                (span.start_byte, span.end_byte),
                (start, start + spelling.len()),
                "diagnostic must identify the exact original token for {source:?}: {diagnostics:?}"
            );
        }
    }
}
