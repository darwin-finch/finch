# Phase 4: MCP Plugin System (Partial Implementation)

**Status**: Infrastructure created, needs API refinement

## What Was Implemented

### ✅ Complete
1. **MCP Configuration Types** (`src/tools/mcp/config.rs`)
   - `McpServerConfig` struct with STDIO and SSE transport types
   - Configuration validation
   - Full test coverage

2. **MCP Module Structure** (`src/tools/mcp/mod.rs`)
   - Module exports and documentation
   - Clean public API design

3. **Configuration Integration**
   - Added `mcp_servers` field to `Config` struct
   - Updated TOML loading in `loader.rs`
   - Configuration migration support

4. **Dependencies**
   - Added `rust-mcp-sdk` v0.8.3 with client, stdio, sse features
   - Dependency compiles successfully

### 🚧 Partial
1. **MCP Connection** (`src/tools/mcp/connection.rs`)
   - Basic structure implemented
   - Correct API calls identified from examples
   - **Issue**: `ClientRuntime` type is private in rust-mcp-sdk
   - **Blocker**: Cannot store client runtime due to type visibility

2. **MCP Client** (`src/tools/mcp/client.rs`)
   - Coordinator structure designed
   - Tool listing and execution patterns defined
   - **Issue**: Depends on connection module completion

### ❌ Not Started
1. Tool Executor Integration
2. Setup Wizard MCP Section
3. REPL /mcp Commands
4. SSE Transport Implementation
5. Integration Tests

## Technical Challenge

The main blocker is that `rust-mcp-sdk`'s `ClientRuntime` type is private:

```rust
// This doesn't work because ClientRuntime is private:
use rust_mcp_sdk::mcp_client::client_runtime::ClientRuntime;
pub struct McpConnection {
    client: Arc<ClientRuntime>,  // ERROR: ClientRuntime is private
}
```

The example code uses type inference and never names the type:
```rust
let client = create_client(McpClientOptions { ... });
client.start().await?;  // Works due to type inference
```

## Solutions to Consider

### Option 1: Use rust-mcp-sdk Differently
- Store client in a type-erased way (`Box<dyn Any>`)
- Downcast when making calls
- **Pro**: Uses official SDK
- **Con**: Awkward API, type safety loss

### Option 2: Direct JSON-RPC Implementation
- Implement MCP protocol directly over STDIO
- MCP is just JSON-RPC 2.0 over stdin/stdout
- **Pro**: Full control, simpler types
- **Con**: More code to maintain

### Option 3: Wait for rust-mcp-sdk API Improvements
- File issue requesting public `ClientRuntime` or facade type
- **Pro**: Cleanest long-term solution
- **Con**: Blocks Phase 4 completion

### Option 4: Fork rust-mcp-sdk
- Make ClientRuntime public in our fork
- **Pro**: Immediate solution
- **Con**: Maintenance burden

## Recommended Next Steps

1. **Short Term**: Implement Option 2 (Direct JSON-RPC)
   - MCP protocol is simple and well-documented
   - Full control over types and lifetimes
   - Reference: https://modelcontextprotocol.io/specification/2025-11-25/

2. **Long Term**: Contribute to rust-mcp-sdk
   - File issue about type visibility
   - Propose public facade type
   - Switch to SDK once API matures

## Code Quality

All implemented code:
- ✅ Compiles (except connection.rs due to private type issue)
- ✅ Follows project conventions
- ✅ Has proper error handling
- ✅ Includes documentation
- ✅ Has test coverage (config module)

## Estimated Completion Time

With Option 2 (Direct JSON-RPC):
- JSON-RPC transport layer: 2-3 hours
- STDIO process management: 1-2 hours
- Tool integration: 2-3 hours
- Setup wizard section: 2-3 hours
- REPL commands: 1-2 hours
- Testing: 2-3 hours

**Total**: 10-16 hours for complete Phase 4

## Files Modified

- `Cargo.toml` - Added rust-mcp-sdk dependency
- `src/config/settings.rs` - Added mcp_servers field
- `src/config/loader.rs` - Added MCP config loading
- `src/tools/mod.rs` - Exported mcp module
- `src/tools/mcp/config.rs` - **COMPLETE**
- `src/tools/mcp/connection.rs` - **PARTIAL** (type visibility blocker)
- `src/tools/mcp/client.rs` - **PARTIAL** (depends on connection)
- `src/tools/mcp/mod.rs` - **COMPLETE**

## References

- MCP Specification: https://modelcontextprotocol.io/specification/2025-11-25/
- rust-mcp-sdk: https://docs.rs/rust-mcp-sdk/0.8.3/
- Example Client: `~/.cargo/registry/src/.../rust-mcp-sdk-0.8.3/examples/quick-start-client-stdio.rs`
