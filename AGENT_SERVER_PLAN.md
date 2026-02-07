# Agent Server Plan - VS Code Integration

**Goal:** Enable VS Code to connect to Shammah's agent server and use Qwen for local code generation.

## Current State

### What Works ✅
- HTTP server with Axum (`src/server/`)
- Claude-compatible `/v1/messages` endpoint
- Session management for multi-turn conversations
- Router integration (routes queries)
- Metrics logging
- Tracing integration (Phase 3.5 Part 2)

### What's Missing ❌
1. **No Qwen/LocalGenerator in daemon mode**
   - `run_daemon()` doesn't initialize BootstrapLoader
   - `run_daemon()` doesn't create LocalGenerator
   - Server has no access to Qwen model
   - Line 113-120 in handlers.rs: "TODO: Use actual local generator"

2. **No tool execution in server mode**
   - Server doesn't initialize ToolExecutor
   - No tool definitions sent to Claude
   - No multi-turn tool execution loop

3. **No streaming support**
   - VS Code expects streaming responses
   - Current implementation is request/response only

4. **VS Code API compatibility unclear**
   - Need to verify Claude API format works with VS Code
   - May need adjustments to request/response format

## Implementation Plan

### Phase 1: Add Qwen/LocalGenerator to Server ✅ (High Priority)

**Goal:** Make daemon mode initialize and use Qwen model like REPL mode does.

#### Part A: Initialize Qwen in daemon mode
- [ ] Update `run_daemon()` in `src/main.rs`:
  - [ ] Add BootstrapLoader initialization
  - [ ] Add GeneratorState with Arc<RwLock<>>
  - [ ] Spawn background model loading task
  - [ ] Add tokenizer initialization
  - [ ] Create LocalGenerator with Qwen injection
  - [ ] Add LoRA TrainingCoordinator
  - [ ] Add Sampler for weighted training

#### Part B: Pass LocalGenerator to AgentServer
- [ ] Update `AgentServer::new()` signature:
  - [ ] Add `local_generator: Arc<RwLock<LocalGenerator>>` parameter
  - [ ] Add `bootstrap_loader: Arc<BootstrapLoader>` parameter
  - [ ] Store as fields on AgentServer

#### Part C: Use LocalGenerator in handlers
- [ ] Update `handle_message()` in `src/server/handlers.rs`:
  - [ ] Replace TODO on line 113 with actual local generation
  - [ ] Check generator state (Ready vs Loading)
  - [ ] If Ready: use `local_generator.generate()`
  - [ ] If Loading: fall back to Claude with status
  - [ ] Add error handling for generation failures

#### Part D: Add progressive bootstrap status
- [ ] Add `/v1/status` endpoint:
  - [ ] Return generator state (Initializing/Downloading/Loading/Ready/Failed)
  - [ ] Return download progress if downloading
  - [ ] Return model info when ready
- [ ] Useful for debugging and monitoring

**Testing:**
```bash
# Start server
cargo run -- daemon --bind 127.0.0.1:8000

# Check status
curl http://localhost:8000/v1/status

# Send test query
curl -X POST http://localhost:8000/v1/messages \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen-2.5-3b",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'
```

---

### Phase 2: Add Tool Execution Support ✅ (High Priority)

**Goal:** Enable server to execute tools like read, glob, grep for code understanding.

#### Part A: Initialize ToolExecutor in daemon mode
- [ ] Update `run_daemon()`:
  - [ ] Add ToolRegistry initialization
  - [ ] Register all tools (Read, Glob, Grep, Bash, etc.)
  - [ ] Create PermissionManager
  - [ ] Create ToolExecutor
  - [ ] Pass to AgentServer

#### Part B: Add tool support to AgentServer
- [ ] Update `AgentServer` struct:
  - [ ] Add `tool_executor: Arc<RwLock<ToolExecutor>>` field
  - [ ] Add `tool_definitions: Vec<ToolDefinition>` field

#### Part C: Implement tool execution in handlers
- [ ] Update `handle_message()`:
  - [ ] Send tool definitions with Claude request
  - [ ] Parse tool_use blocks from response
  - [ ] Execute tools via ToolExecutor
  - [ ] Handle multi-turn tool loops (max 10 turns)
  - [ ] Add tool results to conversation

#### Part D: Tool confirmation for server mode
- [ ] Decision: Auto-approve tools in server mode
  - [ ] No user present to confirm
  - [ ] Use saved patterns from REPL mode
  - [ ] Log all tool executions for audit
- [ ] OR: Provide webhook for approval
  - [ ] POST to configured URL with tool details
  - [ ] Wait for approval response
  - [ ] Timeout after 30s

**Testing:**
```bash
curl -X POST http://localhost:8000/v1/messages \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen-2.5-3b",
    "messages": [{
      "role": "user",
      "content": "Read the file README.md and summarize it"
    }]
  }'
```

---

### Phase 3: Add Streaming Support ✅ (High Priority for VS Code)

**Goal:** Stream responses chunk-by-chunk for better UX in VS Code.

#### Part A: Add streaming endpoint
- [ ] Add `/v1/messages/stream` endpoint:
  - [ ] Same request format as `/v1/messages`
  - [ ] Return Server-Sent Events (SSE)
  - [ ] Stream chunks as they arrive from Claude
  - [ ] Stream Qwen generation token-by-token

#### Part B: Update local generation for streaming
- [ ] Update LocalGenerator API:
  - [ ] Add `generate_stream()` method
  - [ ] Return `Stream<Item = String>`
  - [ ] Yield tokens as generated

#### Part C: VS Code compatibility
- [ ] Check if VS Code expects:
  - [ ] OpenAI format: `data: {...}\n\n`
  - [ ] Claude format: different structure
  - [ ] Custom format: define our own
- [ ] Test with actual VS Code extension

**SSE Response Format:**
```
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" there"}}

data: {"type":"content_block_stop","index":0}

data: {"type":"message_stop"}
```

---

### Phase 4: VS Code Extension Configuration ✅ (Medium Priority)

**Goal:** Configure VS Code to use Shammah server.

#### Option A: Use existing Claude/OpenAI extension
- [ ] Research which extension to use:
  - [ ] Continue (supports custom servers)
  - [ ] Cody (Sourcegraph)
  - [ ] Cursor (if it supports external servers)
  - [ ] Claude Dev extension

#### Option B: Create custom extension (future)
- [ ] VS Code extension in TypeScript
- [ ] Call Shammah server directly
- [ ] Custom UI for model selection
- [ ] Show training stats

#### VS Code Configuration Example:
```json
{
  "continue.apiUrl": "http://localhost:8000/v1/messages",
  "continue.apiKey": "not-required",
  "continue.model": "qwen-2.5-3b"
}
```

---

### Phase 5: Testing & Validation ✅

#### Manual Testing
- [ ] Start server: `cargo run -- daemon`
- [ ] Send simple query with curl
- [ ] Verify Qwen responds (not Claude fallback)
- [ ] Send query requiring tool use
- [ ] Verify tools execute correctly
- [ ] Test streaming endpoint
- [ ] Test multi-turn conversation
- [ ] Test session management

#### VS Code Integration Testing
- [ ] Install Continue extension (or chosen extension)
- [ ] Configure to point to localhost:8000
- [ ] Test simple code generation
- [ ] Test code understanding (requires tools)
- [ ] Test multi-file changes
- [ ] Test conversation history

#### Load Testing
- [ ] Use `wrk` or `ab` for load testing
- [ ] Test 10 concurrent sessions
- [ ] Monitor memory usage
- [ ] Check for memory leaks
- [ ] Verify session cleanup

---

## Success Criteria

✅ **Phase 1 Complete:**
- [ ] Daemon mode starts successfully
- [ ] Qwen model loads in background
- [ ] Status endpoint shows model state
- [ ] Local queries use Qwen (not Claude)
- [ ] Response quality is good

✅ **Phase 2 Complete:**
- [ ] Tools are registered
- [ ] Tool-requiring queries execute tools
- [ ] Multi-turn tool loops work
- [ ] Tool results are correct

✅ **Phase 3 Complete:**
- [ ] Streaming endpoint responds
- [ ] Chunks arrive incrementally
- [ ] No buffering delays
- [ ] Stream closes properly

✅ **Phase 4 Complete:**
- [ ] VS Code connects to server
- [ ] Code generation works
- [ ] Code understanding works
- [ ] Multi-turn conversations work

---

## Non-Goals (Out of Scope)

- ❌ Authentication (use for local dev only, or add later)
- ❌ Multi-user support (single developer use case)
- ❌ Distributed deployment (run locally)
- ❌ GPU support (Qwen runs on CPU/Metal)
- ❌ Custom VS Code extension (use existing for now)

---

## Architecture Diagram (Target)

```
┌─────────────────────────────────────────────────────┐
│                   VS Code Editor                    │
│  ┌───────────────────────────────────────────────┐  │
│  │  Continue Extension (or similar)              │  │
│  │  - Code generation requests                   │  │
│  │  - Streaming responses                        │  │
│  │  - Multi-turn conversations                   │  │
│  └─────────────────┬───────────────────────────┬─┘  │
│                    │ HTTP                      │    │
└────────────────────┼───────────────────────────┼────┘
                     │                           │
                     v                           v
┌─────────────────────────────────────────────────────┐
│        Shammah Agent Server (daemon mode)           │
│  ┌─────────────────────────────────────────────┐    │
│  │  HTTP Endpoints (Axum)                      │    │
│  │  - POST /v1/messages (request/response)     │    │
│  │  - POST /v1/messages/stream (SSE)           │    │
│  │  - GET /v1/status (model status)            │    │
│  │  - GET /health (health check)               │    │
│  └──────────────┬──────────────────────────────┘    │
│                 │                                    │
│  ┌──────────────v──────────────────────────────┐    │
│  │  Session Manager                            │    │
│  │  - Per-session conversation history         │    │
│  │  - Session timeout (30 min)                 │    │
│  └──────────────┬──────────────────────────────┘    │
│                 │                                    │
│  ┌──────────────v──────────────────────────────┐    │
│  │  Router                                     │    │
│  │  - Crisis detection                         │    │
│  │  - Forward reason analysis                  │    │
│  │  - Local vs Claude decision                 │    │
│  └──────┬──────────────────┬───────────────────┘    │
│         │                  │                        │
│         v                  v                        │
│  ┌─────────────┐    ┌──────────────────┐           │
│  │  Qwen 3B    │    │  Claude API      │           │
│  │  (Local)    │    │  (Fallback)      │           │
│  │  + LoRA     │    │                  │           │
│  └──────┬──────┘    └──────────────────┘           │
│         │                                           │
│  ┌──────v──────────────────────────────┐           │
│  │  Tool Executor                      │           │
│  │  - Read, Glob, Grep, Bash          │           │
│  │  - Auto-approve with saved patterns│           │
│  └─────────────────────────────────────┘           │
└─────────────────────────────────────────────────────┘
```

---

## Timeline Estimate

- **Phase 1** (Qwen integration): 3-4 hours
- **Phase 2** (Tool execution): 2-3 hours
- **Phase 3** (Streaming): 2-3 hours
- **Phase 4** (VS Code setup): 1-2 hours
- **Phase 5** (Testing): 2-3 hours

**Total**: 10-15 hours (2 full work days)

---

## Next Steps

1. Complete Phase 3.5 Part 3 output refactoring (paused)
2. Start Agent Server Phase 1: Add Qwen to daemon mode
3. Test with curl to verify local generation works
4. Continue with Phase 2-5 sequentially
5. Resume Phase 3.5 Part 3 once server is working

---

## Notes

- Agent server work takes priority over completing output refactoring
- Output refactoring (195 calls remaining) can be done incrementally
- Server mode doesn't need TUI, but needs output routing for logging
- Tracing integration (Phase 3.5 Part 2) already works in daemon mode ✅
