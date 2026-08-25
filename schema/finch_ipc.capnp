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
