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
}

struct BrainWireMessage {
  union {
    snapshot @0 :BrainSnapshot;
    event    @1 :BrainEvent;
  }
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
}
