# MemTree Console - Tree-Structured Conversation Interface

## Overview

The MemTree Console is a hierarchical, tree-structured conversation interface inspired by Claude Code's clean output format. It organizes conversation history as a navigable tree where you can expand/collapse sections and see the relationship between messages, responses, and tool calls.

## Architecture

### Core Components

**ConsoleNode** - A single node in the tree
```rust
pub struct ConsoleNode {
    pub id: NodeId,
    pub node_type: ConsoleNodeType,
    pub text: String,
    pub children: Vec<NodeId>,
    pub expanded: bool,
    pub depth: usize,
    pub timestamp: SystemTime,
}
```

**ConsoleNodeType** - Different types of nodes
- `UserMessage` - User queries (❯)
- `AssistantResponse` - AI responses (⏺)
- `ToolCall` - Tool executions (⏵)
- `ToolResult` - Tool outputs (✓/✗)
- `System` - System messages (ℹ)
- `Thinking` - Processing time (✻)

**MemTreeConsole** - Main console state
```rust
pub struct MemTreeConsole {
    tree: Arc<RwLock<MemTree>>,         // Underlying memory tree
    nodes: HashMap<NodeId, ConsoleNode>, // Quick lookup
    display_root: Option<NodeId>,        // Current view root
    selected: Option<NodeId>,            // Selected node
    scroll_offset: usize,                // Viewport scroll
    input_buffer: String,                // User input
}
```

## Visual Structure

```
❯ What is 2+2?                          ← User message (expandable)
  ⏺ Let me calculate that for you       ← Assistant response
    ⏵ Bash(python -c "print(2+2)")      ← Tool call (collapsed)
    ⏺ The answer is 4                   ← Assistant response
  ✻ Churned for 1.2s                     ← Thinking time

❯ How do I read a file?
  ⏺ You can use the Read tool
    ⏵ Read(example.txt)                 ← Tool call (collapsed)
      ✓ File contents loaded            ← Tool result (hidden)
```

## Navigation

- **↑/↓** - Move selection up/down through visible nodes
- **Space** - Expand/collapse selected node
- **Enter** - Execute selected action
- **Ctrl+O** - Expand all children of selected node

## Key Features

### 1. Hierarchical Structure
- User messages are top-level parents
- Responses and tool calls are children
- Can navigate into/out of branches

### 2. Expand/Collapse
- Tool calls collapsed by default (keep interface clean)
- Can expand to see details
- Respects hierarchy (collapsed parent hides all children)

### 3. Visual Indicators
Like Claude Code:
- **⏺** - Tool execution
- **⏵** - Expandable (collapsed)
- **▼** - Expandable (expanded)
- **❯** - User input
- **✻** - Thinking/processing
- **✓/✗** - Success/failure

### 4. Smart Truncation
Long output can be collapsed:
```
⏵ Bash(cargo build)
  ⎿ warning: unused variable
     error[E0433]: unresolved import
     … +42 lines (ctrl+o to expand)
```

## Implementation Status

✅ **Complete:**
- Basic tree structure (ConsoleNode, ConsoleNodeType)
- Node creation (user messages, responses, tool calls, tool results, thinking)
- Expand/collapse logic
- Visual rendering (icons, indentation, selection)
- Navigation (up/down, select)
- Integration with MemTree
- Event loop integration (EventHandler translates ReplEvent to tree operations)
- Tool execution integration (formats tool calls, displays results)

⚪ **Todo:**
- Input handling (multi-line input widget)
- Keyboard shortcuts (full navigation controls)
- Export/import tree structure
- Search within tree
- Persistence across sessions
- Wire up EventHandler in main REPL loop

## Usage Example

```rust
use finch::cli::MemTreeConsole;
use finch::memory::MemTree;
use std::sync::{Arc, RwLock};

// Create console
let tree = Arc::new(RwLock::new(MemTree::new()));
let mut console = MemTreeConsole::new(tree);

// Add user message
let user_id = console.add_user_message("Hello".to_string())?;

// Add response
let response_id = console.add_assistant_response(user_id, "Hi!".to_string())?;

// Add tool call
let tool_id = console.add_tool_call(
    response_id,
    "Read".to_string(),
    "file.txt".to_string()
)?;

// Navigate
console.select_next();      // Move down
console.toggle_selected();  // Expand/collapse

// Render
console.render(&mut frame, area);
```

## Comparison with Current REPL

| Feature | Current REPL | MemTree Console |
|---------|--------------|-----------------|
| Output format | Flat stream | Hierarchical tree |
| Tool calls | Inline | Expandable nodes |
| Navigation | Scroll only | Tree navigation |
| History | Linear | Branching |
| Context | Lost on scroll | Preserved in tree |
| Search | No | Planned |

## Next Steps

1. **Event Loop** - Integrate with existing REPL event system
2. **Tool Integration** - Hook up actual tool execution
3. **Input Handler** - Multi-line input with history
4. **Keyboard Shortcuts** - Full navigation controls
5. **Persistence** - Save/load tree state
6. **Search** - Find nodes by content

## File Locations

- **Core**: `src/cli/memtree_console/console.rs`
- **Event Handler**: `src/cli/memtree_console/event_handler.rs`
- **Module**: `src/cli/memtree_console/mod.rs`
- **Memory**: `src/memory/memtree.rs`
- **Tests**: Tests included in `console.rs` and `event_handler.rs`

## Testing

```bash
cargo test --lib cli::memtree_console
```

Current tests:
- ✅ Basic tree structure
- ✅ Expand/collapse
- ✅ Node creation
- ✅ Parent-child relationships
