# Integration Tests

This directory contains integration tests for Shammah's daemon and TUI features.

## Running Tests

### Brain-safe unit suite

Any test or smoke that can construct a named Brain must run through the
fail-closed isolation wrapper:

```bash
./scripts/test_brains.sh
```

The wrapper assigns an explicit disposable `HOME`, exposes its Brain root as
`FINCH_BRAIN_TEST_ROOT`, removes it whether the command succeeds or fails, and
compares the caller's real `~/.finch/brains` tree, file contents, node types,
symlink targets, and portable POSIX mode/owner/link/inode metadata before and
after the suite. ACLs, filesystem flags, and extended attributes are outside
this portable guard. It refuses to run if it cannot distinguish the disposable
home from the production home. To wrap a narrower test command, pass it as
arguments, for example:

```bash
./scripts/test_brains.sh cargo test --lib brain::store
```

Run the isolation harness's own regression checks with:

```bash
./scripts/test_brain_isolation.sh
```

### All Tests (excluding ignored)
```bash
./scripts/test_brains.sh cargo test --test '*'
```

### Daemon Integration Tests
```bash
./scripts/test_brains.sh cargo test --test daemon_integration_test -- --ignored
```

**Requirements:**
- Daemon binary built (`cargo build --release`)
- Ports 11440-11441 available
- Network access (localhost only)

### TUI Integration Tests
```bash
./scripts/test_brains.sh cargo test --test tui_integration_test
```

**Unit tests** (don't require daemon):
```bash
./scripts/test_brains.sh cargo test --test tui_integration_test --lib
```

**Full TUI tests** (require PTY):
```bash
./scripts/test_brains.sh cargo test --test tui_integration_test -- --ignored
```

## Test Categories

### Daemon Tests (`daemon_integration_test.rs`)

1. **`test_daemon_spawn_and_health`** - Verifies daemon can start and health endpoint responds
2. **`test_daemon_query`** - Tests full query flow through daemon
3. **`test_fallback_without_daemon`** - Verifies CLI falls back to teacher API when daemon is down
4. **`test_daemon_config_parsing`** - Validates config file parsing

### TUI Tests (`tui_integration_test.rs`)

1. **`test_tui_initialization`** - Verifies TUI starts without crashing
2. **`test_shadow_buffer_rendering`** - Tests shadow buffer implementation
3. **`test_message_wrapping`** - Validates ANSI-aware text wrapping
4. **`test_scrollback_buffer`** - Tests scrollback message storage
5. **`test_output_manager`** - Validates output routing and stdout control
6. **`test_non_interactive_mode`** - Ensures TUI is disabled for piped input

## Test Status

| Test | Status | Notes |
|------|--------|-------|
| Daemon spawn/health | ✅ Works | Requires daemon binary |
| Daemon query | ⚠️ Partial | Needs config management |
| Daemon fallback | ✅ Works | |
| Config parsing | ✅ Works | Unit test |
| TUI initialization | ⚠️ Limited | Needs PTY for full test |
| Shadow buffer | ✅ Works | Unit test |
| Message wrapping | ✅ Works | Unit test |
| Scrollback | ✅ Works | Unit test |
| Output manager | ✅ Works | Unit test |
| Non-interactive | ✅ Works | |

## Known Limitations

### TUI Testing
- **PTY Required**: Full interactive TUI tests need a pseudo-TTY
- **Manual Testing**: Complex TUI flows should be tested manually
- **Escape Codes**: Automated tests can't verify visual rendering

**Solutions:**
1. Use `expect` for scripted TUI interactions (see below)
2. Manual testing in real terminal
3. Unit tests for individual components (shadow buffer, wrapping, etc.)

### Daemon Testing
- **Port Conflicts**: Tests use ports 11440-11441 to avoid conflicts
- **Timing**: Some tests have sleep() for daemon startup
- **Config**: Tests need proper config file management

## Manual Testing Checklist

### Daemon Mode
```bash
# 1. Start daemon
finch daemon --bind 127.0.0.1:11435 &

# 2. Run CLI (should connect to daemon)
finch
# Expected: "✓ Connected to daemon"

# 3. Run query
> What is 2+2?
# Expected: "→ Using daemon for query"

# 4. Stop daemon
pkill -f "finch daemon"

# 5. Try query again
> What is 3+3?
# Expected: "⚠️ Daemon failed" → "→ Falling back to teacher API"
```

### TUI Mode
```bash
# 1. Run interactive REPL
finch

# 2. Verify TUI elements visible:
#    - Input area (bottom)
#    - Status bar
#    - Scrollback (Shift+PgUp)

# 3. Test commands
> /help
> /history
> /exit

# 4. Test streaming
> Write a haiku
# Verify: Text appears gradually (streaming)

# 5. Test shadow buffer
> Very long message that wraps across multiple lines...
# Verify: Text wraps cleanly, no overflow
```

## Using Expect for TUI Tests

Example expect script:
```tcl
#!/usr/bin/expect -f
set timeout 10

spawn finch

expect ">" { send "test query\r" }
expect ">" { send "/exit\r" }
expect eof
```

Run with:
```bash
./test_tui.exp
```

## CI/CD Integration

For automated testing in CI:

```yaml
# .github/workflows/test.yml
- name: Verify Brain isolation harness
  run: ./scripts/test_brain_isolation.sh
- name: Run unit tests
  run: ./scripts/test_brains.sh cargo test --lib

- name: Run integration tests (non-ignored)
  run: ./scripts/test_brains.sh cargo test --test '*'

- name: Run daemon tests
  run: |
    cargo build --release
    ./scripts/test_brains.sh cargo test --test daemon_integration_test -- --ignored
```

## Future Improvements

- [ ] Add expect-based TUI interaction tests
- [ ] Add config file fixture management
- [ ] Add performance/stress tests for daemon
- [ ] Add multi-client daemon tests
- [ ] Add TUI regression tests (screenshots?)
- [ ] Add tool execution integration tests
- [ ] Add session restore tests
