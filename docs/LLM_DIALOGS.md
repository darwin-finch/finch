# LLM-Prompted User Dialogs - Implementation Guide

## Overview

This document describes Shammah's implementation of Claude Code-style LLM-prompted user dialogs, allowing the AI to request clarification or gather user preferences during execution.

## Architecture

### Design Philosophy

**AskUserQuestion is NOT a regular tool** - it's a special system capability that:
- Requires UI integration (cannot run headless)
- Blocks LLM execution until user responds
- Must be handled at the client level (not in tool executor)
- Uses existing DialogWidget infrastructure

### Data Flow

```
LLM Generates Request
    ↓
Parser detects AskUserQuestion call
    ↓
Event loop intercepts (before tool execution)
    ↓
TUI shows dialog(s) using DialogWidget
    ↓
User selects answers
    ↓
Answers packaged as AskUserQuestionOutput
    ↓
Returned to LLM as tool result
    ↓
LLM continues with user's choices
```

### Integration Points

1. **Tool Parser** (`src/models/tool_parser.rs`)
   - Detects AskUserQuestion in LLM output
   - Extracts JSON parameters

2. **Event Loop** (`src/cli/repl_event/event_loop.rs`)
   - Intercepts AskUserQuestion before regular tool execution
   - Special handling path for user dialogs

3. **TUI Renderer** (`src/cli/tui/mod.rs`)
   - `show_llm_question()` method to display dialogs
   - Collects answers sequentially (1-4 questions)
   - Returns HashMap<String, String> of answers

4. **LLM Dialogs Module** (`src/cli/llm_dialogs.rs`)
   - Core data structures (Question, QuestionOption)
   - Validation logic
   - Conversion to/from Dialog format

## API Reference

### Core Data Structures

```rust
/// A single option in a question
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

/// A question to ask the user
pub struct Question {
    pub question: String,        // Full question text
    pub header: String,           // Short header (max 12 chars)
    pub options: Vec<QuestionOption>, // 2-4 options
    pub multi_select: bool,       // Allow multiple selections?
}

/// Input from LLM
pub struct AskUserQuestionInput {
    pub questions: Vec<Question>, // 1-4 questions
}

/// Output returned to LLM
pub struct AskUserQuestionOutput {
    pub questions: Vec<Question>,        // Echo back
    pub answers: HashMap<String, String>, // question → answer(s)
}
```

### Validation Rules

- **Question count**: 1-4 questions per request
- **Option count**: 2-4 options per question
- **Header length**: Maximum 12 characters
- **Required fields**: question, header, label, description

### Example LLM Output

```xml
<tool_use>
<name>AskUserQuestion</name>
<parameters>{
  "questions": [
    {
      "question": "Which library should we use for state management?",
      "header": "State Lib",
      "options": [
        {
          "label": "Redux",
          "description": "Predictable state container with time-travel debugging"
        },
        {
          "label": "Zustand",
          "description": "Lightweight with minimal boilerplate"
        },
        {
          "label": "Jotai",
          "description": "Atomic state management with React"
        }
      ],
      "multi_select": false
    },
    {
      "question": "Which features should we include?",
      "header": "Features",
      "options": [
        {
          "label": "Authentication",
          "description": "User login and registration"
        },
        {
          "label": "Database",
          "description": "Persistent data storage"
        },
        {
          "label": "API",
          "description": "RESTful API endpoints"
        }
      ],
      "multi_select": true
    }
  ]
}</parameters>
</tool_use>
```

### Example Output (Single-Select)

```json
{
  "questions": [...],
  "answers": {
    "Which library should we use for state management?": "Zustand"
  }
}
```

### Example Output (Multi-Select)

```json
{
  "questions": [...],
  "answers": {
    "Which features should we include?": "Authentication, API"
  }
}
```

## Implementation Status

### ✅ Complete

1. **Core Data Structures** (`src/cli/llm_dialogs.rs`)
   - Question, QuestionOption, Input/Output types
   - Validation functions
   - Dialog conversion helpers
   - 8/8 unit tests passing

2. **Module Integration** (`src/cli/mod.rs`)
   - Module declared and exported
   - Types re-exported for use

### 🚧 In Progress

3. **Event Loop Integration** (`src/cli/repl_event/event_loop.rs`)
   - TODO: Detect AskUserQuestion in tool calls
   - TODO: Route to special handler instead of ToolExecutor
   - TODO: Package result and return to LLM

4. **TUI Integration** (`src/cli/tui/mod.rs`)
   - TODO: `show_llm_question(input)` method
   - TODO: Display questions sequentially
   - TODO: Collect answers into HashMap
   - TODO: Handle cancellation gracefully

### 📋 Future Enhancements

5. **Advanced Features**
   - Free-text input support (via "Other" option)
   - Question dependencies (skip based on previous answers)
   - Timeout handling (60 seconds per question)
   - Answer validation and constraints
   - Custom styling per question type

## Integration Guide

### Step 1: Detect AskUserQuestion Call

```rust
// In event loop, after LLM generates response
if let Some(tool_use) = extract_tool_use(&response) {
    if tool_use.name == "AskUserQuestion" {
        // Special handling
        let input: AskUserQuestionInput = serde_json::from_value(tool_use.input)?;
        let output = tui.show_llm_question(&input).await?;
        let result_json = serde_json::to_string(&output)?;

        // Return to LLM as tool result
        return ToolResult::success(tool_use.id, result_json);
    }
}
```

### Step 2: Display Dialogs in TUI

```rust
// In TuiRenderer
pub async fn show_llm_question(
    &mut self,
    input: &AskUserQuestionInput,
) -> Result<AskUserQuestionOutput> {
    // Validate input
    llm_dialogs::validate_input(input)?;

    let mut answers = HashMap::new();

    // Show each question sequentially
    for question in &input.questions {
        // Convert to Dialog
        let dialog = llm_dialogs::question_to_dialog(question);

        // Show dialog and wait for answer
        let result = self.show_dialog(dialog).await?;

        // Extract answer
        if let Some(answer) = llm_dialogs::extract_answer(question, &result) {
            answers.insert(question.question.clone(), answer);
        } else if result.is_cancelled() {
            anyhow::bail!("User cancelled dialog");
        }
    }

    Ok(AskUserQuestionOutput {
        questions: input.questions.clone(),
        answers,
    })
}
```

### Step 3: Return Result to LLM

```rust
// Format as tool result
let output_json = serde_json::to_string_pretty(&output)?;
let tool_result = ToolResult::success(tool_use_id, output_json);

// LLM receives:
{
  "questions": [...],
  "answers": {
    "Which library?": "Zustand",
    "Which features?": "Authentication, API"
  }
}
```

## User Experience

### Display Format

```
┌─────────────────────────────────────────────────┐
│ State Lib                                       │
├─────────────────────────────────────────────────┤
│ Which library should we use for state           │
│ management?                                     │
│                                                 │
│ > Redux - Predictable state container          │
│   Zustand - Lightweight with minimal           │
│   Jotai - Atomic state management              │
│                                                 │
│ [↑/↓] Navigate  [Enter] Select  [Esc] Cancel   │
└─────────────────────────────────────────────────┘
```

### Multi-Select Format

```
┌─────────────────────────────────────────────────┐
│ Features                                        │
├─────────────────────────────────────────────────┤
│ Which features should we include?               │
│                                                 │
│ [✓] Authentication - User login                │
│ [ ] Database - Persistent storage              │
│ [✓] API - RESTful endpoints                    │
│                                                 │
│ [Space] Toggle  [Enter] Confirm  [Esc] Cancel  │
└─────────────────────────────────────────────────┘
```

## Benefits

1. **Better UX**: No need for LLM to ask in chat and wait for text response
2. **Faster**: Structured choices are quicker than typing
3. **Clearer**: Users see all options upfront with descriptions
4. **Type-Safe**: Validated answers prevent parsing errors
5. **Reusable**: Same dialog infrastructure as tool confirmations

## Comparison with Claude Code

| Feature | Claude Code | Shammah |
|---------|-------------|---------|
| Question limit | 1-4 | 1-4 ✅ |
| Option limit | 2-4 | 2-4 ✅ |
| Multi-select | Yes | Yes ✅ |
| Free-text | Via "Other" | Planned |
| Timeout | 60 seconds | Planned |
| Nested questions | No | No |
| Subagent support | No | TBD |

## Testing

### Unit Tests

```bash
cargo test llm_dialogs
```

Tests cover:
- Input validation (question count, option count, header length)
- Dialog conversion (Question → Dialog)
- Answer extraction (DialogResult → String)
- Multi-select answer joining

### Integration Test Example

```rust
#[tokio::test]
async fn test_llm_question_flow() {
    let mut tui = TuiRenderer::new();

    let input = AskUserQuestionInput {
        questions: vec![Question {
            question: "Choose one".to_string(),
            header: "Choice".to_string(),
            options: vec![
                QuestionOption {
                    label: "Option A".to_string(),
                    description: "First option".to_string(),
                },
                QuestionOption {
                    label: "Option B".to_string(),
                    description: "Second option".to_string(),
                },
            ],
            multi_select: false,
        }],
    };

    // Simulate user selecting option 0
    let output = tui.show_llm_question(&input).await.unwrap();

    assert_eq!(output.answers.len(), 1);
    assert_eq!(output.answers["Choose one"], "Option A");
}
```

## Next Steps

1. **Event Loop Integration** (2-4 hours)
   - Add AskUserQuestion detection
   - Route to TUI handler
   - Return result to LLM

2. **TUI Integration** (2-4 hours)
   - Implement `show_llm_question()` method
   - Sequential dialog display
   - Answer collection

3. **Testing** (2 hours)
   - End-to-end integration tests
   - User acceptance testing
   - Edge case handling

4. **Documentation** (1 hour)
   - Update USER_GUIDE.md
   - Add examples to README.md
   - Document limitations

**Total Effort**: 8-12 hours (as estimated in STATUS.md)

## References

- Claude Agent SDK: https://platform.claude.com/docs/en/agent-sdk/user-input
- Existing dialog system: `src/cli/tui/dialog.rs`
- Tool confirmation system: `src/cli/repl_event/event_loop.rs`
