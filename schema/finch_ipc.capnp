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
  inputJson @2 :Text;  # JSON-encoded input
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
  inputJson @2 :Text;
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
  }
}

struct UsageUpdate {
  inputTokens  @0 :UInt32;
  outputTokens @1 :UInt32;
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

struct BrainProgramRequest {
  brain      @0 :Text;
  requestSeq @1 :UInt64;
  language   @2 :ProgramLanguage;
  source     @3 :Text;
  runId      @4 :Text;
}

struct BrainProgramResult {
  output          @0 :Text;
  runtimeRevision @1 :UInt64;
  checkpointJson  @2 :Data; # Transitional typed checkpoint payload; schema becomes native later.
  error           @3 :Text;
}

struct BrainTurnRequest {
  brain       @0 :Text;
  requestSeq  @1 :UInt64;
  prompt      @2 :Text;
  contextJson @3 :Data; # Transitional canonical Message list; schema becomes native later.
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
  checkpointJson  @4 :Data; # Transitional typed checkpoint payload; schema becomes native later.
  error           @5 :Text;
  turnEvents      @6 :List(BrainTurnEvent);
}

enum BrainTurnEventKind {
  call              @0;
  result            @1;
  approvalRequested @2;
  approvalDecided   @3;
}

# Ordered provider/runner lifecycle produced while servicing one Brain prompt.
# Arbitrary provider arguments and policy details remain JSON values at their
# respective boundaries, while the event envelope and ordering are typed.
struct BrainTurnEvent {
  kind         @0 :BrainTurnEventKind;
  toolId       @1 :Text;
  name         @2 :Text; # Present for call events.
  inputJson    @3 :Data; # Present for call events.
  output       @4 :Text; # Present for result events.
  isError      @5 :Bool; # Present for result events.
  approvalId   @6 :Text;
  approvalKind @7 :Text; # "tool" or "vm_capability".
  subject      @8 :Text; # Tool name or capability name.
  detailJson   @9 :Data; # Present for approval requests.
  decisionJson @10 :Data; # Present for approval decisions.
  approvalAudience @11 :BrainApprovalAudience; # Present for approval requests.
}

interface BrainRunner {
  runProgram @0 (request :BrainProgramRequest) -> (result :BrainProgramResult);
  runTurn    @1 (request :BrainTurnRequest) -> (result :BrainTurnResult);
  cancelRun  @2 (brain :Text, runId :Text) -> (cancelled :Bool, error :Text);
}

# Per-turn reverse capability. The runner publishes an addressed approval and
# suspends until the daemon returns the decision submitted by that attachment.
interface BrainTurnControl {
  requestApproval @0 (event :BrainTurnEvent) -> (decisionJson :Data);
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
  inputJson  @3 :Data;
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
  detailJson      @6 :Data;
}

struct BrainApprovalDecided {
  requestSeq  @0 :UInt64;
  approvalId  @1 :Text;
  decisionJson @2 :Data;
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

struct BrainEvent {
  schemaVersion        @0 :UInt32;
  brainId              @1 :Text;
  seq                  @2 :UInt64;
  environmentGeneration @3 :UInt64;
  sender               @4 :Text;
  createdMs            @5 :UInt64;
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
  }
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
  }
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
  ping @2 () -> (version :Text);

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
      -> (runtimeRevision :UInt64, checkpointJson :Data);

  # Return the canonical named-Brain lifecycle capability. Keeping this as a
  # capability allows later protocol evolution without adding every Brain
  # operation directly to FinchDaemon.
  brainService @5 () -> (service :BrainService);
}
