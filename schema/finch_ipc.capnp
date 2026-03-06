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
# Brain sessions
# ---------------------------------------------------------------------------

enum BrainState {
  running          @0;
  waitingForInput  @1;
  planReady        @2;
  completed        @3;
  failed           @4;
  cancelled        @5;
}

struct BrainSummary {
  id              @0 :Text;
  name            @1 :Text;
  task            @2 :Text;
  state           @3 :BrainState;
  ageSecs         @4 :UInt64;
}

struct BrainDetails {
  id              @0 :Text;
  name            @1 :Text;
  task            @2 :Text;
  state           @3 :BrainState;
  question        @4 :Text;               # non-empty when waitingForInput
  questionOptions @5 :List(Text);
  plan            @6 :Text;               # non-empty when planReady
  result          @7 :Text;               # non-empty when completed
  errorMsg        @8 :Text;               # non-empty when failed
  eventLog        @9 :List(Text);
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
# Main daemon interface
# ---------------------------------------------------------------------------

interface FinchDaemon {
  # Blocking (non-streaming) query.
  query @0 (messages :List(Message), tools :List(ToolDefinition))
        -> (response :QueryResponse);

  # Streaming query — server calls receiver.onChunk() for each chunk,
  # then sends a final chunk with `done` set before the method resolves.
  queryStream @1 (messages    :List(Message),
                  tools       :List(ToolDefinition),
                  receiver    :StreamReceiver) -> ();

  # Brain session management.
  spawnBrain           @2 (taskDescription :Text, provider :Text)  -> (id :Text);
  listBrains           @3 ()                                        -> (brains :List(BrainSummary));
  getBrain             @4 (id :Text)                                -> (details :BrainDetails);
  answerBrainQuestion  @5 (id :Text, answer :Text)                  -> ();
  respondToBrainPlan   @6 (id :Text, approved :Bool, instruction :Text) -> ();
  cancelBrain          @7 (id :Text)                                -> ();

  # Health.
  ping @8 () -> (version :Text);
}
