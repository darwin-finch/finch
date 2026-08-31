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

## Rust test entrypoints

- `tests/daemon_integration_test.rs` fails closed without authenticated
  supervisor proof. Its daemon receives the sealed HOME, password, IPC socket,
  and inherited kernel-assigned listener.
- `tests/daemon_log_rotation.rs` is non-Brain: it drives the daemon log
  retention writer over a `tempfile` directory. It constructs no Brain, spawns
  no daemon, binds no endpoint, and never touches the user's Finch state.
- `tests/daemon_upgrade_preflight_test.rs` is non-Brain: it supplies an explicit
  `tempfile` stage and empty Brain root to a production preflight boundary.
- `tests/worker_integration_test.rs` is non-Brain: it drives stateless Axum
  handlers in-process and never launches or contacts a daemon.
- `tests/service_discovery_test.rs` is non-Brain and does not advertise a
  service. Its manual examples are not automated entrypoints.
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
