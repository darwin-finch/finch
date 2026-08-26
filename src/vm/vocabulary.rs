use super::effects::{
    CapabilityKind, CapabilityRequirement, EffectSet, FileSelector, FileSelectorTemplate,
    FileSelectorTemplatePart, McpSelectorTemplate, NetworkSelectorTemplate,
    ProcessSelectorTemplate, ProgramSelectorTemplate, ResourceRoot, ResourceSelector,
};
use super::signature::{ControlEffect, StackRow, StackSignature, SuspensionSignature};
use super::types::Type;
use super::verifier::Vocabulary;
use once_cell::sync::Lazy;
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

/// The executable destination of a built-in word after verification.
///
/// This is deliberately part of the core-word contract instead of being
/// inferred from its effect row.  A pure word may execute in the interpreter,
/// while an effectful word may be lowered to a portable host request; the
/// effect row expresses authority, not implementation ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreHostBinding {
    SessionEmit,
    VmVocabulary,
    CapabilityList,
    FileRead,
    FileHash,
    TreeList,
    TreeMerkle,
    FileSize,
    FileSlice,
    FileLinesOpen,
    FileLinesNext,
    FileLinesClose,
    CsvOpen,
    CsvSummary,
    CsvNext,
    CsvClose,
    WorkbookOpen,
    WorkbookSheetOpen,
    WorkbookSheets,
    WorkbookRange,
    WorkbookSummary,
    StreamNext,
    StreamClose,
    FileWrite,
    ProcessRun,
    McpCall,
    ProposalOpen,
    NetworkConnect,
    NetworkSend,
    MemoryRecall,
    MemoryStore,
    ScheduleCreate,
    ScheduleGet,
    ScheduleCancel,
    AgentSpawn,
    AgentSpawnWith,
    AgentAwait,
    AgentPoll,
    AgentCancel,
    AutomationAvailability,
    AutomationDisplays,
    AutomationWindows,
    AutomationClick,
    AutomationType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreWordImplementation {
    /// The typed interpreter evaluates the word directly on its private VM
    /// stack.  These words must have an `execute_core` arm.
    Interpreter,
    /// The frontends lower the word to a typed VM instruction rather than a
    /// generic call (for example `yield` and output-handle operations).
    VmInstruction,
    /// The frontends lower the word to this specific capability request. The
    /// host adapter must authorize, journal, and resume it by this binding,
    /// never by an untyped source-name convention.
    HostEffect(CoreHostBinding),
}

/// One inspectable production core-word contract.
///
/// Frontends, the verifier, provider discovery, and interpreter dispatch all
/// consume this registry.  This prevents a word from acquiring a signature or
/// documentation without also declaring where its verified implementation
/// lives.
#[derive(Debug, Clone)]
pub struct CoreWordSpec {
    pub signature: StackSignature,
    pub documentation: CoreWordDocumentation,
    pub implementation: CoreWordImplementation,
}

/// Return provider-neutral documentation for a registered core word.
///
/// Keep this total: the stack signature and capability requirement remain the
/// normative contract even while prose for a newly added word is minimal.
fn core_word_documentation_template(name: &str) -> CoreWordDocumentation {
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
        "project-path" => CoreWordDocumentation { summary: "Resolve text beneath the application-installed project root. The root binding and file authority are separate.", lisp: "(project-path text)", forth: "text project-path", example: "(project-file-read (project-path \"src/main.rs\"))" },
        "task-output-path" => CoreWordDocumentation { summary: "Resolve text beneath the application-installed task-output root, allowing narrowly scoped generated artifacts without workspace authority.", lisp: "(task-output-path text)", forth: "text task-output-path", example: "(task-output-file-write (task-output-path \"report.txt\") (bytes \"done\"))" },
        "file-read" => CoreWordDocumentation { summary: "Read all bytes from an authorized workspace path. Prefer file-slice or cursor resources for large inputs.", lisp: "(file-read path)", forth: "path file-read", example: "(file-read (path \"Cargo.toml\"))" },
        "file-hash" => CoreWordDocumentation { summary: "Compute an authorized file's SHA-256 digest as lowercase hexadecimal without exposing its contents to the VM or model context.", lisp: "(file-hash path)", forth: "path file-hash", example: "(file-hash (path \"data.csv\"))" },
        "tree-list" => CoreWordDocumentation { summary: "Return deterministic bounded metadata for entries below an authorized directory. The result contains entries with path/kind/size fields plus a truncated flag; symlinks and unsupported file kinds are rejected.", lisp: "(tree-list path max-entries)", forth: "path max-entries tree-list", example: "(tree-list (path \"src\") 1000)" },
        "tree-merkle" => CoreWordDocumentation { summary: "Compute a deterministic SHA-256 Merkle-style digest of an authorized directory subtree in sorted relative-path order. Symlinks are rejected and traversal is bounded.", lisp: "(tree-merkle path)", forth: "path tree-merkle", example: "(tree-merkle (path \"src\"))" },
        "file-slice" => CoreWordDocumentation { summary: "Read a bounded byte range from an authorized workspace path: offset and maximum byte count.", lisp: "(file-slice path offset length)", forth: "path offset length file-slice", example: "(file-slice (path \"data.csv\") 0 4096)" },
        "file-size" => CoreWordDocumentation { summary: "Return the byte length of an authorized workspace file without reading its contents.", lisp: "(file-size path)", forth: "path file-size", example: "(file-size (path \"data.csv\"))" },
        "file-lines-open" => CoreWordDocumentation { summary: "Open an authorized text-file stream<string>. The opaque stream owns no forgeable path authority.", lisp: "(file-lines-open path)", forth: "path file-lines-open", example: "(file-lines-open (path \"large.log\"))" },
        "file-lines-next" | "file-lines-close" => CoreWordDocumentation { summary: "Compatibility aliases for stream-next and stream-close on file-line streams. Prefer the generic stream operations in new programs.", lisp: "(stream-next stream), (stream-close stream)", forth: "stream stream-next; stream stream-close", example: "(match-option (stream-next lines) (some line (say line)) (none (stream-close lines)))" },
        "csv-open" => CoreWordDocumentation { summary: "Open an authorized stream<list<string>> of CSV records. CSV parsing preserves quoted fields and records spanning physical lines.", lisp: "(csv-open path)", forth: "path csv-open", example: "(let ((rows (csv-open (path \"data.csv\")))) (stream-next rows))" },
        "csv-summary" => CoreWordDocumentation { summary: "Scan at most max_rows data records after the header and return bounded JSON column statistics without materializing the CSV in model context. The limit must be 1..100000.", lisp: "(csv-summary path max_rows)", forth: "path max_rows csv-summary", example: "(csv-summary (path \"data.csv\") 10000)" },
        "csv-next" | "csv-close" => CoreWordDocumentation { summary: "Compatibility aliases for stream-next and stream-close on CSV streams. Prefer the generic stream operations in new programs.", lisp: "(stream-next stream), (stream-close stream)", forth: "stream stream-next; stream stream-close", example: "(stream-next rows)" },
        "workbook-open" => CoreWordDocumentation { summary: "Open the first sheet of an authorized XLSX/XLS/ODS workbook as an opaque stream<list<string>>. Rows stay in the host cursor and enter the VM one at a time through stream-next.", lisp: "(workbook-open path)", forth: "path workbook-open", example: "(workbook-open (path \"report.xlsx\"))" },
        "workbook-sheet-open" => CoreWordDocumentation { summary: "Open one named sheet of an authorized XLSX/XLS/ODS workbook as an opaque stream<list<string>>.", lisp: "(workbook-sheet-open path sheet)", forth: "path sheet workbook-sheet-open", example: "(workbook-sheet-open (path \"report.xlsx\") \"Summary\")" },
        "workbook-sheets" => CoreWordDocumentation { summary: "List sheet names in an authorized XLSX/XLS/ODS workbook without exposing workbook contents.", lisp: "(workbook-sheets path)", forth: "path workbook-sheets", example: "(workbook-sheets (path \"report.xlsx\"))" },
        "workbook-range" => CoreWordDocumentation { summary: "Return a bounded rectangular slice from one workbook sheet using zero-based row and column offsets. Row and column counts must be positive and their product must not exceed 10000 cells.", lisp: "(workbook-range path sheet start-row start-column row-count column-count)", forth: "path sheet start-row start-column row-count column-count workbook-range", example: "(workbook-range (path \"report.xlsx\") \"Data\" 1 0 20 6)" },
        "workbook-summary" => CoreWordDocumentation { summary: "Treat the first row of one workbook sheet as headers, scan at most max_rows following rows, and return bounded JSON column statistics. The limit must be 1..100000.", lisp: "(workbook-summary path sheet max-rows)", forth: "path sheet max-rows workbook-summary", example: "(workbook-summary (path \"report.xlsx\") \"Data\" 10000)" },
        "stream-next" => CoreWordDocumentation { summary: "Advance an opaque stream<T> by at most one item and return some(T), or none at end of stream. It cannot forge or widen the source stream's authority.", lisp: "(stream-next stream)", forth: "stream stream-next", example: "(match-option (stream-next rows) (some row row) (none \"done\"))" },
        "stream-close" => CoreWordDocumentation { summary: "Close an opaque stream<T>, release its backing cursor or producer, and make future operations fail safely.", lisp: "(stream-close stream)", forth: "stream stream-close", example: "rows stream-close" },
        "file-write" | "host-file-write" | "project-file-write" | "task-output-file-write" => CoreWordDocumentation { summary: "Write bytes to an authorized refined path. This is an external mutation and requires an explicit write capability grant.", lisp: "(file-write path bytes)", forth: "path bytes file-write", example: "(file-write (path \"generated.txt\") (bytes \"hello\\n\"))" },
        "host-file-read" => CoreWordDocumentation { summary: "Read all bytes from an authorized host-machine path. It requires both an installed host root and a matching read grant.", lisp: "(host-file-read path)", forth: "path host-file-read", example: "(host-file-read (host-path \"/tmp/report.txt\"))" },
        "project-file-read" | "task-output-file-read" => CoreWordDocumentation { summary: "Read bytes beneath the matching application-installed resource root after an exact file-read grant.", lisp: "(project-file-read (project-path \"README.md\"))", forth: "s\"README.md\" project-path project-file-read", example: "(project-file-read (project-path \"README.md\"))" },
        "process-run" => CoreWordDocumentation { summary: "Run an approved executable from the exact opened object bound to its canonical path, inode, and content identity, with a list of string arguments and no shell. PATH and relative lookup and symlinks are rejected. Platforms without stable opened-object execution report this capability as unsupported. Use proposal-open for editable scripts.", lisp: "(process-run command arguments)", forth: "command arguments process-run", example: "(process-run \"/usr/bin/git\" (list \"status\" \"--short\"))" },
        "mcp-call" => CoreWordDocumentation { summary: "Call one tool on one connected MCP server through the managed JSON boundary. Server and tool names are capability parameters; MCP descriptions remain untrusted data.", lisp: "(mcp-call server tool arguments-json)", forth: "server tool arguments-json mcp-call", example: "(mcp-call \"github\" \"issue_get\" (result-unwrap (json-parse \"{\\\"owner\\\":\\\"darwin-finch\\\",\\\"repo\\\":\\\"finch\\\",\\\"issue_number\\\":42}\")))" },
        "proposal-open" => CoreWordDocumentation { summary: "Ask the host to open a human-editable artifact proposal. Approval may execute the edited artifact through its normal validator, return edited text for chat, or cancel; this word does not run a shell itself.", lisp: "(proposal-open language title source)", forth: "language title source proposal-open", example: "(proposal-open \"python\" \"Report\" \"print('hello')\\n\")" },
        "mem-recall" | "mem-store" => CoreWordDocumentation { summary: "Read matching session memory entries or store one text memory entry. Both use the host memory tree and require their respective memory capability.", lisp: "(mem-recall query), (mem-store text)", forth: "query mem-recall; text mem-store", example: "(mem-store \"tested release candidate\")" },
        "agent-spawn" | "agent-await" | "agent-poll" | "agent-cancel" => CoreWordDocumentation { summary: "Create or control a separate typed child-agent task. Agent tasks have their own stack, budget, ancestry, and attenuated grants; poll and await return typed snapshot/result records rather than serialized text.", lisp: "(agent-spawn task), (agent-poll handle), (agent-await handle), (agent-cancel handle)", forth: "task agent-spawn; handle agent-poll; handle agent-await; handle agent-cancel", example: "(agent-poll (agent-spawn \"summarize recent test failures\"))" },
        "agent-spawn-with" => CoreWordDocumentation { summary: "Spawn a bounded child agent from an explicit typed task specification, including role, parent-authored background, verified context references, an explicit opaque capability-grant subset, optional provider/model selection, and resource budgets. Empty background/provider/model strings select no override.", lisp: "(agent-spawn-with spec)", forth: "spec agent-spawn-with", example: "(agent-spawn-with { :task \"inspect failures\" :role \"explore\" :background \"\" :provider \"\" :model \"\" :context-refs (empty-list record{kind:string,id:string,sha256:string}) :capabilities (empty-list resource<capability-grant>) :max-turns 4 :timeout-ms 60000 :max-output-bytes 65536 })" },
        "defer" => CoreWordDocumentation { summary: "Turn a pure zero-argument yielding closure into a cooperative fiber<Y,R>. The runtime owns its private continuation and runs it only through fiber operations.", lisp: "(defer closure) or (defer :fiber closure)", forth: "['] producer defer", example: "(defer (lambda () (begin (yield 1) 2)))" },
        "defer-cpu" => CoreWordDocumentation { summary: "Run a pure zero-argument non-producing closure on the bounded native worker pool and return task<R>.", lisp: "(defer :cpu closure) or (defer-cpu closure)", forth: "['] work defer-cpu", example: "(defer :cpu (lambda () (* 6 7)))" },
        "fiber-next" => CoreWordDocumentation { summary: "Advance one cooperative producer. It returns ok(Y) for a yielded value or err(end(R)) for the terminal return.", lisp: "(fiber-next fiber)", forth: "fiber fiber-next", example: "(match-result (fiber-next producer) (ok value value) (err end end))" },
        "fiber-join" => CoreWordDocumentation { summary: "Advance a cooperative producer through remaining yields and return its terminal R. It consumes no host thread.", lisp: "(fiber-join fiber)", forth: "fiber fiber-join", example: "producer fiber-join" },
        "fiber-cancel" => CoreWordDocumentation { summary: "Cancel and consume a cooperative producer handle. The runtime retains a tombstone so later stale-handle use fails deterministically.", lisp: "(fiber-cancel fiber)", forth: "fiber fiber-cancel", example: "producer fiber-cancel" },
        "task-poll" | "task-join" | "task-cancel" => CoreWordDocumentation { summary: "Inspect, join, or cooperatively cancel a scheduler-owned task<T>. Poll returns record{task:task<T>,value:option<T>} so observing a running task never destroys the handle needed to poll, join, or cancel it later. CPU and agent task kinds remain distinct at runtime.", lisp: "(task-poll task), (task-join task), (task-cancel task)", forth: "task task-poll; task task-join; task task-cancel", example: "(task-join (record-get (task-poll (defer :cpu (lambda () 42))) \"task\"))" },
        "yield" => CoreWordDocumentation { summary: "Publish one typed value and suspend the exact VM continuation. Yielding unit is a cooperative timeslice; producer fibers consume other payload types.", lisp: "(yield value); (yield) is unit shorthand", forth: "value yield; use unit yield for a timeslice", example: "(begin (yield 42) (say \"resumed\"))" },
        "unit" => CoreWordDocumentation { summary: "Push the unit value. It is useful when an operation needs an explicit no-information value, including unit yield.", lisp: "nil", forth: "unit", example: "unit yield" },
        "some" | "none" | "is-some" | "unwrap" => CoreWordDocumentation { summary: "Construct, test, or project typed option values. Prefer exhaustive match-option/if-some over unwrap when none is expected control flow.", lisp: "(some value), (none), (is-some option), (unwrap option)", forth: "value some; none; option is-some; option unwrap", example: "(match-option (some 42) (some n (say (int-to-string n))) (none (say \"missing\")))" },
        "ok" | "err" | "is-ok" | "result-unwrap" | "result-error" => CoreWordDocumentation { summary: "Construct, test, or project typed result values. Prefer exhaustive match-result/if-ok over projecting an unknown branch.", lisp: "(ok value), (err error), (is-ok result), (result-unwrap result)", forth: "value ok; error err; result is-ok; result result-unwrap", example: "(match-result (ok 42) (ok n (say (int-to-string n))) (err e (say e)))" },
        "network-connect" | "network-send" => CoreWordDocumentation { summary: "Open an approved network connection or send bytes over an existing opaque socket. The socket is not forgeable and calls remain capability-checked.", lisp: "(network-connect host port), (network-send socket bytes)", forth: "host port network-connect; socket bytes network-send", example: "(network-connect \"example.com\" 443)" },
        "schedule-create" => CoreWordDocumentation { summary: "Create a capability-bound scheduled event using a callback descriptor and time. Scheduled work never gains new authority when it fires.", lisp: "(schedule-create callback when)", forth: "callback when schedule-create", example: "(schedule-create \"daily-summary\" 1770000000)" },
        "schedule-get" => CoreWordDocumentation { summary: "Inspect one opaque schedule handle. Returns some(json) while the host still knows the schedule, or none; callback authority remains redacted inside its host-owned context.", lisp: "(schedule-get schedule)", forth: "schedule schedule-get", example: "(schedule-get (schedule-create \"daily-summary\" 1770000000))" },
        "schedule-cancel" => CoreWordDocumentation { summary: "Cancel one pending opaque schedule handle without deleting its durable record. Returns false if it was unknown or no longer pending.", lisp: "(schedule-cancel schedule)", forth: "schedule schedule-cancel", example: "(schedule-cancel schedule)" },
        "vm-vocabulary" => CoreWordDocumentation { summary: "Return the serialized current typed vocabulary. Use search_word/inspect_word for compact targeted discovery.", lisp: "(vm-vocabulary)", forth: "vm-vocabulary", example: "(say (vm-vocabulary))" },
        "capability-list" => CoreWordDocumentation { summary: "List reusable grants applicable to this ProgramRun as metadata paired with opaque capability-grant resources. The resource, not its printed UUID or JSON metadata, is the selectable authority reference.", lisp: "(capability-list)", forth: "capability-list", example: "(capability-list)" },
        "automation-availability" | "automation-displays" | "automation-windows" | "automation-click" | "automation-type" => CoreWordDocumentation { summary: "Inspect or operate desktop automation through the host adapter. Availability and every concrete target remain capability-checked at the execution boundary.", lisp: "(automation-availability), (automation-click x y button count), (automation-type text delay)", forth: "automation-availability; x y button count automation-click; text delay automation-type", example: "(automation-availability)" },
        "list-length" | "list-get" | "list-append" | "list-uncons" => CoreWordDocumentation { summary: "Inspect or immutably decompose/extend a homogeneous typed list. list-uncons returns none for empty or some(record{head:A,tail:list<A>}); list-append returns a replacement list.", lisp: "(list-length items), (list-get items index), (list-append items value), (list-uncons items)", forth: "items list-length; items index list-get; items value list-append; items list-uncons", example: "(match-option (list-uncons (list 4 8)) (some pair (unwrap (record-get pair \"head\"))) (none 0))" },
        "map-get" | "map-set" | "map-keys" | "map-entries" | "map-length" => CoreWordDocumentation { summary: "Inspect or immutably update a typed map. map-get returns option<V>; map-set returns a replacement map; map-entries returns insertion-ordered key/value typed records. None mutates a shared value.", lisp: "(map-get map key), (map-set map key value), (map-keys map), (map-entries map), (map-length map)", forth: "map key map-get; map key value map-set; map map-keys; map map-entries; map map-length", example: "(unwrap (record-get (list-get (map-entries (map \"answer\" 42)) 0) \"value\"))" },
        "str-cat" => CoreWordDocumentation { summary: "Concatenate text without adding whitespace or a newline. Lisp accepts two or more strings and lowers them to repeated calls of the binary Co-Forth word.", lisp: "(str-cat first second ...)", forth: "left right str-cat", example: "(say (str-cat \"answer: \" (int-to-string 42) \".\"))" },
        "bytes" | "int-to-string" | "atoi" | "space" => CoreWordDocumentation { summary: "Pure text and byte conversion helpers.", lisp: "(bytes text), (int-to-string n), (atoi text), (space)", forth: "text bytes; n int-to-string; text atoi; space", example: "42 int-to-string say" },
        "json-parse" | "json-stringify" | "json-get" | "json-index" | "json-keys" | "json-as-map" | "json-as-string" | "json-as-int" | "json-as-float" | "json-as-bool" => CoreWordDocumentation { summary: "Pure managed JSON operations. json-parse returns result<json,string>; field/index lookup and scalar projections return options rather than coercing or treating text as authority. json-keys returns an empty typed list for a non-object; json-as-map explicitly normalizes an object to map<string,json>.", lisp: "(json-parse text), (json-get value field), (json-index value index), (json-keys value), (json-as-map value), (json-as-string value), (json-as-int value), (json-as-float value), (json-as-bool value)", forth: "text json-parse; json field json-get; json index json-index; json json-keys; json json-as-map; json json-as-string|json-as-int|json-as-float|json-as-bool", example: "s\" {\\\"answer\\\":42}\" json-parse result-unwrap json-as-map unwrap s\" answer\" map-get unwrap json-as-int unwrap" },
        "dup" | "drop" | "swap" => CoreWordDocumentation { summary: "Pure stack shuffles. Prefer Lisp let bindings or Co-Forth locals for complex programs rather than deep positional juggling.", lisp: "Usually use let instead of stack shuffles.", forth: "value dup; value drop; left right swap", example: "3 dup * int-to-string say" },
        "+" => CoreWordDocumentation { summary: "Add two integers and return their sum.", lisp: "(+ a b)", forth: "a b +", example: "2 3 +" },
        "-" => CoreWordDocumentation { summary: "Subtract the right integer from the left integer.", lisp: "(- left right)", forth: "left right -", example: "10 3 -" },
        "*" => CoreWordDocumentation { summary: "Multiply two integers and return their product.", lisp: "(* a b)", forth: "a b *", example: "2 144 *" },
        "/" => CoreWordDocumentation { summary: "Divide the left integer by the nonzero right integer.", lisp: "(/ dividend divisor)", forth: "dividend divisor /", example: "144 2 /" },
        "mod" => CoreWordDocumentation { summary: "Return the integer remainder after division.", lisp: "(mod dividend divisor)", forth: "dividend divisor mod", example: "7 3 mod" },
        "negate" => CoreWordDocumentation { summary: "Return the additive inverse of one integer.", lisp: "(negate value)", forth: "value negate", example: "7 negate" },
        "abs" => CoreWordDocumentation { summary: "Return the nonnegative absolute value of one integer.", lisp: "(abs value)", forth: "value abs", example: "-7 abs" },
        "=" => CoreWordDocumentation { summary: "Compare two compatible values for equality.", lisp: "(= left right)", forth: "left right =", example: "42 42 =" },
        "<" => CoreWordDocumentation { summary: "Return whether the left integer is less than the right integer.", lisp: "(< left right)", forth: "left right <", example: "2 3 <" },
        ">" => CoreWordDocumentation { summary: "Return whether the left integer is greater than the right integer.", lisp: "(> left right)", forth: "left right >", example: "3 2 >" },
        "<=" => CoreWordDocumentation { summary: "Return whether the left integer is at most the right integer.", lisp: "(<= left right)", forth: "left right <=", example: "2 2 <=" },
        ">=" => CoreWordDocumentation { summary: "Return whether the left integer is at least the right integer.", lisp: "(>= left right)", forth: "left right >=", example: "3 2 >=" },
        "not" => CoreWordDocumentation { summary: "Invert one boolean value.", lisp: "(not flag)", forth: "flag not", example: "false not" },
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
        suspension: None,
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
    rooted_path_selector("host-machine")
}

fn host_path_template() -> FileSelectorTemplate {
    rooted_path_template("host-machine")
}

fn project_path_selector() -> FileSelector {
    rooted_path_selector("project")
}

fn project_path_template() -> FileSelectorTemplate {
    rooted_path_template("project")
}

fn task_output_path_selector() -> FileSelector {
    rooted_path_selector("task.output")
}

fn task_output_path_template() -> FileSelectorTemplate {
    rooted_path_template("task.output")
}

fn rooted_path_selector(root: &str) -> FileSelector {
    FileSelector::parse(&format!("${{{root}}}/**")).expect("valid resource root")
}

fn rooted_path_template(root: &str) -> FileSelectorTemplate {
    let upper_bound = rooted_path_selector(root);
    FileSelectorTemplate {
        root: upper_bound.root.clone(),
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
pub fn agent_task_spec_type() -> Type {
    Type::Record(vec![
        ("task".into(), Type::String),
        ("role".into(), Type::String),
        ("background".into(), Type::String),
        ("provider".into(), Type::String),
        ("model".into(), Type::String),
        (
            "context-refs".into(),
            Type::list(Type::Record(vec![
                ("kind".into(), Type::String),
                ("id".into(), Type::String),
                ("sha256".into(), Type::String),
            ])),
        ),
        (
            "capabilities".into(),
            Type::list(Type::Resource("capability-grant".into())),
        ),
        ("max-turns".into(), Type::Int),
        ("timeout-ms".into(), Type::Int),
        ("max-output-bytes".into(), Type::Int),
    ])
}

pub fn capability_grant_entry_type() -> Type {
    Type::Record(vec![
        ("grant".into(), Type::Resource("capability-grant".into())),
        ("requirement".into(), Type::Json),
    ])
}

pub fn tree_entry_type() -> Type {
    Type::Record(vec![
        ("path".into(), Type::String),
        ("kind".into(), Type::String),
        ("size".into(), Type::Int),
    ])
}

pub fn tree_listing_type() -> Type {
    Type::Record(vec![
        ("entries".into(), Type::list(tree_entry_type())),
        ("truncated".into(), Type::Bool),
    ])
}

pub fn agent_task_result_type() -> Type {
    Type::Record(vec![
        ("task-id".into(), Type::String),
        ("agent-id".into(), Type::String),
        ("status".into(), Type::String),
        ("final-message".into(), Type::String),
        ("diagnostics".into(), Type::list(Type::String)),
        ("turns".into(), Type::Int),
        ("elapsed-ms".into(), Type::Int),
        ("provider-model".into(), Type::String),
        ("starting-context-hash".into(), Type::String),
        ("depth".into(), Type::Int),
    ])
}

pub fn agent_task_snapshot_type() -> Type {
    Type::Record(vec![
        ("task-id".into(), Type::String),
        ("agent-id".into(), Type::String),
        ("status".into(), Type::String),
        ("task".into(), Type::String),
        ("role".into(), Type::String),
        ("provider-model".into(), Type::String),
        ("starting-context-hash".into(), Type::String),
        ("depth".into(), Type::Int),
        ("complete".into(), Type::Bool),
    ])
}

fn core_signatures() -> Vocabulary {
    let a = Type::Variable("A".into());
    let y = Type::Variable("Y".into());
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
            "project-path".into(),
            pure(
                vec![Type::String],
                vec![Type::Path(project_path_selector())],
            ),
        ),
        (
            "task-output-path".into(),
            pure(
                vec![Type::String],
                vec![Type::Path(task_output_path_selector())],
            ),
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
            pure(vec![Type::Json], vec![Type::list(Type::String)]),
        ),
        (
            "json-as-map".into(),
            pure(
                vec![Type::Json],
                vec![Type::Option(Box::new(Type::Map(
                    Box::new(Type::String),
                    Box::new(Type::Json),
                )))],
            ),
        ),
        (
            "json-as-string".into(),
            pure(vec![Type::Json], vec![Type::Option(Box::new(Type::String))]),
        ),
        (
            "json-as-int".into(),
            pure(vec![Type::Json], vec![Type::Option(Box::new(Type::Int))]),
        ),
        (
            "json-as-float".into(),
            pure(vec![Type::Json], vec![Type::Option(Box::new(Type::Float))]),
        ),
        (
            "json-as-bool".into(),
            pure(vec![Type::Json], vec![Type::Option(Box::new(Type::Bool))]),
        ),
        // The single typed suspension primitive. A unit payload is a plain
        // cooperative timeslice; other payload types are available to a
        // producer/fiber host through the saved suspension record.
        (
            "yield".into(),
            StackSignature {
                type_parameters: vec!["Y".into()],
                input: StackRow::polymorphic("S", vec![y.clone()]),
                output: StackRow::polymorphic("S", Vec::new()),
                effects: EffectSet::pure(),
                control: ControlEffect::MaySuspend,
                suspension: Some(SuspensionSignature::one_way(y)),
            },
        ),
        (
            "defer".into(),
            pure(
                vec![Type::Function {
                    arguments: Vec::new(),
                    result: Box::new(Type::Variable("R".into())),
                    effects: EffectSet::pure(),
                    suspension: Some(SuspensionSignature::one_way(Type::Variable("Y".into()))),
                }],
                vec![Type::Fiber(
                    Box::new(Type::Variable("Y".into())),
                    Box::new(Type::Variable("R".into())),
                )],
            ),
        ),
        (
            "defer-cpu".into(),
            pure(
                vec![Type::Function {
                    arguments: Vec::new(),
                    result: Box::new(Type::Variable("R".into())),
                    effects: EffectSet::pure(),
                    // A non-suspending closure is a valid subtype of this
                    // contract; CPU workers also accept unit timeslices so
                    // cancellation can be observed at a VM boundary.
                    suspension: Some(SuspensionSignature::one_way(Type::Unit)),
                }],
                vec![Type::Task(Box::new(Type::Variable("R".into())))],
            ),
        ),
        (
            "fiber-next".into(),
            pure(
                vec![Type::Fiber(
                    Box::new(Type::Variable("Y".into())),
                    Box::new(Type::Variable("R".into())),
                )],
                vec![Type::fiber_step(
                    Type::Variable("Y".into()),
                    Type::Variable("R".into()),
                )],
            ),
        ),
        (
            "fiber-join".into(),
            pure(
                vec![Type::Fiber(
                    Box::new(Type::Variable("Y".into())),
                    Box::new(Type::Variable("R".into())),
                )],
                vec![Type::Variable("R".into())],
            ),
        ),
        (
            "fiber-cancel".into(),
            pure(
                vec![Type::Fiber(
                    Box::new(Type::Variable("Y".into())),
                    Box::new(Type::Variable("R".into())),
                )],
                vec![Type::Unit],
            ),
        ),
        (
            "task-poll".into(),
            pure(
                vec![Type::Task(Box::new(Type::Variable("R".into())))],
                vec![Type::task_poll(Type::Variable("R".into()))],
            ),
        ),
        (
            "task-join".into(),
            pure(
                vec![Type::Task(Box::new(Type::Variable("R".into())))],
                vec![Type::Variable("R".into())],
            ),
        ),
        (
            "task-cancel".into(),
            pure(
                vec![Type::Task(Box::new(Type::Variable("R".into())))],
                vec![Type::Unit],
            ),
        ),
        ("unit".into(), pure(Vec::new(), vec![Type::Unit])),
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
        (
            "file-hash".into(),
            capability(
                vec![Type::Path(
                    FileSelector::parse("./**").expect("valid workspace root"),
                )],
                vec![Type::String],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        (
            "tree-list".into(),
            capability(
                vec![
                    Type::Path(FileSelector::parse("./**").expect("valid workspace root")),
                    Type::Int,
                ],
                vec![tree_listing_type()],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        (
            "tree-merkle".into(),
            capability(
                vec![Type::Path(
                    FileSelector::parse("./**").expect("valid workspace root"),
                )],
                vec![Type::String],
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
        // Streams are host-issued opaque cursors. Only their opening word
        // receives a path selector; `stream-next`/`stream-close` operate on
        // that already-authorized handle and cannot fabricate a path.
        (
            "file-lines-open".into(),
            capability(
                vec![Type::Path(
                    FileSelector::parse("./**").expect("valid workspace root"),
                )],
                vec![Type::Stream(Box::new(Type::String))],
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
                vec![Type::Stream(Box::new(Type::String))],
                vec![Type::Option(Box::new(Type::String))],
                unscoped(CapabilityKind::FileRead),
            ),
        ),
        (
            "file-lines-close".into(),
            capability(
                vec![Type::Stream(Box::new(Type::String))],
                vec![Type::Unit],
                unscoped(CapabilityKind::FileRead),
            ),
        ),
        // CSV records need their own stream: quoted fields may legally span
        // physical lines, so a line cursor cannot safely model a CSV row.
        (
            "csv-open".into(),
            capability(
                vec![Type::Path(
                    FileSelector::parse("./**").expect("valid workspace root"),
                )],
                vec![Type::Stream(Box::new(Type::list(Type::String)))],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        (
            "csv-summary".into(),
            capability(
                vec![
                    Type::Path(FileSelector::parse("./**").expect("valid workspace root")),
                    Type::Int,
                ],
                vec![Type::Json],
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
                vec![Type::Stream(Box::new(Type::list(Type::String)))],
                vec![Type::Option(Box::new(Type::list(Type::String)))],
                unscoped(CapabilityKind::FileRead),
            ),
        ),
        (
            "csv-close".into(),
            capability(
                vec![Type::Stream(Box::new(Type::list(Type::String)))],
                vec![Type::Unit],
                unscoped(CapabilityKind::FileRead),
            ),
        ),
        (
            "workbook-open".into(),
            capability(
                vec![Type::Path(
                    FileSelector::parse("./**").expect("valid workspace root"),
                )],
                vec![Type::Stream(Box::new(Type::list(Type::String)))],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        (
            "workbook-sheet-open".into(),
            capability(
                vec![
                    Type::Path(FileSelector::parse("./**").expect("valid workspace root")),
                    Type::String,
                ],
                vec![Type::Stream(Box::new(Type::list(Type::String)))],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        (
            "workbook-sheets".into(),
            capability(
                vec![Type::Path(
                    FileSelector::parse("./**").expect("valid workspace root"),
                )],
                vec![Type::list(Type::String)],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        (
            "workbook-range".into(),
            capability(
                vec![
                    Type::Path(FileSelector::parse("./**").expect("valid workspace root")),
                    Type::String,
                    Type::Int,
                    Type::Int,
                    Type::Int,
                    Type::Int,
                ],
                vec![Type::list(Type::list(Type::String))],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        (
            "workbook-summary".into(),
            capability(
                vec![
                    Type::Path(FileSelector::parse("./**").expect("valid workspace root")),
                    Type::String,
                    Type::Int,
                ],
                vec![Type::Json],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: path_template(),
                    },
                },
            ),
        ),
        // Generic stream operations share the same bounded cursor contract
        // across file, CSV, and future workbook/producer backends. The
        // unscoped FileRead request is safely covered only by a path-scoped
        // stream-open grant; host ownership/generation checks reject forged
        // or cross-run handles at the call boundary.
        (
            "stream-next".into(),
            capability(
                vec![Type::Stream(Box::new(a.clone()))],
                vec![Type::Option(Box::new(a.clone()))],
                unscoped(CapabilityKind::FileRead),
            ),
        ),
        (
            "stream-close".into(),
            capability(
                vec![Type::Stream(Box::new(a.clone()))],
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
            "project-file-read".into(),
            capability(
                vec![Type::Path(project_path_selector())],
                vec![Type::Bytes],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: project_path_template(),
                    },
                },
            ),
        ),
        (
            "project-file-write".into(),
            capability(
                vec![Type::Path(project_path_selector()), Type::Bytes],
                vec![Type::Unit],
                CapabilityRequirement {
                    capability: CapabilityKind::FileWrite,
                    selector: ResourceSelector::FileTemplate {
                        template: project_path_template(),
                    },
                },
            ),
        ),
        (
            "task-output-file-read".into(),
            capability(
                vec![Type::Path(task_output_path_selector())],
                vec![Type::Bytes],
                CapabilityRequirement {
                    capability: CapabilityKind::FileRead,
                    selector: ResourceSelector::FileTemplate {
                        template: task_output_path_template(),
                    },
                },
            ),
        ),
        (
            "task-output-file-write".into(),
            capability(
                vec![Type::Path(task_output_path_selector()), Type::Bytes],
                vec![Type::Unit],
                CapabilityRequirement {
                    capability: CapabilityKind::FileWrite,
                    selector: ResourceSelector::FileTemplate {
                        template: task_output_path_template(),
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
            "list-append".into(),
            pure(
                vec![Type::list(a.clone()), a.clone()],
                vec![Type::list(a.clone())],
            ),
        ),
        (
            "list-uncons".into(),
            pure(
                vec![Type::list(a.clone())],
                vec![Type::Option(Box::new(Type::Record(vec![
                    ("head".into(), a.clone()),
                    ("tail".into(), Type::list(a.clone())),
                ])))],
            ),
        ),
        (
            "map-get".into(),
            pure(
                vec![
                    Type::Map(Box::new(Type::Variable("K".into())), Box::new(a.clone())),
                    Type::Variable("K".into()),
                ],
                vec![Type::Option(Box::new(a.clone()))],
            ),
        ),
        (
            "map-set".into(),
            pure(
                vec![
                    Type::Map(Box::new(Type::Variable("K".into())), Box::new(a.clone())),
                    Type::Variable("K".into()),
                    a.clone(),
                ],
                vec![Type::Map(
                    Box::new(Type::Variable("K".into())),
                    Box::new(a.clone()),
                )],
            ),
        ),
        (
            "map-keys".into(),
            pure(
                vec![Type::Map(
                    Box::new(Type::Variable("K".into())),
                    Box::new(Type::Variable("V".into())),
                )],
                vec![Type::list(Type::Variable("K".into()))],
            ),
        ),
        (
            "map-entries".into(),
            pure(
                vec![Type::Map(
                    Box::new(Type::Variable("K".into())),
                    Box::new(Type::Variable("V".into())),
                )],
                vec![Type::list(Type::Record(vec![
                    ("key".into(), Type::Variable("K".into())),
                    ("value".into(), Type::Variable("V".into())),
                ]))],
            ),
        ),
        (
            "map-length".into(),
            pure(
                vec![Type::Map(
                    Box::new(Type::Variable("K".into())),
                    Box::new(Type::Variable("V".into())),
                )],
                vec![Type::Int],
            ),
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
            "capability-list".into(),
            capability(
                Vec::new(),
                vec![Type::list(capability_grant_entry_type())],
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
        (
            "mcp-call".into(),
            capability(
                vec![Type::String, Type::String, Type::Json],
                vec![Type::Json],
                CapabilityRequirement {
                    capability: CapabilityKind::McpCall,
                    selector: ResourceSelector::McpTemplate {
                        template: McpSelectorTemplate {
                            server_argument: 0,
                            tool_argument: 1,
                            allowed_servers: Vec::new(),
                            allowed_tools: Vec::new(),
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
            "schedule-get".into(),
            capability(
                vec![Type::Resource("schedule".into())],
                vec![Type::Option(Box::new(Type::Json))],
                CapabilityRequirement {
                    capability: CapabilityKind::ScheduleRead,
                    selector: ResourceSelector::Schedule { policy: None },
                },
            ),
        ),
        (
            "schedule-cancel".into(),
            capability(
                vec![Type::Resource("schedule".into())],
                vec![Type::Bool],
                CapabilityRequirement {
                    capability: CapabilityKind::ScheduleManage,
                    selector: ResourceSelector::Schedule { policy: None },
                },
            ),
        ),
        (
            "agent-spawn".into(),
            capability(
                vec![Type::String],
                vec![Type::Task(Box::new(agent_task_result_type()))],
                unscoped(CapabilityKind::AgentSpawn),
            ),
        ),
        (
            "agent-spawn-with".into(),
            capability(
                vec![agent_task_spec_type()],
                vec![Type::Task(Box::new(agent_task_result_type()))],
                unscoped(CapabilityKind::AgentSpawn),
            ),
        ),
        (
            "agent-await".into(),
            capability(
                vec![Type::Task(Box::new(agent_task_result_type()))],
                vec![agent_task_result_type()],
                unscoped(CapabilityKind::AgentAwait),
            ),
        ),
        (
            "agent-poll".into(),
            capability(
                vec![Type::Task(Box::new(agent_task_result_type()))],
                vec![agent_task_snapshot_type()],
                unscoped(CapabilityKind::AgentPoll),
            ),
        ),
        (
            "agent-cancel".into(),
            capability(
                vec![Type::Task(Box::new(agent_task_result_type()))],
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

/// The immutable production registry. It is initialized once so every
/// verifier, provider-discovery call, and interpreter dispatch in this
/// process observes the identical versioned core contract.
static CORE_WORD_REGISTRY: Lazy<BTreeMap<String, CoreWordSpec>> = Lazy::new(|| {
    core_signatures()
        .into_iter()
        .map(|(name, signature)| {
            let implementation = match name.as_str() {
                // These forms are first-class IR instructions so that their
                // suspension/output semantics cannot be reimplemented by a
                // generic host-call path.
                "yield" | "defer" | "defer-cpu" | "fiber-next" | "fiber-join" | "fiber-cancel"
                | "task-poll" | "task-join" | "task-cancel" | "output-open" | "output-append"
                | "output-replace" | "output-status" | "output-progress" | "output-complete"
                | "output-fail" => CoreWordImplementation::VmInstruction,
                _ if signature.effects.is_pure() => CoreWordImplementation::Interpreter,
                "say" | "emit" => CoreWordImplementation::HostEffect(CoreHostBinding::SessionEmit),
                "vm-vocabulary" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::VmVocabulary)
                }
                "capability-list" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::CapabilityList)
                }
                "file-read" | "host-file-read" | "project-file-read" | "task-output-file-read" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::FileRead)
                }
                "file-hash" => CoreWordImplementation::HostEffect(CoreHostBinding::FileHash),
                "tree-list" => CoreWordImplementation::HostEffect(CoreHostBinding::TreeList),
                "tree-merkle" => CoreWordImplementation::HostEffect(CoreHostBinding::TreeMerkle),
                "file-size" => CoreWordImplementation::HostEffect(CoreHostBinding::FileSize),
                "file-slice" => CoreWordImplementation::HostEffect(CoreHostBinding::FileSlice),
                "file-lines-open" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::FileLinesOpen)
                }
                "file-lines-next" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::FileLinesNext)
                }
                "file-lines-close" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::FileLinesClose)
                }
                "csv-open" => CoreWordImplementation::HostEffect(CoreHostBinding::CsvOpen),
                "csv-summary" => CoreWordImplementation::HostEffect(CoreHostBinding::CsvSummary),
                "csv-next" => CoreWordImplementation::HostEffect(CoreHostBinding::CsvNext),
                "csv-close" => CoreWordImplementation::HostEffect(CoreHostBinding::CsvClose),
                "workbook-open" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::WorkbookOpen)
                }
                "workbook-sheet-open" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::WorkbookSheetOpen)
                }
                "workbook-sheets" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::WorkbookSheets)
                }
                "workbook-range" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::WorkbookRange)
                }
                "workbook-summary" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::WorkbookSummary)
                }
                "stream-next" => CoreWordImplementation::HostEffect(CoreHostBinding::StreamNext),
                "stream-close" => CoreWordImplementation::HostEffect(CoreHostBinding::StreamClose),
                "file-write"
                | "host-file-write"
                | "project-file-write"
                | "task-output-file-write" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::FileWrite)
                }
                "process-run" => CoreWordImplementation::HostEffect(CoreHostBinding::ProcessRun),
                "mcp-call" => CoreWordImplementation::HostEffect(CoreHostBinding::McpCall),
                "proposal-open" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::ProposalOpen)
                }
                "network-connect" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::NetworkConnect)
                }
                "network-send" => CoreWordImplementation::HostEffect(CoreHostBinding::NetworkSend),
                "mem-recall" => CoreWordImplementation::HostEffect(CoreHostBinding::MemoryRecall),
                "mem-store" => CoreWordImplementation::HostEffect(CoreHostBinding::MemoryStore),
                "schedule-create" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::ScheduleCreate)
                }
                "schedule-get" => CoreWordImplementation::HostEffect(CoreHostBinding::ScheduleGet),
                "schedule-cancel" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::ScheduleCancel)
                }
                "agent-spawn" => CoreWordImplementation::HostEffect(CoreHostBinding::AgentSpawn),
                "agent-spawn-with" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::AgentSpawnWith)
                }
                "agent-await" => CoreWordImplementation::HostEffect(CoreHostBinding::AgentAwait),
                "agent-poll" => CoreWordImplementation::HostEffect(CoreHostBinding::AgentPoll),
                "agent-cancel" => CoreWordImplementation::HostEffect(CoreHostBinding::AgentCancel),
                "automation-availability" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::AutomationAvailability)
                }
                "automation-displays" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::AutomationDisplays)
                }
                "automation-windows" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::AutomationWindows)
                }
                "automation-click" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::AutomationClick)
                }
                "automation-type" => {
                    CoreWordImplementation::HostEffect(CoreHostBinding::AutomationType)
                }
                _ => unreachable!("every effectful core word needs a host binding: {name}"),
            };
            let documentation = core_word_documentation_template(&name);
            (
                name,
                CoreWordSpec {
                    signature,
                    documentation,
                    implementation,
                },
            )
        })
        .collect()
});

/// Return the immutable production registry. Callers that need to extend a
/// vocabulary for a module must first copy the relevant signatures; core
/// contracts themselves are never mutable runtime state.
pub fn core_word_registry() -> &'static BTreeMap<String, CoreWordSpec> {
    &CORE_WORD_REGISTRY
}

/// Return the complete contract for one core word.  This is the canonical
/// lookup used by execution and provider discovery.
pub fn core_word_spec(name: &str) -> Option<CoreWordSpec> {
    core_word_registry().get(name).cloned()
}

/// Return provider-neutral documentation for a registered core word.
///
/// Unknown names receive the generic fallback so older discovery clients can
/// still display a useful response while the caller reports the missing word.
pub fn core_word_documentation(name: &str) -> CoreWordDocumentation {
    core_word_spec(name)
        .map(|spec| spec.documentation)
        .unwrap_or_else(|| core_word_documentation_template(name))
}

/// Canonical signatures for verifier-facing consumers.  The returned map is
/// derived from the complete core-word registry so signatures cannot drift
/// from documentation or execution ownership.
pub fn core_vocabulary() -> Vocabulary {
    core_word_registry()
        .iter()
        .map(|(name, spec)| (name.clone(), spec.signature.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_words_are_generated_from_one_registry() {
        let vocabulary = core_vocabulary();
        assert_eq!(vocabulary["dup"].to_string(), "( S A -- S A A ! pure )");
        assert!(!vocabulary["agent-spawn"].effects.is_pure());
        assert_eq!(
            vocabulary["agent-spawn"].output.values,
            vec![Type::Task(Box::new(agent_task_result_type()))]
        );
        assert_eq!(
            vocabulary["agent-await"].output.values,
            vec![agent_task_result_type()]
        );
        assert_eq!(
            vocabulary["agent-poll"].output.values,
            vec![agent_task_snapshot_type()]
        );
        assert_eq!(
            vocabulary["tree-list"].output.values,
            vec![tree_listing_type()]
        );
    }

    #[test]
    fn registry_declares_signature_documentation_and_execution_ownership() {
        let registry = core_word_registry();
        let say = &registry["say"];
        assert_eq!(
            say.implementation,
            CoreWordImplementation::HostEffect(CoreHostBinding::SessionEmit)
        );
        assert_eq!(say.documentation.forth, "text say");
        assert!(!say.signature.effects.is_pure());

        assert_eq!(
            registry["+"].implementation,
            CoreWordImplementation::Interpreter
        );
        assert_eq!(
            registry["output-open"].implementation,
            CoreWordImplementation::VmInstruction
        );

        let signatures: Vocabulary = registry
            .iter()
            .map(|(name, spec)| (name.clone(), spec.signature.clone()))
            .collect();
        assert_eq!(signatures, core_vocabulary());
    }

    #[test]
    fn registry_never_classifies_an_effectful_word_as_local_interpreter_code() {
        for (name, spec) in core_word_registry() {
            match spec.implementation {
                CoreWordImplementation::Interpreter => assert!(
                    spec.signature.effects.is_pure(),
                    "interpreter word '{name}' must not hide an effect row"
                ),
                CoreWordImplementation::HostEffect(_) => assert!(
                    !spec.signature.effects.is_pure(),
                    "host-effect word '{name}' must declare an effect row"
                ),
                CoreWordImplementation::VmInstruction => {}
            }
        }
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
            CapabilityKind::McpCall,
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
