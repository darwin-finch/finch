# MCP (Model Context Protocol) Architecture

**Status**: 🚧 Infrastructure complete, connection layer in progress
**Target Version**: 0.6.0
**Last Updated**: February 2026

## Overview

MCP (Model Context Protocol) is Anthropic's open standard for connecting AI assistants to external tools and data sources. Shammah's MCP integration enables dynamic tool discovery and execution from external servers without code changes.

## What is MCP?

**Model Context Protocol** allows AI assistants to:
- Connect to external tool servers (local or remote)
- Discover available tools dynamically
- Execute tools with proper parameter validation
- Handle streaming responses and complex data types

**Protocol**: JSON-RPC 2.0 over STDIO (subprocess) or HTTP+SSE (remote server)

## Architecture Design

### Client-Side Execution (Like Tool Pass-Through)

MCP tool execution happens **on the client side**, consistent with Shammah's tool pass-through architecture:

```
┌─────────────────────────────────────────────────┐
│  REPL Client (runs locally on user's machine)  │
│                                                 │
│  ┌───────────────────────────────────────────┐ │
│  │  MCP Client                               │ │
│  │  - Manages server connections             │ │
│  │  - Caches active sessions                 │ │
│  └───┬───────────────────────────────────────┘ │
│      │                                         │
│      v                                         │
│  ┌───────────────────────────────────────────┐ │
│  │  MCP Server (subprocess or remote)        │ │
│  │  - npx @modelcontextprotocol/server-*     │ │
│  │  - Custom servers (Python, Rust, etc.)    │ │
│  │  - Exposes tools via JSON-RPC             │ │
│  └───────────────────────────────────────────┘ │
└─────────────────────────────────────────────────┘

        │
        v
┌─────────────────────────────────────────────────┐
│  Daemon Server (model inference)                │
│  - Receives tool results from client            │
│  - Generates responses                          │
│  - Does NOT execute MCP tools directly          │
└─────────────────────────────────────────────────┘
```

### Why Client-Side?

1. **Security**: User controls API keys (GitHub, Slack, etc.)
2. **Filesystem Access**: MCP servers may need local file access
3. **User Approval**: User sees and approves tool usage on their machine
4. **Consistency**: Matches existing tool pass-through pattern
5. **Session Management**: MCP connections cached per client session

### Flow Diagram

```
User: "Create a GitHub issue for bug #42"
    ↓
┌─────────────────────────────────────────┐
│ 1. REPL → Daemon                        │
│    Send query with available tools      │
│    (includes MCP tool definitions)      │
└─────────────────────────────────────────┘
    ↓
┌─────────────────────────────────────────┐
│ 2. Daemon → REPL                        │
│    Returns tool_use block:              │
│    {                                    │
│      tool: "mcp_github_create_issue",   │
│      params: {                          │
│        repo: "user/repo",               │
│        title: "Bug #42",                │
│        body: "..."                      │
│      }                                  │
│    }                                    │
└─────────────────────────────────────────┘
    ↓
┌─────────────────────────────────────────┐
│ 3. REPL (Client-Side)                   │
│    a. Parse tool name: mcp_github_*     │
│    b. Connect to github MCP server      │
│       (or use cached connection)        │
│    c. Send JSON-RPC request:            │
│       {                                 │
│         method: "tools/call",           │
│         params: {                       │
│           name: "create_issue",         │
│           arguments: { ... }            │
│         }                               │
│       }                                 │
│    d. Receive JSON-RPC response         │
│    e. Format as tool result             │
└─────────────────────────────────────────┘
    ↓
┌─────────────────────────────────────────┐
│ 4. REPL → Daemon                        │
│    Send tool results in conversation    │
│    [                                    │
│      {                                  │
│        type: "tool_result",             │
│        tool_use_id: "...",              │
│        content: "Issue created: #123"   │
│      }                                  │
│    ]                                    │
└─────────────────────────────────────────┘
    ↓
┌─────────────────────────────────────────┐
│ 5. Daemon → REPL                        │
│    Final response:                      │
│    "I've created GitHub issue #123      │
│     for bug #42"                        │
└─────────────────────────────────────────┘
```

## Configuration

### Config File Format

```toml
# ~/.finch/config.toml

# Filesystem access (local subprocess)
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/finch/code"]
transport = "stdio"
enabled = true

# GitHub integration (local subprocess with API key)
[mcp_servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
transport = "stdio"
env = { GITHUB_TOKEN = "$GITHUB_TOKEN" }  # Reads from environment
enabled = true

# Postgres database (remote HTTP+SSE server)
[mcp_servers.postgres]
url = "http://localhost:3000/mcp"
transport = "sse"
enabled = false  # Disabled servers are not started

# Custom internal tools
[mcp_servers.internal_api]
command = "/usr/local/bin/internal-mcp-server"
args = ["--config", "/etc/internal/mcp.json"]
transport = "stdio"
env = { API_KEY = "$INTERNAL_API_KEY" }
enabled = true
```

### Configuration Structure

```rust
pub struct McpServerConfig {
    /// Transport type (stdio or sse)
    pub transport: TransportType,

    /// Command to execute (for STDIO)
    pub command: Option<String>,

    /// Command arguments (for STDIO)
    pub args: Vec<String>,

    /// Environment variables (for STDIO)
    pub env: HashMap<String, String>,

    /// URL (for SSE)
    pub url: Option<String>,

    /// Whether server is enabled
    pub enabled: bool,
}

pub enum TransportType {
    Stdio,  // Launch subprocess, communicate via stdin/stdout
    Sse,    // Connect to HTTP server, use Server-Sent Events
}
```

## Transport Types

### STDIO Transport (Subprocess)

**Use case**: Local MCP servers (npm packages, binaries)

**How it works**:
1. Launch subprocess with `Command::new(command).args(args).spawn()`
2. Set up stdin/stdout pipes
3. Send JSON-RPC requests to stdin
4. Read JSON-RPC responses from stdout
5. Keep process alive for session duration

**Example**:
```rust
let process = Command::new("npx")
    .args(&["-y", "@modelcontextprotocol/server-github"])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .env("GITHUB_TOKEN", token)
    .spawn()?;

// Send JSON-RPC request
let request = json!({
    "jsonrpc": "2.0",
    "id": 1,
    "method": "tools/list",
    "params": {}
});
writeln!(process.stdin, "{}", request)?;

// Read JSON-RPC response
let response: Value = serde_json::from_reader(process.stdout)?;
```

### SSE Transport (HTTP+Server-Sent Events)

**Use case**: Remote MCP servers, internal APIs

**How it works**:
1. Connect to HTTP endpoint
2. Use Server-Sent Events for server→client messages
3. Use HTTP POST for client→server requests
4. Handle reconnection on disconnect

**Example**:
```rust
let client = reqwest::Client::new();

// Connect to SSE stream
let event_source = client
    .get("http://localhost:3000/mcp/sse")
    .send()
    .await?;

// Send tool call
let response = client
    .post("http://localhost:3000/mcp/call")
    .json(&json!({
        "method": "tools/call",
        "params": {
            "name": "query_database",
            "arguments": { "sql": "SELECT * FROM users" }
        }
    }))
    .send()
    .await?;
```

## JSON-RPC Protocol

### Request Format

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/list",
  "params": {}
}
```

### Response Format

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "tools": [
      {
        "name": "create_issue",
        "description": "Create a GitHub issue",
        "inputSchema": {
          "type": "object",
          "properties": {
            "repo": { "type": "string" },
            "title": { "type": "string" },
            "body": { "type": "string" }
          },
          "required": ["repo", "title"]
        }
      }
    ]
  }
}
```

### Tool Call Request

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "tools/call",
  "params": {
    "name": "create_issue",
    "arguments": {
      "repo": "user/repo",
      "title": "Bug in login",
      "body": "Steps to reproduce..."
    }
  }
}
```

### Tool Call Response

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "Created issue #123: https://github.com/user/repo/issues/123"
      }
    ]
  }
}
```

## Implementation Status

### ✅ Complete

1. **Configuration System** (`src/tools/mcp/config.rs`)
   - `McpServerConfig` struct
   - `TransportType` enum
   - Validation logic
   - Full test coverage (8 tests)

2. **Config Integration**
   - `Config.mcp_servers` field
   - TOML serialization/deserialization
   - Config loading in `loader.rs`

3. **Module Structure**
   - `src/tools/mcp/mod.rs` - Public API
   - `src/tools/mcp/config.rs` - Configuration types
   - `src/tools/mcp/connection.rs` - Connection wrapper (partial)
   - `src/tools/mcp/client.rs` - Client coordinator (partial)

4. **Dependencies**
   - `rust-mcp-sdk` v0.8.3 added
   - Compiles successfully

### 🚧 In Progress

1. **Connection Layer** (`src/tools/mcp/connection.rs`)
   - **Blocker**: `rust-mcp-sdk` has private `ClientRuntime` type
   - Can't store client runtime in struct
   - Options:
     - Implement JSON-RPC directly (recommended)
     - Use type erasure (`Box<dyn Any>`)
     - Wait for SDK improvements

2. **Client Coordinator** (`src/tools/mcp/client.rs`)
   - Depends on connection layer
   - Design complete, implementation blocked

### ❌ Not Started

1. **Tool Executor Integration**
   - Add MCP tools to `list_available_tools()`
   - Route `mcp_*` tool calls to MCP client
   - Handle pass-through from daemon→client

2. **Setup Wizard MCP Section**
   - List configured servers
   - Add/remove/edit servers
   - Test connections
   - Enable/disable servers

3. **REPL Commands**
   - `/mcp list` - Show configured servers
   - `/mcp enable <name>` - Enable server
   - `/mcp disable <name>` - Disable server
   - `/mcp reload` - Reconnect to all servers
   - `/mcp test <name>` - Test server connection

4. **SSE Transport**
   - HTTP client
   - Server-Sent Events parsing
   - Reconnection logic

5. **Integration Tests**
   - STDIO transport test with mock server
   - Tool discovery test
   - Tool execution test
   - Error handling tests

## Next Steps

### 1. Complete Connection Layer (8-10 hours)

Implement JSON-RPC 2.0 directly instead of using rust-mcp-sdk:

```rust
// src/tools/mcp/connection.rs
pub struct McpConnection {
    name: String,
    process: Child,  // For STDIO
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    request_id: AtomicU64,
    tools: Vec<Tool>,
}

impl McpConnection {
    pub async fn connect_stdio(
        name: String,
        config: &McpServerConfig,
    ) -> Result<Self> {
        // Launch subprocess
        // Send initialize request
        // Discover tools
        // Return connection
    }

    pub async fn call_tool(
        &mut self,
        tool_name: &str,
        params: Value,
    ) -> Result<String> {
        // Send JSON-RPC request
        // Parse response
        // Return result
    }
}
```

### 2. Integrate with ToolExecutor (2-3 hours)

```rust
// src/tools/executor.rs
impl ToolExecutor {
    pub fn list_available_tools(&self) -> Vec<ToolDefinition> {
        let mut tools = self.builtin_tools();

        // Add MCP tools with "mcp_<server>_<tool>" prefix
        if let Some(mcp) = &self.mcp_client {
            tools.extend(mcp.list_tools());
        }

        tools
    }

    pub async fn execute(&mut self, tool_name: &str, params: Value) -> Result<ToolResult> {
        // Try built-in tools
        if let Some(tool) = self.builtin_tools.get(tool_name) {
            return tool.execute(params).await;
        }

        // Try MCP tools
        if tool_name.starts_with("mcp_") {
            if let Some(mcp) = &mut self.mcp_client {
                return mcp.execute_tool(tool_name, params).await;
            }
        }

        Err(anyhow!("Unknown tool: {}", tool_name))
    }
}
```

### 3. Add Setup Wizard Section (2-3 hours)

```rust
// src/cli/setup_wizard.rs
WizardSection::McpServers => {
    // List configured servers
    // Add new server with form:
    //   - Name
    //   - Transport (STDIO/SSE)
    //   - Command + args (STDIO)
    //   - URL (SSE)
    //   - Environment variables
    //   - Enabled checkbox
    // Edit/delete existing servers
}
```

### 4. Add REPL Commands (1-2 hours)

```rust
// src/cli/repl.rs
match input.trim() {
    "/mcp list" => {
        for (name, config) in &self.config.mcp_servers {
            println!("{}: {} ({})",
                name,
                config.command.as_ref().unwrap_or(&config.url.as_ref().unwrap()),
                if config.enabled { "enabled" } else { "disabled" }
            );
        }
    }
    "/mcp reload" => {
        self.mcp_client = McpClient::from_config(&self.config).await?;
        println!("Reconnected to MCP servers");
    }
    // ... more commands
}
```

### 5. Test Coverage (2-3 hours)

```rust
// tests/mcp_integration_test.rs
#[tokio::test]
async fn test_stdio_connection() {
    // Create mock MCP server
    // Connect via STDIO
    // Verify tools discovered
}

#[tokio::test]
async fn test_tool_execution() {
    // Connect to mock server
    // Call tool
    // Verify result
}

#[tokio::test]
async fn test_error_handling() {
    // Test connection failures
    // Test tool call failures
    // Test invalid responses
}
```

## Example Usage

Once complete:

```bash
# Configure MCP server
cat >> ~/.finch/config.toml << EOF
[mcp_servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
transport = "stdio"
env = { GITHUB_TOKEN = "$GITHUB_TOKEN" }
enabled = true
EOF

# Use in REPL
$ finch
> /mcp list
github: npx -y @modelcontextprotocol/server-github (enabled)

> Create a GitHub issue in my repo titled "Add dark mode"
[Connects to GitHub MCP server]
[Calls create_issue tool]
Created issue #42: https://github.com/user/repo/issues/42

> List all open issues
[Calls list_issues tool]
Found 5 open issues:
- #42: Add dark mode
- #41: Fix login bug
...
```

## References

- **MCP Specification**: https://modelcontextprotocol.io/specification/2025-11-25/
- **MCP Servers**: https://github.com/modelcontextprotocol/servers
- **rust-mcp-sdk**: https://docs.rs/rust-mcp-sdk/0.8.3/
- **JSON-RPC 2.0**: https://www.jsonrpc.org/specification

## Related Documentation

- `docs/ARCHITECTURE.md` - Overall system architecture
- `docs/TOOL_CONFIRMATION.md` - Tool permission system
- `docs/DAEMON_MODE.md` - Daemon architecture details
- `docs/PHASE_4_MCP_PARTIAL.md` - Implementation status and challenges
