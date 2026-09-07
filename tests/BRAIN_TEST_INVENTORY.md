# Brain test and smoke isolation inventory

This inventory is enforced by `scripts/test_brain_isolation.sh`. A new shell
or integration entrypoint matching its Brain/daemon/IPC patterns fails the
isolation gate until it is reviewed and classified here.

## Supervisor infrastructure

- `scripts/test_brains.sh` is the only public generic runner. It execs the
  authenticated `finch-test-supervisor`.
- `scripts/lib/brain_test_isolation.sh` authenticates inherited proof and
  listener descriptors and re-execs unsupervised launchers through that runner.
- `scripts/test_brain_isolation.sh` is the adversarial self-test. It creates a
  fake guarded user store and invokes only its explicitly named supervisor.

## Supervised shell launchers

The following scripts call `brain_test_isolation_reexec_launcher` before they
create state, bind endpoints, discover daemons, or spawn Finch:

- `scripts/demo_boot.sh`
- `scripts/smoke_vm_wire_provider.sh`
- `scripts/stress_test.sh`
- `scripts/test_persistence.sh`
- `scripts/test_server.sh`
- `scripts/test_tool_passthrough.sh` (live, credentialed provider smoke)
- `scripts/test_tui_debug.sh`

The self-test invokes every launcher in proof-only mode, exercises both HTTP
launchers past re-exec with an inherited-listener fixture, and proves an
unrelated same-name process survives the TUI smoke.

## Delegating shell entrypoints

These match the enforced scan but call no
`brain_test_isolation_reexec_launcher` themselves, which is why they are absent
from the self-test's launcher-probe loop: they create nothing to isolate and
hand every process they are responsible for to a launcher that is isolated.

- `scripts/bench_startup_time_to_ready.sh` launches no Finch. It exports two
  `FINCH_BENCH_STARTUP_*` variables and `exec`s the cargo slot wrapper onto
  `scripts/test_brains.sh cargo test --release --test startup_time_to_ready`,
  so the only processes it causes are created by the authenticated supervisor
  and reaped with it. It constructs no Brain, binds no endpoint, performs no
  daemon discovery, and touches no path under the user's `~/.finch`; the Brain
  inventory it benchmarks is the synthetic one the test binary seeds in a
  disposable HOME. It matches the scan on `brain`, `daemon` and `cargo test`
  in its scope comments and in that one command line. An earlier revision did
  launch a pty child directly and clean up with `pkill -f`, which is the
  pid-signalling `AGENTS.md` forbids; the delegation is what replaced it.

## Rust test entrypoints

- `tests/daemon_integration_test.rs` fails closed without authenticated
  supervisor proof. Its daemon receives the sealed HOME, password, IPC socket,
  and inherited kernel-assigned listener.
- `tests/daemon_stdio_binding.rs` is non-Brain: it binds this test process's
  own stdout and stderr to a `tempfile` log and restores them on drop. It
  spawns nothing, so no process leaves the supervisor's group. It is a single
  test because libtest would otherwise run its cases in parallel threads over
  process-global descriptors.
- `tests/daemon_log_rotation.rs` is non-Brain: it drives the daemon log
  retention writer over a `tempfile` directory. It constructs no Brain, spawns
  no daemon, binds no endpoint, and never touches the user's Finch state.
- `tests/daemon_upgrade_preflight_test.rs` is non-Brain: it supplies an explicit
  `tempfile` stage and empty Brain root to a production preflight boundary.
- `tests/worker_integration_test.rs` is non-Brain: it drives stateless Axum
  handlers in-process and never launches or contacts a daemon.
- `tests/service_discovery_test.rs` is non-Brain and does not advertise a
  service. Its manual examples are not automated entrypoints.
- `tests/startup_time_to_ready.rs` drives interactive startup through a real
  pty. It constructs no Brain object: it writes `brains/<name>/events.jsonl`
  directories by hand under a `tempfile` HOME, writes that HOME's
  `config.toml` with `use_daemon = false`, and spawns one child, the built
  `finch` binary, with `HOME`, `HF_HOME` and the three XDG variables repointed
  into that directory, every provider credential removed by name, and every
  inherited `FINCH_BRAIN_TEST_*`/`FINCH_TEST_*` variable removed so the child
  cannot claim supervisor authority it was not given. It never reads or writes
  the user's Finch state. Because `use_daemon = false` the child performs no
  daemon discovery, spawns no daemon and issues no `GET /health`, and
  `FINCH_BRAIN_TEST_NO_AUTO_SPAWN=1` is set as well so an auto-spawn would
  fail closed rather than escape. The child is a plain `Command` spawn that
  calls none of the allowlisted session- or group-creating APIs, so it stays
  inside the supervisor's owned process group; `Session::drop` kills and reaps
  that one child by handle and signals no pid it did not create. It is a
  separate binary rather than a `src/` unit test because `Repl` takes its
  interactive branch only when stdout `is_terminal()`, which requires a pty
  slave on a child's three standard descriptors -- something a `#[cfg(test)]`
  function inside the library process cannot arrange for itself. Its
  `bench_startup_time_to_ready` case is `#[ignore]`d and runs only from the
  benchmark script above.
- `tests/no_external_provider_binary_test.rs` is the independent #173
  binary-removal regression. It uses its own `tempfile` HOME and process group;
  it neither constructs a Brain nor reads the user's Finch state.
- `tests/live.rs` and `tests/live/{impcpd,parity,providers}.rs` are ignored,
  credentialed live-provider tests. They do not construct Brains, and their
  documented invocation still uses `scripts/test_brains.sh` so config/cache
  reads occur under the disposable HOME.

Brain unit and protocol tests live inside `src/`. CI invokes their filters only
through `scripts/test_brains.sh`; production `BrainStore::new` and
`AgentServer::new` boundaries reject claimed but unauthenticated isolation
before creating state.
