use super::effects::{
    CapabilityKind, CapabilityRequirement, EffectSet, FileSelector, FileSelectorTemplate,
    FileSelectorTemplatePart, NetworkSelectorTemplate, ProcessSelectorTemplate,
    ProgramSelectorTemplate, ResourceRoot, ResourceSelector,
};
use super::signature::{ControlEffect, StackRow, StackSignature};
use super::types::Type;
use super::verifier::Vocabulary;
use std::collections::BTreeMap;

/// Provider-facing protocol documentation for an executable core word.
///
/// This belongs to the VM contract rather than a particular provider adapter:
/// the adapter may serialize it for a model, while another embedder can expose
/// the same contract through its own discovery UI.
#[derive(Debug, Clone, Copy)]
pub struct CoreWordDocumentation {
    pub summary: &'static str,
    pub lisp: &'static str,
    pub forth: &'static str,
    pub example: &'static str,
}

/// Return provider-neutral documentation for a registered core word.
///
/// Keep this total: the stack signature and capability requirement remain the
/// normative contract even while prose for a newly added word is minimal.
pub fn core_word_documentation(name: &str) -> CoreWordDocumentation {
    match name {
        "say" => CoreWordDocumentation { summary: "Append one exact text chunk to the current response stream. It adds no space or newline and leaves no value on the stack.", lisp: "(say text)", forth: "text say", example: "(say (str-cat \"answer: \" (int-to-string (+ 2 3))))" },
        "emit" => CoreWordDocumentation { summary: "Alias of say for a terminal response chunk. Prefer say in provider responses.", lisp: "(emit text)", forth: "text emit", example: "s\"progress\\n\" emit" },
        "output-open" => CoreWordDocumentation { summary: "Create an independent, host-issued output handle for a progress or replaceable status item.", lisp: "(output-open title)", forth: "title output-open", example: "(let ((h (output-open \"Download\"))) (output-status h \"starting\"))" },
        "output-append" => CoreWordDocumentation { summary: "Append exact text to an explicit output handle; unlike say it does not select a global active work item.", lisp: "(output-append handle text)", forth: "handle text output-append", example: "h s\"chunk complete\\n\" output-append" },
        "output-replace" => CoreWordDocumentation { summary: "Replace an explicit output handle's displayed body with text.", lisp: "(output-replace handle text)", forth: "handle text output-replace", example: "h s\"42% complete\" output-replace" },
        "output-status" => CoreWordDocumentation { summary: "Set transient status text on an explicit output handle.", lisp: "(output-status handle text)", forth: "handle text output-status", example: "h s\"running\" output-status" },
        "output-progress" => CoreWordDocumentation { summary: "Set bounded progress on an explicit output handle as completed and total integer units.", lisp: "(output-progress handle completed total)", forth: "handle completed total output-progress", example: "h 42 100 output-progress" },
        "output-complete" => CoreWordDocumentation { summary: "Mark an explicit output handle complete.", lisp: "(output-complete handle)", forth: "handle output-complete", example: "h output-complete" },
        "output-fail" => CoreWordDocumentation { summary: "Mark an explicit output handle failed with a human-readable reason.", lisp: "(output-fail handle reason)", forth: "handle reason output-fail", example: "h s\"network unavailable\" output-fail" },
        "path" => CoreWordDocumentation { summary: "Resolve text as a normalized workspace-relative path value. It cannot escape the workspace root.", lisp: "(path relative-text)", forth: "relative-text path", example: "(file-read (path \"src/main.rs\"))" },
        "host-path" => CoreWordDocumentation { summary: "Resolve text under the explicitly installed host-machine root. This identifies a host path but grants no authority by itself.", lisp: "(host-path text)", forth: "text host-path", example: "s\"/tmp/report.txt\" host-path" },
        "file-read" => CoreWordDocumentation { summary: "Read all bytes from an authorized workspace path. Prefer file-slice or cursor resources for large inputs.", lisp: "(file-read path)", forth: "path file-read", example: "(file-read (path \"Cargo.toml\"))" },
        "file-slice" => CoreWordDocumentation { summary: "Read a bounded byte range from an authorized workspace path: offset and maximum byte count.", lisp: "(file-slice path offset length)", forth: "path offset length file-slice", example: "(file-slice (path \"data.csv\") 0 4096)" },
        "file-size" => CoreWordDocumentation { summary: "Return the byte length of an authorized workspace file without reading its contents.", lisp: "(file-size path)", forth: "path file-size", example: "(file-size (path \"data.csv\"))" },
        "file-lines-open" => CoreWordDocumentation { summary: "Open an authorized text-file line cursor. The opaque cursor owns no forgeable path authority.", lisp: "(file-lines-open path)", forth: "path file-lines-open", example: "(file-lines-open (path \"large.log\"))" },
        "file-lines-next" => CoreWordDocumentation { summary: "Return some(line) from a line cursor or none at EOF. Close the cursor when finished.", lisp: "(file-lines-next cursor)", forth: "cursor file-lines-next", example: "(match-option (file-lines-next c) (some line (say line)) (none (file-lines-close c)))" },
        "file-lines-close" => CoreWordDocumentation { summary: "Close a line cursor and release its host resource.", lisp: "(file-lines-close cursor)", forth: "cursor file-lines-close", example: "c file-lines-close" },
        "csv-open" | "csv-next" | "csv-close" => CoreWordDocumentation { summary: "Open, advance, or close an authorized CSV record cursor. csv-next returns some(list<string>) or none at EOF; use it instead of loading a large CSV at once.", lisp: "(csv-open path), (csv-next cursor), (csv-close cursor)", forth: "path csv-open; cursor csv-next; cursor csv-close", example: "(let ((c (csv-open (path \"data.csv\")))) (csv-next c))" },
        "file-write" | "host-file-write" => CoreWordDocumentation { summary: "Write bytes to an authorized refined path. This is an external mutation and requires an explicit write capability grant.", lisp: "(file-write path bytes)", forth: "path bytes file-write", example: "(file-write (path \"generated.txt\") (bytes \"hello\\n\"))" },
        "host-file-read" => CoreWordDocumentation { summary: "Read all bytes from an authorized host-machine path. It requires both an installed host root and a matching read grant.", lisp: "(host-file-read path)", forth: "path host-file-read", example: "(host-file-read (host-path \"/tmp/report.txt\"))" },
        "process-run" => CoreWordDocumentation { summary: "Run an approved executable directly with a list of string arguments; it never invokes a shell. Use proposal-open for editable scripts.", lisp: "(process-run command arguments)", forth: "command arguments process-run", example: "(process-run \"git\" (list \"status\" \"--short\"))" },
        "proposal-open" => CoreWordDocumentation { summary: "Ask the host to open a human-editable artifact proposal. Approval may execute the edited artifact through its normal validator, return edited text for chat, or cancel; this word does not run a shell itself.", lisp: "(proposal-open language title source)", forth: "language title source proposal-open", example: "(proposal-open \"python\" \"Report\" \"print('hello')\\n\")" },
        "mem-recall" | "mem-store" => CoreWordDocumentation { summary: "Read matching session memory entries or store one text memory entry. Both use the host memory tree and require their respective memory capability.", lisp: "(mem-recall query), (mem-store text)", forth: "query mem-recall; text mem-store", example: "(mem-store \"tested release candidate\")" },
        "agent-spawn" | "agent-await" | "agent-poll" | "agent-cancel" => CoreWordDocumentation { summary: "Create or control a separate typed child-agent task. Agent tasks have their own stack, budget, ancestry, and attenuated grants; they are not fibers or shared-stack threads.", lisp: "(agent-spawn task), (agent-poll handle), (agent-await handle), (agent-cancel handle)", forth: "task agent-spawn; handle agent-poll; handle agent-await; handle agent-cancel", example: "(agent-poll (agent-spawn \"summarize recent test failures\"))" },
        "yield" => CoreWordDocumentation { summary: "Cooperatively return the remaining VM frames to Finch's event-loop trampoline. It is stack-neutral and may occur repeatedly; it is not a first-class continuation or generator value.", lisp: "(yield)", forth: "yield", example: "(begin (say \"working...\") (yield) (say \"done\"))" },
        "some" | "none" | "is-some" | "unwrap" => CoreWordDocumentation { summary: "Construct, test, or project typed option values. Prefer exhaustive match-option/if-some over unwrap when none is expected control flow.", lisp: "(some value), (none), (is-some option), (unwrap option)", forth: "value some; none; option is-some; option unwrap", example: "(match-option (some 42) (some n (say (int-to-string n))) (none (say \"missing\")))" },
        "ok" | "err" | "is-ok" | "result-unwrap" | "result-error" => CoreWordDocumentation { summary: "Construct, test, or project typed result values. Prefer exhaustive match-result/if-ok over projecting an unknown branch.", lisp: "(ok value), (err error), (is-ok result), (result-unwrap result)", forth: "value ok; error err; result is-ok; result result-unwrap", example: "(match-result (ok 42) (ok n (say (int-to-string n))) (err e (say e)))" },
        "network-connect" | "network-send" => CoreWordDocumentation { summary: "Open an approved network connection or send bytes over an existing opaque socket. The socket is not forgeable and calls remain capability-checked.", lisp: "(network-connect host port), (network-send socket bytes)", forth: "host port network-connect; socket bytes network-send", example: "(network-connect \"example.com\" 443)" },
        "schedule-create" => CoreWordDocumentation { summary: "Create a capability-bound scheduled event using a callback descriptor and time. Scheduled work never gains new authority when it fires.", lisp: "(schedule-create callback when)", forth: "callback when schedule-create", example: "(schedule-create \"daily-summary\" 1770000000)" },
        "vm-vocabulary" => CoreWordDocumentation { summary: "Return the serialized current typed vocabulary. Use the external search_vm_vocabulary/describe_vm_word tools for compact targeted discovery.", lisp: "(vm-vocabulary)", forth: "vm-vocabulary", example: "(say (vm-vocabulary))" },
        "automation-availability" | "automation-displays" | "automation-windows" | "automation-click" | "automation-type" => CoreWordDocumentation { summary: "Inspect or operate desktop automation through the host adapter. Availability and every concrete target remain capability-checked at the execution boundary.", lisp: "(automation-availability), (automation-click x y button count), (automation-type text delay)", forth: "automation-availability; x y button count automation-click; text delay automation-type", example: "(automation-availability)" },
        "list-length" | "list-get" => CoreWordDocumentation { summary: "Return a typed list's length or one element at a zero-based integer index.", lisp: "(list-length items), (list-get items index)", forth: "items list-length; items index list-get", example: "(list-get (list 4 8 15 16) 2)" },
        "str-cat" | "bytes" | "int-to-string" | "atoi" | "space" => CoreWordDocumentation { summary: "Pure text/byte conversion helpers. str-cat preserves both inputs exactly; say adds no formatting of its own.", lisp: "(str-cat left right), (bytes text), (int-to-string n), (atoi text), (space)", forth: "left right str-cat; text bytes; n int-to-string; text atoi; space", example: "s\"answer: \" 42 int-to-string str-cat say" },
        "json-parse" | "json-stringify" | "json-get" | "json-index" | "json-keys" | "json-as-string" | "json-as-int" | "json-as-float" | "json-as-bool" => CoreWordDocumentation { summary: "Pure managed JSON operations. json-parse returns result<json,string>; field/index lookup and scalar projections return options rather than coercing or treating text as authority. json-keys returns an empty typed list for a non-object.", lisp: "(json-parse text), (json-get value field), (json-index value index), (json-keys value), (json-as-string value), (json-as-int value), (json-as-float value), (json-as-bool value)", forth: "text json-parse; json field json-get; json index json-index; json json-keys; json json-as-string|json-as-int|json-as-float|json-as-bool", example: "s\" {\\\"answer\\\":42}\" json-parse result-unwrap s\" answer\" json-get unwrap json-as-int unwrap" },
        "dup" | "drop" | "swap" => CoreWordDocumentation { summary: "Pure stack shuffles. Prefer Lisp let bindings or Co-Forth locals for complex programs rather than deep positional juggling.", lisp: "Usually use let instead of stack shuffles.", forth: "value dup; value drop; left right swap", example: "3 dup * int-to-string say" },
        "+" | "-" | "*" | "/" | "mod" | "negate" | "abs" | "=" | "<" | ">" | "<=" | ">=" | "not" => CoreWordDocumentation { summary: "Pure typed arithmetic, comparison, or boolean operation. Operators consume their inputs and push one result.", lisp: "(+ a b), (- a b), (* a b), (<= a b), (not flag)", forth: "a b +; a b -; a b *; a b <=; flag not", example: "(say (int-to-string (+ (* 6 7) 1)))" },
        _ => CoreWordDocumentation { summary: "Typed Finch core word. Its exact stack signature and capability requirements are the normative contract; retrieve the language definition for shared control-flow rules.", lisp: "Use this word in normal prefix Lisp call position.", forth: "Use this word in normal postfix Co-Forth position.", example: "Use search_vm_vocabulary with this exact name to inspect its signature." },
    }
}

fn pure(input: Vec<Type>, output: Vec<Type>) -> StackSignature {
    StackSignature::pure(
        StackRow::polymorphic("S", input),
        StackRow::polymorphic("S", output),
    )
}

fn capability(
    input: Vec<Type>,
    output: Vec<Type>,
    requirement: CapabilityRequirement,
) -> StackSignature {
    StackSignature {
        type_parameters: Vec::new(),
        input: StackRow::polymorphic("S", input),
        output: StackRow::polymorphic("S", output),
        effects: EffectSet::from_requirement(requirement),
        control: ControlEffect::MaySuspend,
    }
}

fn unscoped(kind: CapabilityKind) -> CapabilityRequirement {
    CapabilityRequirement {
        capability: kind,
        selector: ResourceSelector::None,
    }
}

fn path_template() -> FileSelectorTemplate {
    let upper_bound = FileSelector::parse("./**").expect("valid workspace root");
    FileSelectorTemplate {
        root: ResourceRoot::Workspace,
        parts: vec![FileSelectorTemplatePart::Argument {
            index: 0,
            bound: upper_bound.clone(),
        }],
        upper_bound,
    }
}

fn host_path_selector() -> FileSelector {
    FileSelector::parse("${host-machine}/**").expect("valid host-machine root")
}

fn host_path_template() -> FileSelectorTemplate {
    let upper_bound = host_path_selector();
    FileSelectorTemplate {
        root: ResourceRoot::HostMachine,
        parts: vec![FileSelectorTemplatePart::Argument {
            index: 0,
            bound: upper_bound.clone(),
        }],
        upper_bound,
    }
}

/// Canonical signatures for the first verified core. Runtime implementations,
/// provider documentation, and tests consume this registry rather than keeping
/// independent handwritten signature tables.
pub fn core_vocabulary() -> Vocabulary {
    let a = Type::Variable("A".into());
    let int_binary = || pure(vec![Type::Int, Type::Int], vec![Type::Int]);
    let comparison = || pure(vec![Type::Int, Type::Int], vec![Type::Bool]);
    BTreeMap::from([
        ("+".into(), int_binary()),
        ("-".into(), int_binary()),
        ("*".into(), int_binary()),
        ("/".into(), int_binary()),
        ("mod".into(), int_binary()),
        ("=".into(), comparison()),
        ("<".into(), comparison()),
        (">".into(), comparison()),
        ("<=".into(), comparison()),
        (">=".into(), comparison()),
        ("negate".into(), pure(vec![Type::Int], vec![Type::Int])),
        ("abs".into(), pure(vec![Type::Int], vec![Type::Int])),
        ("not".into(), pure(vec![Type::Bool], vec![Type::Bool])),
        (
            "path".into(),
            pure(
                vec![Type::String],
                vec![Type::Path(
                    FileSelector::parse("./**").expect("valid workspace root"),
                )],
            ),
        ),
        // `host-path` is intentionally a different refined type from
        // workspace `path`. It only identifies a child of the host-installed
        // root; it neither installs that root nor grants filesystem access.
        (
            "host-path".into(),
            pure(vec![Type::String], vec![Type::Path(host_path_selector())]),
        ),
        (
            "dup".into(),
            pure(vec![a.clone()], vec![a.clone(), a.clone()]),
        ),
        ("drop".into(), pure(vec![a.clone()], Vec::new())),
        (
            "swap".into(),
            pure(
                vec![Type::Variable("A".into()), Type::Variable("B".into())],
                vec![Type::Variable("B".into()), Type::Variable("A".into())],
            ),
        ),
        (
            "str-cat".into(),
            pure(vec![Type::String, Type::String], vec![Type::String]),
        ),
        ("bytes".into(), pure(vec![Type::String], vec![Type::Bytes])),
        ("space".into(), pure(Vec::new(), vec![Type::String])),
        (
            "json-parse".into(),
            pure(
                vec![Type::String],
                vec![Type::result(Type::Json, Type::String)],
            ),
        ),
        (
            "json-stringify".into(),
            pure(vec![Type::Json], vec![Type::String]),
        ),
        (
            "json-get".into(),
            pure(
                vec![Type::Json, Type::String],
                vec![Type::Option(Box::new(Type::Json))],
            ),
        ),
        (
            "json-index".into(),
            pure(
                vec![Type::Json, Type::Int],
                vec![Type::Option(Box::new(Type::Json))],
            ),
        ),
        (
            "json-keys".into(),
            pure(
                vec![Type::Json],
                vec![Type::list(Type::String)],
            ),
        ),
        (
            "json-as-string".into(),
            pure(
                vec![Type::Json],
                vec![Type::Option(Box::new(Type::String))],
            ),
        ),
        (
            "json-as-int".into(),
            pure(
                vec![Type::Json],
                vec![Type::Option(Box::new(Type::Int))],
            ),
        ),
        (
            "json-as-float".into(),
            pure(
                vec![Type::Json],
                vec![Type::Option(Box::new(Type::Float))],
            ),
        ),
        (
            "json-as-bool".into(),
            pure(
                vec![Type::Json],
                vec![Type::Option(Box::new(Type::Bool))],
            ),
        ),
        // A control-only cooperative scheduling point. The source frontends
        // lower this word to `Instruction::Yield`, not a normal core call.
        ("yield".into(), pure(Vec::new(), Vec::new())),
        (
            "int-to-string".into(),
            pure(vec![Type::Int], vec![Type::String]),
        ),
        ("atoi".into(), pure(vec![Type::String], vec![Type::Int])),
        (
            "some".into(),
            pure(vec![a.clone()], vec![Type::Option(Box::new(a.clone()))]),
        ),
        (
            "none".into(),
            pure(Vec::new(), vec![Type::Option(Box::new(a.clone()))]),
        ),
        (
            "is-some".into(),
            pure(vec![Type::Option(Box::new(a.clone()))], vec![Type::Bool]),
        ),
        (
            "unwrap".into(),
            pure(vec![Type::Option(Box::new(a.clone()))], vec![a.clone()]),
        ),
        (
            "ok".into(),
            pure(
                vec![a.clone()],
                vec![Type::Result(Box::new(a.clone()), Box::new(Type::Dynamic))],
            ),
        ),
        (
            "err".into(),
            pure(
                vec![a.clone()],
                vec![Type::Result(Box::new(Type::Dynamic), Box::new(a.clone()))],
            ),
        ),
        (
            "is-ok".into(),
            pure(
                vec![Type::Result(
                    Box::new(a.clone()),
                    Box::new(Type::Variable("E".into())),
                )],
                vec![Type::Bool],
            ),
        ),
        (
            "result-unwrap".into(),
            pure(
                vec![Type::Result(
                    Box::new(a.clone()),
                    Box::new(Type::Variable("E".into())),
                )],
                vec![a.clone()],
            ),
        ),
        (
            "result-error".into(),
            pure(
                vec![Type::Result(
                    Box::new(Type::Variable("A".into())),
                    Box::new(Type::Variable("E".into())),
                )],
                vec![Type::Variable("E".into())],
            ),
        ),
        (
            "file-read".into(),
            capability(
                vec![Type::Path(
                    FileSelector::parse("./**").expect("valid workspace root"),
                )],
                vec![Type::Bytes],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        // Bounded range reads keep large CSV/text processing out of the
        // model context and avoid materializing an entire file in the VM.
        // The path still determines the concrete `file.read` selector.
        (
            "file-size".into(),
            capability(
                vec![Type::Path(
                    FileSelector::parse("./**").expect("valid workspace root"),
                )],
                vec![Type::Int],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        (
            "file-slice".into(),
            capability(
                vec![
                    Type::Path(FileSelector::parse("./**").expect("valid workspace root")),
                    Type::Int,
                    Type::Int,
                ],
                vec![Type::Bytes],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        // Line cursors are host-issued resources. Only `file-lines-open`
        // receives a path selector; `next`/`close` can operate under the
        // already-authorized opaque resource and cannot fabricate a path.
        (
            "file-lines-open".into(),
            capability(
                vec![Type::Path(
                    FileSelector::parse("./**").expect("valid workspace root"),
                )],
                vec![Type::Resource("file-line-cursor".into())],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        (
            "file-lines-next".into(),
            capability(
                vec![Type::Resource("file-line-cursor".into())],
                vec![Type::Option(Box::new(Type::String))],
                unscoped(CapabilityKind::FileRead),
            ),
        ),
        (
            "file-lines-close".into(),
            capability(
                vec![Type::Resource("file-line-cursor".into())],
                vec![Type::Unit],
                unscoped(CapabilityKind::FileRead),
            ),
        ),
        // CSV records need their own cursor: quoted fields may legally span
        // physical lines, so a line cursor cannot safely model a CSV row.
        (
            "csv-open".into(),
            capability(
                vec![Type::Path(
                    FileSelector::parse("./**").expect("valid workspace root"),
                )],
                vec![Type::Resource("csv-record-cursor".into())],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        (
            "csv-next".into(),
            capability(
                vec![Type::Resource("csv-record-cursor".into())],
                vec![Type::Option(Box::new(Type::list(Type::String)))],
                unscoped(CapabilityKind::FileRead),
            ),
        ),
        (
            "csv-close".into(),
            capability(
                vec![Type::Resource("csv-record-cursor".into())],
                vec![Type::Unit],
                unscoped(CapabilityKind::FileRead),
            ),
        ),
        (
            "file-write".into(),
            capability(
                vec![
                    Type::Path(FileSelector::parse("./**").expect("valid workspace root")),
                    Type::Bytes,
                ],
                vec![Type::Unit],
                CapabilityRequirement {
                    capability: CapabilityKind::FileWrite,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        // Whole-machine operations remain structurally distinct from their
        // workspace counterparts. A host adapter must explicitly install the
        // host root, and grants still have to cover the concrete selector.
        (
            "host-file-read".into(),
            capability(
                vec![Type::Path(host_path_selector())],
                vec![Type::Bytes],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: host_path_template(),
                    },
                },
            ),
        ),
        (
            "host-file-write".into(),
            capability(
                vec![Type::Path(host_path_selector()), Type::Bytes],
                vec![Type::Unit],
                CapabilityRequirement {
                    capability: CapabilityKind::FileWrite,
                    selector: ResourceSelector::FileTemplate {
                        template: host_path_template(),
                    },
                },
            ),
        ),
        (
            "list-length".into(),
            pure(vec![Type::list(a.clone())], vec![Type::Int]),
        ),
        (
            "list-get".into(),
            pure(vec![Type::list(a.clone()), Type::Int], vec![a.clone()]),
        ),
        (
            "say".into(),
            capability(
                vec![Type::String],
                Vec::new(),
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "emit".into(),
            capability(
                vec![Type::String],
                Vec::new(),
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "output-open".into(),
            capability(
                vec![Type::String],
                vec![Type::Resource("output-handle".into())],
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "output-append".into(),
            capability(
                vec![Type::Resource("output-handle".into()), Type::String],
                Vec::new(),
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "output-replace".into(),
            capability(
                vec![Type::Resource("output-handle".into()), Type::String],
                Vec::new(),
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "output-status".into(),
            capability(
                vec![Type::Resource("output-handle".into()), Type::String],
                Vec::new(),
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "output-progress".into(),
            capability(
                vec![Type::Resource("output-handle".into()), Type::Int, Type::Int],
                Vec::new(),
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "output-complete".into(),
            capability(
                vec![Type::Resource("output-handle".into())],
                Vec::new(),
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "output-fail".into(),
            capability(
                vec![Type::Resource("output-handle".into()), Type::String],
                Vec::new(),
                unscoped(CapabilityKind::SessionEmit),
            ),
        ),
        (
            "vm-vocabulary".into(),
            capability(
                Vec::new(),
                vec![Type::String],
                unscoped(CapabilityKind::VmRead),
            ),
        ),
        (
            "process-run".into(),
            capability(
                vec![Type::String, Type::list(Type::String)],
                vec![Type::String],
                CapabilityRequirement {
                    capability: CapabilityKind::ProcessRun,
                    selector: ResourceSelector::ProcessTemplate {
                        template: ProcessSelectorTemplate {
                            executable_argument: 0,
                            allowed_executables: Vec::new(),
                        },
                    },
                },
            ),
        ),
        // A proposal is an explicit, user-editable artifact boundary.  The
        // source itself is just data here: accepting it never executes a
        // shell, Python, or Finch program implicitly.  The host returns
        // `none` for cancel, `some(ok source)` for execute, and
        // `some(err context)` for a request to continue the conversation.
        (
            "proposal-open".into(),
            capability(
                vec![Type::String, Type::String, Type::String],
                vec![Type::Option(Box::new(Type::Result(
                    Box::new(Type::String),
                    Box::new(Type::String),
                )))],
                CapabilityRequirement {
                    capability: CapabilityKind::ProgramInvoke,
                    selector: ResourceSelector::ProgramTemplate {
                        template: ProgramSelectorTemplate {
                            language_argument: 0,
                            allowed_languages: vec![
                                "bash".into(),
                                "sh".into(),
                                "shell".into(),
                                "python".into(),
                                "py".into(),
                                "lisp".into(),
                                "finch".into(),
                                "forth".into(),
                                "coforth".into(),
                                "co-forth".into(),
                                "text".into(),
                            ],
                        },
                    },
                },
            ),
        ),
        (
            "network-connect".into(),
            capability(
                vec![Type::String, Type::Int],
                vec![Type::Resource("network-socket".into())],
                CapabilityRequirement {
                    capability: CapabilityKind::NetworkConnect,
                    selector: ResourceSelector::NetworkTemplate {
                        template: NetworkSelectorTemplate {
                            host_argument: 0,
                            port_argument: 1,
                            allowed_hosts: Vec::new(),
                            allowed_ports: Vec::new(),
                        },
                    },
                },
            ),
        ),
        (
            "network-send".into(),
            capability(
                vec![Type::Resource("network-socket".into()), Type::Bytes],
                vec![Type::Bytes],
                unscoped(CapabilityKind::NetworkConnect),
            ),
        ),
        (
            "mem-recall".into(),
            capability(
                vec![Type::String],
                vec![Type::list(Type::String)],
                CapabilityRequirement {
                    capability: CapabilityKind::MemoryRead,
                    selector: ResourceSelector::Memory {
                        tree: "session".into(),
                        path: "**".into(),
                    },
                },
            ),
        ),
        (
            "mem-store".into(),
            capability(
                vec![Type::String],
                vec![Type::Resource("memory-node".into())],
                CapabilityRequirement {
                    capability: CapabilityKind::MemoryWrite,
                    selector: ResourceSelector::Memory {
                        tree: "session".into(),
                        path: "**".into(),
                    },
                },
            ),
        ),
        (
            "schedule-create".into(),
            capability(
                vec![Type::String, Type::Int],
                vec![Type::Resource("schedule".into())],
                CapabilityRequirement {
                    capability: CapabilityKind::ScheduleCreate,
                    selector: ResourceSelector::Schedule { policy: None },
                },
            ),
        ),
        (
            "agent-spawn".into(),
            capability(
                vec![Type::String],
                vec![Type::Task(Box::new(Type::String))],
                unscoped(CapabilityKind::AgentSpawn),
            ),
        ),
        (
            "agent-await".into(),
            capability(
                vec![Type::Task(Box::new(Type::String))],
                vec![Type::String],
                unscoped(CapabilityKind::AgentAwait),
            ),
        ),
        (
            "agent-poll".into(),
            capability(
                vec![Type::Task(Box::new(Type::String))],
                vec![Type::String],
                unscoped(CapabilityKind::AgentPoll),
            ),
        ),
        (
            "agent-cancel".into(),
            capability(
                vec![Type::Task(Box::new(Type::String))],
                vec![Type::Unit],
                unscoped(CapabilityKind::AgentCancel),
            ),
        ),
        (
            "automation-availability".into(),
            capability(
                Vec::new(),
                vec![Type::String],
                CapabilityRequirement {
                    capability: CapabilityKind::AutomationInspect,
                    selector: ResourceSelector::Automation { application: None },
                },
            ),
        ),
        (
            "automation-displays".into(),
            capability(
                Vec::new(),
                vec![Type::String],
                CapabilityRequirement {
                    capability: CapabilityKind::AutomationInspect,
                    selector: ResourceSelector::Automation { application: None },
                },
            ),
        ),
        (
            "automation-windows".into(),
            capability(
                Vec::new(),
                vec![Type::String],
                CapabilityRequirement {
                    capability: CapabilityKind::AutomationInspect,
                    selector: ResourceSelector::Automation { application: None },
                },
            ),
        ),
        (
            "automation-click".into(),
            capability(
                vec![Type::Float, Type::Float, Type::String, Type::Int],
                vec![Type::String],
                CapabilityRequirement {
                    capability: CapabilityKind::AutomationWrite,
                    selector: ResourceSelector::Automation { application: None },
                },
            ),
        ),
        (
            "automation-type".into(),
            capability(
                vec![Type::String, Type::Int],
                vec![Type::String],
                CapabilityRequirement {
                    capability: CapabilityKind::AutomationWrite,
                    selector: ResourceSelector::Automation { application: None },
                },
            ),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_words_are_generated_from_one_registry() {
        let vocabulary = core_vocabulary();
        assert_eq!(vocabulary["dup"].to_string(), "( S A -- S A A ! {} )");
        assert!(!vocabulary["agent-spawn"].effects.is_pure());
    }

    #[test]
    fn every_registered_core_word_has_a_specific_discovery_contract() {
        for name in core_vocabulary().keys() {
            let documentation = core_word_documentation(name);
            assert!(
                !documentation.summary.starts_with("Typed Finch core word."),
                "core word '{name}' needs specific provider documentation"
            );
            assert!(!documentation.lisp.is_empty());
            assert!(!documentation.forth.is_empty());
            assert!(!documentation.example.is_empty());
        }
    }

    #[test]
    fn language_schema_advertises_every_core_capability_kind() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../vocabulary/language/schema.json")).unwrap();
        let advertised = schema["$defs"]["capability"]["properties"]["capability"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        for capability in [
            CapabilityKind::VmRead,
            CapabilityKind::VmWrite,
            CapabilityKind::FileRead,
            CapabilityKind::FileWrite,
            CapabilityKind::NetworkConnect,
            CapabilityKind::AutomationInspect,
            CapabilityKind::AutomationWrite,
            CapabilityKind::AgentSpawn,
            CapabilityKind::AgentAwait,
            CapabilityKind::AgentPoll,
            CapabilityKind::AgentCancel,
            CapabilityKind::ProcessRun,
            CapabilityKind::SessionEmit,
            CapabilityKind::MemoryRead,
            CapabilityKind::MemoryWrite,
            CapabilityKind::MemoryConsolidate,
            CapabilityKind::ScheduleCreate,
            CapabilityKind::ScheduleRead,
            CapabilityKind::ScheduleManage,
            CapabilityKind::ProgramInvoke,
            CapabilityKind::UnsafeMemory,
        ] {
            let serialized = serde_json::to_value(capability).unwrap();
            assert!(advertised.contains(serialized.as_str().unwrap()));
        }
    }
}
