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
- Loopback networking
- A live teacher credential only for the ignored query smoke

Each test daemon owns a temporary HOME, per-test Unix socket, and
kernel-assigned loopback port. Its RAII guard stops and reaps only the child it
spawned. The tests never discover or reuse an ambient daemon.

Ignored in-crate IPC/remote Brain smokes fail closed unless their owned fixture
is supplied explicitly with `FINCH_TEST_IPC_SOCKET`, `FINCH_TEST_DAEMON_ADDR`,
`FINCH_TEST_BRAIN_ADDR`, and `FINCH_TEST_BRAIN_PASSWORD` as applicable. Those
values must identify the disposable daemon launched inside the same wrapper;
the tests never fall back to standard Finch sockets, ports, or config.

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

Use `./scripts/test_tui_debug.sh` for the executable smoke. It owns the exact
Finch child PID and never sends a signal by process name.

## Test Categories

### Daemon Tests (`daemon_integration_test.rs`)

1. **`test_daemon_spawn_and_health`** - Verifies daemon can start and health endpoint responds
2. **`test_daemon_query`** - Tests full query flow through daemon
3. **`test_daemon_config_parsing`** - Validates isolated endpoint configuration

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
| Daemon query | 🔒 Ignored live smoke | Requires a teacher credential |
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

Use the repository PTY integration harness for scripted interaction and unit
tests for individual components such as shadow-buffering and wrapping. Do not
start an unowned interactive Finch process from a test.

### Daemon Testing
- **Endpoints**: daemon tests bind `127.0.0.1:0` and receive the actual address
  through an isolated test-only address file
- **Readiness**: tests poll their owned endpoint and fail on early child exit
- **Config**: each test writes config only under its disposable HOME

## Safe executable smoke checklist

### Daemon Mode
```bash
./scripts/test_server.sh
```

The launcher selects an ephemeral endpoint, waits for readiness, fails on HTTP
errors, and reaps only its own daemon. For the live provider/tool path, set the
required credential and run `./scripts/test_tool_passthrough.sh`.

### TUI Mode
```bash
./scripts/test_tui_debug.sh
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

- [ ] Expand PTY-based TUI interaction tests
- [ ] Add performance/stress tests for daemon
- [ ] Add multi-client daemon tests
- [ ] Add TUI regression tests (screenshots?)
- [ ] Add tool execution integration tests
- [ ] Add session restore tests
