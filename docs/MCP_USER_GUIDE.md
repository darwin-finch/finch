# MCP Client - User Guide

**Model Context Protocol (MCP)** enables Finch to use external tools from MCP servers without writing custom integrations. Finch is an MCP client; its existing OpenAI-compatible HTTP endpoint remains the interface for applications that drive Finch as a model.

## Table of Contents

- [What is MCP?](#what-is-mcp)
- [Quick Start](#quick-start)
- [Installing MCP Servers](#installing-mcp-servers)
- [Configuration](#configuration)
- [Using MCP Tools](#using-mcp-tools)
- [REPL Commands](#repl-commands)
- [Popular MCP Servers](#popular-mcp-servers)
- [Troubleshooting](#troubleshooting)

## What is MCP?

**Model Context Protocol** is an open standard created by Anthropic for connecting AI assistants to external data sources and tools. MCP servers are small programs that expose tools via a JSON-RPC interface.

**Benefits**:
- ✅ **Extensible** - Add new capabilities without modifying Finch
- ✅ **Standard** - Works with hundreds of existing MCP servers
- ✅ **Secure** - Tools run in separate processes with permission control
- ✅ **Simple** - Configure via TOML, use via natural language

**Examples of what you can do**:
- Read/write files on your computer (filesystem server)
- Create GitHub issues and PRs (github server)
- Query databases (postgres, sqlite servers)
- Access Google Drive documents (gdrive server)
- Send Slack messages (slack server)
- And hundreds more...

## Quick Start

**1. Install a simple MCP server** (filesystem example):
```bash
npm install -g @modelcontextprotocol/server-filesystem
```

**2. Add to your config** (`~/.finch/config.toml`):
```toml
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
transport = "stdio"
enabled = true
```

**3. Start Finch**:
```bash
finch
```

**4. Verify connection**:
```
> /mcp list
📡 Connected MCP Servers:
  • filesystem
```

**5. Use the tools**:
```
> List all files in /tmp using the filesystem server
```

Finch will automatically include `mcp_filesystem_list_directory` in the tools available to its model.

## Installing MCP Servers

Most MCP servers are distributed via npm (Node Package Manager). You need Node.js installed first.

### Install Node.js (if needed)

**macOS**:
```bash
brew install node
```

**Linux**:
```bash
# Ubuntu/Debian
sudo apt install nodejs npm

# Fedora
sudo dnf install nodejs npm
```

**Windows**:
Download from [nodejs.org](https://nodejs.org)

### Install MCP Servers Globally

```bash
# Install a server globally (recommended)
npm install -g @modelcontextprotocol/server-filesystem

# Verify installation
which npx  # Should show /usr/local/bin/npx or similar
```

**Note**: Global installation (`-g`) means you can use the server from anywhere.

## Configuration

MCP servers are configured in `~/.finch/config.toml` under the `[mcp_servers]` section.

### Basic Configuration

```toml
[mcp_servers.<server_name>]
command = "npx"                    # Command to run
args = ["-y", "<package_name>"]    # Arguments to command
transport = "stdio"                # Communication method (stdio only for now)
enabled = true                     # Whether to connect on startup
env = { }                          # Environment variables (optional)
timeout_secs = 300                 # Per-request timeout (optional; default 300)
```

### Configuration Fields

| Field | Required | Description | Example |
|-------|----------|-------------|---------|
| `command` | Yes | Executable to run | `"npx"`, `"node"`, `"/path/to/binary"` |
| `args` | Yes | Command arguments | `["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]` |
| `transport` | Yes | Communication protocol | `"stdio"` (only option currently) |
| `enabled` | Yes | Connect on startup | `true` or `false` |
| `env` | No | Environment variables | `{ API_KEY = "$MY_API_KEY" }` |
| `timeout_secs` | No | Maximum seconds for initialize, discovery, or a tool call | `300` |

### Example: Multiple Servers

```toml
# Filesystem access (read-only)
[mcp_servers.filesystem_readonly]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/you/projects"]
transport = "stdio"
enabled = true

# GitHub integration
[mcp_servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
transport = "stdio"
enabled = true
env = { GITHUB_TOKEN = "$GITHUB_TOKEN" }  # Reads from environment

# Database access
[mcp_servers.postgres]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres"]
transport = "stdio"
enabled = false  # Disabled by default
env = { DATABASE_URL = "postgresql://localhost:5432/mydb" }
```

### Environment Variables

Environment values can be:
1. **Literal values**: `{ API_KEY = "abc123" }`
2. **Exact references**: `{ API_KEY = "$MY_API_KEY" }` or `{ API_KEY = "${MY_API_KEY}" }`

Finch resolves exact references from its own environment without invoking a shell. A missing referenced variable prevents that server from connecting and is reported in the logs. Interpolation inside a larger string is deliberately not performed.

**Security Note**: Store sensitive tokens in your shell environment (e.g., `~/.zshrc`), not in the config file:

```bash
# In ~/.zshrc or ~/.bashrc
export GITHUB_TOKEN="ghp_your_token_here"
export DATABASE_URL="postgresql://..."
```

Then reference in config:
```toml
[mcp_servers.github]
env = { GITHUB_TOKEN = "$GITHUB_TOKEN" }
```

## Using MCP Tools

Once configured, MCP tools work just like built-in tools. You don't need to know the exact tool names - just ask naturally!

### Natural Language Examples

**Filesystem**:
```
> What files are in my Downloads folder?
> Read the contents of README.md
> Show me all Python files in the current directory
```

**GitHub** (if github server configured):
```
> Create an issue in my repo titled "Fix authentication bug"
> List all open pull requests in the anthropics/claude repository
> Show me recent commits on the main branch
```

**Database** (if postgres server configured):
```
> Query my database for all users created in the last week
> Show me the schema of the users table
> Count how many orders exist in the database
```

### Direct Tool Invocation

If you know the exact tool name, you can use it directly:

```
> Use mcp_filesystem_read_file to read /etc/hosts
> Use mcp_github_create_issue with title "Bug" and body "Description"
```

### Tool Name Format

MCP tools are automatically prefixed to avoid conflicts:
```
mcp_<server>_<tool>

Examples:
  mcp_filesystem_read_file          (from "filesystem" server)
  mcp_github_create_issue           (from "github" server)
  mcp_postgres_query                (from "postgres" server)
```

## REPL Commands

Finch provides `/mcp` commands to manage MCP servers at runtime.

### /mcp list

**List all connected MCP servers**:
```
> /mcp list
📡 Connected MCP Servers:
  • filesystem
  • github
  • postgres
```

Shows which servers are currently connected and available.

### /mcp tools [server]

**List all tools** (from all servers):
```
> /mcp tools
🔧 All MCP Tools:
  • filesystem_read_file
    Read contents of a file
  • filesystem_list_directory
    List files in a directory
  • github_create_issue
    Create a new issue in a repository
  ...
```

**List tools from specific server**:
```
> /mcp tools filesystem
🔧 MCP Tools from 'filesystem' server:
  • filesystem_read_file
    Read contents of a file
  • filesystem_list_directory
    List files in a directory
  • filesystem_write_file
    Write contents to a file
```

### /mcp refresh

**Refresh tool list from all servers**:
```
> /mcp refresh
Refreshing MCP tools...
✓ Refreshed MCP tools (24 tools available)
```

Use this if you update an MCP server or if tools aren't appearing correctly.

### /mcp reload

**Reconnect to all configured servers**:
```
> /mcp reload
Reconnecting to configured MCP servers...
✓ Connected to 2 MCP server(s) with 17 tool(s)
```

Disabled servers remain disabled. A failed server is logged while other configured servers continue connecting.

## Popular MCP Servers

### Official Anthropic Servers

#### Filesystem
**Access local files and directories**

```bash
npm install -g @modelcontextprotocol/server-filesystem
```

**Config**:
```toml
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/directory"]
transport = "stdio"
enabled = true
```

**Tools**:
- `read_file` - Read file contents
- `list_directory` - List files in directory
- `write_file` - Write to file
- `move_file` - Move/rename files
- `create_directory` - Create directory
- `search_files` - Search for files

**Security**: Restricts access to the specified directory only.

#### GitHub
**Interact with GitHub repositories**

```bash
npm install -g @modelcontextprotocol/server-github
```

**Config**:
```toml
[mcp_servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
transport = "stdio"
enabled = true
env = { GITHUB_TOKEN = "$GITHUB_TOKEN" }
```

**Setup GitHub Token**:
1. Go to https://github.com/settings/tokens
2. Generate new token (classic)
3. Select scopes: `repo`, `read:org`
4. Copy token
5. Add to shell: `export GITHUB_TOKEN="ghp_..."`

**Tools**:
- `create_issue` - Create new issue
- `create_pull_request` - Create PR
- `list_issues` - List issues
- `search_repositories` - Search repos
- `get_file_contents` - Read file from repo

#### PostgreSQL
**Query PostgreSQL databases**

```bash
npm install -g @modelcontextprotocol/server-postgres
```

**Config**:
```toml
[mcp_servers.postgres]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres"]
transport = "stdio"
enabled = true
env = { DATABASE_URL = "$DATABASE_URL" }
```

**Tools**:
- `query` - Execute SQL query
- `list_tables` - Show all tables
- `describe_table` - Show table schema

### Community Servers

#### Google Drive
**Access Google Drive files**

```bash
npm install -g @modelcontextprotocol/server-gdrive
```

Requires OAuth setup - see server documentation.

#### Slack
**Send messages and interact with Slack**

```bash
npm install -g @modelcontextprotocol/server-slack
```

Requires Slack app token - see server documentation.

#### SQLite
**Query SQLite databases**

```bash
npm install -g @modelcontextprotocol/server-sqlite
```

#### Brave Search
**Web search via Brave API**

```bash
npm install -g @modelcontextprotocol/server-brave-search
```

### Finding More Servers

**Official list**: https://github.com/modelcontextprotocol/servers

**npm search**: `npm search modelcontextprotocol`

## Troubleshooting

### MCP servers not connecting

**Symptom**: `/mcp list` shows no servers, or specific server missing

**Causes & Solutions**:

1. **Server not installed**:
   ```bash
   # Check if package is installed
   npm list -g @modelcontextprotocol/server-filesystem

   # If not found, install it
   npm install -g @modelcontextprotocol/server-filesystem
   ```

2. **Config error**:
   - Check `~/.finch/config.toml` syntax
   - Ensure `enabled = true`
   - Verify command path: `which npx`
   - Check args are valid

3. **Permission issues**:
   - Check file permissions on config
   - Verify MCP server has execute permissions
   - Check directory access (for filesystem server)

4. **Environment variable not set**:
   ```bash
   # Verify environment variable exists
   echo $GITHUB_TOKEN

   # If empty, set it in ~/.zshrc or ~/.bashrc
   export GITHUB_TOKEN="ghp_..."

   # Reload shell
   source ~/.zshrc
   ```

5. **Check Finch logs**:
   ```bash
   # Look for MCP-related errors
   finch 2>&1 | grep -i mcp
   ```

### Tools not appearing

**Symptom**: `/mcp list` shows server, but `/mcp tools` shows no tools

**Solutions**:

1. **Refresh tools**:
   ```
   > /mcp refresh
   ```

2. **Restart Finch**:
   ```bash
   # Exit
   > /quit

   # Start again
   finch
   ```

3. **Check server compatibility**:
   - Ensure server implements MCP tools correctly
   - Test server directly: `npx -y @modelcontextprotocol/server-filesystem /tmp`
   - Check for JSON-RPC responses

### Tools not executing

**Symptom**: AI tries to use tool but gets error

**Causes & Solutions**:

1. **Tool permission denied**:
   - Check if tool confirmation dialog appeared
   - Approve the tool execution
   - Or enable auto-approve: `config.features.auto_approve_tools = true`

2. **Invalid parameters**:
   - Tool may have specific parameter requirements
   - Check tool schema: `/mcp tools <server>`
   - Verify AI is passing correct JSON structure

3. **Server crashed**:
   - Restart Finch to reconnect
   - Check if server command still works: `which npx`
   - Look for stderr output in terminal

### "MCP plugin system not configured"

**Symptom**: Commands show this message

**Solution**: Add at least one MCP server to `~/.finch/config.toml`:

```toml
[mcp_servers.example]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
transport = "stdio"
enabled = true
```

### Node.js / npm not found

**Symptom**: `command not found: npx` or similar

**Solution**:
```bash
# macOS
brew install node

# Linux (Ubuntu/Debian)
sudo apt install nodejs npm

# Verify
which node
which npx
```

### Slow tool execution

**Causes**:
- MCP server doing expensive operation (database query, API call)
- Network latency (for remote resources)
- Large file operations

**Normal behavior** - MCP tools run in separate processes, so some latency is expected.

## Security Best Practices

1. **Limit filesystem access**:
   ```toml
   # ❌ DON'T give access to entire filesystem
   args = ["-y", "@modelcontextprotocol/server-filesystem", "/"]

   # ✅ DO restrict to specific directories
   args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/you/projects"]
   ```

2. **Use environment variables for secrets**:
   ```toml
   # ❌ DON'T put secrets in config
   env = { API_KEY = "abc123secretkey" }

   # ✅ DO use environment variables
   env = { API_KEY = "$MY_API_KEY" }
   ```

3. **Disable unused servers**:
   ```toml
   [mcp_servers.rarely_used]
   enabled = false  # Won't connect on startup
   ```

4. **Review tool permissions**:
   - Don't auto-approve all tools blindly
   - Review what each tool does before first use
   - Use patterns for repetitive safe operations only

5. **Keep servers updated**:
   ```bash
   npm update -g @modelcontextprotocol/server-filesystem
   ```

## Advanced Configuration

### Custom Server Paths

If you built an MCP server yourself or it's not an npm package:

```toml
[mcp_servers.custom]
command = "/usr/local/bin/my-mcp-server"
args = ["--port", "9000", "--data-dir", "/var/data"]
transport = "stdio"
enabled = true
```

### Multiple Instances

Run multiple instances of the same server with different configs:

```toml
[mcp_servers.projects_workspace]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/you/projects"]
transport = "stdio"
enabled = true

[mcp_servers.documents_workspace]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/you/Documents"]
transport = "stdio"
enabled = true
```

### Debugging MCP Communication

Enable debug logging to see JSON-RPC messages:

```toml
[features]
debug_logging = true
```

Then check logs for MCP-related messages:
```bash
finch 2>&1 | grep -i "mcp\|jsonrpc"
```

## Further Reading

- **MCP Specification**: https://modelcontextprotocol.io/specification/2026-07-28/
- **Official Server List**: https://github.com/modelcontextprotocol/servers
- **JSON-RPC 2.0 Spec**: https://www.jsonrpc.org/specification
- **Finch documentation map**: [`README.md`](README.md)

## Getting Help

**Issues**: https://github.com/darwin-finch/finch/issues

**Questions**: Create a GitHub discussion or issue

**Contributing**: PRs welcome for new MCP integrations or documentation improvements!

---

**Last Updated**: 2026-08-21
**Finch Version**: 0.7.30+
**Newest MCP Protocol Version**: 2026-07-28 (with negotiation for earlier revisions)
