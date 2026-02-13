# Prompt Suggestions - User Guide

## Overview

Shammah provides contextual prompt suggestions to help users discover features and improve their workflow, inspired by Claude Code's suggestion system.

## How It Works

### Hardcoded Suggestions (Currently Active)

The system displays context-aware suggestions in the status bar based on your current state:

**First Run:**
```
💡 Type a question to get started • Try /help [Ctrl+/]
```

**After Query:**
```
💡 Rate response with 'g' (good) or 'b' (bad) • Ask a follow-up question
```

**During Streaming:**
```
💡 Press Ctrl+C to cancel streaming [Ctrl+C]
```

**On Error:**
```
💡 Try rephrasing your query • Check /local status
```

### Suggestion Contexts

The system automatically detects and responds to these contexts:

1. **FirstRun** - Just started the session
   - Onboarding tips
   - Feature discovery

2. **Idle** - No recent activity
   - Usage tips after 30+ seconds idle
   - Keyboard shortcuts

3. **QueryComplete** - Just finished a query
   - Feedback prompts (rate with 'g'/'b')
   - Follow-up suggestions

4. **QueryError** - Query failed
   - Troubleshooting tips
   - Status checks

5. **Streaming** - Response is streaming
   - Cancellation hints
   - Scroll tips

6. **ModelLoading** - Model downloading/loading
   - Status information
   - Patience messaging

7. **ToolExecution** - Tools are running
   - Progress info
   - Cancellation option

## LLM-Generated Suggestions (Future Enhancement)

### Architecture

The infrastructure supports LLM-generated suggestions inspired by Claude Code:

```rust
// Generate suggestion prompt
let prompt = SuggestionManager::get_suggestion_prompt(
    conversation_history,
    user_stated_intent,
);

// Send to teacher model
let response = teacher.query(prompt).await?;

// Parse and update
let suggestions = SuggestionManager::parse_llm_suggestions(&response);
suggestion_manager.update_llm_suggestions(suggestions);
```

### Integration Example

```rust
// In REPL event loop, after each query:
async fn generate_suggestions(
    suggestion_manager: &mut SuggestionManager,
    conversation: &ConversationHistory,
    teacher: &TeacherSession,
) {
    // Build conversation context
    let history = conversation.format_for_llm();

    // Get suggestion prompt
    let prompt = SuggestionManager::get_suggestion_prompt(&history, None);

    // Query teacher model (non-blocking)
    tokio::spawn(async move {
        if let Ok(response) = teacher.query_async(prompt).await {
            let suggestions = SuggestionManager::parse_llm_suggestions(&response);
            suggestion_manager.update_llm_suggestions(suggestions);
        }
    });
}
```

### System Prompt

Based on Claude Code's approach, the system uses a specialized prompt:

```
Based on the conversation history, generate 2-3 helpful next action
suggestions for the user.

Each suggestion should be:
- Actionable and specific
- Relevant to the current context
- Helpful for discovering features or improving workflow

Format each suggestion on a new line starting with a dash (-).

Conversation history:
[conversation context]

Suggestions:
```

### Enabling LLM Suggestions

```rust
// Switch from hardcoded to LLM mode
tui_renderer.suggestions.set_source(SuggestionSource::LLM);
```

The system will:
1. Use LLM suggestions if available and fresh (<60 seconds old)
2. Fall back to hardcoded if unavailable/stale
3. Automatically refresh when stale

## Configuration

Suggestions can be disabled in the config:

```toml
[suggestions]
enabled = false  # Default: true
```

Or programmatically:

```rust
tui_renderer.suggestions.set_enabled(false);
```

## API Reference

### SuggestionManager

```rust
// Create manager
let mut manager = SuggestionManager::new();

// Update context
manager.set_context(SuggestionContext::QueryComplete);

// Get suggestions
let suggestions = manager.get_suggestions(); // Vec<Suggestion>

// Get formatted line for status bar
let line = manager.get_suggestion_line(); // Option<String>

// Track activity
manager.increment_query_count();

// LLM integration
manager.set_source(SuggestionSource::LLM);
manager.update_llm_suggestions(vec![...]);
```

### TuiRenderer

```rust
// Update suggestion context
tui_renderer.update_suggestion_context(SuggestionContext::Idle);

// Record interaction (auto-updates to QueryComplete)
tui_renderer.record_interaction(query, response);
```

## Future Enhancements

- [ ] LLM integration in REPL event loop
- [ ] Clickable suggestions (auto-fill input on select)
- [ ] Suggestion history/cycling
- [ ] Per-context suggestion preferences
- [ ] Suggestion analytics (track which are most helpful)

## References

- [Claude Code System Prompts](https://github.com/Piebald-AI/claude-code-system-prompts)
- Source: `src/cli/suggestions.rs`
- Integration: `src/cli/tui/mod.rs`
- Status display: `src/cli/tui/status_widget.rs`
