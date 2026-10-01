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
./scripts/test_brains.sh cargo test -p finch-brain --lib store
```

The Rust supervisor creates and owns one OS process group, retains its leader
until group termination, escalates TERM to KILL within a bound, proves the
group quiescent, and only then reaps and removes HOME. Launchers never signal
PIDs. Isolated commands reject daemon discovery, reuse, and auto-spawn. Test
code is trusted not to enable job control or call `setsid`, `setpgid`, or
`CommandExt::process_group`. The isolation self-test mechanically scans the
supervised launchers and daemon integration paths for those escape APIs.
Deliberately hostile same-UID code that evades that source contract is outside
this harness's boundary.

The disposable HOME is an unguessable mode-0700 directory and production
constructors receive its canonical path through the sealed proof. Path-based
Brain storage therefore relies on the trusted-child contract; it does not claim
resistance to a malicious same-UID child replacing ancestors. A parent-held
before/after digest still guards the real Brain store.

The inherited descriptor protocol reserves FD108 for the supervisor's sealed,
read-only proof backup, FD109 for proof-key peer authentication, and FD110/111
for the Brain/daemon listener backups. Rust restores the production proof FD9
from FD108 and then revalidates its access mode, inode metadata, signed content,
and supervisor ancestry before any constructor may read configuration or create
state. This explicit restore is required because a script interpreter may use a
low descriptor while reading a launcher; missing or mismatched backup authority
fails closed.

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
- Loopback networking
- A live teacher credential only for the ignored query smoke

The tests spawn `env!("CARGO_BIN_EXE_finch")`
(`tests/daemon_integration_test.rs`), so `cargo test` builds the binary it
needs; no separate `cargo build --release` is required.

Each test daemon uses the supervisor's disposable HOME, per-suite Unix socket,
inherited port-zero listener, and sealed random Brain password. Its RAII guard
stops and reaps only the direct child on ordinary returns; the process-group
supervisor remains authoritative for signal and failure teardown. The tests
never discover, probe, or reuse an ambient daemon.

Ignored remote Brain smokes consume the supervisor's inherited port-zero
listeners and sealed random credential; they never accept a caller-supplied
loopback endpoint. Ignored IPC smokes use the supervised daemon inside the
sealed HOME. The supervisor scrubs every ambient TCP endpoint and password.
Unix IPC validates the socket path before and after connect and authenticates
the connected peer as a member of the supervisor-owned process group.

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

### Daemon-owned Claude CLI Subscription session tests (`claude_cli_daemon_session_test.rs`, issue #1354)

Real spawned-daemon-subprocess, real Cap'n Proto IPC production-boundary tests for
`BrainService.claudeCliRound` — the daemon owns the `claude` process/MCP bridge socket while
tool execution/approval stay frontend-side. Drives a fake `claude` binary
(`FINCH_TEST_CLAUDE_CLI_BINARY`) through the real bridge-socket protocol, no real Claude
subscription login required:

```bash
./scripts/test_brains.sh cargo test --test claude_cli_daemon_session_test -- --ignored
```

1. **`production_boundary_claude_cli_round_fails_closed_on_a_mismatched_reattach`** - A
   reattaching frontend whose request doesn't answer the real pending call gets a named error;
   the parked call survives and still completes correctly afterward.
2. **`production_boundary_frontend_disconnect_mid_pending_tool_call_does_not_lose_the_session`** -
   The originating frontend disconnects while a tool call is parked; a fresh frontend resumes and
   completes it, with the underlying `claude` process invoked exactly once.
3. **`production_boundary_daemon_restart_kills_the_live_claude_child_with_no_orphan`** - A real
   SIGTERM to the daemon (the same signal `finch daemon-stop` sends) reaps its live, parked
   `claude` child with no orphan.

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

CI runs the same gates from `.github/workflows/ci.yml` (build, formatting,
clippy, and `cargo test --all-targets`, which compiles the integration tests
and their `CARGO_BIN_EXE_finch` daemon binary) and
`.github/workflows/issue-56-brain-isolation.yml`, which drives the isolation
harness and supervised tests through `./scripts/test_brains.sh`, including the
ignored daemon spawn/health smoke:

```bash
./scripts/test_brain_isolation.sh
./scripts/test_brains.sh cargo test --lib
./scripts/test_brains.sh cargo test --test '*'
./scripts/test_brains.sh cargo test --test daemon_integration_test test_daemon_spawn_and_health -- --exact --ignored
```

The broad `finch-brain` isolation-module process covers every ordinary and newly added isolation
test. Two proof-validation tests that mutate shared sealed-listener challenge state are explicit
exceptions: the broad process names each with `--skip`, and the workflow immediately runs each
skipped test in its own supervised `--exact` process. `scripts/check_ci_workflow_manifest.py` pins
that skip/exact pairing on both Ubuntu and macOS so a skipped test cannot silently lose coverage.

## Future Improvements

- [ ] Expand PTY-based TUI interaction tests
- [ ] Add performance/stress tests for daemon
- [ ] Add multi-client daemon tests
- [ ] Add TUI regression tests (screenshots?)
- [ ] Add tool execution integration tests
- [ ] Add session restore tests
