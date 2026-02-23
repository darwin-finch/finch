# Plan Mode Redesign - Claude Code Style

**Goal:** Implement Claude Code's plan mode workflow - read-only exploration with approval-gated execution.

**User Request Context:**
- User can explicitly request planning: "Please create a plan for X"
- Claude can also enter plan mode on its own initiative when appropriate
- Plan mode is read-only until user approves
- Context is cleared on approval before execution

---

## Overview

### Current Problems
1. **Manual commands** - `/show-plan`, `/save-plan`, `/approve`, `/reject`, `/done` are unnecessary
2. **Manual toggle** - `/plan` toggle (Shift+Tab) is too rigid
3. **Command-driven** - Should be tool-driven, not command-driven
4. **No automatic detection** - No way for Claude to naturally enter/exit plan mode

### Desired Workflow

```
1. User requests or Claude decides to plan
   ↓
2. Claude calls EnterPlanMode tool
   → System restricts to read-only tools (Read, Glob, Grep, WebFetch)
   ↓
3. Claude explores codebase (read-only)
   - Uses Read/Glob/Grep to understand code
   - Uses AskUserQuestion for clarification
   - Develops implementation plan
   ↓
4. Claude calls PresentPlan tool with plan content
   → System shows approval dialog:
      [ ] Approve and execute (clears context, enables all tools)
      [ ] Modify plan (opens text input for changes)
      [ ] Reject plan (exit plan mode, return to normal)
   ↓
5a. If approved:
    - Clear conversation context
    - Enable all tools (Bash, Write, Edit, etc.)
    - Return approval to Claude
    - Claude executes the plan

5b. If modified:
    - Show text input dialog for user feedback
    - Keep plan mode active
    - Claude revises plan based on feedback
    - Go back to step 4

5c. If rejected:
    - Exit plan mode (enable all tools)
    - Return to normal conversation
```

---

## Phase 1: Dialog Enhancements (2-3 hours)

### Problem
Current dialogs auto-submit on selection. Claude Code uses:
- Checkboxes for options (single or multi-select)
- "Other" option that opens text input
- Explicit submit button (Enter key)

### Implementation

#### 1.1: Add "Other" Option Support

**File:** `src/cli/tui/dialog.rs`

Add field to DialogType variants:
```rust
pub enum DialogType {
    Select {
        options: Vec<DialogOption>,
        selected_index: usize,
        allow_custom: bool,  // NEW: Enable "Other" option
    },
    MultiSelect {
        options: Vec<DialogOption>,
        selected_indices: HashSet<usize>,
        cursor_index: usize,
        allow_custom: bool,  // NEW
    },
    // ... existing variants
}
```

Add custom text input state:
```rust
pub struct Dialog {
    pub title: String,
    pub help_message: Option<String>,
    pub dialog_type: DialogType,
    pub custom_input: Option<String>,  // NEW: Stores custom text if "Other" selected
}
```

#### 1.2: Add Submit Button to Dialogs

**Current behavior:** Selecting an option immediately returns result

**New behavior:**
- Arrow keys navigate options
- Space toggles selection (multi-select) or selects option (single-select)
- If "Other" is selected, show text input field below options
- Enter submits the dialog with selected options

**File:** `src/cli/tui/dialog.rs` - `handle_key_event()`

```rust
KeyCode::Enter => {
    // Check if "Other" is selected and has text
    if self.custom_input.is_some() {
        return Some(DialogResult::CustomText(self.custom_input.clone().unwrap()));
    }

    // Return selected options
    match &self.dialog_type {
        DialogType::Select { selected_index, .. } => {
            Some(DialogResult::Selected(*selected_index))
        }
        DialogType::MultiSelect { selected_indices, .. } => {
            Some(DialogResult::MultiSelected(selected_indices.iter().copied().collect()))
        }
        // ...
    }
}

KeyCode::Char('o') | KeyCode::Char('O') => {
    // Toggle "Other" input mode
    if self.custom_input.is_none() {
        self.custom_input = Some(String::new());
    } else {
        self.custom_input = None;
    }
    None
}
```

#### 1.3: Update Dialog Widget Rendering

**File:** `src/cli/tui/dialog_widget.rs`

Add rendering for custom input field:
```rust
fn render_select_with_custom(...) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // Render options
    for (idx, option) in options.iter().enumerate() {
        // ... existing rendering
    }

    // Add "Other" option if enabled
    if allow_custom {
        lines.push(Line::from(Span::styled(
            format!("{}. Other (specify below)", options.len() + 1),
            style
        )));
    }

    // Render custom input field if active
    if let Some(custom_text) = custom_input {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("> {}", custom_text),
            Style::default().fg(Color::Cyan)
        )));
    }

    lines
}
```

#### 1.4: Update DialogResult

**File:** `src/cli/tui/dialog.rs`

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum DialogResult {
    Selected(usize),
    MultiSelected(Vec<usize>),
    TextEntered(String),
    CustomText(String),  // NEW: Custom "Other" response
    Confirmed(bool),
    Cancelled,
}
```

**Testing:**
- Create test dialog with `allow_custom: true`
- Verify "Other" option appears
- Verify pressing 'o' toggles custom input
- Verify typing in custom input works
- Verify Enter submits with custom text

---

## Phase 2: Plan Mode Tools (2-3 hours)

### 2.1: Add EnterPlanMode Tool

**File:** `src/tools/implementations/enter_plan_mode.rs` (NEW)

```rust
use anyhow::Result;
use serde_json::Value;

/// EnterPlanMode - Claude signals it wants to enter read-only planning mode
pub struct EnterPlanModeTool;

impl EnterPlanModeTool {
    pub async fn execute(
        _input: Value,
        context: &mut super::ToolContext,
    ) -> Result<String> {
        // Set plan mode state in context
        context.plan_mode_active = true;
        context.plan_content = None;

        Ok("Entered plan mode. You can now explore the codebase using Read, Glob, Grep, and WebFetch tools. \
            When ready, use PresentPlan to show your plan for approval.".to_string())
    }

    pub fn definition() -> serde_json::Value {
        json!({
            "name": "EnterPlanMode",
            "description": "Enter read-only planning mode to explore codebase before making changes. \
                           In plan mode, only Read, Glob, Grep, and WebFetch tools are available. \
                           Use this when you need to research before proposing changes.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "reason": {
                        "type": "string",
                        "description": "Brief explanation of why planning is needed (optional)"
                    }
                },
                "required": []
            }
        })
    }
}
```

### 2.2: Add PresentPlan Tool

**File:** `src/tools/implementations/present_plan.rs` (NEW)

```rust
use anyhow::{Result, bail};
use serde_json::Value;

/// PresentPlan - Claude presents implementation plan for user approval
pub struct PresentPlanTool;

impl PresentPlanTool {
    pub async fn execute(
        input: Value,
        context: &mut super::ToolContext,
    ) -> Result<String> {
        // Verify we're in plan mode
        if !context.plan_mode_active {
            bail!("Not in plan mode. Use EnterPlanMode first.");
        }

        let plan_content = input["plan"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'plan' field"))?;

        // Store plan for potential revision
        context.plan_content = Some(plan_content.to_string());

        // Show approval dialog (blocking)
        let dialog = Dialog {
            title: "Approve Implementation Plan".to_string(),
            help_message: Some("Review the plan below and choose an action".to_string()),
            dialog_type: DialogType::Select {
                options: vec![
                    DialogOption::with_description(
                        "Approve",
                        "Clear context and execute this plan"
                    ),
                    DialogOption::with_description(
                        "Request changes",
                        "Provide feedback to revise the plan"
                    ),
                    DialogOption::with_description(
                        "Reject",
                        "Exit plan mode without executing"
                    ),
                ],
                selected_index: 0,
                allow_custom: true,  // Enable "Other" option
            },
            custom_input: None,
        };

        // Show plan content as output message
        context.output_manager.write_section("📋 Implementation Plan", plan_content);

        // Show dialog and wait for response
        let result = context.show_dialog(dialog)?;

        match result {
            DialogResult::Selected(0) => {
                // Approved - clear context and exit plan mode
                context.conversation.clear();
                context.plan_mode_active = false;

                Ok("Plan approved! Context has been cleared. You can now execute the plan using all available tools.".to_string())
            }
            DialogResult::Selected(1) | DialogResult::CustomText(_) => {
                // Request changes - stay in plan mode
                let feedback = if let DialogResult::CustomText(text) = result {
                    text
                } else {
                    "Please revise the plan based on user feedback.".to_string()
                };

                Ok(format!("User feedback: {}\n\nPlease revise the plan and call PresentPlan again when ready.", feedback))
            }
            DialogResult::Selected(2) | DialogResult::Cancelled => {
                // Rejected - exit plan mode
                context.plan_mode_active = false;
                context.plan_content = None;

                Ok("Plan rejected. Exited plan mode.".to_string())
            }
            _ => bail!("Unexpected dialog result"),
        }
    }

    pub fn definition() -> serde_json::Value {
        json!({
            "name": "PresentPlan",
            "description": "Present your implementation plan to the user for approval. \
                           The plan should include: what changes will be made, which files will be modified, \
                           and step-by-step execution order. User can approve (context is cleared), \
                           request changes (you can revise), or reject (exit plan mode).",
            "input_schema": {
                "type": "object",
                "properties": {
                    "plan": {
                        "type": "string",
                        "description": "Detailed implementation plan in markdown format. \
                                      Should include: overview, affected files, step-by-step changes, \
                                      testing strategy, and potential risks."
                    }
                },
                "required": ["plan"]
            }
        })
    }
}
```

### 2.3: Update ToolContext

**File:** `src/tools/executor.rs`

Add plan mode state to context:
```rust
pub struct ToolContext {
    // ... existing fields
    pub plan_mode_active: bool,
    pub plan_content: Option<String>,
}
```

### 2.4: Update Tool Executor

**File:** `src/tools/executor.rs` - `execute_tool()`

Add tool restriction check:
```rust
pub async fn execute_tool(&self, tool_use: &ToolUse) -> Result<String> {
    let context = self.context.read().await;

    // Check if tool is allowed in current mode
    if context.plan_mode_active {
        let read_only_tools = ["Read", "Glob", "Grep", "WebFetch", "EnterPlanMode", "PresentPlan", "AskUserQuestion"];

        if !read_only_tools.contains(&tool_use.name.as_str()) {
            anyhow::bail!(
                "Tool '{}' is not allowed in plan mode. \
                 Only read-only tools are available: Read, Glob, Grep, WebFetch. \
                 To execute this tool, present your plan using PresentPlan and get user approval.",
                tool_use.name
            );
        }
    }

    // ... rest of execution
}
```

### 2.5: Register New Tools

**File:** `src/tools/implementations/mod.rs`

```rust
pub mod enter_plan_mode;
pub mod present_plan;

// Add to tool registry
pub fn get_tool_definitions() -> Vec<Value> {
    vec![
        // ... existing tools
        enter_plan_mode::EnterPlanModeTool::definition(),
        present_plan::PresentPlanTool::definition(),
    ]
}
```

**Testing:**
- Ask Claude: "Please create a plan to add a new feature"
- Verify Claude calls EnterPlanMode
- Verify only read-only tools work
- Verify Claude can call PresentPlan
- Verify approval dialog shows with 3 options
- Verify "Other" option opens text input
- Verify approval clears context

---

## Phase 3: Remove Old Plan Mode Code (1 hour)

### 3.1: Remove Command Handlers

**File:** `src/cli/repl_event/event_loop.rs`

Remove methods:
- `handle_approve_command()` (line ~1324)
- `handle_reject_command()` (line ~1442)
- `handle_show_plan_command()` (line ~1466)
- `handle_save_plan_command()` (line ~1494)
- `handle_done_command()` (line ~1537)

Remove command handling in main loop:
```rust
// DELETE these cases:
Command::Approve => { self.handle_approve_command().await?; }
Command::Reject => { self.handle_reject_command().await?; }
Command::ShowPlan => { self.handle_show_plan_command().await?; }
Command::SavePlan => { self.handle_save_plan_command().await?; }
Command::Done => { self.handle_done_command().await?; }
```

### 3.2: Remove Command Enum Variants

**File:** `src/cli/commands.rs`

Remove from Command enum:
```rust
// DELETE these:
Approve,
Reject,
ShowPlan,
SavePlan,
Done,
```

Remove from parsing:
```rust
// DELETE these lines:
"/approve" | "/execute" => return Some(Command::Approve),
"/reject" | "/cancel" => return Some(Command::Reject),
"/show-plan" => return Some(Command::ShowPlan),
"/save-plan" => return Some(Command::SavePlan),
"/done" | "/complete" => return Some(Command::Done),
```

### 3.3: Remove Manual Plan Mode Toggle

**File:** `src/cli/repl_event/event_loop.rs`

Remove or simplify:
- `Command::PlanModeToggle` handler
- `update_plan_mode_indicator()` method

**Decision:** Keep `/plan <task>` as a hint to Claude?
- Option A: Remove entirely (user just asks naturally)
- Option B: Keep `/plan <task>` to inject hint into conversation
  - Converts to: "Please create an implementation plan for: <task>"
  - Claude sees this and decides to call EnterPlanMode

**Recommendation:** Keep `/plan <task>` as syntax sugar for explicitly requesting plans.

### 3.4: Remove ReplMode Enum

**File:** `src/cli/repl.rs`

Plan mode state is now in ToolContext, not in REPL mode enum.

Remove:
```rust
pub enum ReplMode {
    Normal,
    Planning { task: String, plan_file: PathBuf },
    Executing { task: String, plan_file: PathBuf },
}
```

Replace with simpler state tracking if needed, or rely entirely on ToolContext.

---

## Phase 4: Integration & Testing (1-2 hours)

### 4.1: Test Scenarios

**Test 1: User-initiated planning**
```
User: Please create a plan to add a new logging system
→ Claude calls EnterPlanMode
→ Claude uses Read/Glob to explore code
→ Claude calls AskUserQuestion: "Which logging library?"
→ User selects from dialog
→ Claude calls PresentPlan with detailed plan
→ Approval dialog shows
→ User approves
→ Context cleared
→ Claude executes plan with Write/Edit/Bash tools
```

**Test 2: Claude-initiated planning**
```
User: Add error handling to the API
→ Claude decides to plan first
→ Claude calls EnterPlanMode
→ Claude explores codebase
→ Claude calls PresentPlan
→ User requests changes via "Other" option
→ Claude revises plan
→ Claude calls PresentPlan again
→ User approves
→ Execution proceeds
```

**Test 3: Plan rejection**
```
User: Refactor the authentication system
→ Claude calls EnterPlanMode
→ Claude explores and creates plan
→ Claude calls PresentPlan
→ User rejects
→ Plan mode exits
→ Normal conversation resumes
```

**Test 4: Tool restrictions**
```
→ Claude in plan mode
→ Claude tries to call Write tool
→ Error: "Tool 'Write' is not allowed in plan mode"
→ Claude continues with read-only tools
```

### 4.2: Edge Cases

- User interrupts planning (Ctrl+C)
- Network error during plan presentation
- Multiple plan revisions (user keeps requesting changes)
- Context clearing fails (show error, don't execute)
- Tool restrictions bypassed (security check)

---

## Files to Create/Modify

### New Files
- `src/tools/implementations/enter_plan_mode.rs` (80 lines)
- `src/tools/implementations/present_plan.rs` (150 lines)
- `docs/PLAN_MODE_REDESIGN.md` (this file)

### Modified Files
- `src/cli/tui/dialog.rs` - Add custom input support (50 lines changed)
- `src/cli/tui/dialog_widget.rs` - Render custom input (40 lines changed)
- `src/tools/executor.rs` - Add plan mode state + restrictions (60 lines changed)
- `src/tools/implementations/mod.rs` - Register new tools (10 lines)
- `src/cli/repl_event/event_loop.rs` - Remove old handlers (200 lines removed)
- `src/cli/commands.rs` - Remove command variants (30 lines removed)
- `src/cli/repl.rs` - Remove ReplMode enum (50 lines removed)

### Total Effort
- Phase 1 (Dialog enhancements): 2-3 hours
- Phase 2 (Plan mode tools): 2-3 hours
- Phase 3 (Remove old code): 1 hour
- Phase 4 (Testing): 1-2 hours
- **Total: 6-9 hours**

---

## Success Criteria

1. ✅ User can say "Please create a plan for X" naturally
2. ✅ Claude can decide to enter plan mode on its own
3. ✅ Only read-only tools work in plan mode
4. ✅ Claude presents plan via PresentPlan tool
5. ✅ Approval dialog has "Approve", "Request changes", "Reject", and "Other" options
6. ✅ "Other" option opens text input for custom feedback
7. ✅ Approving clears context before execution
8. ✅ Rejecting exits plan mode cleanly
9. ✅ No manual commands needed (/approve, /reject, etc.)
10. ✅ Works seamlessly with existing AskUserQuestion dialogs

---

## Future Enhancements

- **Plan visualization:** Show plan as structured markdown with syntax highlighting
- **Plan diff view:** Show before/after file diffs in plan
- **Plan history:** Save approved plans for reference
- **Multi-step plans:** Break large plans into phases with intermediate approvals
- **Plan templates:** Common patterns (refactor, new feature, bug fix)
- **Risk assessment:** Claude highlights high-risk changes in plan

---

## Notes

- Plan mode is entirely Claude-driven via tool calls
- No special state management in REPL needed
- Context clearing is automatic and safe
- Tool restrictions enforced at executor level
- Dialogs match Claude Code UX (checkboxes + submit)
- Custom "Other" option allows flexible user feedback
- Works with both user-requested and Claude-initiated planning
