# Tool Confirmation Feature

## Overview

The tool confirmation feature adds interactive prompts before executing tools in Shammah. This gives users control over what actions Claude performs on their behalf, with options to approve once, remember the approval for the session, or deny execution.

## How It Works

### Confirmation Flow

When Claude requests to use a tool in interactive mode:

1. **Generate Signature**: A unique signature is created from the tool name and context
2. **Check Persistent Patterns**: Check if signature matches any saved patterns in `~/.finch/tool_patterns.json`
3. **Check Session Cache**: If not in persistent store, check if approved this session
4. **Prompt User**: If not cached, display a confirmation prompt with six options:
   - **Option 1**: Approve once (execute now, ask again next time)
   - **Option 2**: Approve exact match for this session (remember until restart)
   - **Option 3**: Approve pattern for this session (match similar commands)
   - **Option 4**: Approve exact match persistently (save to disk, remember forever)
   - **Option 5**: Approve pattern persistently (save to disk, match similar commands)
   - **Option 6**: Deny (skip tool execution and send error to Claude)
5. **Save if Persistent**: If option 4 or 5 selected, save to `~/.finch/tool_patterns.json`
6. **Execute or Deny**: Proceed based on user choice

### Pattern-Based Confirmation

In addition to exact signature matching, Shammah supports **pattern-based approvals** that can match multiple similar tool executions:

**Session vs Persistent Patterns:**
- **Session patterns** (options 2-3): Stored in memory, cleared on exit
- **Persistent patterns** (options 4-5): Saved to `~/.finch/tool_patterns.json`, survive restarts

**Pattern Priority (most specific to least specific):**
1. **Exact match in persistent store** - highest priority
2. **Exact match in session cache** - if not in persistent store
3. **Pattern match in persistent store** - specific patterns before general ones
4. **Pattern match in session cache** - if no persistent patterns match
5. **Prompt user** - if no matches found

**Pattern Types:**
- **Wildcard patterns**: Use `*` for single-level wildcard or `**` for recursive matching
- **Regex patterns**: Use full Rust regex syntax for complex matching rules

See [Pattern Types](#pattern-types) section below for detailed examples.

## Pattern Management Commands

You can manage confirmation patterns using the `/patterns` command family:

### List Patterns

```bash
/patterns
/patterns list
```

Shows all saved patterns and exact approvals with:
- Pattern ID (for removal)
- Tool name
- Pattern string
- Match count
- Last used timestamp
- Creation date

**Example output:**
```
Persistent Patterns (saved to disk):
─────────────────────────────────────
[abc123-def456] bash: cargo * in /project
  Matches: 15
  Last used: 2026-01-30 14:23:45 UTC
  Created: 2026-01-29 10:15:30 UTC

[xyz789-abc123] read: reading /project/src/**
  Matches: 42
  Last used: 2026-01-30 15:10:12 UTC
  Created: 2026-01-29 11:20:00 UTC

Total: 2 patterns, 5 exact approvals
```

### Add Pattern

```bash
/patterns add
```

Interactively create a new pattern:
1. Enter tool name (bash, read, grep, etc.)
2. Enter pattern string (with wildcards or regex)
3. Choose pattern type (wildcard or regex)
4. Enter description

**Example interaction:**
```
Tool name (bash, read, grep, etc.): bash
Pattern (use * for wildcards): cargo * in *
Pattern type (wildcard/regex) [wildcard]: wildcard
Description: Allow all cargo commands in any directory
✓ Pattern saved persistently
```

### Remove Pattern

```bash
/patterns remove <id>
/patterns rm <id>        # Short alias
```

Remove a specific pattern or approval by its ID (shown in `/patterns list`).

**Example:**
```bash
/patterns rm abc123-def456
✓ Removed pattern: cargo * in /project
```

### Clear All Patterns

```bash
/patterns clear
```

Remove all persistent patterns and exact approvals. Requires confirmation prompt.

**Example:**
```
Are you sure you want to clear ALL patterns? (yes/no): yes
✓ Cleared 5 patterns and 10 exact approvals
```

## Pattern Types

Shammah supports two types of patterns for matching tool signatures:

### Wildcard Patterns

Wildcard patterns use simple glob-style matching:

| Wildcard | Meaning | Example |
|----------|---------|---------|
| `*` | Single-level wildcard | `cargo *` matches `cargo test`, `cargo build` |
| `**` | Recursive wildcard (paths) | `/project/**` matches any file under `/project/` |

**Wildcard Examples:**

```bash
# Match any cargo command in a specific directory
cargo * in /project

# Match cargo test in any directory
cargo test in *

# Match any cargo command in any directory
cargo * in *

# Match any file under /project
reading /project/**

# Match all Rust source files (with path)
reading /project/src/**.rs

# Match fetching any docs.rs page
fetching https://docs.rs/*
```

### Regex Patterns

Regex patterns provide full Rust regex power for complex matching:

**Regex Examples:**

```bash
# Match cargo test or build only
^cargo (test|build)$

# Match reading any Rust source file
^reading /project/src/.*\.rs$

# Match npm commands with specific flags
^npm (test|run) --.*$

# Match git commands (but not git push)
^git (?!push).* in .*$

# Match URLs with query parameters
^fetching https://example\.com/api\?.*$

# Match file paths with specific extensions
^reading .*\.(json|yaml|toml)$
```

**When to use each:**

| Use Case | Pattern Type | Example |
|----------|--------------|---------|
| Simple command prefixes | Wildcard | `cargo *` |
| Directory hierarchies | Wildcard | `/project/**` |
| Alternation (OR logic) | Regex | `cargo (test\|build)` |
| Negative lookahead | Regex | `git (?!push).*` |
| Character classes | Regex | `[a-z]+\.rs$` |
| Complex file extensions | Regex | `.*\.(json\|yaml)$` |

### Comparing Wildcard vs Regex

Same goal, different approaches:

```bash
# Goal: Match cargo test/build/check only

# Wildcard approach (matches TOO much):
cargo * in /project    # Also matches "cargo run", "cargo install", etc.

# Regex approach (precise):
^cargo (test|build|check) in /project$    # Only test, build, check
```

```bash
# Goal: Match reading Rust files in src/

# Wildcard approach:
reading /project/src/**/*.rs    # Simple and readable

# Regex approach:
^reading /project/src/.*\.rs$    # More control, same result
```

**Tip**: Start with wildcards for simplicity. Use regex when you need:
- Alternation (A OR B OR C)
- Negative matching (anything BUT X)
- Character classes ([a-z], [0-9])
- Anchors (^ for start, $ for end)

### Tool Signatures

Each tool execution generates a context-specific signature that identifies the operation:

| Tool | Signature Format | Example |
|------|------------------|---------|
| `bash` | `{command} in {working_dir}` | `cargo test in /Users/foo/project` |
| `read` | `reading {file_path}` | `reading /path/to/file.txt` |
| `grep` | `pattern '{pattern}' in {path}` | `pattern 'fn main' in src/` |
| `glob` | `pattern {pattern}` | `pattern **/*.rs` |
| `web_fetch` | `fetching {url}` | `fetching https://docs.rs/tokio` |
| `save_and_exec` | `{command} in {working_dir}` | `cargo build in /Users/foo/project` |

## Example Prompts

### Bash Command

```
  Tool Execution Request:
  ─────────────────────────
  Tool: bash
  Command: cargo fmt
  Description: Format code with rustfmt

  Do you want to proceed?
  ❯ 1. Yes
    2. Yes, and don't ask again for cargo fmt in /Users/finch/repos/claude-proxy
    3. No

  Choice [1-3]: _
```

### Read File

```
  Tool Execution Request:
  ─────────────────────────
  Tool: read
  File: /Users/finch/repos/claude-proxy/src/main.rs

  Do you want to proceed?
  ❯ 1. Yes
    2. Yes, and don't ask again for reading /Users/finch/repos/claude-proxy/src/main.rs
    3. No

  Choice [1-3]: _
```

### Web Fetch

```
  Tool Execution Request:
  ─────────────────────────
  Tool: web_fetch
  URL: https://docs.rs/tokio
  Prompt: Get the latest version number

  Do you want to proceed?
  ❯ 1. Yes
    2. Yes, and don't ask again for fetching https://docs.rs/tokio
    3. No

  Choice [1-6]: _
```

### Updated Confirmation Prompt (with Patterns)

```
  Tool Execution Request:
  ─────────────────────────
  Tool: bash
  Command: cargo test
  Description: Run tests

  Do you want to proceed?
  ❯ 1. Yes (once only)
    2. Yes, and remember exact command for this session
    3. Yes, and remember pattern for this session
    4. Yes, and ALWAYS allow this exact command
    5. Yes, and ALWAYS allow this pattern
    6. No (deny)

  Choice [1-6]: 3

  What should the pattern match?
  ❯ 1. cargo * in /project
    2. cargo test in *
    3. cargo * in *

  Choice [1-3]: 1

✓ Approved for this session (pattern)
✓ Success
```

## Pattern Statistics

Shammah tracks usage statistics for all patterns and approvals:

**Match Counts:**
- Every time a pattern or exact approval matches a tool execution, its match count increments
- View counts with `/patterns list`
- Higher counts indicate frequently-used patterns

**Last Used Timestamps:**
- Tracks when each pattern was last matched
- Helps identify stale patterns for cleanup
- Displayed in UTC timezone

**Pattern Metadata:**
- **created_at**: When pattern was first created
- **created_by**: Optional field for user identification (future feature)
- **description**: Human-readable explanation of what pattern does
- **match_count**: Number of times pattern has matched
- **last_used**: Most recent match timestamp

**Automatic Cleanup:**

Shammah automatically prunes unused patterns:
- Patterns with 0 matches older than 30 days are removed
- Keeps frequently-used patterns indefinitely
- Manual cleanup available via `/patterns clear`

## User Interaction

### Approving Once (Option 1)

When you select option 1:
- Tool executes immediately
- No cache entry is created
- Next time Claude tries the same operation, you'll be prompted again

**Use case**: Testing or one-off operations you don't want to repeat

### Approving Exact Match - Session (Option 2)

When you select option 2:
- Tool executes immediately
- **Exact signature** is added to session cache
- Future **identical** operations execute without prompting
- Cache persists only for current session (cleared on exit)

**Use case**:
- Repetitive operations during a single session
- When you trust this specific command but don't want it saved permanently
- Testing a pattern before making it persistent

**Example**: Approve `cargo test in /project` for this session only

### Approving Pattern - Session (Option 3)

When you select option 3:
- Tool executes immediately
- You're prompted to choose a **pattern** that matches similar operations
- Pattern is added to session cache (memory only)
- Future operations matching pattern execute without prompting
- Cache persists only for current session (cleared on exit)

**Use case**:
- Want to approve a category of commands (e.g., all cargo commands)
- Don't want permanent approval yet
- Experimenting with pattern scope

**Example**: Choose pattern `cargo * in /project` to approve all cargo commands in that directory

### Approving Exact Match - Persistent (Option 4)

When you select option 4:
- Tool executes immediately
- **Exact signature** is saved to `~/.finch/tool_patterns.json`
- Future **identical** operations execute without prompting
- Approval persists across restarts
- Can be removed with `/patterns rm <id>`

**Use case**:
- Frequently-used commands you completely trust
- Want permanent approval without pattern matching
- Maximum specificity

**Example**: Always allow `cargo fmt in /project` without asking

### Approving Pattern - Persistent (Option 5)

When you select option 5:
- Tool executes immediately
- You're prompted to choose a **pattern** that matches similar operations
- Pattern is saved to `~/.finch/tool_patterns.json`
- Future operations matching pattern execute without prompting
- Approval persists across restarts
- Can be removed with `/patterns rm <id>`

**Use case**:
- Want to approve categories of commands permanently
- Trust all commands in a specific directory
- Want to approve tool access to entire directory trees

**Example**: Choose pattern `cargo * in *` to approve all cargo commands everywhere

### Denying (Option 6)

When you select option 6:
- Tool execution is skipped
- Error message sent to Claude: "Tool execution denied by user"
- Claude receives feedback and can adapt its approach
- No cache entry is created (neither session nor persistent)

**Use case**:
- Operations you don't want to perform
- When Claude is going in the wrong direction
- Dangerous or destructive commands
- Testing Claude's error recovery

## Cache Behavior

### Session Cache (Options 2-3)

**What Gets Cached:**
- Option 2: Exact signatures (tool name + full context)
- Option 3: Pattern rules (tool name + pattern string)

**Lifetime:**
- Created when user selects option 2 or 3
- Persists for duration of current Shammah session only
- Cleared when session ends (Ctrl+C, `/quit`, or process exit)
- Not saved to disk (memory-only)

**Matching:**
- **Exact signatures** (option 2): Must match precisely
- **Patterns** (option 3): Use wildcard/regex matching

### Persistent Store (Options 4-5)

**What Gets Saved:**
- Option 4: Exact approvals (tool name + full context)
- Option 5: Pattern rules (tool name + pattern string)

**Storage:**
- Saved to `~/.finch/tool_patterns.json`
- Loaded on startup
- Persists across restarts
- Survives system reboots

**File Format (JSON):**
```json
{
  "version": 2,
  "patterns": [
    {
      "id": "abc123-def456",
      "pattern": "cargo * in /project",
      "tool_name": "bash",
      "description": "Allow cargo commands in project",
      "created_at": "2026-01-30T12:00:00Z",
      "match_count": 15,
      "pattern_type": "wildcard",
      "last_used": "2026-01-30T15:30:00Z"
    }
  ],
  "exact_approvals": [
    {
      "id": "xyz789-abc123",
      "signature": "cargo test in /project",
      "tool_name": "bash",
      "created_at": "2026-01-29T10:00:00Z",
      "match_count": 42
    }
  ]
}
```

**Automatic Migration:**
- Old v1 format automatically upgraded to v2 on load
- New fields added with default values

### Match Priority

When checking if a tool execution is approved:

1. **Check persistent exact approvals** (option 4)
2. **Check session exact approvals** (option 2)
3. **Check persistent patterns** (option 5) - most specific first
4. **Check session patterns** (option 3) - most specific first
5. **Prompt user** if no matches found

**Pattern Specificity Ranking:**
- Patterns with fewer wildcards rank higher
- `cargo test in /project` (0 wildcards) > `cargo * in /project` (1 wildcard)
- `cargo * in /project` (1 wildcard) > `cargo * in *` (2 wildcards)
- Exact text match always highest priority

### Cache Hit Examples

✅ **Exact Match:**
- Saved: `cargo test in /project`
- Matches: `cargo test in /project` ✓
- Doesn't match: `cargo test --all in /project` ✗

✅ **Wildcard Pattern:**
- Saved: `cargo * in /project`
- Matches: `cargo test in /project` ✓
- Matches: `cargo build in /project` ✓
- Matches: `cargo test --all in /project` ✓
- Doesn't match: `npm test in /project` ✗

✅ **Recursive Wildcard:**
- Saved: `reading /project/**`
- Matches: `reading /project/src/main.rs` ✓
- Matches: `reading /project/tests/test.rs` ✓
- Doesn't match: `reading /other/file.rs` ✗

✅ **Regex Pattern:**
- Saved: `^cargo (test|build)$` (tool: bash)
- Matches: `cargo test` ✓
- Matches: `cargo build` ✓
- Doesn't match: `cargo run` ✗

## Non-Interactive Mode

When Shammah runs in non-interactive mode (pipes, scripts, daemon):
- **No prompts shown**
- **All tools execute automatically**
- **No caching needed**

This ensures Shammah works seamlessly in automation contexts.

## Technical Details

### Implementation

The confirmation system is implemented across multiple modules:

**Core Logic:**
- `src/tools/executor.rs`: `ToolExecutor`, `ToolSignature`, `ApprovalSource`, `generate_tool_signature()`
- `src/tools/patterns.rs`: `ToolPattern`, `ExactApproval`, `PersistentPatternStore`, pattern matching
- `src/cli/repl.rs`: `ConfirmationResult`, `confirm_tool_execution()`, `build_pattern_from_signature()`

**Pattern Matching:**
- `src/tools/patterns.rs`: Wildcard and regex pattern matching algorithms
- Specificity ranking (fewer wildcards = higher priority)
- Persistent storage with atomic writes

### Key Data Structures

**ToolSignature**
```rust
pub struct ToolSignature {
    pub tool_name: String,      // e.g., "bash"
    pub context_key: String,    // e.g., "cargo test in /project"
}
```

**ToolPattern**
```rust
pub struct ToolPattern {
    pub id: String,                      // UUID for removal
    pub pattern: String,                 // e.g., "cargo * in *"
    pub tool_name: String,               // Must match signature tool
    pub description: String,             // Human-readable
    pub created_at: DateTime<Utc>,       // Creation timestamp
    pub match_count: u64,                // Usage statistics
    pub pattern_type: PatternType,       // Wildcard or Regex
    pub last_used: Option<DateTime<Utc>>, // Last match time
    pub created_by: Option<String>,      // User identification
    compiled_regex: Option<Regex>,       // Compiled regex (cached)
}
```

**ExactApproval**
```rust
pub struct ExactApproval {
    pub id: String,               // UUID for removal
    pub signature: String,        // Full context_key
    pub tool_name: String,        // Tool name
    pub created_at: DateTime<Utc>, // Creation timestamp
    pub match_count: u64,         // Usage statistics
}
```

**PersistentPatternStore**
```rust
pub struct PersistentPatternStore {
    pub version: u32,                      // Schema version (currently 2)
    pub patterns: Vec<ToolPattern>,        // Pattern rules
    pub exact_approvals: Vec<ExactApproval>, // Exact matches
}
```

**ConfirmationResult**
```rust
pub enum ConfirmationResult {
    ApproveOnce,
    ApproveExactSession(ToolSignature),
    ApprovePatternSession(ToolPattern),
    ApproveExactPersistent(ToolSignature),
    ApprovePatternPersistent(ToolPattern),
    Deny,
}
```

**ApprovalSource**
```rust
pub enum ApprovalSource {
    PersistentExact,      // From option 4
    PersistentPattern,    // From option 5
    SessionExact,         // From option 2
    SessionPattern,       // From option 3
}
```

### Integration Flow

Confirmation happens in `ToolExecutor::execute_tool_loop()` before each tool execution:

1. **Generate Signature**: Create `ToolSignature` from tool use + working directory
2. **Check Persistent Store**: Call `persistent_store.matches(signature)` - checks exact approvals first, then patterns (by specificity)
3. **Check Session Cache**: If no persistent match, check in-memory session cache
4. **Prompt User**: If no matches and interactive mode, call `confirm_tool_execution()`
5. **Handle Result**:
   - `ApproveOnce`: Execute tool only
   - `ApproveExactSession`: Execute + add to session cache
   - `ApprovePatternSession`: Execute + add pattern to session cache
   - `ApproveExactPersistent`: Execute + save exact approval to disk
   - `ApprovePatternPersistent`: Execute + save pattern to disk
   - `Deny`: Return error to Claude
6. **Execute or Error**: Run tool or return "denied by user" error
7. **Update Statistics**: Increment match count and update `last_used` timestamp

### Pattern Matching Algorithm

**Wildcard Matching:**
```rust
// Simple wildcards: "*" matches any text
pattern_matches("cargo * in /dir", "cargo test in /dir") // true
pattern_matches("cargo * in /dir", "npm test in /dir")   // false

// Recursive wildcards: "**" matches paths
pattern_matches("reading /project/**", "reading /project/src/main.rs") // true
```

**Regex Matching:**
```rust
// Full Rust regex syntax
let pattern = ToolPattern::new_with_type(
    r"^cargo (test|build)$",
    "bash",
    "Allow test and build",
    PatternType::Regex,
);

pattern.matches(&sig_test)  // true for "cargo test"
pattern.matches(&sig_build) // true for "cargo build"
pattern.matches(&sig_run)   // false for "cargo run"
```

**Specificity Ranking:**
```rust
// Count wildcards, fewer = more specific
"cargo test in /project"    // 0 wildcards (highest priority)
"cargo * in /project"       // 1 wildcard
"cargo test in *"           // 1 wildcard
"cargo * in *"              // 2 wildcards (lowest priority)

// When multiple patterns match, most specific wins
```

### File Format

**Location:** `~/.finch/tool_patterns.json`

**Schema (v2):**
```json
{
  "version": 2,
  "patterns": [
    {
      "id": "uuid-v4",
      "pattern": "cargo * in /project",
      "tool_name": "bash",
      "description": "Allow cargo commands",
      "created_at": "2026-01-30T12:00:00Z",
      "match_count": 15,
      "pattern_type": "wildcard",
      "last_used": "2026-01-30T15:30:00Z",
      "created_by": null
    }
  ],
  "exact_approvals": [
    {
      "id": "uuid-v4",
      "signature": "cargo test in /project",
      "tool_name": "bash",
      "created_at": "2026-01-29T10:00:00Z",
      "match_count": 42
    }
  ]
}
```

**Atomic Writes:**
- Write to temp file first (`.tmp` extension)
- Atomic rename to final path
- Prevents corruption on crash/interrupt

**Migration:**
- v1 → v2: Automatic on load
- New fields get default values via serde
- Compiled regex regenerated after deserialization

## Examples

### Example 1: Repetitive Command

```
User: Format the code
→ Claude requests: bash -c "cargo fmt"

  Tool Execution Request:
  Tool: bash
  Command: cargo fmt

  Choice [1-3]: 2    # Approve and remember

✓ Approved (won't ask again this session)
✓ Success

User: Format the code again
→ Claude requests: bash -c "cargo fmt"
✓ Success           # No prompt, uses cached approval
```

### Example 2: Pattern Matching - Session

```
User: Run tests
→ Claude requests: bash -c "cargo test"

  Choice [1-6]: 3    # Approve pattern for session

  What should the pattern match?
  ❯ 1. cargo * in /project
    2. cargo test in *
    3. cargo * in *

  Choice [1-3]: 1    # Match all cargo commands in /project

✓ Approved for this session (pattern)
✓ Success

User: Build the project
→ Claude requests: bash -c "cargo build"
✓ Success           # No prompt - matches pattern "cargo * in /project"

User: Run npm tests
→ Claude requests: bash -c "npm test"

  Choice [1-6]: _   # New prompt - doesn't match cargo pattern
```

### Example 3: Persistent Pattern

```
User: Read the main file
→ Claude requests: read file_path=/project/src/main.rs

  Choice [1-6]: 5    # Approve pattern persistently

  What should the pattern match?
  ❯ 1. reading /project/src/main.rs
    2. reading /project/src/**
    3. reading *

  Choice [1-3]: 2    # Match all files under /project/src/

✓ Saved persistently (pattern)
✓ Success

# ... later, after restart ...

User: Read the lib file
→ Claude requests: read file_path=/project/src/lib.rs
✓ Success           # No prompt - matches persistent pattern

User: Read tests
→ Claude requests: read file_path=/project/tests/test.rs

  Choice [1-6]: _   # New prompt - tests/ not under src/
```

### Example 4: Pattern Priority

```
# Setup: Two patterns saved
#   1. "cargo test in /project" (exact, persistent)
#   2. "cargo * in *" (general, persistent)

User: Run tests
→ Claude requests: bash -c "cargo test"
✓ Success           # Matches pattern #1 (more specific)

User: Build project
→ Claude requests: bash -c "cargo build"
✓ Success           # Matches pattern #2 (only match)
```

### Example 5: Denial and Recovery

```
User: Delete all temporary files
→ Claude requests: bash -c "find . -name '*.tmp' -delete"

  Choice [1-6]: 6    # Deny - too risky

✗ Denied by user

Claude: I understand you don't want to delete those files.
        Would you like me to just list them instead?
```

### Example 6: Managing Patterns

```bash
# List all saved patterns
$ /patterns
Persistent Patterns:
  [abc123] bash: cargo * in /project (15 matches)
  [def456] read: reading /project/src/** (42 matches)

# Add a new pattern manually
$ /patterns add
Tool name: bash
Pattern: npm * in /project
Pattern type [wildcard]: wildcard
Description: Allow npm commands in project
✓ Pattern saved

# Remove a pattern
$ /patterns rm abc123
✓ Removed pattern: cargo * in /project

# Clear everything (with confirmation)
$ /patterns clear
Are you sure? (yes/no): yes
✓ Cleared all patterns
```

## Testing

Run the demo to see signature generation:

```bash
cargo run --example tool_confirmation_demo
```

Run tests:

```bash
cargo test --lib confirmation
cargo test --lib signature
cargo test --lib approval
```

## Future Enhancements

### Completed Features ✅

- ✅ **Persistent Cache**: Implemented as `~/.finch/tool_patterns.json`
- ✅ **Pattern-Based Rules**: Wildcard and regex patterns for categories of commands
- ✅ **Regex Patterns**: Full Rust regex syntax support
- ✅ **Review Mode**: `/patterns` command family to view/manage patterns

### Potential Improvements

These improvements are not currently implemented but could be added:

1. **Timeout**: Auto-deny if no response in 30 seconds
2. **Batch Approval**: "Approve all remaining tools in this conversation"
3. **Config File Integration**: `allow_tools = ["read", "glob"]` in config.toml
4. **Risk Levels**: Auto-approve "safe" tools (read, glob), always prompt for "dangerous" ones (bash with rm)
5. **Pattern Templates**: Pre-built pattern library (e.g., "Allow all cargo commands", "Allow reading Rust files")
6. **Pattern Testing**: Test pattern against example signatures before saving
7. **Pattern Import/Export**: Share patterns between machines
8. **Smart Suggestions**: Suggest patterns based on approval history
9. **Audit Log**: Record all tool executions (approved and denied) for security review
10. **Per-Project Patterns**: Different patterns for different directories

## Design Principles

1. **Safe by Default**: Prompt for every new operation
2. **User Control**: User decides what executes
3. **Session Scope**: Cache clears on exit (no persistent state)
4. **Context Aware**: Signatures include relevant parameters
5. **Non-Blocking**: Non-interactive mode works without prompts
6. **Transparent**: Clear indication of what will execute
7. **Recoverable**: Denial doesn't break the conversation

## Security Considerations

### What This Protects Against

- **Unintended Commands**: User sees and approves all bash commands
- **Sensitive File Access**: User controls which files can be read
- **External Requests**: User controls which URLs are fetched
- **Destructive Operations**: User can deny dangerous commands

### What This Doesn't Protect Against

- **Approved Malicious Commands**: If user approves "rm -rf /", it will run
- **Session Hijacking**: If attacker gains access during session, cached approvals are valid
- **Social Engineering**: If Claude convinces user to approve bad commands
- **Code Injection**: This doesn't prevent vulnerabilities in tool implementations

### Best Practices

1. **Read Carefully**: Always read the full command before approving
2. **Be Conservative**: Use option 1 (approve once) for unfamiliar operations
3. **Review Context**: Check working directory and file paths
4. **Ask Questions**: If unsure, deny and ask Claude to explain
5. **Start Fresh**: Restart session to clear cache if concerned
6. **Trust Gradually**: Build up cached approvals over time

## Troubleshooting

### Prompt Not Appearing

**Symptom**: Tools execute without prompts

**Causes**:
- Running in non-interactive mode (piped input)
- Tool signature already approved in session cache
- Tool signature matches persistent pattern or exact approval

**Solution**:
- Check if stdout is a terminal: `finch` (not `echo "query" | finch`)
- View active patterns: `/patterns list`
- Clear session cache: restart Shammah
- Clear persistent patterns: `/patterns clear`

### Pattern Not Matching

**Symptom**: Expected pattern match, but still prompted

**Possible causes**:
1. **Tool name mismatch**: Pattern tool must match exactly (`bash` ≠ `save_and_exec`)
2. **Wildcard position**: `cargo * in *` matches `cargo test in /dir`, not `cargo in /dir test`
3. **Case sensitivity**: Patterns are case-sensitive unless using regex with `(?i)`
4. **Path format**: Absolute vs relative paths must match
5. **Regex syntax error**: Invalid regex won't match anything

**Solution**:
- Check pattern details: `/patterns list`
- Test with exact approval first (option 4)
- For debugging, remove pattern and recreate with corrected pattern string

### Invalid Input Loop

**Symptom**: "Invalid choice" message repeats

**Cause**: Entering invalid input (not 1-6, y, n, yes, no)

**Solution**: Enter a valid choice (1-6)

### Ctrl+C Not Working

**Symptom**: Can't exit prompt with Ctrl+C

**Cause**: Input handler may be blocking

**Solution**:
- Press Ctrl+D (EOF) to treat as denial
- Enter "6" to deny and continue

### Pattern File Corruption

**Symptom**: Error loading patterns on startup

**Cause**: `~/.finch/tool_patterns.json` is corrupted or has invalid JSON

**Solution**:
```bash
# Backup existing file
mv ~/.finch/tool_patterns.json ~/.finch/tool_patterns.json.bak

# Restart Shammah (creates fresh empty file)
finch

# If backup had important patterns, manually inspect and fix JSON
cat ~/.finch/tool_patterns.json.bak | jq .
```

### Unwanted Pattern Matches

**Symptom**: Pattern is too broad, matches unintended commands

**Cause**: Pattern with too many wildcards (e.g., `* in *`)

**Solution**:
1. Remove broad pattern: `/patterns rm <id>`
2. Create more specific patterns:
   - `cargo * in /project` instead of `* in *`
   - `reading /project/**` instead of `reading *`
3. Use regex for precise matching: `^cargo (test|build)$` instead of `cargo *`

## FAQ

**Q: Can I skip prompts for all tools?**
A: Yes, using patterns. Create broad patterns like `* in *` for each tool, though this is not recommended for security.

**Q: Do cached approvals persist across sessions?**
A: Depends on which option you choose:
- Options 2-3: Session only (cleared on exit)
- Options 4-5: Persistent (saved to disk, survive restarts)

**Q: Can I approve multiple similar commands at once?**
A: Yes, using pattern-based approvals (options 3 and 5). Choose a pattern that matches the commands you want to approve.

**Q: What if I accidentally approve something dangerous?**
A: The tool executes immediately. To prevent future executions:
- Session approvals: Restart Shammah
- Persistent approvals: Use `/patterns rm <id>` to remove the pattern

**Q: Can I review what's been approved?**
A: Yes, use `/patterns` or `/patterns list` to view all saved patterns and exact approvals with their match counts and timestamps.

**Q: Does this work in daemon mode?**
A: No. Daemon mode is non-interactive, so all tools execute automatically without prompts.

**Q: Can I share patterns between machines?**
A: Not directly, but you can copy `~/.finch/tool_patterns.json` between machines. Future enhancement may add import/export.

**Q: What's the difference between wildcard and regex patterns?**
A: Wildcards are simpler (`*` for any text), regex is more powerful (alternation, character classes, lookahead). See [Pattern Types](#pattern-types) section.

**Q: Can I test a pattern before saving it?**
A: Not currently. Best practice: use session patterns (option 3) first to test, then recreate as persistent (option 5) once confident.

**Q: How do I allow all cargo commands but prompt for other bash commands?**
A: Create a pattern for `cargo * in *` (option 5). All other bash commands will still prompt.

**Q: Can I have different patterns for different projects?**
A: Yes, use directory-specific patterns like `cargo * in /project1` and `cargo * in /project2`. The pattern matcher respects full paths.

## Quick Reference

### Approval Options

| Option | Name | Scope | Storage | Use Case |
|--------|------|-------|---------|----------|
| 1 | Approve once | Single execution | None | One-off operations, testing |
| 2 | Exact match (session) | Identical commands | Memory | Repetitive commands this session |
| 3 | Pattern (session) | Similar commands | Memory | Experimenting with patterns |
| 4 | Exact match (persistent) | Identical commands | Disk | Frequently-used specific command |
| 5 | Pattern (persistent) | Similar commands | Disk | Trust entire category of commands |
| 6 | Deny | None | None | Reject dangerous/unwanted operations |

### Pattern Commands

| Command | Purpose | Example |
|---------|---------|---------|
| `/patterns` | List all patterns | Shows IDs, match counts, timestamps |
| `/patterns list` | List all patterns | Alias for `/patterns` |
| `/patterns add` | Add pattern interactively | Prompts for tool, pattern, type |
| `/patterns rm <id>` | Remove pattern | `/patterns rm abc123-def` |
| `/patterns remove <id>` | Remove pattern | Alias for `rm` |
| `/patterns clear` | Remove all patterns | Requires confirmation |

### Pattern Syntax

| Pattern | Type | Example | Matches | Doesn't Match |
|---------|------|---------|---------|---------------|
| `*` | Wildcard | `cargo *` | `cargo test`, `cargo build` | `npm test` |
| `**` | Recursive | `/project/**` | Any file under `/project/` | Files outside `/project/` |
| Regex | Regex | `^cargo (test\|build)$` | `cargo test`, `cargo build` | `cargo run` |
| Exact | Wildcard | `cargo test` | `cargo test` only | `cargo test --all` |

### Match Priority

1. **Exact persistent** - option 4 matches
2. **Exact session** - option 2 matches
3. **Pattern persistent** - option 5 matches (most specific first)
4. **Pattern session** - option 3 matches (most specific first)
5. **Prompt** - no matches found

## Summary

The tool confirmation feature gives you fine-grained control over Claude's actions while maintaining a smooth interactive experience. Pattern-based approvals let you define categories of trusted commands, while exact approvals provide maximum specificity.

Key takeaways:
- ✓ You control what executes (6 approval options)
- ✓ Session and persistent caching options
- ✓ Pattern-based approvals for categories of commands
- ✓ Wildcard and regex pattern support
- ✓ Pattern management via `/patterns` commands
- ✓ Clear, context-specific signatures
- ✓ Match statistics and timestamps
- ✓ Non-interactive mode unaffected
- ✓ Persistent storage in `~/.finch/tool_patterns.json`
