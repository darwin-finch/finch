# Finch IPC — Cap'n Proto schema for CLI ↔ daemon communication.
#
# Transport: Unix domain socket at ~/.finch/daemon.sock
# The HTTP server (port 11435) is kept for external OpenAI-compatible clients
# (VS Code / Continue.dev).  This schema is the internal fast path.

@0xb5d8e7a1c3f09d2e;

# ---------------------------------------------------------------------------
# Conversation types
# ---------------------------------------------------------------------------

struct ContentBlock {
  union {
    text      @0 :Text;
    toolUse   @1 :ToolUseBlock;
    toolResult @2 :ToolResultBlock;
    thinking  @3 :Text;
  }
}

struct ToolUseBlock {
  id        @0 :Text;
  name      @1 :Text;
  input     @2 :JsonValue;
}

struct ToolResultBlock {
  toolUseId @0 :Text;
  content   @1 :Text;
  isError   @2 :Bool;
}

struct Message {
  role    @0 :Text;           # "user" | "assistant" | "system"
  content @1 :List(ContentBlock);
}

# ---------------------------------------------------------------------------
# Tool definitions
# ---------------------------------------------------------------------------

struct ToolDefinition {
  name             @0 :Text;
  description      @1 :Text;
  inputSchemaJson  @2 :Text;  # JSON Schema
}

struct ToolUse {
  id        @0 :Text;
  name      @1 :Text;
  input     @2 :JsonValue;
}

# ---------------------------------------------------------------------------
# Query / response
# ---------------------------------------------------------------------------

struct QueryResponse {
  text         @0 :Text;
  toolUses     @1 :List(ToolUse);
  model        @2 :Text;
  inputTokens  @3 :UInt32;
  outputTokens @4 :UInt32;
  latencyMs    @5 :UInt64;
}

struct StreamChunk {
  union {
    textDelta       @0 :Text;
    toolUseComplete @1 :ToolUse;
    usageUpdate     @2 :UsageUpdate;
    done            @3 :Void;
    error           @4 :Text;
    responseMetadata @5 :StreamResponseMetadata;
    allowanceUpdate  @6 :AllowanceUpdate;
  }
}

struct StreamResponseMetadata {
  model @0 :Text;
}

struct UsageUpdate {
  inputTokens  @0 :UInt32;
  outputTokens @1 :UInt32;
}

struct AllowanceUpdate {
  hasPrimary          @0 :Bool;
  primaryUsedPercent  @1 :Float32;
  hasSecondary        @2 :Bool;
  secondaryUsedPercent @3 :Float32;
}

# ---------------------------------------------------------------------------
# Streaming callback capability
# ---------------------------------------------------------------------------

interface StreamReceiver {
  # Called by the server for each chunk.  The client returns a promise so
  # the server can apply backpressure if needed.
  onChunk @0 (chunk :StreamChunk) -> ();
}

# ---------------------------------------------------------------------------
# Named-Brain environment runner callback
# ---------------------------------------------------------------------------

enum ProgramLanguage {
  forth @0;
  lisp  @1;
}

# Schema-native representation for JSON-compatible dynamic values. This keeps
# arbitrary tool/provider payloads structured on the wire without serializing
# an opaque JSON byte string.
struct JsonValue {
  union {
    nullValue @0 :Void;
    boolValue @1 :Bool;
    signed    @2 :Int64;
    unsigned  @3 :UInt64;
    float     @4 :Float64;
    text      @5 :Text;
    array     @6 :List(JsonValue);
    object    @7 :List(JsonField);
  }
}

struct JsonField {
  name  @0 :Text;
  value @1 :JsonValue;
}

# ---------------------------------------------------------------------------
# Portable typed-runtime checkpoints
# ---------------------------------------------------------------------------

# This is reducible VM state only. Host handles, grants, sockets, processes,
# and approval authority are never represented by this schema.
struct TypedRuntimeCheckpoint {
  version        @0 :UInt32;
  stack          @1 :List(TypedValue);
  functions      @2 :List(NamedFunction);
  producerFibers @3 :List(NamedProducerFiber);
}

struct NamedFunction {
  name     @0 :Text;
  function @1 :VmFunction;
}

struct NamedProducerFiber {
  id     @0 :Text;
  record @1 :ProducerFiberRecord;
}

enum TaskKind {
  agent    @0;
  cpuFiber @1;
}

struct TypedValue {
  union {
    unit       @0 :Void;
    boolValue  @1 :Bool;
    intValue   @2 :Int64;
    uintValue  @3 :UInt64;
    floatValue @4 :Float64;
    charValue  @5 :UInt32;
    symbol     @6 :Text;
    string     @7 :Text;
    bytes      @8 :Data;
    json       @9 :JsonValue;
    path       @10 :PathValue;
    list       @11 :ListValue;
    map        @12 :MapValue;
    option     @13 :OptionValue;
    result     @14 :ResultValue;
    record     @15 :List(NamedTypedValue);
    variant    @16 :VariantValue;
    closure    @17 :ClosureValue;
    task       @18 :TaskValue;
    fiber      @19 :FiberValue;
    stream     @20 :StreamValue;
    resource   @21 :ResourceValue;
    dynamicValue @22 :DynamicValue;
  }
}

struct PathValue {
  selector @0 :FileSelector;
  relative @1 :Text;
}

struct ListValue {
  elementType @0 :TypedType;
  values      @1 :List(TypedValue);
}

struct MapValue {
  keyType   @0 :TypedType;
  valueType @1 :TypedType;
  entries   @2 :List(TypedMapEntry);
}

struct TypedMapEntry {
  key   @0 :TypedValue;
  value @1 :TypedValue;
}

struct OptionValue {
  innerType @0 :TypedType;
  hasValue  @1 :Bool;
  value     @2 :TypedValue;
}

struct ResultValue {
  okType    @0 :TypedType;
  errorType @1 :TypedType;
  isOk      @2 :Bool;
  value     @3 :TypedValue;
}

struct NamedTypedValue {
  name  @0 :Text;
  value @1 :TypedValue;
}

struct VariantValue {
  name     @0 :Text;
  hasValue @1 :Bool;
  value    @2 :TypedValue;
}

struct ClosureValue {
  function  @0 :Text;
  captures  @1 :List(TypedValue);
  signature @2 :StackSignature;
}

struct TaskValue {
  id         @0 :Text;
  resultType @1 :TypedType;
  kind       @2 :TaskKind;
}

struct FiberValue {
  id         @0 :Text;
  yieldType  @1 :TypedType;
  resultType @2 :TypedType;
}

struct StreamValue {
  id          @0 :Text;
  elementType @1 :TypedType;
  kind        @2 :Text;
  generation  @3 :UInt64;
}

struct ResourceValue {
  kind       @0 :Text;
  handle     @1 :Text;
  generation @2 :UInt64;
}

struct DynamicValue {
  runtimeType @0 :TypedType;
  value       @1 :TypedValue;
}

struct TypedType {
  union {
    unit       @0 :Void;
    boolType   @1 :Void;
    intType    @2 :Void;
    uintType   @3 :Void;
    floatType  @4 :Void;
    charType   @5 :Void;
    symbolType @6 :Void;
    stringType @7 :Void;
    bytesType  @8 :Void;
    jsonType   @9 :Void;
    path       @10 :FileSelector;
    list       @11 :TypedType;
    map        @12 :MapType;
    option     @13 :TypedType;
    result     @14 :ResultType;
    record     @15 :List(TypedField);
    variant    @16 :List(TypedVariant);
    function   @17 :FunctionType;
    task       @18 :TypedType;
    fiber      @19 :FiberType;
    stream     @20 :TypedType;
    resource   @21 :Text;
    capability @22 :Text;
    variable   @23 :Text;
    dynamicType @24 :Void;
  }
}

struct MapType {
  key   @0 :TypedType;
  value @1 :TypedType;
}

struct ResultType {
  ok    @0 :TypedType;
  error @1 :TypedType;
}

struct FiberType {
  yieldType  @0 :TypedType;
  resultType @1 :TypedType;
}

struct TypedField {
  name @0 :Text;
  type @1 :TypedType;
}

struct TypedVariant {
  name       @0 :Text;
  hasPayload @1 :Bool;
  payload    @2 :TypedType;
}

struct FunctionType {
  arguments     @0 :List(TypedType);
  result        @1 :TypedType;
  effects       @2 :List(CapabilityRequirement);
  hasSuspension @3 :Bool;
  suspension    @4 :SuspensionSignature;
}

enum ResourceRootKind {
  workspace   @0;
  project     @1;
  taskOutput  @2;
  hostMachine @3;
  named       @4;
}

struct ResourceRoot {
  kind @0 :ResourceRootKind;
  name @1 :Text;
}

struct FileSelector {
  root    @0 :ResourceRoot;
  pattern @1 :Text;
}

struct FileSelectorTemplate {
  root       @0 :ResourceRoot;
  parts      @1 :List(FileSelectorTemplatePart);
  upperBound @2 :FileSelector;
}

struct FileSelectorTemplatePart {
  union {
    literal  @0 :Text;
    argument @1 :FileSelectorTemplateArgument;
  }
}

struct FileSelectorTemplateArgument {
  index @0 :UInt64;
  bound @1 :FileSelector;
}

struct NetworkSelectorTemplate {
  hostArgument @0 :UInt64;
  portArgument @1 :UInt64;
  allowedHosts @2 :List(Text);
  allowedPorts @3 :List(UInt16);
}

struct ProcessSelectorTemplate {
  executableArgument @0 :UInt64;
  allowedExecutables @1 :List(Text);
}

struct ProgramSelectorTemplate {
  languageArgument @0 :UInt64;
  allowedLanguages @1 :List(Text);
}

struct McpSelectorTemplate {
  serverArgument @0 :UInt64;
  toolArgument   @1 :UInt64;
  allowedServers @2 :List(Text);
  allowedTools   @3 :List(Text);
}

enum CapabilityKind {
  vmRead            @0;
  vmWrite           @1;
  fileRead          @2;
  fileWrite         @3;
  networkConnect    @4;
  automationInspect @5;
  automationWrite   @6;
  agentSpawn        @7;
  agentAwait        @8;
  agentPoll         @9;
  agentCancel       @10;
  processRun        @11;
  sessionEmit       @12;
  memoryRead        @13;
  memoryWrite       @14;
  memoryConsolidate @15;
  scheduleCreate    @16;
  scheduleRead      @17;
  scheduleManage    @18;
  programInvoke     @19;
  mcpCall           @20;
  unsafeMemory      @21;
}

struct CapabilityRequirement {
  capability @0 :CapabilityKind;
  selector   @1 :ResourceSelector;
}

struct ResourceSelector {
  union {
    none            @0 :Void;
    file            @1 :FileSelector;
    fileTemplate    @2 :FileSelectorTemplate;
    networkTemplate @3 :NetworkSelectorTemplate;
    network         @4 :NetworkSelector;
    automation      @5 :AutomationSelector;
    agent           @6 :AgentSelector;
    process         @7 :List(Text);
    processTemplate @8 :ProcessSelectorTemplate;
    program         @9 :List(Text);
    programTemplate @10 :ProgramSelectorTemplate;
    mcp             @11 :McpSelector;
    mcpTemplate     @12 :McpSelectorTemplate;
    memory          @13 :MemorySelector;
    schedule        @14 :ScheduleSelector;
  }
}

struct NetworkSelector {
  host  @0 :Text;
  ports @1 :List(UInt16);
}

struct AutomationSelector {
  hasApplication @0 :Bool;
  application    @1 :Text;
}

struct AgentSelector {
  providers   @0 :List(Text);
  maxDepth    @1 :UInt16;
  maxChildren @2 :UInt16;
}

struct McpSelector {
  server @0 :Text;
  tool   @1 :Text;
}

struct MemorySelector {
  tree @0 :Text;
  path @1 :Text;
}

struct ScheduleSelector {
  hasPolicy @0 :Bool;
  policy    @1 :Text;
}

enum ControlEffect {
  returns      @0;
  mayThrow     @1;
  maySuspend   @2;
  neverReturns @3;
}

struct StackRow {
  hasTail @0 :Bool;
  tail    @1 :Text;
  values  @2 :List(TypedType);
}

struct SuspensionSignature {
  yieldType  @0 :TypedType;
  resumeType @1 :TypedType;
}

struct StackSignature {
  typeParameters @0 :List(Text);
  input          @1 :StackRow;
  output         @2 :StackRow;
  effects        @3 :List(CapabilityRequirement);
  control        @4 :ControlEffect;
  hasSuspension  @5 :Bool;
  suspension     @6 :SuspensionSignature;
}

enum SourceLanguage {
  forth    @0;
  lisp     @1;
  finchIr  @2;
  native   @3;
  provider @4;
}

struct SourceSpan {
  sourceId    @0 :Text;
  startByte   @1 :UInt64;
  endByte     @2 :UInt64;
  startLine   @3 :UInt64;
  startColumn @4 :UInt64;
  endLine     @5 :UInt64;
  endColumn   @6 :UInt64;
}

struct SourceOrigin {
  language     @0 :SourceLanguage;
  hasSpan      @1 :Bool;
  span         @2 :SourceSpan;
  hasWord      @3 :Bool;
  word         @4 :Text;
  hasExpansion @5 :Bool;
  expansion    @6 :SourceOrigin;
}

enum UiOperation {
  create   @0;
  append   @1;
  replace  @2;
  status   @3;
  progress @4;
  complete @5;
  fail     @6;
}

struct VmFunction {
  name             @0 :Text;
  hasDocumentation @1 :Bool;
  documentation    @2 :Text;
  signature        @3 :StackSignature;
  locals           @4 :List(TypedType);
  captures         @5 :List(TypedType);
  entry            @6 :UInt32;
  blocks           @7 :List(BasicBlock);
}

struct BasicBlock {
  id           @0 :UInt32;
  instructions @1 :List(LocatedInstruction);
}

struct LocatedInstruction {
  instruction @0 :Instruction;
  origin      @1 :SourceOrigin;
}

struct Instruction {
  union {
    constant          @0 :TypedValue;
    makeList          @1 :CountedType;
    makeMap           @2 :CountedMapType;
    makeRecord        @3 :List(TypedField);
    makeVariant       @4 :VariantInstruction;
    variantGet        @5 :VariantInstruction;
    recordGet         @6 :RecordGetInstruction;
    recordSet         @7 :RecordSetInstruction;
    dup               @8 :Void;
    drop              @9 :Void;
    swap              @10 :Void;
    localGet          @11 :UInt32;
    localSet          @12 :UInt32;
    captureGet        @13 :UInt32;
    makeClosure       @14 :MakeClosureInstruction;
    call              @15 :Text;
    callClosure       @16 :StackSignature;
    capabilityRequest @17 :CapabilityInstruction;
    outputOpen        @18 :Void;
    uiEffect          @19 :UiInstruction;
    yield             @20 :TypedType;
    deferFiber        @21 :Void;
    nextFiber         @22 :Void;
    joinFiber         @23 :Void;
    cancelFiber       @24 :Void;
    deferCpu          @25 :Void;
    pollCpuFiber      @26 :Void;
    joinCpuFiber      @27 :Void;
    cancelCpuFiber    @28 :Void;
    propagateResult   @29 :ResultType;
    jump              @30 :UInt32;
    branch            @31 :BranchInstruction;
    returnInstruction @32 :Void;
    trap              @33 :Text;
  }
}

struct CountedType {
  type  @0 :TypedType;
  count @1 :UInt32;
}

struct CountedMapType {
  keyType   @0 :TypedType;
  valueType @1 :TypedType;
  count     @2 :UInt32;
}

struct VariantInstruction {
  variants       @0 :List(TypedVariant);
  tag            @1 :Text;
  hasPayloadType @2 :Bool;
  payloadType    @3 :TypedType;
}

struct RecordGetInstruction {
  field     @0 :Text;
  valueType @1 :TypedType;
}

struct RecordSetInstruction {
  field      @0 :Text;
  valueType  @1 :TypedType;
  recordType @2 :List(TypedField);
}

struct MakeClosureInstruction {
  function     @0 :Text;
  captureCount @1 :UInt32;
  signature    @2 :StackSignature;
}

struct CapabilityInstruction {
  requirement @0 :CapabilityRequirement;
  input       @1 :List(TypedType);
  output      @2 :List(TypedType);
}

struct UiInstruction {
  operation @0 :UiOperation;
  input     @1 :List(TypedType);
  output    @2 :List(TypedType);
}

struct BranchInstruction {
  thenBlock @0 :UInt32;
  elseBlock @1 :UInt32;
}

struct VmModule {
  version   @0 :UInt32;
  name      @1 :Text;
  entry     @2 :Text;
  functions @3 :List(NamedFunction);
}

struct VerifiedFunction {
  name                  @0 :Text;
  inferredEffects       @1 :List(CapabilityRequirement);
  hasInferredSuspension @2 :Bool;
  inferredSuspension    @3 :SuspensionSignature;
  entryStack            @4 :List(TypedType);
  blockStacks           @5 :List(BlockStack);
}

struct BlockStack {
  block @0 :UInt32;
  stack @1 :List(TypedType);
}

struct VerifiedModule {
  module    @0 :VmModule;
  functions @1 :List(VerifiedFunction);
}

struct VmFrame {
  function    @0 :Text;
  block       @1 :UInt32;
  instruction @2 :UInt64;
  stackBase   @3 :UInt64;
  outputArity @4 :UInt64;
  outputTypes @5 :List(TypedType);
  locals      @6 :List(TypedValue);
  captures    @7 :List(TypedValue);
}

struct VmContinuation {
  stack              @0 :List(TypedValue);
  frames             @1 :List(VmFrame);
  fuel               @2 :UInt64;
  nextEffectSequence @3 :UInt64;
}

enum Severity {
  note    @0;
  warning @1;
  error   @2;
}

enum DiagnosticPhase {
  reader            @0;
  macroExpansion    @1;
  nameResolution    @2;
  typeInference     @3;
  verification      @4;
  linking           @5;
  authorization     @6;
  availability      @7;
  approval          @8;
  interpretation    @9;
  hostCall          @10;
  nativeExecution   @11;
  transactionCommit @12;
  childExecution    @13;
  cancellation      @14;
  resourceLimit     @15;
}

struct VmDiagnostic {
  code            @0 :Text;
  severity        @1 :Severity;
  phase           @2 :DiagnosticPhase;
  message         @3 :Text;
  hasPrimary      @4 :Bool;
  primary         @5 :SourceOrigin;
  related         @6 :List(SourceOrigin);
  expectedTypes   @7 :List(TypedType);
  foundTypes      @8 :List(TypedType);
  expectedEffects @9 :List(CapabilityRequirement);
  foundEffects    @10 :List(CapabilityRequirement);
  hasCapability   @11 :Bool;
  capability      @12 :CapabilityRequirement;
  trace           @13 :List(Text);
  hints           @14 :List(Text);
  hasCause        @15 :Bool;
  cause           @16 :VmDiagnostic;
}

struct ProducerFiberRecord {
  module     @0 :VerifiedModule;
  yieldType  @1 :TypedType;
  resultType @2 :TypedType;
  state      @3 :ProducerFiberState;
}

struct ProducerFiberState {
  union {
    ready     @0 :VmContinuation;
    completed @1 :TypedValue;
    failed    @2 :VmDiagnostic;
    cancelled @3 :Void;
  }
}

struct VmUiProgress {
  completed @0 :UInt64;
  hasTotal @1 :Bool;
  total @2 :UInt64;
}

struct VmUiSideEffect {
  operation @0 :UiOperation;
  hasTarget @1 :Bool;
  target @2 :TypedValue;
  hasText @3 :Bool;
  text @4 :Text;
  hasProgress @5 :Bool;
  progress @6 :VmUiProgress;
}

struct VmHostSideEffect {
  union {
    emit @0 :Text;
    ui @1 :VmUiSideEffect;
    request @2 :List(TypedValue);
  }
}

struct VmSideEffect {
  protocolVersion @0 :UInt32;
  sequence @1 :UInt64;
  requirement @2 :CapabilityRequirement;
  event @3 :VmHostSideEffect;
  output @4 :List(TypedType);
  origin @5 :SourceOrigin;
}

struct VmEffectJournalState {
  union {
    proposed @0 :Void;
    awaitingApproval @1 :Void;
    awaitingHostResult @2 :Void;
    acknowledged @3 :List(TypedValue);
    denied @4 :Void;
    cancelled @5 :Void;
    failed @6 :VmDiagnostic;
  }
}

struct BrainEffectRecord {
  executionId @0 :Text;
  effect @1 :VmSideEffect;
  state @2 :VmEffectJournalState;
}

struct BrainProgramRequest {
  brain      @0 :Text;
  requestSeq @1 :UInt64;
  language   @2 :ProgramLanguage;
  source     @3 :Text;
  runId      @4 :Text;
  interaction @5 :BrainProgramInteraction;
  hasGrantCeiling @6 :Bool;
  grantCeiling @7 :List(CapabilityRequirement);
  # Reverse host bridge bound by the daemon to this exact authenticated run.
  # The runner never receives or supplies participant attachment credentials.
  control @8 :BrainProgramControl;
}

enum BrainProgramInteraction {
  interactive    @0;
  noninteractive @1;
}

struct BrainProgramResult {
  output          @0 :Text;
  runtimeRevision @1 :UInt64;
  checkpoint      @2 :TypedRuntimeCheckpoint;
  error           @3 :Text;
  effectJournal   @4 :List(BrainEffectRecord);
}

struct BrainTurnRequest {
  brain       @0 :Text;
  requestSeq  @1 :UInt64;
  prompt      @2 :Text;
  context     @3 :List(Message);
  approvalAudience @4 :BrainApprovalAudience;
  control          @5 :BrainTurnControl;
  runId            @6 :Text;
}

enum BrainAttachmentRole {
  runner     @0;
  driver     @1;
  consultant @2;
  observer   @3;
}

struct BrainApprovalAudience {
  brainId               @0 :Text;
  brain                 @1 :Text;
  subject               @2 :Text;
  role                  @3 :BrainAttachmentRole;
  environmentGeneration @4 :UInt64;
  attachmentId          @5 :Text;
}

struct BrainTurnResult {
  source          @0 :Text;
  language        @1 :ProgramLanguage;
  output          @2 :Text;
  runtimeRevision @3 :UInt64;
  checkpoint      @4 :TypedRuntimeCheckpoint;
  error           @5 :Text;
  turnEvents      @6 :List(BrainTurnEvent);
  effectJournal   @7 :List(BrainEffectRecord);
  hasCommitAck    @8 :Bool;
  commitAck       @9 :BrainTurnCommitAck;
}

# Optional reverse capability returned by the runner with a completed turn.
# The daemon invokes it only after the canonical Brain events, runtime
# checkpoint, and terminal run transition have committed. Frontends use this
# boundary for operations such as self-restart that must not race durability.
interface BrainTurnCommitAck {
  committed @0 (status :BrainRunStatus, detail :Text) -> ();
}

struct BrainMemoryProjectionRequest {
  brainId    @0 :Text;
  brain      @1 :Text;
  runId      @2 :Text;
  requestSeq @3 :UInt64;
  prompt     @4 :Text;
  source     @5 :Text;
}

enum BrainTurnEventKind {
  call              @0;
  result            @1;
  approvalRequested @2;
  approvalDecided   @3;
}

# Ordered provider/runner lifecycle produced while servicing one Brain prompt.
# Arbitrary provider arguments and policy details use the schema-native
# JsonValue union while the event envelope and ordering remain typed.
struct BrainTurnEvent {
  kind         @0 :BrainTurnEventKind;
  toolId       @1 :Text;
  name         @2 :Text; # Present for call events.
  input        @3 :JsonValue; # Present for call events.
  output       @4 :Text; # Present for result events.
  isError      @5 :Bool; # Present for result events.
  approvalId   @6 :Text;
  approvalKind @7 :Text; # "tool" or "vm_capability".
  subject      @8 :Text; # Tool name or capability name.
  detail       @9 :JsonValue; # Present for approval requests.
  decision     @10 :JsonValue; # Present for approval decisions.
  approvalAudience @11 :BrainApprovalAudience; # Present for approval requests.
}

interface BrainRunner {
  runProgram @0 (request :BrainProgramRequest) -> (result :BrainProgramResult);
  runTurn    @1 (request :BrainTurnRequest) -> (result :BrainTurnResult);
  cancelRun  @2 (brain :Text, runId :Text) -> (cancelled :Bool, error :Text);
  projectMemory @3 (request :BrainMemoryProjectionRequest) -> (inserted :UInt32, error :Text);
}

# Long-lived reverse capability bound to the exact registered runner lease.
# Child agents may outlive the parent RPC that spawned them, so their durable
# lifecycle cannot be carried by BrainTurnControl or BrainProgramControl.
interface BrainRunnerControl {
  startSubagent @0 (parentRunId :Text, taskId :Text, detail :Text) ->
                   (run :BrainRun);
  finishSubagent @1 (runId :Text, status :BrainRunStatus, detail :Text) ->
                    (run :BrainRun);
}

# Per-program reverse capability for durable host state owned by a Brain run.
# The daemon binds creator identity and authority to the run before handing this
# capability to the frontend runner.
interface BrainProgramControl {
  createSchedule @0 (language :ProgramLanguage,
                     source :Text,
                     grantCeiling :List(CapabilityRequirement),
                     nextDueMs :UInt64,
                     hasIntervalMs :Bool,
                     intervalMs :UInt64,
                     policy :BrainScheduleDeliveryPolicy) ->
                    (schedule :BrainSchedule);
  inspectSchedule @1 (scheduleId :Text) ->
                     (found :Bool, schedule :BrainSchedule);
  cancelSchedule @2 (scheduleId :Text) -> (cancelled :Bool);
  reserveEffect @3 (executionId :Text, effect :VmSideEffect)
      -> (reservation :BrainEffectReservation);
}

# Per-turn reverse capability. The runner publishes an addressed approval and
# suspends until the daemon returns the decision submitted by that attachment.
interface BrainTurnControl {
  requestApproval @0 (event :BrainTurnEvent) -> (decision :JsonValue);
  reserveEffect @1 (executionId :Text, effect :VmSideEffect)
      -> (reservation :BrainEffectReservation);
}

# The reservation capability captures the daemon-minted run/lease authority
# and canonical identity. Neither provenance nor a bearer secret crosses the
# wire. `begin` returns only after the AwaitingHostResult record is fsynced.
interface BrainEffectReservation {
  begin @0 () -> (permit :BrainHostEffectPermit);
  notApplied @1 (reason :Text) -> ();
}

# Possession proves durable begin. It can record one exact monotonic outcome
# even if the parent turn has already been cancelled or disconnected.
interface BrainHostEffectPermit {
  finish @0 (outcome :BrainHostEffectOutcome) -> ();
}

struct BrainHostEffectOutcome {
  union {
    acknowledged @0 :List(TypedValue);
    notApplied   @1 :Text;
    failedPartial @2 :Text;
  }
}

# ---------------------------------------------------------------------------
# Canonical named-Brain snapshot and event stream
# ---------------------------------------------------------------------------

struct BrainEnvironment {
  machine    @0 :Text;
  workspace  @1 :Text;
  generation @2 :UInt64;
}

struct BrainAttachment {
  attachmentId    @0 :Text;
  subject         @1 :Text;
  role            @2 :BrainAttachmentRole;
  acknowledgedSeq @3 :UInt64;
  connected       @4 :Bool;
  hasConnection   @5 :Bool;
  connectionId    @6 :Text;
}

struct BrainRunnerLease {
  leaseId               @0 :Text;
  subject               @1 :Text;
  environmentGeneration @2 :UInt64;
  acquiredMs            @3 :UInt64;
  expiresMs             @4 :UInt64;
}

struct BrainRunnerHandoff {
  handoffId            @0 :Text;
  fromLeaseId          @1 :Text;
  requestedBy          @2 :Text;
  targetSubject        @3 :Text;
  environmentGeneration @4 :UInt64;
  requestedMs          @5 :UInt64;
  expiresMs            @6 :UInt64;
}

struct BrainRunnerHandoffCompleted {
  handoffId @0 :Text;
  lease     @1 :BrainRunnerLease;
}

struct BrainProgram {
  seq      @0 :UInt64;
  sender   @1 :Text;
  language @2 :ProgramLanguage;
  source   @3 :Text;
}

struct BrainToolCall {
  requestSeq @0 :UInt64;
  toolId     @1 :Text;
  name       @2 :Text;
  input      @3 :JsonValue;
}

struct BrainToolResult {
  requestSeq @0 :UInt64;
  toolId     @1 :Text;
  output     @2 :Text;
  isError    @3 :Bool;
}

struct BrainApprovalRequested {
  requestSeq      @0 :UInt64;
  approvalId      @1 :Text;
  approvalKind    @2 :Text;
  subject         @3 :Text;
  hasAudience     @4 :Bool;
  audience        @5 :BrainApprovalAudience;
  detail          @6 :JsonValue;
}

struct BrainApprovalDecided {
  requestSeq  @0 :UInt64;
  approvalId  @1 :Text;
  decision     @2 :JsonValue;
}

struct BrainProgramSubmitted {
  language @0 :ProgramLanguage;
  source   @1 :Text;
}

struct BrainResult {
  requestSeq @0 :UInt64;
  output     @1 :Text;
  hasError   @2 :Bool;
  error      @3 :Text;
}

struct BrainRuntimeCommitted {
  requestSeq       @0 :UInt64;
  runtimeRevision  @1 :UInt64;
  checkpointSha256 @2 :Text;
}

struct BrainEffectRecorded {
  requestSeq @0 :UInt64;
  executionId @1 :Text;
  effect @2 :VmSideEffect;
  state @3 :VmEffectJournalState;
}

# Canonical schema-v15 audit transition. The daemon validates the complete
# typed transition before append; event-stream clients receive the exact JSON
# representation for forward-compatible inspection without gaining authority.
struct BrainEffectAuditTransition {
  json @0 :Text;
}

struct BrainClientAttached {
  attachmentId @0 :Text;
  connectionId @1 :Text;
  subject      @2 :Text;
  role         @3 :BrainAttachmentRole;
}

struct BrainClientDetached {
  attachmentId @0 :Text;
  connectionId @1 :Text;
}

enum BrainRunKind {
  interactive @0;
  speculative @1;
  scheduled   @2;
  subagent    @3;
  maintenance @4;
}

enum BrainRunStatus {
  queuedForEnvironment @0;
  running              @1;
  awaitingApproval     @2;
  completed            @3;
  failed               @4;
  cancelled            @5;
  interrupted          @6;
}

struct BrainRun {
  runId                  @0 :Text;
  kind                   @1 :BrainRunKind;
  hasParentRunId         @2 :Bool;
  parentRunId            @3 :Text;
  requestSeq             @4 :UInt64;
  initiatingAttachmentId @5 :Text;
  initiatedBy            @6 :Text;
  status                 @7 :BrainRunStatus;
  startedMs              @8 :UInt64;
  updatedMs              @9 :UInt64;
  hasDetail              @10 :Bool;
  detail                 @11 :Text;
}

struct BrainRunStatusChanged {
  runId     @0 :Text;
  status    @1 :BrainRunStatus;
  hasDetail @2 :Bool;
  detail    @3 :Text;
}

enum BrainSchedulePolicyKind {
  coalesce       @0;
  boundedCatchUp @1;
}

struct BrainScheduleDeliveryPolicy {
  kind           @0 :BrainSchedulePolicyKind;
  maxCatchUp     @1 :UInt32;
  expiresAfterMs @2 :UInt64;
}

struct BrainSchedule {
  scheduleId     @0 :Text;
  language       @1 :ProgramLanguage;
  source         @2 :Text;
  nextDueMs      @3 :UInt64;
  hasIntervalMs  @4 :Bool;
  intervalMs     @5 :UInt64;
  deliveryPolicy @6 :BrainScheduleDeliveryPolicy;
  active         @7 :Bool;
  initiatingAttachmentId @8 :Text;
  createdBy      @9 :Text;
  grantCeiling   @10 :List(CapabilityRequirement);
  hasModuleIdentity @11 :Bool;
  moduleName     @12 :Text;
  moduleRevision @13 :UInt32;
  moduleSourceSha256 @14 :Text;
}

struct BrainScheduleDue {
  scheduleId     @0 :Text;
  run            @1 :BrainRun;
  dueAtMs        @2 :UInt64;
  firstMissedAtMs @3 :UInt64;
  missedCount    @4 :UInt32;
  hasNextDueMs   @5 :Bool;
  nextDueMs      @6 :UInt64;
  language       @7 :ProgramLanguage;
  source         @8 :Text;
  grantCeiling   @9 :List(CapabilityRequirement);
}

enum BrainTaskStatus {
  pending    @0;
  inProgress @1;
  completed  @2;
}

enum BrainTaskPriority {
  high   @0;
  medium @1;
  low    @2;
}

struct BrainTask {
  id       @0 :Text;
  content  @1 :Text;
  status   @2 :BrainTaskStatus;
  priority @3 :BrainTaskPriority;
}

struct BrainTaskList {
  tasks @0 :List(BrainTask);
}

struct BrainEvent {
  schemaVersion        @0 :UInt32;
  brainId              @1 :Text;
  seq                  @2 :UInt64;
  environmentGeneration @3 :UInt64;
  sender               @4 :Text;
  createdMs            @5 :UInt64;
  hasRunId             @30 :Bool;
  runId                @31 :Text;
  union {
    runnerLeaseAcquired @6  :BrainRunnerLease;
    runnerLeaseReleased @7  :Text;
    clientAttached      @8  :BrainClientAttached;
    clientDetached      @9  :BrainClientDetached;
    prompt              @10 :Text;
    toolCall            @11 :BrainToolCall;
    toolResult          @12 :BrainToolResult;
    approvalRequested   @13 :BrainApprovalRequested;
    approvalDecided     @14 :BrainApprovalDecided;
    program             @15 :BrainProgramSubmitted;
    programPopped       @16 :UInt64;
    result              @17 :BrainResult;
    runtimeCommitted    @18 :BrainRuntimeCommitted;
    runStarted          @19 :BrainRun;
    runStatusChanged    @20 :BrainRunStatusChanged;
    runnerHandoffRequested @21 :BrainRunnerHandoff;
    runnerHandoffCompleted @22 :BrainRunnerHandoffCompleted;
    runnerHandoffCancelled @23 :Text;
    participantMessage     @24 :Text;
    effectRecorded         @25 :BrainEffectRecorded;
    scheduleChanged        @26 :BrainSchedule;
    scheduleDue            @27 :BrainScheduleDue;
    taskListReplaced       @28 :BrainTaskList;
    speculativePrompt      @29 :Text;
    mutationRecorded       @34 :BrainMutationOutcome;
    effectAuditTransition  @35 :BrainEffectAuditTransition;
  }
  hasMutation @32 :Bool;
  mutation    @33 :BrainMutationReceipt;
}

struct BrainMutationOutcome {
  union {
    runCancellationReserved @0 :Text;
    runAlreadyCancelled     @1 :Text;
    scheduleCancellationNoop @2 :Text;
    handoffCancellationNoop @3 :Text;
    runCancellationNoop    @4 :Text;
    runCancellationDispatching @5 :BrainRunCancellationProgress;
    runCancellationReconciled @6 :BrainRunCancellationProgress;
    approvalDecisionDelivered @7 :BrainApprovalDecisionProgress;
  }
}

struct BrainApprovalDecisionProgress {
  requestSeq @0 :UInt64;
  approvalId @1 :Text;
  mutationId @2 :Text;
}

struct BrainRunCancellationProgress {
  runId      @0 :Text;
  mutationId @1 :Text;
}

struct BrainMutationReceipt {
  mutationId           @0 :Text;
  attachmentId         @1 :Text;
  expectedRevision     @2 :UInt64;
  environmentGeneration @3 :UInt64;
  commandSha256        @4 :Text;
}

struct BrainSnapshot {
  brainId         @0 :Text;
  name            @1 :Text;
  environment     @2 :BrainEnvironment;
  revision        @3 :UInt64;
  events          @4 :List(BrainEvent);
  programStack    @5 :List(BrainProgram);
  attachments     @6 :List(BrainAttachment);
  hasRunnerLease  @7 :Bool;
  runnerLease     @8 :BrainRunnerLease;
  runs            @9 :List(BrainRun);
  hasRunnerHandoff @10 :Bool;
  runnerHandoff    @11 :BrainRunnerHandoff;
  schedules        @12 :List(BrainSchedule);
  pendingScheduleDues @13 :List(BrainScheduleDue);
  tasks           @14 :List(BrainTask);
  effectAudits    @15 :List(Text);
}

struct BrainWireMessage {
  union {
    snapshot @0 :BrainSnapshot;
    event    @1 :BrainEvent;
  }
}

# Once a remote WebSocket has authenticated and bound one exact attachment,
# participant mutations carry only intent. Attachment and connection identity
# come from the socket authority boundary rather than forgeable message fields.
struct BrainRemoteCommand {
  requestId @0 :UInt64;
  union {
    submit      @1 :BrainSubmission;
    acknowledge @2 :UInt64;
    detach      @3 :Void;
    requestRunnerHandoff @4 :BrainRunnerHandoffRequest;
    cancelRunnerHandoff  @5 :Text;
    cancelRun            @6 :Text;
    createSchedule       @7 :BrainScheduleCreateRequest;
    cancelSchedule       @8 :Text;
    scheduleInitialization @9 :UInt64;
  }
  # Absent only for connection-lifecycle operations (acknowledge/detach).
  hasMutation @10 :Bool;
  mutation    @11 :BrainRemoteMutation;
}

struct BrainRemoteMutation {
  brainId               @0 :Text;
  expectedRevision      @1 :UInt64;
  environmentGeneration @2 :UInt64;
  idempotencyKey        @3 :Text;
}

struct BrainScheduleCreateRequest {
  language       @0 :ProgramLanguage;
  source         @1 :Text;
  grantCeiling   @2 :List(CapabilityRequirement);
  nextDueMs      @3 :UInt64;
  hasIntervalMs  @4 :Bool;
  intervalMs     @5 :UInt64;
  policy         @6 :BrainScheduleDeliveryPolicy;
}

struct BrainRunnerHandoffRequest {
  targetSubject        @0 :Text;
  expectedLeaseId      @1 :Text;
  environmentGeneration @2 :UInt64;
  ttlMs                @3 :UInt64;
}

struct BrainRemoteError {
  code    @0 :Text;
  message @1 :Text;
}

struct BrainRemoteReply {
  requestId @0 :UInt64;
  union {
    submitted    @1 :BrainSubmissionOutcome;
    acknowledged @2 :BrainAttachment;
    detached     @3 :Void;
    error        @4 :BrainRemoteError;
    handoffRequested @5 :BrainRunnerHandoff;
    handoffCancelled @6 :Void;
    runCancelled     @7 :BrainRun;
    scheduleCreated  @8 :BrainSchedule;
    scheduleCancelled @9 :Bool;
    initializationScheduled @10 :BrainSchedule;
  }
}

# One framing type in both directions. The server sends ordered Brain
# projections and correlated command replies; the client sends commands.
struct BrainRemoteEnvelope {
  union {
    projection @0 :BrainWireMessage;
    command    @1 :BrainRemoteCommand;
    reply      @2 :BrainRemoteReply;
  }
}

# A participant submits intent, never a complete BrainEvent envelope. The
# daemon assigns identity, ordering, timestamps, run state, and every internal
# lifecycle event.
struct BrainSubmission {
  union {
    prompt          @0 :Text;
    program         @1 :BrainProgramSubmitted;
    programPopped   @2 :UInt64;
    approvalDecided @3 :BrainApprovalDecided;
    participantMessage @4 :Text;
    taskListReplaced @5 :BrainTaskList;
    speculativePrompt @6 :Text;
  }
}

struct BrainSubmissionOutcome {
  accepted  @0 :BrainEvent;
  hasRun    @1 :Bool;
  run       @2 :BrainRun;
  hasResult @3 :Bool;
  result    @4 :BrainEvent;
}

# Ordered projection callback. A watch call first sends one snapshot, then
# every event after that snapshot on the same capability, with RPC
# backpressure preserving delivery order.
interface BrainWireReceiver {
  onMessage @0 (message :BrainWireMessage) -> ();
}

# One versioned lifecycle contract shared by local Cap'n Proto IPC and remote
# adapters. The local Unix-socket adapter trusts the host boundary but still
# validates attachment identity, connection identity, and role on every
# mutation. Remote transports add scoped credential checks before entering the
# same service implementation.
interface BrainService {
  snapshot @0 (brain :Text) -> (snapshot :BrainSnapshot);

  attach @1 (brain :Text,
             subject :Text,
             role :BrainAttachmentRole,
             hasAttachmentId :Bool,
             attachmentId :Text) -> (attachment :BrainAttachment);

  acknowledge @2 (brain :Text,
                  attachmentId :Text,
                  connectionId :Text,
                  seq :UInt64) -> (attachment :BrainAttachment);

  detach @3 (brain :Text,
             attachmentId :Text,
             connectionId :Text) -> ();

  submit @4 (brain :Text,
             attachmentId :Text,
             connectionId :Text,
             submission :BrainSubmission) -> (outcome :BrainSubmissionOutcome);

  watch @5 (brain :Text,
            attachmentId :Text,
            connectionId :Text,
            receiver :BrainWireReceiver) -> ();

  acquireRunner @6 (brain :Text,
                    subject :Text,
                    environment :BrainEnvironment,
                    hasLeaseId :Bool,
                    leaseId :Text,
                    ttlMs :UInt64) -> (lease :BrainRunnerLease);

  releaseRunner @7 (brain :Text, leaseId :Text) -> ();

  requestRunnerHandoff @8 (brain :Text,
                           requestedBy :Text,
                           targetSubject :Text,
                           expectedLeaseId :Text,
                           environment :BrainEnvironment,
                           ttlMs :UInt64) -> (handoff :BrainRunnerHandoff);

  acceptRunnerHandoff @9 (brain :Text,
                          targetSubject :Text,
                          handoffId :Text,
                          environment :BrainEnvironment,
                          ttlMs :UInt64) -> (lease :BrainRunnerLease);

  cancelRunnerHandoff @10 (brain :Text,
                           handoffId :Text,
                           sender :Text) -> ();

  inspectRun @11 (brain :Text, runId :Text) -> (run :BrainRun);

  cancelRun @12 (brain :Text,
                 attachmentId :Text,
                 connectionId :Text,
                 runId :Text) -> (run :BrainRun);

  # Bind an opaque frontend runner identity to this exact Cap'n Proto
  # connection before acquiring or accepting a lease. The claim disappears
  # with the connection and cannot be replayed by another local client.
  claimRunnerIdentity @13 (subject :Text) -> ();

  createSchedule @14 (brain :Text,
                      attachmentId :Text,
                      connectionId :Text,
                      language :ProgramLanguage,
                      source :Text,
                      grantCeiling :List(CapabilityRequirement),
                      nextDueMs :UInt64,
                      hasIntervalMs :Bool,
                      intervalMs :UInt64,
                      policy :BrainScheduleDeliveryPolicy) -> (schedule :BrainSchedule);

  inspectSchedule @15 (brain :Text, scheduleId :Text) ->
                      (found :Bool, schedule :BrainSchedule);

  cancelSchedule @16 (brain :Text,
                      attachmentId :Text,
                      connectionId :Text,
                      scheduleId :Text) -> (cancelled :Bool);

  # Schedule only the daemon-persisted reviewed initialization module. The
  # caller supplies neither program source nor capabilities.
  scheduleInitialization @17 (brain :Text,
                              attachmentId :Text,
                              connectionId :Text,
                              nextDueMs :UInt64) -> (schedule :BrainSchedule);
}

# ---------------------------------------------------------------------------
# Main daemon interface
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# Event bus
# ---------------------------------------------------------------------------

struct Event {
  name    @0 :Text;         # dispatch key (e.g. "peer.join", "vocab.sync")
  id      @1 :Text;         # UUID — stable across continuations
  payload @2 :AnyPointer;   # handler-specific struct; cast by name
}

# ---------------------------------------------------------------------------
# Out-of-band control messages (binary channel, not RPC)
# ---------------------------------------------------------------------------

struct ControlMessage {
  # Sent as raw Cap'n Proto bytes over the quit channel.
  # The quit watcher task decodes these independently of the event loop.
  union {
    quit      @0 :Void;   # User requested clean exit (/quit or Ctrl+D)
    interrupt @1 :Void;   # Reserved: interrupt current operation
  }
}

interface FinchDaemon {
  # Blocking (non-streaming) query.
  query @0 (messages :List(Message), tools :List(ToolDefinition))
        -> (response :QueryResponse);

  # Streaming query — server calls receiver.onChunk() for each chunk,
  # then sends a final chunk with `done` set before the method resolves.
  queryStream @1 (messages    :List(Message),
                  tools       :List(ToolDefinition),
                  receiver    :StreamReceiver) -> ();

  # Health.
  ping @2 () -> (version :Text, protocolVersion :UInt32);

  # Co-Forth: send a program, get back the stack.
  # The request IS the sentence.  The response IS the result.
  # stack: all values, bottom to top.  Top is the "return value".
  # output: anything printed by . cr etc.
  # error: non-empty if evaluation failed.
  evalForth @3 (program :Text) -> (stack :List(Int64), output :Text, error :Text);

  # Register the callback belonging to the frontend's current runner lease.
  # The durable reducible VM state is returned so a restarted frontend can
  # hydrate before accepting work. Host authority is deliberately absent.
  registerBrainRunner @4 (brain :Text, leaseId :Text, runner :BrainRunner)
      -> (runtimeRevision :UInt64, checkpoint :TypedRuntimeCheckpoint,
          control :BrainRunnerControl);

  # Return the canonical named-Brain lifecycle capability. Keeping this as a
  # capability allows later protocol evolution without adding every Brain
  # operation directly to FinchDaemon.
  brainService @5 () -> (service :BrainService);
}
