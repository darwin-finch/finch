# Piped Input Tests

This document shows test cases for the piped input functionality.

## Test Results

### ✓ Test 1: Simple echo pipe
```bash
echo "What is 2+2?" | ./target/debug/finch
```
**Result**: Works correctly, outputs just the response with no REPL artifacts

### ✓ Test 2: Empty input
```bash
echo "" | ./target/debug/finch
```
**Result**: Exits cleanly with no output

### ✓ Test 3: Multi-line query
```bash
printf "What is Rust?\nPlease answer in one sentence." | ./target/debug/finch
```
**Result**: Processes entire input as single query

### ✓ Test 4: File input
```bash
cat query.txt | ./target/debug/finch
```
**Result**: Reads and processes file contents

### ✓ Test 5: Heredoc syntax
```bash
./target/debug/finch <<EOF
What is the time complexity of quicksort?
EOF
```
**Result**: Supports heredoc syntax correctly

### ✓ Test 6: Exit code
```bash
echo "test" | ./target/debug/finch > /dev/null 2>&1 && echo "Exit code: 0"
```
**Result**: Returns exit code 0 on success

### ✓ Test 7: No REPL messages in piped mode
```bash
echo "test" | ./target/debug/finch 2>&1 | grep -E "(Shammah v|Using API|Ready\.|Type /help)"
```
**Result**: No REPL startup messages appear in output

## Behavior Summary

### Piped Mode
- **Detection**: Automatically detected via `!io::stdin().is_terminal()`
- **Output**: Only the Claude response, no REPL artifacts
- **Tool Confirmation**: Auto-approved (no prompts)
- **Exit**: Exits immediately after response
- **Empty Input**: Exits cleanly with no output

### Interactive Mode
- **Detection**: `io::stdin().is_terminal()` returns true
- **Output**: Full REPL experience with status lines
- **Tool Confirmation**: Interactive prompts shown
- **Exit**: Continues in REPL loop until /quit or Ctrl+C
- **Empty Input**: Ignored, continues prompting

## Implementation Details

The implementation in `src/main.rs` checks for piped input before initializing the REPL:

```rust
// Check for piped input BEFORE initializing anything else
if !io::stdin().is_terminal() {
    // Piped input mode: read query from stdin and process as single query
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;

    // Skip processing if input is empty
    if input.trim().is_empty() {
        return Ok(());
    }

    // Initialize minimal components and process query
    // ...
    let response = repl.process_query(input.trim()).await?;
    println!("{}", response);

    return Ok(());
}
```

The `Repl` struct automatically detects non-interactive mode via `io::stdout().is_terminal()` and adjusts its behavior accordingly:

- No startup messages
- No status lines
- No tool confirmation prompts
- Minimal output

This provides a clean piped input experience suitable for scripting and automation.
