# UI Improvements - Claude Code-Style Interface

## New REPL Interface

The CLI now matches Claude Code's visual style with real-time training status.

### Startup Screen

```
Shammah v0.1.0 - Constitutional AI Proxy
Using API key from: ~/.shammah/config.toml ✓
Loaded 10 constitutional patterns ✓
Loaded crisis detection keywords ✓
Online learning: ENABLED (threshold models) ✓

Ready. Type /help for commands.
Training: 0 queries | Local: 0% | Forward: 0% | Success: 0% | Confidence: 0.95 | Approval: 0%

──────────────────────────────────────────────────────────────────────
>
```

### Query Interaction Example

```
──────────────────────────────────────────────────────────────────────
> What is Rust?
──────────────────────────────────────────────────────────────────────

✓ Crisis check: PASS
✗ Pattern match: NONE
→ Routing: FORWARDING TO CLAUDE
✓ Received response (1,240ms)

Learning... ✓

Rust is a systems programming language that focuses on safety, speed,
and concurrency. It achieves memory safety without garbage collection...

Training: 1 queries | Local: 0% | Forward: 100% | Success: 0% | Confidence: 0.95 | Approval: 100%

──────────────────────────────────────────────────────────────────────
> Hello!
──────────────────────────────────────────────────────────────────────

✓ Crisis check: PASS
✓ Pattern match: reciprocity (0.94)
→ Routing: LOCAL (12ms)

Learning... ✓

This relates to reciprocity dynamics - how the way we treat others
creates expectations of how we'll be treated in return...

Training: 2 queries | Local: 50% | Forward: 50% | Success: 50% | Confidence: 0.95 | Approval: 100%

──────────────────────────────────────────────────────────────────────
>
```

## Key Features

### Claude Code-Style Prompt
- **Separator lines** above and below each query (70 chars)
- **"> " prompt** instead of "You: "
- **Clean visual separation** between queries

### Real-Time Training Status
Displayed below the prompt after each interaction:

| Metric | Description | Example |
|--------|-------------|---------|
| **Training** | Total queries processed | `42 queries` |
| **Local** | Percentage handled locally | `30%` |
| **Forward** | Percentage forwarded to Claude | `70%` |
| **Success** | Local attempt success rate | `85%` |
| **Confidence** | Router confidence threshold | `0.92` |
| **Approval** | Validator approval rate | `95%` |

### Visual Feedback

**Analysis phase:**
```
Analyzing...
```
(Shows while routing decision is being made, then clears)

**Routing results:**
```
✓ Crisis check: PASS
✓ Pattern match: reciprocity (0.94)
→ Routing: LOCAL (12ms)
```

**Learning confirmation:**
```
Learning... ✓
```
(Shows models are updating in real-time)

### Color Coding

- **Gray** (`\x1b[90m`): Status line and system messages
- **Default** (`\x1b[0m`): User content and responses
- **Success** (✓): Passed checks
- **Warning** (⚠️): Crisis detected
- **Info** (→): Routing decision

## Training Visibility

Users can now see:
1. **How many queries** have been processed
2. **Local vs Forward ratio** evolving over time
3. **Success rate** of local attempts
4. **Confidence threshold** adapting
5. **Approval rate** from validator

This provides transparency into the online learning process and helps users understand when the system is ready for more local processing.

## Example Session Progress

```
Query 1:  Training: 1 queries  | Local: 0%   | Forward: 100% | Confidence: 0.95
Query 10: Training: 10 queries | Local: 10%  | Forward: 90%  | Confidence: 0.95
Query 50: Training: 50 queries | Local: 20%  | Forward: 80%  | Confidence: 0.92
Query 100: Training: 100 queries | Local: 35% | Forward: 65% | Confidence: 0.88
Query 200: Training: 200 queries | Local: 45% | Forward: 55% | Confidence: 0.82
```

As the system learns:
- **Local%** increases (handling more queries locally)
- **Forward%** decreases (less reliance on Claude API)
- **Confidence** threshold adapts based on performance
- **Success%** stabilizes as patterns are learned
