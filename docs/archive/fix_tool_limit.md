# Fix Tool Use Limit - Only Limit Repeated Same Commands

## Problem
Current tool use limit of 3 turns is too restrictive and prevents legitimate multi-tool workflows. It should only limit when the SAME tool is being executed repeatedly.

## Solution
Modify tool limiting logic to:
1. Track tool usage per tool name, not total tool count
2. Only limit when same tool is used repeatedly (e.g., 3+ times in a row)
3. Reset counter when different tool is used

## Files to Check/Modify

### 1. Find where tool limits are enforced:
```bash
grep -r "max_tool_turns\|tool.*turn\|tool.*limit" src/ --include="*.rs"
```

### 2. Look for tool counting logic:
```bash
grep -r "tool.*count\|turn.*count" src/ --include="*.rs"
```

### 3. Key files to examine:
- `src/tools/permissions.rs` (has max_tool_turns field)
- `src/tools/executor.rs` (likely has enforcement logic)
- `src/claude.rs` (might have tool turn tracking)
- `src/cli/repl.rs` (might track tool usage in conversations)

## Implementation Strategy

### Current Logic (to find):
```rust
// Something like:
if tool_count >= max_tool_turns {
    return Err("Too many tool uses");
}
```

### New Logic (to implement):
```rust
// Track per-tool usage
let mut consecutive_tool_usage: HashMap<String, usize> = HashMap::new();

// In tool execution:
let tool_name = tool_use.name.clone();
let count = consecutive_tool_usage.entry(tool_name.clone())
    .and_modify(|c| *c += 1)
    .or_insert(1);

if *count >= max_consecutive_same_tool {
    return Err(format!("Tool '{}' used {} times consecutively. Possible infinite loop.", tool_name, count));
}

// Reset other tools when different tool is used
consecutive_tool_usage.retain(|name, _| name == &tool_name);
```

## Testing
1. Verify multiple different tools can be used in sequence
2. Verify same tool used 3+ times gets limited
3. Verify tool counter resets when switching tools

## Commit Message
```
fix: only limit repeated same tool usage, not total tool count

- Change tool limiting from total turn count to consecutive same-tool count
- Allow unlimited different tools in sequence
- Prevent infinite loops by limiting same tool to 3 consecutive uses
- Reset counter when switching between different tools

Fixes overly restrictive tool use limits that prevented legitimate workflows.
```
