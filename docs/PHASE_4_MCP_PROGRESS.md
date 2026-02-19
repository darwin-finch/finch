# Phase 4: MCP Plugin System - In Progress

**Date**: 2026-02-18
**Status**: 🚧 Core Implementation Complete, Integration Remaining
**Progress**: 60% (JSON-RPC STDIO complete)

## What Has Been Implemented

### ✅ Complete: JSON-RPC STDIO Transport

**File**: `src/tools/mcp/connection.rs`

**Implementation**:
- Direct JSON-RPC 2.0 over STDIO (bypassed rust-mcp-sdk private type issue)
- Process spawning with tokio (`tokio::process::Command`)
- Async communication using tokio IO (AsyncBufReadExt, AsyncWriteExt)
- Full MCP protocol support:
  - `initialize` - Connect and initialize
  - `tools/list` - Discover available tools
  - `tools/call` - Execute tools
  - `notifications/initialized` - Send ready notification

**Key Features**:
```rust
pub struct McpConnection {
    name: String,
    config: McpServerConfig,
    tools: Vec<McpTool>,          // Cached tool list
    server_info: Option<McpServerInfo>,
    is_connected: bool,
    child: Option<TokioChild>,     // Process handle
    stdin: Arc<Mutex<TokioChildStdin>>,   // JSON-RPC requests
    stdout: Arc<Mutex<BufReader<TokioChildStdout>>>, // JSON-RPC responses
    next_id: Arc<AtomicU64>,      // Request ID counter
}
```

**Methods**:
- `connect()` - Spawn MCP server process, initialize, discover tools
- `list_tools()` - Get cached tool list
- `refresh_tools()` - Re-query tools from server
- `call_tool()` - Execute a tool with JSON parameters
- `shutdown()` - Kill server process gracefully

**JSON-RPC Protocol**:
```rust
// Request format
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "read_file",
    "arguments": {"path": "/etc/hosts"}
  }
}

// Response format
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [
      {"type": "text", "text": "file contents..."}
    ]
  }
}
```

### ✅ Complete: MCP Client Coordinator

**File**: `src/tools/mcp/client.rs`

**Implementation**:
- Manages multiple MCP server connections concurrently
- Tool name prefixing to avoid conflicts: `mcp_<server>_<tool>`
- Graceful connection failures (logs warnings, continues with other servers)
- Schema conversion from MCP format to our ToolInputSchema

**Key Features**:
```rust
pub struct McpClient {
    connections: Arc<RwLock<HashMap<String, Arc<RwLock<McpConnection>>>>>,
}
```

**Methods**:
- `from_config()` - Connect to all enabled MCP servers
- `list_tools()` - Aggregate tools from all servers
- `execute_tool()` - Route tool call to appropriate server
- `refresh_all_tools()` - Re-query all servers
- `list_servers()` - Get connected server names
- `disconnect()` / `disconnect_all()` - Shutdown connections

**Tool Name Prefixing**:
```
Server: "filesystem"
Tool: "read_file"
Prefixed: "mcp_filesystem_read_file"
```

### ✅ Complete: Configuration (Already Done)

**File**: `src/tools/mcp/config.rs`

**TOML Format**:
```toml
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/finch"]
transport = "stdio"
enabled = true
env = { }

[mcp_servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
transport = "stdio"
enabled = true
env = { GITHUB_TOKEN = "$GITHUB_TOKEN" }
```

### ✅ Complete: Testing

**Test Coverage**:
- ✅ JSON-RPC serialization/deserialization
- ✅ Error handling for invalid configs
- ✅ Client creation and lifecycle
- ✅ Config validation
- ✅ Schema conversion
- ✅ Tool name prefixing

**Test Results**:
```bash
cargo test --lib tools::mcp
# Result: 16 passed, 0 failed
```

**All Tests**:
```bash
cargo test --lib
# Result: 353 passed, 0 failed, 11 ignored
```

## What Remains To Do

### 🚧 Step 1: Tool Executor Integration (2-3 hours)

**File**: `src/tools/executor.rs`

**Tasks**:
1. Add `mcp_client: Option<Arc<McpClient>>` field to ToolExecutor
2. Add `with_mcp()` method to create MCP client from config
3. Update `list_available_tools()` to include MCP tools
4. Update `execute()` to route MCP tools to MCP client
5. Handle tool name prefixing/unprefixing

**Example**:
```rust
impl ToolExecutor {
    pub async fn with_mcp(mut self, config: &Config) -> Result<Self> {
        if !config.mcp_servers.is_empty() {
            let mcp_client = McpClient::from_config(&config.mcp_servers).await?;
            self.mcp_client = Some(Arc::new(mcp_client));
        }
        Ok(self)
    }

    pub async fn execute(&mut self, tool_name: &str, params: Value) -> Result<String> {
        // Try built-in tools first
        if let Some(tool) = self.tools.get(tool_name) {
            return tool.execute(params, &self.context).await;
        }

        // Try MCP tools
        if tool_name.starts_with("mcp_") {
            if let Some(mcp) = &self.mcp_client {
                return mcp.execute_tool(tool_name, params).await;
            }
        }

        anyhow::bail!("Unknown tool: {}", tool_name)
    }
}
```

### 🚧 Step 2: REPL Integration (1-2 hours)

**File**: `src/cli/repl.rs`

**Tasks**:
1. Create MCP client in REPL initialization
2. Add to ToolExecutor via `with_mcp()`
3. Add MCP tools to available tools list

**Code**:
```rust
// In create_repl():
let mut executor = ToolExecutor::new()
    .with_permission_manager(permission_manager)
    .with_context(tool_context);

// Add MCP support
if !config.mcp_servers.is_empty() {
    executor = executor.with_mcp(&config).await?;
    output_status!("MCP: Loaded {} servers", config.mcp_servers.len());
}
```

### 🚧 Step 3: REPL /mcp Commands (1-2 hours)

**File**: `src/cli/repl.rs`

**Commands to Add**:
```
/mcp list              # List connected MCP servers
/mcp tools <server>    # List tools from a specific server
/mcp refresh           # Refresh tools from all servers
/mcp enable <server>   # Enable a disabled server
/mcp disable <server>  # Disable an enabled server
/mcp reload            # Reconnect to all servers
```

**Implementation**:
```rust
match input.trim() {
    "/mcp list" => {
        if let Some(mcp) = &executor.mcp_client {
            let servers = mcp.list_servers().await;
            for name in servers {
                println!("  • {}", name);
            }
        } else {
            println!("No MCP servers configured");
        }
    }
    // ... other commands
}
```

### 🚧 Step 4: Setup Wizard MCP Section (2-3 hours)

**File**: `src/cli/setup_wizard.rs`

**Tasks**:
1. Add `WizardSection::McpServers` variant
2. Add `SectionState::McpServers` with server list
3. Implement add/edit/remove server UI
4. Save to config.toml

**UI Layout**:
```
┌─────────────────────────────────────────┐
│ MCP Servers                             │
├─────────────────────────────────────────┤
│                                         │
│  Configured servers:                    │
│                                         │
│  1. filesystem (enabled)                │
│     Command: npx -y @modelcon...        │
│     Transport: STDIO                    │
│                                         │
│  2. github (disabled)                   │
│     Command: npx -y @modelcon...        │
│     Transport: STDIO                    │
│                                         │
│  [a] Add server                         │
│  [e] Edit selected                      │
│  [d] Delete selected                    │
│  [Enter] Continue                       │
└─────────────────────────────────────────┘
```

### 🚧 Step 5: Integration Testing (2-3 hours)

**Test with Real MCP Server**:

1. Install filesystem MCP server:
```bash
npm install -g @modelcontextprotocol/server-filesystem
```

2. Configure in `~/.finch/config.toml`:
```toml
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
transport = "stdio"
enabled = true
```

3. Test in REPL:
```bash
cargo run
> /mcp list
# Should show: filesystem

> Use mcp_filesystem_list_directory to list files in /tmp
# Should execute and show directory contents
```

4. Test error handling:
```bash
# Add invalid server to config
[mcp_servers.broken]
command = "nonexistent_command"
transport = "stdio"
enabled = true

cargo run
# Should log warning but still start successfully
```

### 🚧 Step 6: Documentation (1-2 hours)

**Create**: `docs/MCP_USER_GUIDE.md`

**Contents**:
- What is MCP?
- How to install MCP servers
- How to configure servers in Shammah
- Examples of popular MCP servers
- Troubleshooting guide

**Popular MCP Servers**:
```bash
# Filesystem
npm install -g @modelcontextprotocol/server-filesystem

# GitHub
npm install -g @modelcontextprotocol/server-github

# PostgreSQL
npm install -g @modelcontextprotocol/server-postgres

# Google Drive
npm install -g @modelcontextprotocol/server-gdrive

# Slack
npm install -g @modelcontextprotocol/server-slack
```

## Progress Tracker

| Task | Status | Estimated | Actual |
|------|--------|-----------|--------|
| JSON-RPC STDIO Transport | ✅ Complete | 2-3h | 2h |
| MCP Client Coordinator | ✅ Complete | 1-2h | 1h |
| Configuration Types | ✅ Complete | 1h | Done earlier |
| Tool Executor Integration | ⏳ Next | 2-3h | - |
| REPL Integration | ⏳ Todo | 1-2h | - |
| REPL /mcp Commands | ⏳ Todo | 1-2h | - |
| Setup Wizard Section | ⏳ Todo | 2-3h | - |
| Integration Testing | ⏳ Todo | 2-3h | - |
| Documentation | ⏳ Todo | 1-2h | - |

**Overall**: 60% complete (6h of 10-16h estimated)

## Technical Decisions

### Why Direct JSON-RPC Instead of rust-mcp-sdk?

**Problem**: rust-mcp-sdk's `ClientRuntime` type is private
**Solution**: Implement JSON-RPC 2.0 directly

**Benefits**:
- ✅ Full control over types and lifetimes
- ✅ No dependency on unstable SDK internals
- ✅ Simpler implementation (~300 lines vs. SDK complexity)
- ✅ Easy to debug and maintain
- ✅ No blocking on SDK API improvements

**Trade-offs**:
- ⚠️ Need to maintain JSON-RPC protocol ourselves
- ⚠️ Missing some advanced MCP features (resources, prompts)
- ✅ But: Can add these later as needed

### Architecture Choices

**Process Management**: tokio::process instead of std::process
- Reason: Async/await support, better integration with async runtime

**Communication**: Line-delimited JSON over STDIO
- Reason: Simple, standard JSON-RPC 2.0 format

**Tool Naming**: Prefix with `mcp_<server>_<tool>`
- Reason: Avoids conflicts between servers and built-in tools
- Example: `mcp_filesystem_read_file`

**Error Handling**: Graceful degradation
- Reason: One failing server shouldn't break all MCP functionality
- Implementation: Log warnings, continue with other servers

## Known Limitations

1. **STDIO Only** - SSE transport not yet implemented
   - Future: Add HTTP+SSE support for remote servers

2. **No Resources/Prompts** - Only tools supported
   - MCP protocol includes resources (URIs) and prompts (templates)
   - Future: Add if users request these features

3. **No Sampling** - Direct tool execution only
   - MCP has sampling API for multi-turn LLM interactions
   - Not needed for our use case (we have our own LLM)

4. **No Server Management UI** - Config file only
   - Setup wizard will add UI (Step 4)

5. **Node.js Dependency** - Most MCP servers use npm
   - Acceptable: Node.js widely installed
   - Alternative: Rust-based MCP servers also work

## Files Modified/Created

| File | Status | Description |
|------|--------|-------------|
| `src/tools/mcp/connection.rs` | ✅ Complete | JSON-RPC STDIO transport |
| `src/tools/mcp/client.rs` | ✅ Complete | Multi-server coordinator |
| `src/tools/mcp/config.rs` | ✅ Complete | Configuration types |
| `src/tools/mcp/mod.rs` | ✅ Complete | Module exports |
| `src/tools/executor.rs` | ⏳ Next | Add MCP integration |
| `src/cli/repl.rs` | ⏳ Next | MCP client + commands |
| `src/cli/setup_wizard.rs` | ⏳ Todo | MCP servers section |
| `docs/MCP_USER_GUIDE.md` | ⏳ Todo | User documentation |

## Next Steps

1. **Integrate with ToolExecutor** (2-3 hours)
2. **Add REPL commands** (1-2 hours)
3. **Test with real MCP server** (1 hour)
4. **Add setup wizard section** (2-3 hours)
5. **Write documentation** (1-2 hours)

**Estimated time to completion**: 6-10 hours

## References

- MCP Specification: https://modelcontextprotocol.io/specification/2025-11-25/
- JSON-RPC 2.0 Spec: https://www.jsonrpc.org/specification
- MCP Servers List: https://github.com/modelcontextprotocol/servers
- rust-mcp-sdk: https://docs.rs/rust-mcp-sdk/0.8.3/
