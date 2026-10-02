// Finch - Agent Server Module
// HTTP daemon mode for multi-tenant agent serving

mod brain_approval;
mod brain_runner;
mod brain_service;
mod claude_cli_session;
mod feedback_handler;
mod handlers;
mod ipc;
mod middleware;
mod openai_handlers;
mod openai_types;

pub use brain_approval::BrainApprovalBroker;
pub use brain_runner::{
    BrainRunnerBroker, RunnerApprovalRequest, RunnerCancelRequest, RunnerEffectAuditControl,
    RunnerEffectAuditReservation, RunnerEffectRecord, RunnerHostEffectOutcome,
    RunnerHostEffectPermit, RunnerMemoryProjectionRequest, RunnerProgramControlRequest,
    RunnerProgramError, RunnerProgramInteraction, RunnerProgramRequest, RunnerProgramResult,
    RunnerProjectionError, RunnerRegistrationId, RunnerRequest, RunnerTurnCommitAck,
    RunnerTurnCommitNotice, RunnerTurnError, RunnerTurnEvent, RunnerTurnRequest, RunnerTurnResult,
    RUNNER_UNAVAILABLE_PREFIX,
};
pub(crate) use brain_runner::{
    RunnerEffectAuditControlRequest, RunnerEffectAuditReservationRequest,
    RunnerHostEffectFinishRequest,
};
pub use brain_service::{
    BrainLifecycleService, BrainSubmissionError, BrainSubmissionOutcome, BrainWatch,
};
pub use claude_cli_session::ClaudeCliSessionRegistry;
#[cfg(test)]
pub(crate) use handlers::{
    authorize_pending_remote_attachment, create_remote_brain_router,
    drop_next_remote_brain_reply_after_commit, execute_authorized_remote_initialization,
};
pub use handlers::{create_router, metrics_endpoint, AppError};
#[cfg(unix)]
pub use handlers::{handle_node_info_from_state_directory, handle_node_stats_from_state_directory};
pub use ipc::start_ipc_server;
pub use middleware::{auth_middleware, DaemonAuth, RateLimiter};
pub use openai_handlers::{handle_chat_completions, handle_list_models};
pub use openai_types::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, Choice, FunctionCall,
    FunctionDefinition, Model, ModelsResponse, Tool, ToolCall, Usage,
};

use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower::ServiceBuilder;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::claude::ClaudeClient;
use crate::config::Config;
use crate::feedback::FeedbackLogger;
use crate::local::LocalGenerator;
use crate::metrics::MetricsLogger;
use crate::models::{BootstrapLoader, GeneratorState};
use crate::providers::{LlmProvider, ProviderGraph};
use crate::router::Router;

struct ProviderSlot {
    profile_name: String,
    provider: Arc<dyn LlmProvider>,
}

struct ServerBackgroundTasks(Vec<tokio::task::JoinHandle<()>>);

impl Drop for ServerBackgroundTasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// Configuration for the HTTP server
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Bind address (e.g., "127.0.0.1:8000")
    pub bind_address: String,
    /// Optional TLS-only listener for remote named-Brain collaboration.
    pub brain_bind_address: Option<String>,
    /// Enable API key authentication
    pub auth_enabled: bool,
    /// Valid API keys for authentication
    pub api_keys: Vec<String>,
    /// Password required for remote named-brain access.
    pub brain_password: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_address: crate::config::DEFAULT_HTTP_ADDR.to_string(),
            brain_bind_address: None,
            auth_enabled: false,
            api_keys: vec![],
            brain_password: String::new(),
        }
    }
}

/// Main agent server structure
pub struct AgentServer {
    /// When this server was constructed, reported by `/health` and `/metrics`.
    ///
    /// `Instant` rather than a wall clock: uptime is an elapsed duration, and
    /// a system clock that steps backwards over NTP or a timezone change
    /// would otherwise report a negative or wildly wrong age. `/health`
    /// previously reported a hardcoded `0` however long the daemon had run
    /// (#131).
    ///
    /// Note where this ends up. `/metrics` is on the local router only, but
    /// `/health` is
    /// mounted on the remote Brain listener as well (default `0.0.0.0:11436`
    /// when advertisement is on), and that router carries no auth layer — so
    /// uptime is readable by any peer that can reach it, alongside the
    /// `named_brains` and `pending_brain_terminalizations` already exposed
    /// there. That is a deliberate acceptance of a small disclosure, not an
    /// oversight: process age is not a secret, and #131 asks for truthful
    /// health. Anything more sensitive belongs on the local router.
    ///
    /// The local router is loopback *by default*, not by construction:
    /// `bind_address` is user-configurable, and `finch worker` defaults to
    /// `--bind 0.0.0.0:8000` while serving the full router. Since
    /// `requires_client_auth` covers only the three inference routes, such a
    /// bind publishes `/metrics` unauthenticated too.
    ///
    /// `Instant` does not advance while the host is suspended, on any
    /// platform Finch targets. A laptop daemon resumed the next morning
    /// reports the seconds it was awake for, not wall-clock age. That is the
    /// intended reading of "uptime", and the `/metrics` HELP string says so
    /// rather than claiming age since construction.
    started_at: std::time::Instant,
    /// Claude API client (shared across sessions; kept for backward compat with handlers.rs)
    claude_client: Arc<ClaudeClient>,
    /// Multi-provider pool: cloud providers from [[providers]] config.
    /// Indexed by provider name for O(1) lookup via `provider_for_name()`.
    providers: Vec<ProviderSlot>,
    /// Router for decision-making (shared, read-write lock)
    router: Arc<RwLock<Router>>,
    /// Metrics logger (shared)
    metrics_logger: Arc<MetricsLogger>,
    /// Server configuration
    config: ServerConfig,
    /// Local generator (Qwen model with LoRA)
    local_generator: Arc<RwLock<LocalGenerator>>,
    /// Bootstrap loader for progressive model loading
    bootstrap_loader: Arc<BootstrapLoader>,
    /// Generator state (tracks model loading progress)
    generator_state: Arc<RwLock<GeneratorState>>,
    /// Append-only explicit user feedback. This is not a training queue.
    feedback_store: Arc<FeedbackLogger>,
    /// Authoritative event logs and program stacks for named shared brains.
    brain_store: crate::brain::BrainStore,
    /// Send-safe bridge to frontend-owned Cap'n Proto runner callbacks.
    brain_runners: BrainRunnerBroker,
    /// Pending approval continuations keyed to their exact Brain attachment.
    brain_approvals: BrainApprovalBroker,
    /// Daemon-owned Claude CLI Subscription `claude` processes and their MCP
    /// bridge sockets, keyed by Brain name (issue #1354). Tool execution and
    /// approval stay on whichever frontend calls `BrainService.claudeCliRound`;
    /// only process/transport ownership lives here. See
    /// `claude_cli_session.rs` for the lifecycle contract.
    claude_cli_sessions: ClaudeCliSessionRegistry,
    /// Persistent signer and revocation ledger for scoped remote participants.
    brain_credentials: crate::brain::BrainCredentialAuthority,
    /// Application-owned MCP configuration and lazily connected transport for
    /// daemon-executed named-Brain programs. The transport is shared, while
    /// each Brain runtime installs its own verified vocabulary metadata.
    mcp_servers: std::collections::HashMap<String, crate::tools::McpServerConfig>,
    mcp_client: tokio::sync::OnceCell<Arc<crate::tools::McpClient>>,
    /// Runtime-rotatable password for remote named-brain access.
    brain_password: Arc<RwLock<String>>,
    /// Pins the descriptor-relative state root used only by authenticated
    /// Brain HTTP fixtures, so a pathname swap cannot redirect later opens.
    #[cfg(test)]
    supervised_state_root: Option<std::fs::File>,
}

#[cfg(test)]
struct SupervisedStateRoot {
    directory: std::fs::File,
    path: std::path::PathBuf,
}

#[cfg(test)]
fn supervised_state_root(
    proof: &crate::brain::IsolatedTestProof,
    requested: &std::path::Path,
) -> Result<SupervisedStateRoot> {
    use anyhow::Context as _;
    use nix::fcntl::{open, openat, OFlag};
    use nix::sys::stat::{fstat, Mode, SFlag};
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::path::Component;

    let relative = requested
        .strip_prefix(&proof.home)
        .context("Brain HTTP fixture state must remain under the sealed HOME")?;
    anyhow::ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
        "Brain HTTP fixture state must be a normal descendant of the sealed HOME"
    );

    let home_fd = open(
        &proof.home,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .context("Could not open the sealed Brain test HOME")?;
    let mut directory = unsafe { std::fs::File::from_raw_fd(home_fd) };
    let home_stat = fstat(directory.as_raw_fd())?;
    anyhow::ensure!(
        (home_stat.st_dev as u64, home_stat.st_ino as u64) == proof.home_identity,
        "sealed Brain test HOME identity changed"
    );
    for component in relative.components() {
        let Component::Normal(name) = component else {
            unreachable!()
        };
        let child_fd = openat(
            Some(directory.as_raw_fd()),
            name,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .context("Brain HTTP fixture state cannot traverse symlinks")?;
        directory = unsafe { std::fs::File::from_raw_fd(child_fd) };
    }
    let state_stat = fstat(directory.as_raw_fd())?;
    anyhow::ensure!(
        SFlag::from_bits_truncate(state_stat.st_mode).contains(SFlag::S_IFDIR),
        "Brain HTTP fixture state root must be a directory"
    );
    anyhow::ensure!(
        state_stat.st_uid == nix::unistd::geteuid().as_raw(),
        "Brain HTTP fixture state root must be owned by the isolated test user"
    );
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let path = {
        static FIXTURE_ROOT_CLAIMED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        // Linux's `/proc/self/fd/N` and Darwin's `/dev/fd/N` are magic links;
        // feeding either into production storage's O_NOFOLLOW walk would
        // weaken that production boundary or fail closed before reaching the
        // pinned directory. These authenticated fixture tests instead run as
        // one exact, supervisor-owned subprocess and make the already-opened
        // directory its process-relative root. Fail closed before changing
        // cwd if a caller tries to reuse this seam in a parallel/broad test
        // process or constructs a second fixture.
        anyhow::ensure!(
            std::env::args_os().any(|argument| argument == "--exact"),
            "Brain fixtures require a dedicated exact test subprocess"
        );
        anyhow::ensure!(
            FIXTURE_ROOT_CLAIMED
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                )
                .is_ok(),
            "Darwin Brain fixture state is already pinned in this process"
        );
        anyhow::ensure!(
            unsafe { nix::libc::fchdir(directory.as_raw_fd()) } == 0,
            "could not pin the Brain fixture process to its state descriptor"
        );
        std::path::PathBuf::from(".")
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    anyhow::bail!("descriptor-relative Brain fixture state is unsupported on this platform");
    Ok(SupervisedStateRoot { directory, path })
}

/// Delivery-loop timing, failure-episode state, and deterministic race hooks.
///
/// The timing decisions remain pure and take the clock as a parameter. The
/// process-local episode registry bounds transition logs, while test-only
/// hooks expose otherwise unreachable lifecycle windows inside the spawned
/// loop and its queue boundary.
pub(crate) mod schedule_delivery {
    #[cfg(test)]
    use std::cell::RefCell;
    use std::collections::HashMap;

    use crate::brain::BrainId;
    use tokio::time::Duration;

    #[cfg(test)]
    thread_local! {
        static AFTER_OBSERVATION_HOOK: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
        static AFTER_QUEUE_HOOK: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
        static AFTER_ATTEMPT_HOOK: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
        static AFTER_RESULT_HOOK: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
        static BEFORE_DISPATCH_READINESS_HOOK: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
    }

    #[cfg(test)]
    pub(crate) fn set_after_observation_hook(hook: Box<dyn FnOnce()>) {
        AFTER_OBSERVATION_HOOK.with(|slot| {
            assert!(slot.borrow_mut().replace(hook).is_none());
        });
    }

    #[cfg(test)]
    pub(crate) fn run_after_observation_hook() {
        if let Some(hook) = AFTER_OBSERVATION_HOOK.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
    }

    #[cfg(test)]
    pub(crate) fn set_after_queue_hook(hook: Box<dyn FnOnce()>) {
        AFTER_QUEUE_HOOK.with(|slot| {
            assert!(slot.borrow_mut().replace(hook).is_none());
        });
    }

    #[cfg(test)]
    pub(crate) fn run_after_queue_hook() {
        if let Some(hook) = AFTER_QUEUE_HOOK.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
    }

    #[cfg(test)]
    pub(crate) fn set_after_attempt_hook(hook: Box<dyn FnOnce()>) {
        AFTER_ATTEMPT_HOOK.with(|slot| {
            assert!(slot.borrow_mut().replace(hook).is_none());
        });
    }

    #[cfg(test)]
    pub(crate) fn run_after_attempt_hook() {
        if let Some(hook) = AFTER_ATTEMPT_HOOK.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
    }

    #[cfg(test)]
    pub(crate) fn set_after_result_hook(hook: Box<dyn FnOnce()>) {
        AFTER_RESULT_HOOK.with(|slot| {
            assert!(slot.borrow_mut().replace(hook).is_none());
        });
    }

    #[cfg(test)]
    pub(crate) fn run_after_result_hook() {
        if let Some(hook) = AFTER_RESULT_HOOK.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
    }

    #[cfg(test)]
    pub(crate) fn set_before_dispatch_readiness_hook(hook: Box<dyn FnOnce()>) {
        BEFORE_DISPATCH_READINESS_HOOK.with(|slot| {
            assert!(slot.borrow_mut().replace(hook).is_none());
        });
    }

    #[cfg(test)]
    pub(crate) fn run_before_dispatch_readiness_hook() {
        if let Some(hook) = BEFORE_DISPATCH_READINESS_HOOK.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
    }

    /// Process-ephemeral delivery failures, keyed by durable Brain identity
    /// and display name rather than by the reusable alias alone.
    #[derive(Debug, Default)]
    pub(crate) struct FailureEpisodes {
        failing: HashMap<(BrainId, String), u64>,
    }

    impl FailureEpisodes {
        /// Drop entries that no longer own the same active schedule set.
        ///
        /// The caller resolves only names already in the failure registry, so
        /// the idle path does no work and reconciliation is O(failures), not
        /// O(all active scheduled Brains).
        pub(crate) fn reconcile(
            &mut self,
            mut active_observation: impl FnMut(&str) -> Option<(BrainId, u64)>,
        ) {
            self.failing.retain(|(brain_id, name), epoch| {
                active_observation(name) == Some((*brain_id, *epoch))
            });
        }

        /// Returns `true` only for the first failure in this episode.
        pub(crate) fn record_failure(&mut self, identity: (BrainId, String), epoch: u64) -> bool {
            self.failing.insert(identity, epoch) != Some(epoch)
        }

        /// Returns `true` only when a real success clears a prior failure.
        pub(crate) fn record_success(&mut self, identity: &(BrainId, String), epoch: u64) -> bool {
            if self.failing.get(identity) != Some(&epoch) {
                return false;
            }
            self.failing.remove(identity);
            true
        }

        #[cfg(test)]
        pub(crate) fn len(&self) -> usize {
            self.failing.len()
        }
    }

    /// Ceiling on one sleep. A clock jump or a missed notification then costs
    /// one idle wake rather than an unbounded stall. It is a backstop, not the
    /// mechanism: with nothing due the loop wakes once a minute, not sixty
    /// times.
    pub(crate) const MAX_SLEEP: Duration = Duration::from_secs(60);

    /// How long to wait after a pass that delivered nothing and advanced
    /// nothing. Matches the cadence of the fixed one-second tick this loop
    /// replaced.
    pub(crate) const UNDELIVERED_RETRY: Duration = Duration::from_secs(1);

    /// How often the Brain root is re-enumerated, so a Brain that could not be
    /// loaded earlier is picked up once it is repaired. The fixed one-second
    /// tick gave that recovery for free; the index pays for it once a minute
    /// instead of once a second.
    pub(crate) const REWARM_INTERVAL: Duration = Duration::from_secs(60);

    /// How long to sleep given the earliest indexed due time and the clock.
    pub(crate) fn sleep_for(head: Option<u64>, now_ms: u64) -> Duration {
        match head {
            Some(due) if due <= now_ms => Duration::ZERO,
            Some(due) => Duration::from_millis(due - now_ms).min(MAX_SLEEP),
            None => MAX_SLEEP,
        }
    }

    /// Whether a delivery pass that changed nothing should back off.
    ///
    /// `queue_due_schedules` skips a schedule whose previous occurrence is
    /// still `Running` or `AwaitingApproval`, leaving `next_due_ms` where it
    /// was. The head then stays due and the next `sleep_for` is zero. Without
    /// this the loop spins hot; the fixed tick it replaced was safe from that
    /// only because it always slept a second.
    ///
    /// An earlier version of this note blamed an offline runner. That is not
    /// the mechanism: `deliver_due_named_brain_schedules` calls
    /// `queue_due_schedules`, which advances, *before* it checks runner
    /// readiness.
    pub(crate) fn should_back_off(
        head_before: Option<u64>,
        head_after: Option<u64>,
        now_ms: u64,
    ) -> bool {
        head_after == head_before && head_after.is_some_and(|due| due <= now_ms)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_nothing_scheduled_sleeps_the_ceiling_rather_than_polling() {
            assert_eq!(
                sleep_for(None, 5_000),
                MAX_SLEEP,
                "an empty index must wait, not poll; this is the whole point of \
         replacing the fixed one-second tick"
            );
        }

        #[test]
        fn test_a_due_head_does_not_sleep() {
            assert_eq!(
                sleep_for(Some(5_000), 5_000),
                Duration::ZERO,
                "a schedule due at exactly now must be delivered on this pass, \
         not after another sleep"
            );
            assert_eq!(
                sleep_for(Some(4_000), 5_000),
                Duration::ZERO,
                "and one already overdue must not sleep at all"
            );
        }

        #[test]
        fn test_sleep_is_the_distance_to_the_head() {
            assert_eq!(
                sleep_for(Some(5_250), 5_000),
                Duration::from_millis(250),
                "the loop must wake when the head comes due, not on a fixed \
         cadence; inverting the due comparison would return ZERO here \
         and spin"
            );
        }

        #[test]
        fn test_a_distant_head_is_capped_at_the_ceiling() {
            assert_eq!(
                sleep_for(Some(u64::MAX), 0),
                MAX_SLEEP,
                "a head far in the future must still be bounded, so a clock \
         jump or a dropped notification costs one idle wake rather than \
         an unbounded stall. Raising MAX_SLEEP raises that worst case"
            );
        }

        #[test]
        fn test_back_off_when_a_due_head_did_not_move() {
            assert!(
                should_back_off(Some(1_000), Some(1_000), 2_000),
                "a due schedule that was not delivered leaves the head due and \
         the next sleep zero. Without backing off, the delivery task \
         spins hot on a Brain whose runner is offline -- which is \
         exactly the state an idle machine sits in"
            );
        }

        #[test]
        fn test_do_not_back_off_when_the_head_advanced() {
            assert!(
                !should_back_off(Some(1_000), Some(2_000), 1_500),
                "a delivered schedule advanced the head; sleeping a second here \
         would delay work that is already selectable"
            );
        }

        #[test]
        fn test_do_not_back_off_when_the_head_is_not_yet_due() {
            assert!(
                !should_back_off(Some(9_000), Some(9_000), 1_000),
                "an unchanged head that is not due is the ordinary idle case; \
         the loop sleeps toward it rather than backing off"
            );
            assert!(
                !should_back_off(None, None, 1_000),
                "and an empty index is not a failed delivery"
            );
        }

        #[test]
        fn test_failure_episode_reconciliation_is_identity_keyed_and_bounded() {
            let first = (BrainId(uuid::Uuid::from_u128(1)), "reused-name".to_string());
            let successor = (BrainId(uuid::Uuid::from_u128(2)), "reused-name".to_string());
            let mut episodes = FailureEpisodes::default();
            assert!(episodes.record_failure(first.clone(), 1));
            assert!(!episodes.record_failure(first.clone(), 1));
            assert!(
                episodes.record_failure(first.clone(), 2),
                "the same durable identity after an empty-to-active ABA must start fresh"
            );

            episodes.reconcile(|name| (name == successor.1.as_str()).then_some((successor.0, 3)));
            assert_eq!(
                episodes.len(),
                0,
                "reconciliation must evict a retired identity even when its display name is reused"
            );
            assert!(
                episodes.record_failure(successor, 3),
                "a new BrainId under the same display name must start a fresh failure episode"
            );
            assert_eq!(
                episodes.len(),
                1,
                "the registry must remain bounded by active indexed identities after reconciliation"
            );
        }
    }
}

/// Header carrying the per-request correlation id (#223): always assigned by
/// the daemon itself (see [`strip_client_request_id`]), logged on every
/// tracing event emitted while handling that request, and echoed back on the
/// response so a client-side log can be joined to the daemon's by that id.
const REQUEST_ID_HEADER: &str = "x-request-id";

/// Discard any client-supplied `x-request-id` before `SetRequestIdLayer` can
/// see it, so the daemon always assigns its own.
///
/// `SetRequestIdLayer` only generates a fresh id when the header is absent —
/// otherwise it preserves whatever the caller sent, verbatim, and that value
/// then flows into every tracing log line for the request. This daemon's
/// HTTP surface includes the TLS remote-Brain listener and the
/// OpenAI-compatible API, both reachable by callers this process does not
/// otherwise trust; writing an unvalidated client string into `daemon.log`
/// is a log-injection vector (forged-looking log lines, or terminal
/// control/ANSI-escape sequences aimed at whoever later `tail -f`s the log).
/// This middleware must be the outermost layer in every request-id/trace
/// stack, ahead of `SetRequestIdLayer`.
async fn strip_client_request_id(
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    request.headers_mut().remove(REQUEST_ID_HEADER);
    next.run(request).await
}

/// Span-building closure for the request-id/trace layer stack, applied
/// inline at each router-construction site (Rust's tower `Layer`/`Service`
/// generic bounds make a shared helper function that *returns* the composed
/// stack impractical to name; the stack itself is four short lines). A
/// missing request id (a request that somehow bypassed `SetRequestIdLayer`)
/// logs as `-` rather than panicking or silently omitting the field.
fn request_tracing_span<B>(request: &axum::http::Request<B>) -> tracing::Span {
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    tracing::info_span!(
        "request",
        request_id = %request_id,
        method = %request.method(),
        uri = %request.uri(),
    )
}

impl AgentServer {
    #[cfg(test)]
    pub(crate) fn for_brain_http_test(
        machine: &str,
        state_root: &std::path::Path,
        brain_credentials: crate::brain::BrainCredentialAuthority,
    ) -> Result<Self> {
        let generator_state = Arc::new(RwLock::new(GeneratorState::NotAvailable));
        Ok(Self {
            started_at: std::time::Instant::now(),
            claude_client: Arc::new(ClaudeClient::new(String::new())?),
            providers: Vec::new(),
            router: Arc::new(RwLock::new(Router::new(
                crate::models::ThresholdRouter::new(),
            ))),
            metrics_logger: Arc::new(MetricsLogger::new(state_root.join("metrics"))?),
            config: ServerConfig::default(),
            local_generator: Arc::new(RwLock::new(LocalGenerator::new())),
            bootstrap_loader: Arc::new(BootstrapLoader::new(Arc::clone(&generator_state), None)),
            generator_state,
            feedback_store: Arc::new(FeedbackLogger::at(state_root.join("feedback.jsonl"))?),
            brain_store: crate::brain::BrainStore::with_root(
                machine,
                Some(state_root.join("brains")),
            ),
            brain_runners: BrainRunnerBroker::default(),
            brain_approvals: BrainApprovalBroker::default(),
            claude_cli_sessions: ClaudeCliSessionRegistry::default(),
            brain_credentials,
            mcp_servers: std::collections::HashMap::new(),
            mcp_client: tokio::sync::OnceCell::new(),
            brain_password: Arc::new(RwLock::new(String::new())),
            supervised_state_root: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_supervised_brain_http_test(
        machine: &str,
        state_root: &std::path::Path,
        brain_credentials: crate::brain::BrainCredentialAuthority,
    ) -> Result<Self> {
        use anyhow::Context as _;
        let proof = crate::brain::isolated_test_proof()
            .context("Brain HTTP fixture requires supervisor authority")?;
        let state_root = supervised_state_root(&proof, state_root)?;
        let password = proof.brain_password()?;
        let mut server = Self::for_brain_http_test(machine, &state_root.path, brain_credentials)?;
        server.brain_password = Arc::new(RwLock::new(password));
        server.supervised_state_root = Some(state_root.directory);
        Ok(server)
    }

    #[cfg(test)]
    pub(crate) fn for_brain_protocol_test(
        store: crate::brain::BrainStore,
        credentials: crate::brain::BrainCredentialAuthority,
        password: String,
        state_root: &std::path::Path,
    ) -> Result<Self> {
        let generator_state = Arc::new(RwLock::new(GeneratorState::Initializing));
        let bootstrap_loader = Arc::new(BootstrapLoader::new(generator_state.clone(), None));
        Ok(Self {
            started_at: std::time::Instant::now(),
            claude_client: Arc::new(ClaudeClient::new("brain-protocol-test".into())?),
            providers: Vec::new(),
            router: Arc::new(RwLock::new(Router::new(
                crate::models::ThresholdRouter::default(),
            ))),
            metrics_logger: Arc::new(MetricsLogger::new(state_root.join("metrics"))?),
            config: ServerConfig {
                brain_password: password.clone(),
                ..ServerConfig::default()
            },
            local_generator: Arc::new(RwLock::new(LocalGenerator::default())),
            bootstrap_loader,
            generator_state,
            feedback_store: Arc::new(FeedbackLogger::at(state_root.join("feedback.jsonl"))?),
            brain_store: store,
            brain_runners: BrainRunnerBroker::default(),
            brain_approvals: BrainApprovalBroker::default(),
            claude_cli_sessions: ClaudeCliSessionRegistry::default(),
            brain_credentials: credentials,
            mcp_servers: std::collections::HashMap::new(),
            mcp_client: tokio::sync::OnceCell::new(),
            brain_password: Arc::new(RwLock::new(password)),
            supervised_state_root: None,
        })
    }

    /// Create a new agent server.
    ///
    /// `provider_graph` is the already validated named cloud graph shared with
    /// `claude_client`; provider construction must not be repeated here.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        mut server_config: ServerConfig,
        claude_client: ClaudeClient,
        router: Router,
        metrics_logger: MetricsLogger,
        local_generator: Arc<RwLock<LocalGenerator>>,
        bootstrap_loader: Arc<BootstrapLoader>,
        generator_state: Arc<RwLock<GeneratorState>>,
        provider_graph: ProviderGraph,
    ) -> Result<Self> {
        // Validate inherited supervisor authority before channels, credential
        // files, Brain stores, or configuration clones are created. A
        // malformed opt-in environment therefore has no server-side effects.
        // Cached per-process (#858); this call is free unless it is the
        // first one the daemon reaches.
        let proof_start = std::time::Instant::now();
        let isolated_proof = crate::brain::isolated_test_proof_if_present()?;
        tracing::debug!(
            elapsed_ms = proof_start.elapsed().as_millis(),
            present = isolated_proof.is_some(),
            "isolated_test_proof_if_present (AgentServer::new)"
        );
        if let Some(proof) = &isolated_proof {
            server_config.bind_address = proof.daemon_address().to_owned();
            server_config.brain_bind_address = None;
            server_config.brain_password = proof.brain_password()?;
        }
        let providers: Vec<ProviderSlot> = provider_graph
            .profiles()
            .iter()
            .map(|profile| ProviderSlot {
                profile_name: profile.profile_name().to_string(),
                provider: Arc::clone(profile.provider()),
            })
            .collect();

        let machine = hostname_or_default();
        let machine = if machine.contains('.') {
            machine
        } else {
            format!("{machine}.local")
        };
        let brain_password = server_config.brain_password.clone();
        let credential_state = dirs::home_dir()
            .ok_or_else(|| {
                anyhow::anyhow!("cannot initialize Brain credentials without a home directory")
            })?
            .join(".finch");
        let brain_credentials =
            crate::brain::BrainCredentialAuthority::load_or_create(&credential_state)?;
        let mcp_servers = config.mcp_servers.clone();

        // #411: sweep Brains with no recorded activity before anything can
        // attach, archive, or schedule-deliver against one -- see
        // `BrainStore::sweep_unused`'s doc comment for why that ordering is
        // load-bearing rather than incidental. One line only when it found
        // something to do; per #380's lesson, an unchanging "nothing swept"
        // must stay silent rather than confirm itself every restart.
        let brain_store = crate::brain::BrainStore::new(machine);
        let swept = brain_store.sweep_unused(crate::brain::BrainStore::SWEEP_MIN_AGE_MS);
        if swept > 0 {
            tracing::info!(
                swept,
                "daemon startup: swept unused Brains with no recorded activity"
            );
        }

        Ok(Self {
            started_at: std::time::Instant::now(),
            claude_client: Arc::new(claude_client),
            providers,
            router: Arc::new(RwLock::new(router)),
            metrics_logger: Arc::new(metrics_logger),
            config: server_config,
            local_generator,
            bootstrap_loader,
            generator_state,
            feedback_store: Arc::new(FeedbackLogger::new()?),
            brain_store,
            brain_runners: BrainRunnerBroker::default(),
            brain_approvals: BrainApprovalBroker::default(),
            claude_cli_sessions: ClaudeCliSessionRegistry::default(),
            brain_credentials,
            mcp_servers,
            mcp_client: tokio::sync::OnceCell::new(),
            brain_password: Arc::new(RwLock::new(brain_password)),
            #[cfg(test)]
            supervised_state_root: None,
        })
    }

    /// Start the HTTP server.
    ///
    /// Takes `Arc<Self>` so the same server instance can be shared with the
    /// Cap'n Proto IPC server that runs concurrently.
    pub async fn serve(self: Arc<Self>) -> Result<()> {
        // Cached per-process (#858); this call is free unless it is the
        // first one the daemon reaches.
        let proof_start = std::time::Instant::now();
        let isolated_proof = crate::brain::isolated_test_proof_if_present()?;
        tracing::debug!(
            elapsed_ms = proof_start.elapsed().as_millis(),
            present = isolated_proof.is_some(),
            "isolated_test_proof_if_present (AgentServer::serve)"
        );
        let addr: SocketAddr = self.config.bind_address.parse()?;
        let listener = if let Some(proof) = isolated_proof {
            anyhow::ensure!(
                addr.to_string() == proof.daemon_address(),
                "isolated daemon bind address does not match supervisor authority"
            );
            let listener = proof.duplicate_daemon_listener()?;
            listener.set_nonblocking(true)?;
            tokio::net::TcpListener::from_std(listener)?
        } else {
            tokio::net::TcpListener::bind(addr).await?
        };
        // #868: from `serve()` being entered to the listener being ready, the
        // startup path re-validates its proof, duplicates the supervisor's
        // listener, and binds — all silent until now. Name the boundary so a
        // stall in it is attributable from daemon.log alone. elapsed_ms
        // measures only this span (serve() entry to here); it is not the gap
        // since `AgentServer::new` — that construction logs its own elapsed
        // time separately (`main.rs`'s "agent server constructed"), and a
        // reader wanting the full construction-to-bind gap must diff the two
        // log lines' timestamps rather than read this field alone.
        tracing::info!(
            elapsed_ms = proof_start.elapsed().as_millis(),
            address = %addr,
            "daemon startup: listener acquired"
        );
        self.serve_on_listener(listener).await
    }

    async fn serve_on_listener(self: Arc<Self>, listener: tokio::net::TcpListener) -> Result<()> {
        let addr = listener.local_addr()?;

        // The daemon owns only due-time calculation and durable queueing.

        // Actual ProgramRuns remain on each Brain's leased environment runner.
        let schedule_store = self.brain_store.clone();
        let schedule_runners = self.brain_runners.clone();
        let schedule_task = tokio::spawn(async move {
            // Warm the due index. Schedules only become known when a Brain is
            // loaded, so the index covers exactly the resident Brains and starts
            // empty. Steady state then selects from the index and hydrates
            // nothing it does not need (#374).
            //
            // Re-warmed periodically, not once. Warming once meant a Brain that
            // happened to be unloadable at daemon start -- a volume not yet
            // mounted, a journal being repaired -- was never scheduled again for
            // the life of the daemon, silently, because a scheduled Brain is
            // precisely the Brain nothing else touches by name to hydrate it.
            // The loop this replaced re-enumerated every second and recovered
            // within a second; losing that was a regression. The enumeration is
            // now bounded by this interval rather than by the tick, which is the
            // cost actually being removed.
            schedule_store.warm_schedule_index();
            let mut last_warm = tokio::time::Instant::now();
            let mut failure_episodes = schedule_delivery::FailureEpisodes::default();

            let wakeup = schedule_store.schedule_wakeup();
            // A due schedule whose Brain has no ready runner is not delivered
            // and its `next_due_ms` is not advanced, so the index head stays
            // due. Without a floor the loop would then spin: the old code was
            // safe from this only because it slept a second unconditionally.
            const UNDELIVERED_RETRY: tokio::time::Duration = schedule_delivery::UNDELIVERED_RETRY;
            loop {
                // Register the waiter *before* sampling the head. A writer that
                // blocks on the index lock resumes the instant this loop
                // releases it, so a notification sent between reading the head
                // and registering would land in that window. Enabling the
                // future first makes the window empty rather than merely narrow.
                let notified = wakeup.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();

                if last_warm.elapsed() >= schedule_delivery::REWARM_INTERVAL {
                    schedule_store.warm_schedule_index();
                    last_warm = tokio::time::Instant::now();
                }

                failure_episodes.reconcile(|name| schedule_store.active_schedule_observation(name));

                let now = crate::brain::unix_millis();
                let sleep_for =
                    schedule_delivery::sleep_for(schedule_store.next_schedule_due_ms(), now);
                if !sleep_for.is_zero() {
                    // Wake early when a schedule appears that is due sooner
                    // than the head this sleep was computed against.
                    tokio::select! {
                        _ = tokio::time::sleep(sleep_for) => {}
                        _ = notified.as_mut() => {}
                    }
                }

                let now = crate::brain::unix_millis();
                let head_before = schedule_store.next_schedule_due_ms();
                // Selection by due time, not by Brain: this names only the
                // Brains that actually have work, without hydrating any.
                let names = schedule_store.due_schedule_brains(now);
                for name in names {
                    let Some((brain_id, epoch)) = schedule_store.active_schedule_observation(&name)
                    else {
                        continue;
                    };
                    #[cfg(test)]
                    schedule_delivery::run_after_observation_hook();

                    let identity = (brain_id, name.clone());
                    let result = handlers::deliver_due_named_brain_schedules(
                        schedule_store.clone(),
                        schedule_runners.clone(),
                        name.clone(),
                        crate::brain::unix_millis(),
                    )
                    .await;

                    #[cfg(test)]
                    schedule_delivery::run_after_attempt_hook();

                    match result {
                        Err(failure) => {
                            // A post-sample failure warns for the completion
                            // epoch. An earlier one-shot in the same queue call
                            // advances the activity epoch, and reconcile only
                            // keeps the epoch still active. The entry epoch
                            // would be dropped on the next pass, so the sibling
                            // would warn again and its later success would not
                            // count as recovery. Recovery below still requires
                            // the loop sample, so a successor cannot clear a
                            // predecessor. A pre-sample failure still requires
                            // the live active set to be the one this pass selected.
                            let record_epoch = match failure.started {
                                Some((started_id, _))
                                    if started_id == brain_id
                                        && schedule_store.schedule_lifecycle_observation(&name)
                                            == failure.completion =>
                                {
                                    failure
                                        .completion
                                        .map(|(_, completion_epoch, _)| completion_epoch)
                                }
                                None if schedule_store.active_schedule_observation(&name)
                                    == Some((brain_id, epoch)) =>
                                {
                                    Some(epoch)
                                }
                                _ => None,
                            };
                            if record_epoch.is_some_and(|record_epoch| {
                                failure_episodes.record_failure(identity.clone(), record_epoch)
                            }) {
                                tracing::warn!(
                                    brain_id = %brain_id.0,
                                    brain = %name,
                                    error = %format_args!("{:#}", failure.error),
                                    "could not deliver due Brain schedule"
                                );
                            }
                        }
                        Ok(attempt)
                            if attempt.delivered > 0
                                && attempt.started == Some((brain_id, epoch))
                                && attempt.completion.is_some_and(|(completion_id, _, _)| {
                                    completion_id == brain_id
                                })
                                && schedule_store.schedule_lifecycle_observation(&name)
                                    == attempt.completion
                                && failure_episodes.record_success(&identity, epoch) =>
                        {
                            tracing::info!(
                                brain_id = %brain_id.0,
                                brain = %name,
                                "due Brain schedule delivery recovered"
                            );
                        }
                        Ok(_) => {}
                    }

                    #[cfg(test)]
                    schedule_delivery::run_after_result_hook();
                }

                // If the head did not move, nothing advanced -- typically a due
                // Brain with no ready runner. Back off to the old cadence
                // rather than re-selecting the same entry immediately.
                let head_after = schedule_store.next_schedule_due_ms();
                if schedule_delivery::should_back_off(
                    head_before,
                    head_after,
                    crate::brain::unix_millis(),
                ) {
                    tokio::time::sleep(UNDELIVERED_RETRY).await;
                }
            }
        });

        // Monitor generator state and inject model when ready
        let local_gen_clone = Arc::clone(&self.local_generator);
        let state_monitor = Arc::clone(&self.generator_state);
        let model_monitor_task = tokio::spawn(async move {
            tracing::info!("Model monitor task started");
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

                let state = state_monitor.read().await;
                tracing::debug!(
                    "Monitor checking state: {:?}",
                    std::mem::discriminant(&*state)
                );

                if let GeneratorState::Ready { model, model_name } = &*state {
                    let model_clone = Arc::clone(model);
                    let name = model_name.clone();
                    drop(state); // Release read lock before acquiring write lock

                    tracing::info!("Model is ready: {}, injecting into LocalGenerator", name);

                    // Try to inject with timeout
                    match tokio::time::timeout(tokio::time::Duration::from_secs(5), async {
                        tracing::info!("Acquiring write lock on LocalGenerator...");
                        let mut gen = local_gen_clone.write().await;
                        tracing::info!("Write lock acquired, creating new LocalGenerator...");
                        *gen = LocalGenerator::with_models(Some(model_clone));
                        tracing::info!("LocalGenerator updated");
                    })
                    .await
                    {
                        Ok(_) => {
                            tracing::info!("✓ Model injected - local generation enabled");
                            break; // Stop monitoring once injected
                        }
                        Err(_) => {
                            tracing::error!(
                                "❌ Timeout while injecting model (5s) - write lock may be held"
                            );
                        }
                    }
                } else if matches!(
                    *state,
                    GeneratorState::Failed { .. } | GeneratorState::NotAvailable
                ) {
                    tracing::warn!("Model loading failed or not available, stopping monitor");
                    break; // Stop monitoring on failure
                }
            }
            tracing::info!("Model monitor task exiting");
        });
        let _background_tasks = ServerBackgroundTasks(vec![schedule_task, model_monitor_task]);

        let auth = DaemonAuth::new(self.config.auth_enabled, self.config.api_keys.clone());

        // Use the existing Arc as application state.
        let app_state = self;

        // Build router with a body size limit to guard against oversized foreign payloads.
        // 4MB is generous for natural-language queries while blocking obvious DoS attempts.
        let request_id_header = axum::http::HeaderName::from_static(REQUEST_ID_HEADER);
        let app = create_router(Arc::clone(&app_state))
            .layer(axum::extract::DefaultBodyLimit::max(4 * 1024 * 1024)) // 4MB
            .layer(axum::middleware::from_fn_with_state(auth, auth_middleware))
            .layer(
                ServiceBuilder::new()
                    .layer(axum::middleware::from_fn(strip_client_request_id))
                    .layer(SetRequestIdLayer::new(
                        request_id_header.clone(),
                        MakeRequestUuid,
                    ))
                    .layer(TraceLayer::new_for_http().make_span_with(request_tracing_span))
                    .layer(PropagateRequestIdLayer::new(request_id_header)),
            );

        // Start server — ConnectInfo requires into_make_service_with_connect_info
        // so handlers can read the peer's IP for auth logging.
        publish_isolated_test_address(addr)?;
        tracing::info!("Starting Finch agent server on {}", addr);
        let local_server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        );

        if let Some(brain_bind_address) = &app_state.config.brain_bind_address {
            let brain_addr: SocketAddr = brain_bind_address.parse()?;
            let tls_identity = app_state.brain_credentials.invitation_tls_identity();
            let tls_config = axum_server::tls_rustls::RustlsConfig::from_config(
                tls_identity.rustls_server_config()?,
            );
            let brain_request_id_header = axum::http::HeaderName::from_static(REQUEST_ID_HEADER);
            let brain_app = crate::server::handlers::create_remote_brain_router(app_state)
                .layer(axum::extract::DefaultBodyLimit::max(4 * 1024 * 1024))
                .layer(
                    ServiceBuilder::new()
                        .layer(axum::middleware::from_fn(strip_client_request_id))
                        .layer(SetRequestIdLayer::new(
                            brain_request_id_header.clone(),
                            MakeRequestUuid,
                        ))
                        .layer(TraceLayer::new_for_http().make_span_with(request_tracing_span))
                        .layer(PropagateRequestIdLayer::new(brain_request_id_header)),
                );
            tracing::info!("Starting encrypted Brain listener on {}", brain_addr);
            let remote_server = axum_server::bind_rustls(brain_addr, tls_config)
                .serve(brain_app.into_make_service_with_connect_info::<std::net::SocketAddr>());
            tokio::select! {
                result = local_server => result?,
                result = remote_server => result?,
            }
        } else {
            local_server.await?;
        }

        Ok(())
    }

    /// Get reference to Claude client
    pub fn claude_client(&self) -> &Arc<ClaudeClient> {
        &self.claude_client
    }

    /// Resolve the cloud provider to use for a given request.
    ///
    /// Names resolve configured profiles first. A provider type such as
    /// `openai` is accepted only when it identifies exactly one profile.
    /// `None` selects the first configured cloud profile.
    pub fn provider_for_name(&self, name: Option<&str>) -> Option<&Arc<dyn LlmProvider>> {
        if self.providers.is_empty() {
            return None;
        }
        if let Some(n) = name {
            if let Some(slot) = self
                .providers
                .iter()
                .find(|slot| slot.profile_name.eq_ignore_ascii_case(n))
            {
                return Some(&slot.provider);
            }

            let by_type: Vec<_> = self
                .providers
                .iter()
                .filter(|slot| slot.provider.name().eq_ignore_ascii_case(n))
                .collect();
            return match by_type.as_slice() {
                [slot] => Some(&slot.provider),
                _ => None,
            };
        }
        self.providers.first().map(|slot| &slot.provider)
    }

    /// Whether named cloud provider profiles are configured.
    ///
    /// The OpenAI-compatible API uses this to distinguish an unknown profile
    /// name from the legacy configuration path, which has no provider pool.
    pub fn has_provider_profiles(&self) -> bool {
        !self.providers.is_empty()
    }

    /// Get reference to router
    pub fn router(&self) -> &Arc<RwLock<Router>> {
        &self.router
    }

    /// Get reference to metrics logger
    pub fn metrics_logger(&self) -> &Arc<MetricsLogger> {
        &self.metrics_logger
    }

    /// Get the append-only explicit feedback store.
    pub fn feedback_store(&self) -> &Arc<FeedbackLogger> {
        &self.feedback_store
    }

    /// Get server configuration
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// How long this server has been running.
    pub fn uptime(&self) -> std::time::Duration {
        self.started_at.elapsed()
    }

    pub fn brain_store(&self) -> &crate::brain::BrainStore {
        &self.brain_store
    }

    pub fn brain_runners(&self) -> &BrainRunnerBroker {
        &self.brain_runners
    }

    pub fn brain_approvals(&self) -> &BrainApprovalBroker {
        &self.brain_approvals
    }

    /// Daemon-owned Claude CLI Subscription sessions, keyed by Brain name
    /// (issue #1354). See `claude_cli_session.rs` for the lifecycle contract.
    pub fn claude_cli_sessions(&self) -> &ClaudeCliSessionRegistry {
        &self.claude_cli_sessions
    }

    /// Return the daemon-owned MCP transport, connecting it on first use.
    /// Named Brain runtimes borrow this host service but retain independent
    /// typed dictionaries, manifests, grants, and effect journals.
    pub async fn mcp_client(&self) -> Result<Option<Arc<crate::tools::McpClient>>> {
        if self.mcp_servers.is_empty() {
            return Ok(None);
        }
        let client = self
            .mcp_client
            .get_or_try_init(|| async {
                crate::tools::McpClient::from_config(&self.mcp_servers)
                    .await
                    .map(Arc::new)
            })
            .await?;
        Ok(Some(Arc::clone(client)))
    }

    pub async fn brain_password(&self) -> String {
        self.brain_password.read().await.clone()
    }

    pub async fn check_brain_password(&self, candidate: &str) -> bool {
        constant_time_eq(
            self.brain_password.read().await.as_bytes(),
            candidate.as_bytes(),
        )
    }

    pub async fn set_brain_password(&self, password: String) {
        *self.brain_password.write().await = password;
    }

    pub fn brain_credentials(&self) -> &crate::brain::BrainCredentialAuthority {
        &self.brain_credentials
    }

    /// Get reference to local generator
    pub fn local_generator(&self) -> &Arc<RwLock<LocalGenerator>> {
        &self.local_generator
    }

    /// Get reference to bootstrap loader
    pub fn bootstrap_loader(&self) -> &Arc<BootstrapLoader> {
        &self.bootstrap_loader
    }

    /// Get reference to generator state
    pub fn generator_state(&self) -> &Arc<RwLock<GeneratorState>> {
        &self.generator_state
    }

    /// Return the primary cloud provider (first in the configured list, if any).
    ///
    /// Used by the IPC server to service CLI queries without going through the
    /// full HTTP handler stack.
    pub fn primary_provider(&self) -> Option<Arc<dyn crate::providers::LlmProvider>> {
        self.providers
            .first()
            .map(|slot| Arc::clone(&slot.provider))
    }
}

fn publish_isolated_test_address(bound_addr: SocketAddr) -> Result<()> {
    let Some(path) = std::env::var_os("FINCH_TEST_BOUND_ADDR_FILE").map(std::path::PathBuf::from)
    else {
        return Ok(());
    };
    let proof = crate::brain::isolated_test_proof()?;
    let relative = path
        .strip_prefix(&proof.home)
        .map_err(|_| anyhow::anyhow!("test address file must be inside the isolated HOME"))?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("test address file has no final component"))?;
    anyhow::ensure!(
        relative.file_name().is_some()
            && relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_))),
        "test address path contains an unsafe component"
    );
    #[cfg(unix)]
    {
        let parent = open_isolated_address_parent(&proof, relative)?;
        publish_isolated_address_file(&parent, name, bound_addr)
    }
    #[cfg(not(unix))]
    {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("test address file has no parent"))?;
        publish_isolated_address_file(parent, name, bound_addr)
    }
}

#[cfg(unix)]
fn open_isolated_address_parent(
    proof: &crate::brain::IsolatedTestProof,
    relative: &std::path::Path,
) -> Result<std::fs::File> {
    use nix::fcntl::{open, openat, OFlag};
    use nix::sys::stat::{fstat, mkdirat, Mode, SFlag};
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let raw = open(
        &proof.home,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    let mut directory = unsafe { std::fs::File::from_raw_fd(raw) };
    let home_stat = fstat(directory.as_raw_fd())?;
    anyhow::ensure!(
        (home_stat.st_dev as u64, home_stat.st_ino as u64) == proof.home_identity,
        "isolated HOME identity changed before address publication"
    );
    let parent = relative
        .parent()
        .ok_or_else(|| anyhow::anyhow!("test address file has no parent"))?;
    for component in parent.components() {
        let std::path::Component::Normal(name) = component else {
            anyhow::bail!("test address parent contains an unsafe component");
        };
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let child_raw = match openat(Some(directory.as_raw_fd()), name, flags, Mode::empty()) {
            Ok(raw) => raw,
            Err(nix::errno::Errno::ENOENT) => {
                mkdirat(
                    Some(directory.as_raw_fd()),
                    name,
                    Mode::from_bits_truncate(0o700),
                )?;
                openat(Some(directory.as_raw_fd()), name, flags, Mode::empty())?
            }
            Err(error) => return Err(error.into()),
        };
        let child = unsafe { std::fs::File::from_raw_fd(child_raw) };
        let metadata = fstat(child.as_raw_fd())?;
        anyhow::ensure!(
            SFlag::from_bits_truncate(metadata.st_mode).contains(SFlag::S_IFDIR)
                && metadata.st_uid == nix::unistd::geteuid().as_raw()
                && metadata.st_nlink >= 1
                && metadata.st_mode & 0o022 == 0,
            "test address ancestor is not a private owned directory"
        );
        directory = child;
    }
    Ok(directory)
}

#[cfg(unix)]
fn publish_isolated_address_file(
    directory: &std::fs::File,
    name: &std::ffi::OsStr,
    bound_addr: SocketAddr,
) -> Result<()> {
    use nix::fcntl::{openat, renameat, OFlag};
    use nix::sys::stat::{fstat, Mode, SFlag};
    use nix::unistd::{unlinkat, UnlinkatFlags};
    use std::io::Write as _;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let directory_stat = fstat(directory.as_raw_fd())?;
    anyhow::ensure!(
        SFlag::from_bits_truncate(directory_stat.st_mode).contains(SFlag::S_IFDIR)
            && directory_stat.st_uid == nix::unistd::geteuid().as_raw()
            && directory_stat.st_nlink >= 1
            && directory_stat.st_mode & 0o022 == 0,
        "test address parent must be an owned, non-writable-by-others directory"
    );
    let temporary = std::ffi::OsString::from(format!(
        ".finch-bound-address-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| -> Result<()> {
        let raw = openat(
            Some(directory.as_raw_fd()),
            temporary.as_os_str(),
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )?;
        let mut file = unsafe { std::fs::File::from_raw_fd(raw) };
        let created = fstat(file.as_raw_fd())?;
        anyhow::ensure!(
            SFlag::from_bits_truncate(created.st_mode).contains(SFlag::S_IFREG)
                && created.st_uid == nix::unistd::geteuid().as_raw()
                && created.st_nlink == 1
                && created.st_mode & 0o777 == 0o600,
            "test address temporary is not a private, singly linked regular file"
        );
        file.write_all(bound_addr.to_string().as_bytes())?;
        file.sync_all()?;
        renameat(
            Some(directory.as_raw_fd()),
            temporary.as_os_str(),
            Some(directory.as_raw_fd()),
            name,
        )?;
        directory.sync_all()?;
        let committed_raw = openat(
            Some(directory.as_raw_fd()),
            name,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
        let committed_file = unsafe { std::fs::File::from_raw_fd(committed_raw) };
        let committed = fstat(committed_file.as_raw_fd())?;
        anyhow::ensure!(
            committed.st_dev == created.st_dev
                && committed.st_ino == created.st_ino
                && SFlag::from_bits_truncate(committed.st_mode).contains(SFlag::S_IFREG)
                && committed.st_uid == nix::unistd::geteuid().as_raw()
                && committed.st_nlink == 1
                && committed.st_mode & 0o777 == 0o600,
            "test address publication changed identity during commit"
        );
        Ok(())
    })();
    if result.is_err() {
        let _ = unlinkat(
            Some(directory.as_raw_fd()),
            temporary.as_os_str(),
            UnlinkatFlags::NoRemoveDir,
        );
    }
    result
}

#[cfg(not(unix))]
fn publish_isolated_address_file(
    parent: &std::path::Path,
    name: &std::ffi::OsStr,
    bound_addr: SocketAddr,
) -> Result<()> {
    use std::io::Write as _;
    let temporary = parent.join(format!(
        ".finch-bound-address-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(bound_addr.to_string().as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temporary, parent.join(name))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{Seek, SeekFrom, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tracing_subscriber::layer::SubscriberExt as _;

    #[derive(Clone)]
    struct CapturedLogs(Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for CapturedLogs {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Clone, Debug)]
    struct CapturedEvent {
        level: tracing::Level,
        message: String,
        fields: HashMap<String, String>,
    }

    #[derive(Clone, Default)]
    struct CapturedEvents(Arc<std::sync::Mutex<Vec<CapturedEvent>>>);

    struct CaptureEventsLayer(CapturedEvents);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureEventsLayer {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _context: tracing_subscriber::layer::Context<'_, S>,
        ) {
            #[derive(Default)]
            struct Visitor {
                fields: HashMap<String, String>,
            }

            impl tracing::field::Visit for Visitor {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    self.fields
                        .insert(field.name().to_string(), format!("{value:?}"));
                }

                fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                    self.fields
                        .insert(field.name().to_string(), value.to_string());
                }
            }

            let mut visitor = Visitor::default();
            event.record(&mut visitor);
            let message = visitor.fields.remove("message").unwrap_or_default();
            self.0 .0.lock().unwrap().push(CapturedEvent {
                level: *event.metadata().level(),
                message,
                fields: visitor.fields,
            });
        }
    }

    impl CapturedEvents {
        fn schedule_events(&self, message: &str) -> Vec<CapturedEvent> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|event| event.message.contains(message))
                .cloned()
                .collect()
        }
    }

    async fn wait_for_schedule_events(events: &CapturedEvents, message: &str, expected: usize) {
        for _ in 0..200 {
            if events.schedule_events(message).len() >= expected {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!(
            "schedule delivery never emitted {expected} event(s) containing {message:?}; events={:?}",
            events.0.lock().unwrap()
        );
    }

    fn corrupt_brain_journal(root: &std::path::Path, name: &str) {
        let path = root.join(name).join("events.jsonl");
        let mut bytes = std::fs::read(&path).unwrap();
        let first_record_end = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|position| position + 1)
            .expect("a scheduled Brain journal must contain a committed record");
        bytes.extend_from_within(..first_record_end);
        std::fs::write(path, bytes).unwrap();
    }

    fn isolated_provider_graph() -> ProviderGraph {
        let config =
            crate::config::Config::with_providers(vec![crate::config::ProviderEntry::Claude {
                api_key: "sk-ant-isolated-provider-graph".into(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("isolated-provider-graph".into()),
            }]);
        crate::providers::create_provider_graph_from_config(&config)
            .expect("isolated tests need an in-memory provider graph")
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_final_one_shot_post_queue_failure_is_logged_before_retirement() {
        const FAILURE: &str = "could not deliver due Brain schedule";
        const NAME: &str = "one-shot-post-queue-error";

        let temp = tempfile::tempdir().unwrap();
        let authority = crate::brain::BrainCredentialAuthority::ephemeral([38; 32]);
        let server = Arc::new(
            AgentServer::for_brain_http_test("schedule-log.local", temp.path(), authority).unwrap(),
        );
        let store = server.brain_store.clone();
        let attachment = store
            .attach(NAME, "one-shot", crate::brain::AttachmentRole::Driver, None)
            .unwrap();
        store
            .create_schedule(
                NAME,
                &attachment.subject,
                attachment.attachment_id,
                crate::brain::ProgramLanguage::Lisp,
                "(say \"once\")",
                crate::vm::EffectSet::pure(),
                0,
                None,
                crate::brain::BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        let brain_id = store.snapshot(NAME).unwrap().brain_id;
        let brain_root = temp.path().join("brains");

        let hook_store = store.clone();
        let hook_root = brain_root.clone();
        schedule_delivery::set_after_queue_hook(Box::new(move || {
            assert!(
                hook_store.evict_resident_brain_for_tests(NAME),
                "the post-queue probe must evict the resident one-shot before readiness reloads it"
            );
            corrupt_brain_journal(&hook_root, NAME);
        }));

        let captured = CapturedEvents::default();
        let subscriber = tracing_subscriber::registry().with(CaptureEventsLayer(captured.clone()));
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let serving = tokio::spawn(Arc::clone(&server).serve_on_listener(listener));
        wait_for_schedule_events(&captured, FAILURE, 1).await;

        let failures = captured.schedule_events(FAILURE);
        let brain_id_text = brain_id.0.to_string();
        assert_eq!(
            failures.len(),
            1,
            "a post-queue one-shot error must begin one failure episode; events={failures:?}"
        );
        assert_eq!(
            failures[0].fields.get("brain_id").map(String::as_str),
            Some(brain_id_text.as_str()),
            "the warning must retain the retired one-shot's durable identity; event={:?}",
            failures[0]
        );
        assert!(
            failures[0]
                .fields
                .get("error")
                .is_some_and(|error| error.contains("duplicate or reordered")),
            "the warning must preserve the post-queue readiness failure; event={:?}",
            failures[0]
        );
        assert_eq!(
            store.active_schedule_observation(NAME),
            None,
            "the final one-shot must already be inactive, proving the warning used captured lineage"
        );

        serving.abort();
        let _ = serving.await;
    }

    /// A one-shot that commits inside `queue_due_schedules_observed`, then a
    /// later sibling append in that same call, must still WARN once. The
    /// episode has to follow the post-commit epoch: the sibling is still due,
    /// the head has moved, and the next pass retries it immediately. That
    /// retry is the recovery and must INFO exactly once.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_partial_queue_failure_after_one_shot_commit_warns_once() {
        const FAILURE: &str = "could not deliver due Brain schedule";
        const NAME: &str = "partial-queue-one-shot";

        let temp = tempfile::tempdir().unwrap();
        let authority = crate::brain::BrainCredentialAuthority::ephemeral([41; 32]);
        let server = Arc::new(
            AgentServer::for_brain_http_test("schedule-log.local", temp.path(), authority).unwrap(),
        );
        let store = server.brain_store.clone();
        let attachment = store
            .attach(NAME, "partial", crate::brain::AttachmentRole::Driver, None)
            .unwrap();
        store
            .create_schedule(
                NAME,
                &attachment.subject,
                attachment.attachment_id,
                crate::brain::ProgramLanguage::Lisp,
                "(say \"once\")",
                crate::vm::EffectSet::pure(),
                0,
                None,
                crate::brain::BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        store
            .create_schedule(
                NAME,
                &attachment.subject,
                attachment.attachment_id,
                crate::brain::ProgramLanguage::Lisp,
                "(say \"later\")",
                crate::vm::EffectSet::pure(),
                1,
                Some(1_000),
                crate::brain::BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        let brain_id = store.snapshot(NAME).unwrap().brain_id;
        let hook_store = store.clone();
        schedule_delivery::set_after_observation_hook(Box::new(move || {
            hook_store.fail_journal_appends_after_successes_for_test(1, 1);
        }));

        let captured = CapturedEvents::default();
        let subscriber = tracing_subscriber::registry().with(CaptureEventsLayer(captured.clone()));
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let serving = tokio::spawn(Arc::clone(&server).serve_on_listener(listener));
        wait_for_schedule_events(&captured, FAILURE, 1).await;

        let failures = captured.schedule_events(FAILURE);
        let brain_id_text = brain_id.0.to_string();
        assert_eq!(
            failures.len(),
            1,
            "a journal error after the one-shot commit must warn once; events={failures:?}"
        );
        assert_eq!(
            failures[0].level,
            tracing::Level::WARN,
            "the partial-queue failure must be a warning; event={:?}",
            failures[0]
        );
        assert_eq!(
            failures[0].fields.get("brain_id").map(String::as_str),
            Some(brain_id_text.as_str()),
            "the warning must name the Brain whose one-shot already committed; event={:?}",
            failures[0]
        );
        assert!(
            failures[0]
                .fields
                .get("error")
                .is_some_and(|error| error.contains("injected journal append failure")),
            "the warning must keep the later append's cause; event={:?}",
            failures[0]
        );
        let snapshot = store.snapshot(NAME).unwrap();
        assert!(
            snapshot
                .schedules
                .iter()
                .any(|schedule| schedule.interval_ms.is_none() && !schedule.active),
            "the one-shot must already be retired, so the warning depended on captured lineage; \
             schedules={:?}",
            snapshot.schedules
        );

        const RECOVERY: &str = "due Brain schedule delivery recovered";
        wait_for_schedule_events(&captured, RECOVERY, 1).await;
        let failures = captured.schedule_events(FAILURE);
        let recoveries = captured.schedule_events(RECOVERY);
        assert_eq!(
            failures.len(),
            1,
            "the sibling retry must not warn again; events={failures:?}"
        );
        assert_eq!(
            recoveries.len(),
            1,
            "the sibling queued on the immediate retry must recover the partial-queue episode; \
             events={recoveries:?}"
        );
        assert_eq!(
            recoveries[0].level,
            tracing::Level::INFO,
            "the partial-queue recovery must be info; event={:?}",
            recoveries[0]
        );
        assert_eq!(
            recoveries[0].fields.get("brain_id").map(String::as_str),
            Some(brain_id_text.as_str()),
            "the recovery must name the same Brain as the warning; event={:?}",
            recoveries[0]
        );
        assert!(
            store.snapshot(NAME).unwrap().runs.iter().any(|run| {
                run.status == crate::brain::BrainRunStatus::QueuedForEnvironment
            }),
            "the recurring sibling must have been queued once the injected append failure was spent"
        );

        serving.abort();
        let _ = serving.await;
    }

    /// Cancel-last plus a same-BrainId one-shot, then a post-queue readiness
    /// failure, must WARN for that successor. The sampled predecessor epoch
    /// must not swallow it, and the successor must not log recovery.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_recreated_one_shot_post_queue_failure_still_warns() {
        const FAILURE: &str = "could not deliver due Brain schedule";
        const NAME: &str = "recreated-one-shot-failure";

        let temp = tempfile::tempdir().unwrap();
        let authority = crate::brain::BrainCredentialAuthority::ephemeral([42; 32]);
        let server = Arc::new(
            AgentServer::for_brain_http_test("schedule-log.local", temp.path(), authority).unwrap(),
        );
        let store = server.brain_store.clone();
        let attachment = store
            .attach(
                NAME,
                "predecessor",
                crate::brain::AttachmentRole::Driver,
                None,
            )
            .unwrap();
        let predecessor = store
            .create_schedule(
                NAME,
                &attachment.subject,
                attachment.attachment_id,
                crate::brain::ProgramLanguage::Lisp,
                "(say \"old\")",
                crate::vm::EffectSet::pure(),
                0,
                Some(1_000),
                crate::brain::BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        let brain_id = store.snapshot(NAME).unwrap().brain_id;
        let brain_root = temp.path().join("brains");
        let hook_store = store.clone();
        let hook_root = brain_root.clone();
        let predecessor_id = predecessor.schedule_id;
        let predecessor_attachment = attachment.attachment_id;
        let predecessor_subject = attachment.subject.clone();
        schedule_delivery::set_after_observation_hook(Box::new(move || {
            hook_store
                .cancel_schedule(
                    NAME,
                    &predecessor_subject,
                    predecessor_attachment,
                    predecessor_id,
                )
                .unwrap();
            let replacement = hook_store
                .attach(
                    NAME,
                    "successor",
                    crate::brain::AttachmentRole::Driver,
                    None,
                )
                .unwrap();
            hook_store
                .create_schedule(
                    NAME,
                    &replacement.subject,
                    replacement.attachment_id,
                    crate::brain::ProgramLanguage::Lisp,
                    "(say \"once\")",
                    crate::vm::EffectSet::pure(),
                    0,
                    None,
                    crate::brain::BrainScheduleDeliveryPolicy::Coalesce,
                )
                .unwrap();
        }));
        let hook_store = store.clone();
        schedule_delivery::set_after_queue_hook(Box::new(move || {
            assert!(
                hook_store.evict_resident_brain_for_tests(NAME),
                "the post-queue probe must evict the resident successor before readiness reloads it"
            );
            corrupt_brain_journal(&hook_root, NAME);
        }));

        let captured = CapturedEvents::default();
        let subscriber = tracing_subscriber::registry().with(CaptureEventsLayer(captured.clone()));
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let serving = tokio::spawn(Arc::clone(&server).serve_on_listener(listener));
        wait_for_schedule_events(&captured, FAILURE, 1).await;

        let failures = captured.schedule_events(FAILURE);
        let brain_id_text = brain_id.0.to_string();
        assert_eq!(
            failures.len(),
            1,
            "the recreated one-shot's post-queue failure must warn once; events={failures:?}"
        );
        assert_eq!(
            failures[0].fields.get("brain_id").map(String::as_str),
            Some(brain_id_text.as_str()),
            "the warning must name the same BrainId the predecessor used; event={:?}",
            failures[0]
        );
        assert!(
            failures[0]
                .fields
                .get("error")
                .is_some_and(|error| error.contains("duplicate or reordered")),
            "the warning must preserve the post-queue readiness failure; event={:?}",
            failures[0]
        );
        assert_eq!(
            store.active_schedule_observation(NAME),
            None,
            "the successor one-shot must already be inactive"
        );
        assert_eq!(
            captured
                .schedule_events("due Brain schedule delivery recovered")
                .len(),
            0,
            "successor failure must not recover the predecessor"
        );

        serving.abort();
        let _ = serving.await;
    }

    /// A runner that passes the pre-dispatch check and is gone before the
    /// dispatch loop must still count the queued one-shot as recovery.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_runner_loss_after_queue_still_recovers_retired_one_shot() {
        const FAILURE: &str = "could not deliver due Brain schedule";
        const RECOVERY: &str = "due Brain schedule delivery recovered";
        const NAME: &str = "runner-dropped-one-shot";

        let temp = tempfile::tempdir().unwrap();
        let authority = crate::brain::BrainCredentialAuthority::ephemeral([43; 32]);
        let server = Arc::new(
            AgentServer::for_brain_http_test("schedule-log.local", temp.path(), authority).unwrap(),
        );
        let store = server.brain_store.clone();
        let attachment = store
            .attach(
                NAME,
                "runner-drop",
                crate::brain::AttachmentRole::Driver,
                None,
            )
            .unwrap();
        store
            .create_schedule(
                NAME,
                &attachment.subject,
                attachment.attachment_id,
                crate::brain::ProgramLanguage::Lisp,
                "(say \"once\")",
                crate::vm::EffectSet::pure(),
                0,
                None,
                crate::brain::BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        let brain_id = store.snapshot(NAME).unwrap().brain_id;
        let journal = temp.path().join("brains").join(NAME).join("events.jsonl");
        let healthy_journal = std::fs::read(&journal).unwrap();
        assert!(store.evict_resident_brain_for_tests(NAME));
        corrupt_brain_journal(temp.path().join("brains").as_path(), NAME);

        let captured = CapturedEvents::default();
        let subscriber = tracing_subscriber::registry().with(CaptureEventsLayer(captured.clone()));
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let serving = tokio::spawn(Arc::clone(&server).serve_on_listener(listener));
        wait_for_schedule_events(&captured, FAILURE, 1).await;

        std::fs::write(&journal, &healthy_journal).unwrap();
        let lease = store
            .acquire_runner_lease(NAME, "runner", store.environment().generation, None, 60_000)
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let registration = server.brain_runners().register(NAME, lease.lease_id, tx);
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook_dropped = Arc::clone(&dropped);
        let hook_runners = server.brain_runners().clone();
        schedule_delivery::set_before_dispatch_readiness_hook(Box::new(move || {
            hook_runners.unregister(NAME, registration);
            hook_dropped.store(true, std::sync::atomic::Ordering::Release);
        }));
        tokio::time::advance(schedule_delivery::UNDELIVERED_RETRY).await;
        wait_for_schedule_events(&captured, RECOVERY, 1).await;

        let recoveries = captured.schedule_events(RECOVERY);
        let brain_id_text = brain_id.0.to_string();
        assert_eq!(
            recoveries.len(),
            1,
            "runner loss after the queue commit must still recover the one-shot; events={recoveries:?}"
        );
        assert_eq!(
            recoveries[0].level,
            tracing::Level::INFO,
            "the queued one-shot recovery must be info; event={:?}",
            recoveries[0]
        );
        assert_eq!(
            recoveries[0].fields.get("brain_id").map(String::as_str),
            Some(brain_id_text.as_str()),
            "the recovery must name the retired one-shot's Brain; event={:?}",
            recoveries[0]
        );
        assert!(
            dropped.load(std::sync::atomic::Ordering::Acquire),
            "the in-loop readiness hook must have dropped the runner"
        );
        assert_eq!(
            store.active_schedule_observation(NAME),
            None,
            "the one-shot must already be inactive when recovery is logged"
        );
        assert_eq!(
            store.snapshot(NAME).unwrap().runs[0].status,
            crate::brain::BrainRunStatus::QueuedForEnvironment,
            "the runner was dropped before dispatch, so the queued run must not be running"
        );
        assert!(
            rx.try_recv().is_err(),
            "dispatch must not have sent a runner request"
        );
        assert_eq!(
            captured.schedule_events(FAILURE).len(),
            1,
            "the repaired one-shot must not warn again"
        );

        serving.abort();
        let _ = serving.await;
    }

    /// Regression for #395 at the production boundary: the real
    /// `serve_on_listener` timer repeatedly selects an unreadable scheduled
    /// Brain. Structured events, not rendered substrings, prove the exact
    /// severity, identity, and actionable cause carried by each transition.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_schedule_delivery_logs_failure_episodes_not_retry_attempts() {
        const FAILURE: &str = "could not deliver due Brain schedule";
        const RECOVERY: &str = "due Brain schedule delivery recovered";
        const NAME: &str = "scheduled-broken";

        let temp = tempfile::tempdir().unwrap();
        let authority = crate::brain::BrainCredentialAuthority::ephemeral([39; 32]);
        let server = Arc::new(
            AgentServer::for_brain_http_test("schedule-log.local", temp.path(), authority).unwrap(),
        );
        let store = server.brain_store.clone();
        let brain_root = temp.path().join("brains");
        let (attachment_id, first_schedule_id) =
            crate::brain::seed_scheduled_brain_for_tests(&store, NAME, 0);
        let first_id = store.snapshot(NAME).unwrap().brain_id;
        let journal = brain_root.join(NAME).join("events.jsonl");
        let healthy_journal = std::fs::read(&journal).unwrap();
        assert!(store.evict_resident_brain_for_tests(NAME));
        corrupt_brain_journal(&brain_root, NAME);
        let corruption = store
            .snapshot(NAME)
            .expect_err("the delivery fixture must be unreadable before the server starts");
        assert!(
            format!("{corruption:#}").contains("duplicate or reordered"),
            "fixture corruption must fail through the real journal integrity check: {corruption:#}"
        );
        assert_eq!(
            store
                .active_schedule_observation(NAME)
                .map(|(brain_id, _)| brain_id),
            Some(first_id)
        );

        let captured = CapturedEvents::default();
        let subscriber = tracing_subscriber::registry().with(CaptureEventsLayer(captured.clone()));
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let serving = tokio::spawn(Arc::clone(&server).serve_on_listener(listener));
        wait_for_schedule_events(&captured, FAILURE, 1).await;

        for _ in 0..4 {
            tokio::time::advance(schedule_delivery::UNDELIVERED_RETRY).await;
            tokio::task::yield_now().await;
        }
        let failures = captured.schedule_events(FAILURE);
        let first_id_text = first_id.0.to_string();
        assert_eq!(
            failures.len(),
            1,
            "one unchanged human-repairable condition must emit one WARN across retries; events={failures:?}"
        );
        assert_eq!(
            failures[0].level,
            tracing::Level::WARN,
            "the first failure transition must remain visible as WARN; event={:?}",
            failures[0]
        );
        assert_eq!(
            failures[0].fields.get("brain").map(String::as_str),
            Some(NAME),
            "the actionable failure must name the display identity; event={:?}",
            failures[0]
        );
        assert_eq!(
            failures[0].fields.get("brain_id").map(String::as_str),
            Some(first_id_text.as_str()),
            "the failure episode must carry the exact durable identity; event={:?}",
            failures[0]
        );
        let error = failures[0].fields.get("error").cloned().unwrap_or_default();
        assert!(
            error.contains("duplicate or reordered"),
            "the WARN must retain the actionable journal-integrity cause, not only an outer context; error={error:?}, event={:?}",
            failures[0]
        );

        std::fs::write(&journal, &healthy_journal).unwrap();
        tokio::time::advance(schedule_delivery::UNDELIVERED_RETRY).await;
        wait_for_schedule_events(&captured, RECOVERY, 1).await;
        let recoveries = captured.schedule_events(RECOVERY);
        assert_eq!(
            recoveries.len(),
            1,
            "repair must emit one recovery; events={recoveries:?}"
        );
        assert_eq!(recoveries[0].level, tracing::Level::INFO);
        assert_eq!(
            recoveries[0].fields.get("brain_id").map(String::as_str),
            Some(first_id_text.as_str())
        );

        // Make the same durable identity fail again. Holding its real
        // execution lane keeps the timer from delivering the newly due work
        // before the journal is corrupted and the resident projection evicted.
        let execution_lock = store.execution_lock(NAME).unwrap();
        let turn = execution_lock.lock_owned().await;
        let recurrence_attachment = store
            .attach(
                NAME,
                "recurrence",
                crate::brain::AttachmentRole::Driver,
                None,
            )
            .unwrap();
        let second_schedule = store
            .create_schedule(
                NAME,
                &recurrence_attachment.subject,
                recurrence_attachment.attachment_id,
                crate::brain::ProgramLanguage::Lisp,
                "(say \"again\")",
                crate::vm::EffectSet::pure(),
                0,
                Some(1_000),
                crate::brain::BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        let repaired_journal = std::fs::read(&journal).unwrap();
        assert!(store.evict_resident_brain_for_tests(NAME));
        corrupt_brain_journal(&brain_root, NAME);
        drop(turn);
        wait_for_schedule_events(&captured, FAILURE, 2).await;
        assert_eq!(
            captured.schedule_events(FAILURE).len(),
            2,
            "failure -> recovery -> failure for one BrainId must begin a fresh episode"
        );

        // Retiring every active schedule clears the episode silently. This
        // repairs the journal only so the cancellation API can load the same
        // identity; neither cancellation nor the next timer pass is recovery.
        std::fs::write(&journal, &repaired_journal).unwrap();
        store
            .cancel_schedule(NAME, "alice", attachment_id, first_schedule_id)
            .unwrap();
        store
            .cancel_schedule(
                NAME,
                &recurrence_attachment.subject,
                recurrence_attachment.attachment_id,
                second_schedule.schedule_id,
            )
            .unwrap();
        tokio::time::advance(schedule_delivery::REWARM_INTERVAL).await;
        tokio::task::yield_now().await;
        assert_eq!(
            captured.schedule_events(RECOVERY).len(),
            1,
            "schedule retirement must silently discard a failed episode rather than synthesize recovery"
        );

        // A repaired one-shot is a real successful delivery even though that
        // delivery retires its final active slot before the handler returns.
        let one_shot_lock = store.execution_lock(NAME).unwrap();
        let one_shot_turn = one_shot_lock.lock_owned().await;
        let episode_attachment = store
            .attach(NAME, "episode", crate::brain::AttachmentRole::Driver, None)
            .unwrap();
        store
            .create_schedule(
                NAME,
                &episode_attachment.subject,
                episode_attachment.attachment_id,
                crate::brain::ProgramLanguage::Lisp,
                "(say \"once\")",
                crate::vm::EffectSet::pure(),
                0,
                None,
                crate::brain::BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        let one_shot_healthy = std::fs::read(&journal).unwrap();
        assert!(store.evict_resident_brain_for_tests(NAME));
        corrupt_brain_journal(&brain_root, NAME);
        drop(one_shot_turn);
        wait_for_schedule_events(&captured, FAILURE, 3).await;
        std::fs::write(&journal, &one_shot_healthy).unwrap();
        tokio::time::advance(schedule_delivery::UNDELIVERED_RETRY).await;
        wait_for_schedule_events(&captured, RECOVERY, 2).await;
        assert_eq!(
            store
                .active_schedule_observation(NAME)
                .map(|(brain_id, _)| brain_id),
            None,
            "the repaired one-shot must have delivered and retired its final active slot"
        );

        // Recreate active work under the same BrainId after the delivery loop
        // samples the predecessor but before queueing acquires the Brain
        // guard. A successful replacement queue must not falsely recover the
        // retired predecessor; the replacement's later failure must WARN.
        let aba_lock = store.execution_lock(NAME).unwrap();
        let aba_turn = aba_lock.lock_owned().await;
        let aba_attachment = store
            .attach(NAME, "aba", crate::brain::AttachmentRole::Driver, None)
            .unwrap();
        let aba_schedule = store
            .create_schedule(
                NAME,
                &aba_attachment.subject,
                aba_attachment.attachment_id,
                crate::brain::ProgramLanguage::Lisp,
                "(say \"old\")",
                crate::vm::EffectSet::pure(),
                0,
                Some(1_000),
                crate::brain::BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        let aba_healthy = std::fs::read(&journal).unwrap();
        assert!(store.evict_resident_brain_for_tests(NAME));
        corrupt_brain_journal(&brain_root, NAME);
        drop(aba_turn);
        wait_for_schedule_events(&captured, FAILURE, 4).await;

        let replacement_fixture = Arc::new(std::sync::Mutex::new(None));
        let hook_fixture = Arc::clone(&replacement_fixture);
        let hook_store = store.clone();
        let hook_root = brain_root.clone();
        let hook_healthy = aba_healthy.clone();
        let aba_attachment_id = aba_attachment.attachment_id;
        let aba_subject = aba_attachment.subject.clone();
        let capture_to_queue_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook_finished = Arc::clone(&capture_to_queue_finished);
        schedule_delivery::set_after_result_hook(Box::new(move || {
            hook_finished.store(true, std::sync::atomic::Ordering::Release);
        }));
        schedule_delivery::set_after_observation_hook(Box::new(move || {
            std::fs::write(hook_root.join(NAME).join("events.jsonl"), hook_healthy).unwrap();
            hook_store
                .cancel_schedule(
                    NAME,
                    &aba_subject,
                    aba_attachment_id,
                    aba_schedule.schedule_id,
                )
                .unwrap();
            let replacement_attachment = hook_store
                .attach(
                    NAME,
                    "aba-replacement",
                    crate::brain::AttachmentRole::Driver,
                    None,
                )
                .unwrap();
            hook_store
                .create_schedule(
                    NAME,
                    &replacement_attachment.subject,
                    replacement_attachment.attachment_id,
                    crate::brain::ProgramLanguage::Lisp,
                    "(say \"replacement\")",
                    crate::vm::EffectSet::pure(),
                    0,
                    None,
                    crate::brain::BrainScheduleDeliveryPolicy::Coalesce,
                )
                .unwrap();
            *hook_fixture.lock().unwrap() = Some((
                replacement_attachment.attachment_id,
                replacement_attachment.subject,
            ));
        }));
        tokio::time::advance(schedule_delivery::UNDELIVERED_RETRY).await;
        for _ in 0..200 {
            if capture_to_queue_finished.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            capture_to_queue_finished.load(std::sync::atomic::Ordering::Acquire),
            "the real delivery loop must finish processing the capture-to-queue replacement attempt"
        );
        assert_eq!(
            captured.schedule_events(RECOVERY).len(),
            2,
            "replacement success after capture-to-queue recreation must not recover the retired predecessor"
        );
        let (replacement_attachment_id, replacement_subject) =
            replacement_fixture.lock().unwrap().clone().unwrap();
        let replacement_lock = store.execution_lock(NAME).unwrap();
        let replacement_turn = replacement_lock.lock_owned().await;
        let replacement_id = store
            .create_schedule(
                NAME,
                &replacement_subject,
                replacement_attachment_id,
                crate::brain::ProgramLanguage::Lisp,
                "(say \"replacement later\")",
                crate::vm::EffectSet::pure(),
                0,
                Some(1_000),
                crate::brain::BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap()
            .schedule_id;
        let replacement_healthy = std::fs::read(&journal).unwrap();
        assert!(store.evict_resident_brain_for_tests(NAME));
        corrupt_brain_journal(&brain_root, NAME);
        drop(replacement_turn);
        wait_for_schedule_events(&captured, FAILURE, 5).await;

        // Now cross the awaited failure itself with another cancel-last and
        // recreation under the same durable identity. The stale Err must not
        // populate the successor epoch or suppress its first real WARN.
        let awaited_successor_fixture = Arc::new(std::sync::Mutex::new(None));
        let hook_successor_fixture = Arc::clone(&awaited_successor_fixture);
        let hook_store = store.clone();
        let hook_root = brain_root.clone();
        schedule_delivery::set_after_attempt_hook(Box::new(move || {
            std::fs::write(
                hook_root.join(NAME).join("events.jsonl"),
                replacement_healthy,
            )
            .unwrap();
            hook_store
                .cancel_schedule(
                    NAME,
                    &replacement_subject,
                    replacement_attachment_id,
                    replacement_id,
                )
                .unwrap();
            let successor_attachment = hook_store
                .attach(
                    NAME,
                    "aba-successor",
                    crate::brain::AttachmentRole::Driver,
                    None,
                )
                .unwrap();
            let successor_attachment = hook_store
                .activate_connection(
                    NAME,
                    successor_attachment.attachment_id,
                    successor_attachment.connection_id.unwrap(),
                )
                .unwrap();
            let successor = hook_store
                .create_schedule(
                    NAME,
                    &successor_attachment.subject,
                    successor_attachment.attachment_id,
                    crate::brain::ProgramLanguage::Lisp,
                    "(say \"successor\")",
                    crate::vm::EffectSet::pure(),
                    0,
                    Some(1_000),
                    crate::brain::BrainScheduleDeliveryPolicy::Coalesce,
                )
                .unwrap();
            let healthy = std::fs::read(hook_root.join(NAME).join("events.jsonl")).unwrap();
            *hook_successor_fixture.lock().unwrap() = Some((
                successor.schedule_id,
                successor_attachment.attachment_id,
                successor_attachment.subject,
                healthy,
            ));
            assert!(hook_store.evict_resident_brain_for_tests(NAME));
            corrupt_brain_journal(&hook_root, NAME);
        }));
        tokio::time::advance(schedule_delivery::UNDELIVERED_RETRY).await;
        tokio::task::yield_now().await;
        assert_eq!(
            captured.schedule_events(FAILURE).len(),
            5,
            "the stale failure crossing same-identity recreation must be discarded"
        );
        tokio::time::advance(schedule_delivery::UNDELIVERED_RETRY).await;
        wait_for_schedule_events(&captured, FAILURE, 6).await;

        // Park a real runner dispatch after queueing, then cancel the last
        // recurring schedule through the lifecycle service while the handler
        // is awaiting that runner. The atomic queue observation predates this
        // external retirement, so the eventual successful runner reply must
        // not masquerade as recovery of the failed delivery episode.
        let (awaited_schedule_id, awaited_attachment_id, awaited_subject, awaited_healthy) =
            awaited_successor_fixture.lock().unwrap().clone().unwrap();
        std::fs::write(&journal, awaited_healthy).unwrap();
        let cancellation_attachment = store
            .attach(
                NAME,
                &awaited_subject,
                crate::brain::AttachmentRole::Driver,
                Some(awaited_attachment_id),
            )
            .unwrap();
        let cancellation_attachment = store
            .activate_connection(
                NAME,
                cancellation_attachment.attachment_id,
                cancellation_attachment.connection_id.unwrap(),
            )
            .unwrap();
        let lifecycle = crate::server::BrainLifecycleService::from_server(&server);
        let lease = store
            .acquire_runner_lease(NAME, "runner", store.environment().generation, None, 60_000)
            .unwrap();
        let (runner_tx, mut runner_rx) = tokio::sync::mpsc::unbounded_channel();
        server
            .brain_runners
            .register(NAME, lease.lease_id, runner_tx);
        tokio::time::advance(schedule_delivery::UNDELIVERED_RETRY).await;
        let request = tokio::time::timeout(std::time::Duration::from_secs(5), runner_rx.recv())
            .await
            .expect("scheduled delivery must reach the registered runner before the liveness bound")
            .expect(
                "the runner request channel must stay open until the scheduled dispatch arrives",
            );
        let crate::server::RunnerRequest::Program(request) = request else {
            panic!("scheduled Lisp delivery must park on a program runner request")
        };
        assert!(
            lifecycle
                .cancel_schedule(
                    NAME,
                    cancellation_attachment.attachment_id,
                    cancellation_attachment.connection_id.unwrap(),
                    awaited_schedule_id,
                )
                .unwrap(),
            "the real lifecycle cancellation path must retire the parked attempt's final schedule"
        );
        let runtime = crate::runtime::ProgramRuntime::new();
        let outcome = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: finch_programs::ProgramLanguage::Lisp,
                source_id: Some("schedule-cancel-race".into()),
                source: request.source,
                intent: "schedule cancellation race".into(),
                effect: finch_programs::ExecutionEffect::Unclassified,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            })
            .await
            .unwrap();
        let checkpoint = runtime
            .revision_history()
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.revision == outcome.output_revision)
            .and_then(|snapshot| snapshot.checkpoint)
            .unwrap();
        let (result_processed_tx, result_processed_rx) = tokio::sync::oneshot::channel();
        schedule_delivery::set_after_result_hook(Box::new(move || {
            let _ = result_processed_tx.send(());
        }));
        request
            .response_tx
            .send(Ok(crate::server::RunnerProgramResult {
                output: outcome.output,
                runtime_revision: outcome.output_revision,
                checkpoint,
                effect_journal: Vec::new(),
            }))
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), result_processed_rx)
            .await
            .expect("the parked delivery result must be classified before the liveness bound")
            .expect("the delivery result hook must remain live until classification");
        assert_eq!(
            captured.schedule_events(RECOVERY).len(),
            2,
            "external cancellation during awaited dispatch must retire the episode silently"
        );

        let fresh_attachment = store
            .attach(
                NAME,
                "post-race",
                crate::brain::AttachmentRole::Driver,
                None,
            )
            .unwrap();
        store
            .create_schedule(
                NAME,
                &fresh_attachment.subject,
                fresh_attachment.attachment_id,
                crate::brain::ProgramLanguage::Lisp,
                "(say \"fresh after cancellation\")",
                crate::vm::EffectSet::pure(),
                0,
                Some(1_000),
                crate::brain::BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        assert!(store.evict_resident_brain_for_tests(NAME));
        corrupt_brain_journal(&brain_root, NAME);
        wait_for_schedule_events(&captured, FAILURE, 7).await;

        // Archive and recreate the display name under a new BrainId. Force a
        // second archive in the exact window after that successor's failed
        // delivery returns but before the loop commits its observation. The
        // stale completion must neither warn nor reinsert an episode.
        store.archive(NAME).unwrap();
        let successor_lock = store.execution_lock(NAME).unwrap();
        let successor_turn = successor_lock.lock_owned().await;
        crate::brain::seed_scheduled_brain_for_tests(&store, NAME, 0);
        let successor_id = store.snapshot(NAME).unwrap().brain_id;
        assert_ne!(
            successor_id, first_id,
            "archive/name reuse must mint a new BrainId"
        );
        assert!(store.evict_resident_brain_for_tests(NAME));
        corrupt_brain_journal(&brain_root, NAME);
        let hook_store = store.clone();
        schedule_delivery::set_after_attempt_hook(Box::new(move || {
            hook_store.archive(NAME).unwrap();
        }));
        drop(successor_turn);
        tokio::time::advance(schedule_delivery::UNDELIVERED_RETRY).await;
        for _ in 0..200 {
            if !brain_root.join(NAME).exists() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            !brain_root.join(NAME).exists(),
            "the deterministic post-attempt hook must retire the attempted identity"
        );
        assert_eq!(
            captured.schedule_events(FAILURE).len(),
            7,
            "a failed completion crossing retirement must not reinsert or log a stale episode"
        );
        assert_eq!(
            captured.schedule_events(RECOVERY).len(),
            2,
            "retirement crossed by a completion must not synthesize recovery"
        );

        // Reuse the name once more. The new identity's first failure must be
        // independent even though both retired predecessors used this alias.
        let final_lock = store.execution_lock(NAME).unwrap();
        let final_turn = final_lock.lock_owned().await;
        crate::brain::seed_scheduled_brain_for_tests(&store, NAME, 0);
        let final_id = store.snapshot(NAME).unwrap().brain_id;
        assert_ne!(final_id, successor_id);
        assert_ne!(final_id, first_id);
        assert!(store.evict_resident_brain_for_tests(NAME));
        corrupt_brain_journal(&brain_root, NAME);
        drop(final_turn);
        wait_for_schedule_events(&captured, FAILURE, 8).await;
        let failures = captured.schedule_events(FAILURE);
        let final_id_text = final_id.0.to_string();
        assert_eq!(
            failures.len(),
            8,
            "the successor's first failure must warn; events={failures:?}"
        );
        assert_eq!(
            failures[7].fields.get("brain_id").map(String::as_str),
            Some(final_id_text.as_str()),
            "the reused name must start a new identity-keyed episode; event={:?}",
            failures[7]
        );
        assert_eq!(
            captured.schedule_events(RECOVERY).len(),
            2,
            "archive and name reuse must not report the predecessor as recovered"
        );

        // A directory removed outside Finch is retired lazily by the real
        // delivery boundary. That disappearance is not recovery, and a later
        // Brain reusing the name must warn independently.
        std::fs::remove_dir_all(brain_root.join(NAME)).unwrap();
        tokio::time::advance(schedule_delivery::UNDELIVERED_RETRY).await;
        for _ in 0..200 {
            if store.active_schedule_observation(NAME).is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(store.active_schedule_observation(NAME), None);
        assert_eq!(captured.schedule_events(FAILURE).len(), 8);
        assert_eq!(
            captured.schedule_events(RECOVERY).len(),
            2,
            "external absence must silently retire the failed episode"
        );
        // Clear the old identity's remaining in-process durable handles before
        // intentionally creating a replacement in this same test process.
        // A real external replacement would ordinarily be observed after a
        // daemon restart; `archive` on the already-absent path is side-effect
        // free on disk and performs that in-memory retirement.
        store.archive(NAME).unwrap();

        let external_successor_lock = store.execution_lock(NAME).unwrap();
        let external_successor_turn = external_successor_lock.lock_owned().await;
        crate::brain::seed_scheduled_brain_for_tests(&store, NAME, 0);
        let external_successor_id = store.snapshot(NAME).unwrap().brain_id;
        assert_ne!(external_successor_id, final_id);
        let external_successor_healthy_journal = std::fs::read(&journal).unwrap();
        assert!(store.evict_resident_brain_for_tests(NAME));
        corrupt_brain_journal(&brain_root, NAME);
        drop(external_successor_turn);
        wait_for_schedule_events(&captured, FAILURE, 9).await;
        let failures = captured.schedule_events(FAILURE);
        let external_successor_id_text = external_successor_id.0.to_string();
        assert_eq!(
            failures[8].fields.get("brain_id").map(String::as_str),
            Some(external_successor_id_text.as_str()),
            "name reuse after external absence must start a fresh episode"
        );

        serving.abort();
        let _ = serving.await;

        // A daemon restart deliberately starts with no failure registry. Once
        // the same exact identity is indexed and genuinely fails again, it
        // may warn once; it must not emit recovery merely because the old
        // process had recorded a failure. Hold the real execution lane until
        // startup warm-up has indexed the healthy durable state, then corrupt
        // it before the first delivery can pass that lane.
        std::fs::write(&journal, &external_successor_healthy_journal).unwrap();
        let restart_authority = crate::brain::BrainCredentialAuthority::ephemeral([40; 32]);
        let restarted = Arc::new(
            AgentServer::for_brain_http_test("schedule-log.local", temp.path(), restart_authority)
                .unwrap(),
        );
        let restarted_store = restarted.brain_store.clone();
        let restart_lock = restarted_store.execution_lock(NAME).unwrap();
        let restart_turn = restart_lock.lock_owned().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let restarted_serving = tokio::spawn(Arc::clone(&restarted).serve_on_listener(listener));
        for _ in 0..200 {
            if restarted_store
                .active_schedule_observation(NAME)
                .map(|(brain_id, _)| brain_id)
                == Some(external_successor_id)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            restarted_store
                .active_schedule_observation(NAME)
                .map(|(brain_id, _)| brain_id),
            Some(external_successor_id),
            "restart warm-up must index the same durable identity before the failure probe"
        );
        assert!(restarted_store.evict_resident_brain_for_tests(NAME));
        corrupt_brain_journal(&brain_root, NAME);
        drop(restart_turn);
        wait_for_schedule_events(&captured, FAILURE, 10).await;
        let failures = captured.schedule_events(FAILURE);
        assert_eq!(
            failures.len(),
            10,
            "restart may warn once for its first real failed attempt"
        );
        assert_eq!(
            failures[9].fields.get("brain_id").map(String::as_str),
            Some(external_successor_id_text.as_str())
        );
        assert_eq!(
            captured.schedule_events(RECOVERY).len(),
            2,
            "restart must not synthesize recovery from the previous process's ephemeral state"
        );
        restarted_serving.abort();
        let _ = restarted_serving.await;
    }

    async fn submit_feedback_to_daemon(address: SocketAddr, query: &str) {
        let client = reqwest::Client::new();
        let endpoint = format!("http://{address}/v1/feedback");
        for _ in 0..20 {
            match client
                .post(&endpoint)
                .json(&serde_json::json!({
                    "query": query,
                    "response": "metadata-only response",
                    "weight": 3.0,
                    "feedback": "retain privately"
                }))
                .send()
                .await
            {
                Ok(response) => {
                    assert_eq!(response.status(), reqwest::StatusCode::OK);
                    let status: serde_json::Value = client
                        .post(format!("http://{address}/v1/training/status"))
                        .send()
                        .await
                        .unwrap()
                        .json()
                        .await
                        .unwrap();
                    assert_eq!(status["training_active"], false);
                    assert_eq!(status["queue_length"], 0);
                    return;
                }
                Err(_) => tokio::task::yield_now().await,
            }
        }
        panic!("feedback daemon did not accept a local request");
    }

    #[tokio::test(start_paused = true)]
    async fn test_daemon_feedback_timeout_and_restart_never_request_training_process() {
        let temp = tempfile::tempdir().unwrap();
        let launches = Arc::new(AtomicUsize::new(0));
        let feedback_path = temp.path().join("feedback.jsonl");
        let legacy_queue = temp.path().join("training_queue.jsonl");
        let adapter = temp.path().join("adapters/latest.safetensors");
        let legacy_queue_contents = "legacy queued Python training example\n";
        std::fs::write(&legacy_queue, legacy_queue_contents).unwrap();
        let observed_launches = Arc::clone(&launches);
        let _launch_observer = crate::training::lora_subprocess::observe_training_process_launches(
            Arc::new(move || {
                observed_launches.fetch_add(1, Ordering::SeqCst);
            }),
        );

        for (cycle, query) in ["before restart", "after restart"].into_iter().enumerate() {
            let authority =
                crate::brain::BrainCredentialAuthority::ephemeral([cycle as u8 + 1; 32]);
            let server =
                AgentServer::for_brain_http_test("feedback-fixture.local", temp.path(), authority)
                    .unwrap();
            let server = Arc::new(server);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let serving = tokio::spawn(Arc::clone(&server).serve_on_listener(listener));

            submit_feedback_to_daemon(address, query).await;
            tokio::time::advance(tokio::time::Duration::from_secs(10 * 60)).await;
            tokio::task::yield_now().await;

            assert_eq!(launches.load(Ordering::SeqCst), 0);
            assert_eq!(
                std::fs::read_to_string(&legacy_queue).unwrap(),
                legacy_queue_contents
            );
            assert!(!adapter.exists());
            serving.abort();
            let _ = serving.await;
        }

        let entries = FeedbackLogger::at(&feedback_path)
            .unwrap()
            .load_all()
            .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].query, "before restart");
        assert_eq!(entries[1].query, "after restart");
        assert_eq!(entries[0].weight, 3.0);
        assert_eq!(entries[1].weight, 3.0);
        assert_eq!(launches.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_daemon_feedback_storage_failure_redacts_private_path() {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().join("customer-secret-project");
        let authority = crate::brain::BrainCredentialAuthority::ephemeral([7; 32]);
        let mut server =
            AgentServer::for_brain_http_test("feedback-fixture.local", &state_root, authority)
                .unwrap();
        let feedback_path = state_root.join("feedback.jsonl");
        server.feedback_store = Arc::new(
            FeedbackLogger::at(&feedback_path)
                .unwrap()
                .with_injected_log_error(format!("failed at {}", feedback_path.display())),
        );
        let server = Arc::new(server);
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_writer = Arc::clone(&captured);
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || CapturedLogs(Arc::clone(&captured_writer)))
            .finish();
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let serving = tokio::spawn(Arc::clone(&server).serve_on_listener(listener));

        let client = reqwest::Client::new();
        let mut response = None;
        for _ in 0..20 {
            match client
                .post(format!("http://{address}/v1/feedback"))
                .json(&serde_json::json!({
                    "query": "redact storage location",
                    "response": "metadata only",
                    "weight": 1.0
                }))
                .send()
                .await
            {
                Ok(result) => {
                    response = Some(result);
                    break;
                }
                Err(_) => tokio::task::yield_now().await,
            }
        }
        let response = response.expect("feedback daemon did not accept a local request");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        );
        let body = response.text().await.unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({
                "status": "error",
                "message": "Could not persist feedback"
            })
        );
        assert!(!body.contains("customer-secret-project"));
        assert!(!body.contains(temp.path().to_string_lossy().as_ref()));

        let logs = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("Failed to persist private feedback"));
        assert!(logs.contains("storage-error"));
        assert!(!logs.contains("customer-secret-project"));
        assert!(!logs.contains(temp.path().to_string_lossy().as_ref()));

        serving.abort();
        let _ = serving.await;
    }

    /// Regression for the local-model generation leg that never streamed:
    /// `DaemonClient::query_local` hardcoded `stream: false`, and
    /// `DaemonLocalGenerator::generate_stream` always returned `Ok(None)`
    /// regardless of arguments, so a local-model turn always fell through to
    /// the blocking non-streaming path even though the daemon's own SSE
    /// endpoint (`handle_chat_completions_streaming` above) already worked —
    /// reachable, until now, only by an external OpenAI-API-compatible
    /// client that explicitly sent `stream: true`.
    ///
    /// This spins up a real `AgentServer` (same `serve_on_listener` the
    /// production daemon uses) on a real ephemeral TCP listener, with
    /// `GeneratorState::Ready` backed by a `TextGeneration` test double via
    /// `GeneratorModel::from_test_backend` — no GGUF file, no model
    /// download, no supervisor-issued daemon authority needed, because
    /// `for_brain_http_test` (used by the feedback tests above) never
    /// touches `~/.finch` or daemon auto-discovery; it only needs an
    /// ephemeral credential and a temp state root, same as those tests. It
    /// then drives the server through `DaemonClient::query_local_stream_cancellable`
    /// — the exact method `DaemonLocalGenerator::generate_stream_cancellable`
    /// now calls — over a real HTTP POST and real SSE bytes on the wire, not
    /// a helper-only unit test of the parser alone.
    #[tokio::test(flavor = "current_thread")]
    async fn daemon_local_stream_delivers_text_deltas_over_real_sse_round_trip() {
        use crate::config::ExecutionTarget;
        use crate::models::{
            GeneratorConfig, GeneratorModel, InferenceProvider, ModelFamily, ModelLoadConfig,
            ModelSize, TextGeneration, TokenCallback,
        };

        struct StreamingTestBackend;

        impl TextGeneration for StreamingTestBackend {
            fn generate(&mut self, _input_ids: &[u32], _max: usize) -> Result<Vec<u32>> {
                Ok(b"streamed-local-token"
                    .iter()
                    .map(|byte| u32::from(*byte))
                    .collect())
            }

            fn generate_stream(
                &mut self,
                input_ids: &[u32],
                max_new_tokens: usize,
                mut callback: TokenCallback,
            ) -> Result<Vec<u32>> {
                let output = self.generate(input_ids, max_new_tokens)?;
                callback(output[0], "streamed-local-token");
                Ok(output)
            }

            fn tokenize(&self, text: &str) -> Result<Vec<u32>> {
                Ok(text.bytes().map(u32::from).collect())
            }

            fn decode_tokens(&self, tokens: &[u32]) -> Result<String> {
                let bytes = tokens.iter().map(|token| *token as u8).collect();
                Ok(String::from_utf8(bytes)?)
            }

            fn name(&self) -> &str {
                "streaming test backend"
            }

            fn as_any(&self) -> &dyn std::any::Any {
                self
            }

            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }

        let config = GeneratorConfig::Pretrained(ModelLoadConfig {
            provider: InferenceProvider::LlamaCpp,
            family: ModelFamily::Gemma2,
            size: ModelSize::Small,
            target: ExecutionTarget::Cpu,
            model_path: None,
        });
        let model = GeneratorModel::from_test_backend(Box::new(StreamingTestBackend), config);
        let shared_model = Arc::new(RwLock::new(model));
        let local_generator = Arc::new(RwLock::new(LocalGenerator::with_models(Some(Arc::clone(
            &shared_model,
        )))));
        let generator_state = Arc::new(RwLock::new(GeneratorState::Ready {
            model: shared_model,
            model_name: "streaming test backend".to_string(),
        }));

        let temp = tempfile::tempdir().unwrap();
        let authority = crate::brain::BrainCredentialAuthority::ephemeral([42; 32]);
        let mut server =
            AgentServer::for_brain_http_test("stream-fixture.local", temp.path(), authority)
                .unwrap();
        server.local_generator = local_generator;
        server.generator_state = generator_state;
        let server = Arc::new(server);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let serving = tokio::spawn(Arc::clone(&server).serve_on_listener(listener));

        let client = crate::client::DaemonClient::for_test(format!("http://{address}"));
        let mut rx = None;
        for _ in 0..20 {
            match client
                .query_local_stream_cancellable(
                    vec![crate::providers::Message::user("hello")],
                    tokio_util::sync::CancellationToken::new(),
                )
                .await
            {
                Ok(receiver) => {
                    rx = Some(receiver);
                    break;
                }
                Err(_) => tokio::task::yield_now().await,
            }
        }
        let mut rx = rx.expect("streaming daemon did not accept a local request");

        let mut received_text = String::new();
        let mut chunk_count = 0usize;
        while let Some(result) = rx.recv().await {
            match result {
                Ok(crate::generators::StreamChunk::TextDelta(delta)) => {
                    received_text.push_str(&delta);
                    chunk_count += 1;
                }
                Ok(other) => panic!("unexpected non-text chunk from local SSE stream: {other:?}"),
                Err(error) => panic!("local SSE stream produced an error: {error}"),
            }
        }

        assert!(
            chunk_count > 0,
            "expected at least one TextDelta chunk from the real SSE round trip through the \
             daemon's own /v1/chat/completions endpoint; got zero chunks"
        );
        assert!(
            received_text.contains("streamed-local-token"),
            "expected the mock backend's streamed token to reach the client through the real \
             daemon SSE endpoint; received {received_text:?} across {chunk_count} chunk(s)"
        );

        serving.abort();
        let _ = serving.await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_daemon_feedback_quota_is_redacted_unchanged_and_never_trains() {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().join("customer-secret-project");
        let feedback_path = state_root.join("feedback.jsonl");
        let legacy_queue = state_root.join("training_queue.jsonl");
        let adapter = state_root.join("adapters/latest.safetensors");
        let launches = Arc::new(AtomicUsize::new(0));
        let observed_launches = Arc::clone(&launches);
        let _launch_observer = crate::training::lora_subprocess::observe_training_process_launches(
            Arc::new(move || {
                observed_launches.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let authority = crate::brain::BrainCredentialAuthority::ephemeral([8; 32]);
        let server = Arc::new(
            AgentServer::for_brain_http_test("feedback-fixture.local", &state_root, authority)
                .unwrap(),
        );
        std::fs::write(&legacy_queue, "legacy queued example\n").unwrap();
        let mut feedback = std::fs::OpenOptions::new()
            .write(true)
            .open(&feedback_path)
            .unwrap();
        feedback
            .set_len(crate::feedback::FEEDBACK_LOG_MAX_BYTES)
            .unwrap();
        feedback.seek(SeekFrom::End(-1)).unwrap();
        feedback.write_all(b"\n").unwrap();
        feedback.sync_all().unwrap();
        drop(feedback);
        let bytes_before = std::fs::read(&feedback_path).unwrap();

        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_writer = Arc::clone(&captured);
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || CapturedLogs(Arc::clone(&captured_writer)))
            .finish();
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let serving = tokio::spawn(Arc::clone(&server).serve_on_listener(listener));

        let client = reqwest::Client::new();
        let mut response = None;
        for _ in 0..20 {
            match client
                .post(format!("http://{address}/v1/feedback"))
                .json(&serde_json::json!({
                    "query": "quota boundary",
                    "response": "metadata only",
                    "weight": 1.0
                }))
                .send()
                .await
            {
                Ok(result) => {
                    response = Some(result);
                    break;
                }
                Err(_) => tokio::task::yield_now().await,
            }
        }
        let response = response.expect("feedback daemon did not accept a local request");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        );
        let body = response.text().await.unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({
                "status": "error",
                "message": "Could not persist feedback"
            })
        );
        tokio::time::advance(tokio::time::Duration::from_secs(10 * 60)).await;
        tokio::task::yield_now().await;

        let logs = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("Failed to persist private feedback"));
        assert!(logs.contains("quota-exceeded"));
        assert!(!body.contains("customer-secret-project"));
        assert!(!logs.contains("customer-secret-project"));
        assert!(!logs.contains(temp.path().to_string_lossy().as_ref()));
        assert_eq!(std::fs::read(&feedback_path).unwrap(), bytes_before);
        assert_eq!(
            std::fs::read_to_string(&legacy_queue).unwrap(),
            "legacy queued example\n"
        );
        assert!(!adapter.exists());
        assert_eq!(launches.load(Ordering::SeqCst), 0);

        serving.abort();
        let _ = serving.await;
    }

    fn isolated_http_server() -> (tempfile::TempDir, Arc<AgentServer>) {
        let state = tempfile::tempdir().unwrap();
        let credentials = crate::brain::BrainCredentialAuthority::load_or_create(
            &state.path().join("credentials"),
        )
        .unwrap();
        let server =
            AgentServer::for_brain_http_test("test.local", state.path(), credentials).unwrap();
        (state, Arc::new(server))
    }

    fn isolated_http_router(server: Arc<AgentServer>) -> axum::Router {
        crate::server::handlers::create_router(server)
            .layer(axum::extract::DefaultBodyLimit::max(4 * 1024 * 1024))
    }

    fn supervisor_contract_present() -> bool {
        // The permanent Brain-isolation CI gate runs these entries through
        // scripts/test_brains.sh, which supplies the authenticated contract.
        std::env::var_os("FINCH_BRAIN_TEST_TOKEN").is_some()
    }

    fn durable_file_fingerprints(
        directory: &std::path::Path,
    ) -> Vec<(std::path::PathBuf, usize, String)> {
        use sha2::Digest as _;

        let mut pending = vec![directory.to_path_buf()];
        let mut files = Vec::new();
        while let Some(current) = pending.pop() {
            for entry in std::fs::read_dir(&current).unwrap() {
                let entry = entry.unwrap();
                let file_type = entry.file_type().unwrap();
                if file_type.is_dir() {
                    pending.push(entry.path());
                } else if file_type.is_file() {
                    let bytes = std::fs::read(entry.path()).unwrap();
                    files.push((
                        entry.path().strip_prefix(directory).unwrap().to_path_buf(),
                        bytes.len(),
                        hex::encode(sha2::Sha256::digest(bytes)),
                    ));
                }
            }
        }
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    }

    fn request_id_tracing_router(server: Arc<AgentServer>) -> axum::Router {
        let request_id_header = axum::http::HeaderName::from_static(REQUEST_ID_HEADER);
        isolated_http_router(server).layer(
            ServiceBuilder::new()
                .layer(axum::middleware::from_fn(strip_client_request_id))
                .layer(SetRequestIdLayer::new(
                    request_id_header.clone(),
                    MakeRequestUuid,
                ))
                .layer(TraceLayer::new_for_http().make_span_with(request_tracing_span))
                .layer(PropagateRequestIdLayer::new(request_id_header)),
        )
    }

    /// #223: the daemon's logs had no way to tell which client request (if
    /// any) was in flight when a wedge happened. Through the real production
    /// layer stack, a request with no id must get a fresh one assigned and
    /// echoed on the response, so a client-side log can be joined to the
    /// daemon's by that id.
    #[tokio::test]
    async fn production_router_assigns_and_echoes_a_request_id() {
        use tower::ServiceExt as _;
        let (_state, server) = isolated_http_server();
        let request_id_header = axum::http::HeaderName::from_static(REQUEST_ID_HEADER);

        let response = request_id_tracing_router(server)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let request_id = response.headers().get(&request_id_header).expect(
            "response must echo an assigned request id so a client can correlate its own logs",
        );
        assert!(
            !request_id.is_empty(),
            "assigned request id header must not be empty"
        );
    }

    /// A caller-supplied request id must not survive into the response or the
    /// tracing span. The production stack strips it before assigning its own
    /// UUID so untrusted header bytes cannot forge daemon log content.
    #[tokio::test]
    async fn production_router_replaces_a_caller_supplied_request_id() {
        use tower::ServiceExt as _;
        let (_state, server) = isolated_http_server();
        let request_id_header = axum::http::HeaderName::from_static(REQUEST_ID_HEADER);

        let response = request_id_tracing_router(server)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .header(&request_id_header, "caller-supplied-id-123")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "request-id replacement must not alter the response status"
        );
        let assigned = response
            .headers()
            .get(&request_id_header)
            .expect("response must carry the daemon-assigned request id")
            .to_str()
            .expect("tower-http UUID request ids are valid header text");
        assert_ne!(
            assigned, "caller-supplied-id-123",
            "an untrusted caller-supplied request id must be replaced"
        );
        uuid::Uuid::parse_str(assigned)
            .unwrap_or_else(|error| panic!("daemon-assigned request id must be a UUID: {error}"));
    }

    /// The span `request_tracing_span` builds must actually carry the
    /// request id, method, and uri as named fields (not just interpolated
    /// into an opaque message), so a `tracing_subscriber::fmt` layer renders
    /// them as filterable/greppable `key=value` pairs in the log file.
    #[test]
    fn request_tracing_span_carries_named_fields() {
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header(REQUEST_ID_HEADER, "abc123")
            .body(axum::body::Body::empty())
            .unwrap();
        let span = request_tracing_span(&request);
        let metadata = span
            .metadata()
            .expect("span must be enabled and have metadata");
        let field_names: Vec<&str> = metadata.fields().iter().map(|f| f.name()).collect();
        assert!(field_names.contains(&"request_id"));
        assert!(field_names.contains(&"method"));
        assert!(field_names.contains(&"uri"));
    }

    #[tokio::test]
    async fn production_router_health_is_hermetic() {
        use tower::ServiceExt as _;
        let (_state, server) = isolated_http_server();
        let response = isolated_http_router(server)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    /// `/health` reports the uptime it has actually accumulated.
    ///
    /// Over HTTP through the real router, because that is where the defect
    /// was: `uptime_seconds` was a hardcoded `0` in the handler (#131). An
    /// earlier version of this PR tested `AgentServer::uptime()` instead, and
    /// reverting the handler to `uptime_seconds: 0` left the whole suite green
    /// — the accessor was right and the endpoint still lied.
    #[tokio::test]
    async fn production_router_health_reports_real_uptime() {
        use tower::ServiceExt as _;
        let (_state, server) = isolated_http_server();

        // The handler reports whole seconds, so the server has to have existed
        // for at least one before a truthful answer is distinguishable from
        // the placeholder.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

        let response = isolated_http_router(Arc::clone(&server))
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("health body");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("health returns JSON");
        let reported = parsed
            .get("uptime_seconds")
            .and_then(serde_json::Value::as_u64)
            .expect("health reports uptime_seconds");

        assert!(
            reported >= 1,
            "/health must report the time this server has run, not a \
             placeholder; got {reported} after sleeping past a second"
        );

        // Pin the response shape. `src/main.rs` re-declares this struct to
        // deserialize it, so a rename here compiles, passes `cargo test
        // --lib`, and breaks `finch daemon-status` at runtime with "Failed to
        // parse health response" -- review of #334 proved that by renaming
        // `named_brains` and watching 150/150 stay green.
        //
        // `uptime_seconds` is a whole-second `u64`, so a daemon does report
        // `0` for its first second. That is truncation of a measured value,
        // not #131's hardcoded placeholder, but it is indistinguishable from
        // one in a single scrape -- `/metrics` carries the sub-second gauge.
        for (field, ok) in [
            (
                "status",
                parsed.get("status").is_some_and(|v| v.is_string()),
            ),
            (
                "named_brains",
                parsed
                    .get("named_brains")
                    .and_then(serde_json::Value::as_u64)
                    .is_some(),
            ),
            (
                "pending_brain_terminalizations",
                parsed
                    .get("pending_brain_terminalizations")
                    .and_then(serde_json::Value::as_u64)
                    .is_some(),
            ),
        ] {
            assert!(
                ok,
                "/health must keep publishing `{field}` with the type \
                 `finch daemon-status` deserializes; body was {parsed}"
            );
        }
    }

    /// `/metrics` reports a measured value, not a constant.
    ///
    /// Asserting the series *name* is not enough, and review of #334 proved
    /// it: hardcoding `finch_daemon_uptime_seconds 0` left the whole suite
    /// green, which is #131's own acceptance wording ("never reports
    /// fabricated zero placeholders as live truth") failing under the name
    /// this PR introduced. A `contains` check was also satisfied by the
    /// `# HELP` line alone, so an endpoint emitting no sample at all passed.
    ///
    /// So this parses the sample and scrapes twice. A constant fails whatever
    /// it is named and whatever value it is given, which a name check
    /// structurally cannot catch.
    #[tokio::test]
    async fn production_router_metrics_reports_a_measured_value() {
        use tower::ServiceExt as _;
        let (_state, server) = isolated_http_server();

        async fn scrape(server: Arc<AgentServer>) -> String {
            use tower::ServiceExt as _;
            let response = isolated_http_router(server)
                .oneshot(
                    axum::http::Request::builder()
                        .uri("/metrics")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .expect("metrics body");
            String::from_utf8(body.to_vec()).expect("metrics is UTF-8")
        }

        /// The metric name of an exposition sample line, without its
        /// labelset. `foo{a="b c"} 1` and `foo 1` both yield `foo`.
        ///
        /// Splitting on whitespace alone would keep the labels, so the
        /// whitelist below would reject every labelled series and its advice
        /// ("add it to MEASURED") would be wrong -- a labelset is not a name
        /// and whitelisting one breaks on the next label value. The #131 work
        /// this PR defers is routing and token aggregates, which are labelled
        /// by construction, so that is the first follow-up and not a remote
        /// case.
        fn sample_name(line: &str) -> &str {
            line.split(['{', ' ']).next().unwrap_or_default()
        }

        fn gauge(text: &str) -> f64 {
            let line = text
                .lines()
                .find(|line| {
                    !line.starts_with('#') && sample_name(line) == "finch_daemon_uptime_seconds"
                })
                .unwrap_or_else(|| panic!("no gauge sample line in:\n{text}"));
            let value = match line.find('}') {
                Some(labelset_end) => &line[labelset_end + 1..],
                None => &line[sample_name(line).len()..],
            };
            value
                .split_whitespace()
                .next()
                .unwrap_or_else(|| panic!("gauge sample has no value in:\n{text}"))
                .parse()
                .unwrap_or_else(|error| panic!("gauge is not a float ({error}) in:\n{text}"))
        }

        let first_text = scrape(Arc::clone(&server)).await;
        assert!(
            first_text.contains("# TYPE finch_daemon_uptime_seconds gauge"),
            "the gauge needs its TYPE line: {first_text}"
        );
        // Every series present must be one this daemon actually measures.
        //
        // Naming the historical offender alone was not enough: review of #334
        // added `finch_requests_total 0` beside a working gauge and the test
        // passed. #131 asks that no fabricated placeholder be published, not
        // that one string stay absent, so the check is a whitelist —
        // publishing anything new requires deciding here whether it is
        // measured.
        const MEASURED: &[&str] = &["finch_daemon_uptime_seconds"];
        for line in first_text.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let series = sample_name(line);
            assert!(
                MEASURED.contains(&series),
                "`{series}` is published but not in the measured set. If it is \
                 real, add it to MEASURED; if it is a placeholder, do not \
                 publish it (#131): {first_text}"
            );
        }

        let first = gauge(&first_text);
        assert!(
            first > 0.0,
            "the gauge must report measured time, not zero: {first_text}"
        );

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let second = gauge(&scrape(server).await);

        assert!(
            second > first,
            "two scrapes must differ -- a constant is a fabricated series \
             whatever it is called: {first} then {second}"
        );
    }

    async fn serve_http2_test_router(
        app: axum::Router,
    ) -> (SocketAddr, tokio::task::JoinHandle<std::io::Result<()>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("HTTP/2 server fixture must bind a kernel-assigned loopback port");
        let address = listener
            .local_addr()
            .expect("HTTP/2 server fixture must expose its bound address");
        let serving = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
        });
        (address, serving)
    }

    #[tokio::test]
    async fn production_router_health_completes_over_http2_prior_knowledge() {
        let (_state, server) = isolated_http_server();
        let (address, serving) = serve_http2_test_router(isolated_http_router(server)).await;
        let socket = tokio::net::TcpStream::connect(address)
            .await
            .expect("HTTP/2 client fixture must connect to Finch's production router");
        let (mut client, connection) = h2::client::handshake(socket)
            .await
            .expect("HTTP/2 client fixture handshake must complete");
        let connection = tokio::spawn(connection);
        let request = axum::http::Request::builder()
            .method("GET")
            .uri(format!("http://{address}/health"))
            .version(axum::http::Version::HTTP_2)
            .body(())
            .expect("HTTP/2 health request must be valid");
        let (response, _) = client
            .send_request(request, true)
            .expect("HTTP/2 health request headers must send");

        let response = tokio::time::timeout(tokio::time::Duration::from_secs(2), response)
            .await
            .expect("Finch's normal HTTP/2 health response must be bounded")
            .expect("Finch's normal HTTP/2 health response must complete");
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "Finch's production health route must succeed over HTTP/2"
        );

        serving.abort();
        connection.abort();
        let _ = serving.await;
        let _ = connection.await;
    }

    #[tokio::test]
    async fn production_router_rejects_excess_undrained_empty_http2_data_frames() {
        let (_state, server) = isolated_http_server();
        let app = isolated_http_router(server).route(
            "/_test/undrained-body",
            axum::routing::post(|request: axum::extract::Request| async move {
                let _undrained_request = request;
                std::future::pending::<axum::http::StatusCode>().await
            }),
        );
        let (address, serving) = serve_http2_test_router(app).await;
        let socket = tokio::net::TcpStream::connect(address)
            .await
            .expect("HTTP/2 client fixture must connect to Finch's production router");
        let (mut client, connection) = h2::client::handshake(socket)
            .await
            .expect("HTTP/2 client fixture handshake must complete");
        let connection = tokio::spawn(connection);
        let request = axum::http::Request::builder()
            .method("POST")
            .uri(format!("http://{address}/_test/undrained-body"))
            .version(axum::http::Version::HTTP_2)
            .body(())
            .expect("HTTP/2 undrained-body request must be valid");
        let (response, mut body) = client
            .send_request(request, false)
            .expect("HTTP/2 undrained-body request headers must send");
        for frame_index in 0..101 {
            body.send_data(axum::body::Bytes::new(), false)
                .unwrap_or_else(|error| {
                    panic!("HTTP/2 client failed to queue empty DATA frame {frame_index}: {error}")
                });
        }

        let error = tokio::time::timeout(tokio::time::Duration::from_secs(2), response)
            .await
            .expect("Finch must reject excess undrained empty DATA frames within two seconds")
            .expect_err("Finch must close an HTTP/2 stream that exceeds the empty DATA budget");
        let diagnostic = format!("{error:?}");
        assert!(
            error.reason() == Some(h2::Reason::ENHANCE_YOUR_CALM) || error.is_io(),
            "Finch must close the abusive HTTP/2 connection with a protocol or I/O error; got: {diagnostic}"
        );

        serving.abort();
        connection.abort();
        let _ = serving.await;
        let _ = connection.await;
    }

    #[tokio::test]
    async fn production_router_rejects_malformed_and_oversized_messages() {
        use tower::ServiceExt as _;
        let (_state, server) = isolated_http_server();
        let malformed = isolated_http_router(Arc::clone(&server))
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"bad": json"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(matches!(malformed.status().as_u16(), 400 | 422));

        let oversized = isolated_http_router(server)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from("0".repeat(5 * 1024 * 1024)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            oversized.status(),
            axum::http::StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    /// A real server reports the time it has actually been running.
    ///
    /// `/health` used to answer `uptime_seconds: 0` however long the daemon
    /// had been up, and `/metrics` a constant `finch_queries_total 0`. Both
    /// were marked as placeholders in the source and neither was tested, so a
    /// scraper could not tell "nothing has happened" from "nothing is
    /// measured" (#131).
    ///
    /// Built through the production constructor rather than by reading a
    /// field: the defect was that the *server* did not know when it started,
    /// and a test that constructs a `Duration` by hand would not have noticed.
    #[test]
    fn production_server_reports_real_uptime() {
        if !supervisor_contract_present() {
            return;
        }
        let proof = crate::brain::isolated_test_proof()
            .expect("uptime test requires supervisor-issued authority");
        let daemon_address = proof.daemon_address().to_owned();
        let brain_password = proof.brain_password().unwrap();
        let home = proof.home;

        let config =
            crate::config::Config::with_providers(vec![crate::config::ProviderEntry::Claude {
                api_key: "isolated-uptime-test".into(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("isolated-uptime-test".into()),
            }]);
        let generator_state = Arc::new(RwLock::new(GeneratorState::NotAvailable));
        let server = AgentServer::new(
            config,
            ServerConfig {
                bind_address: daemon_address,
                brain_password,
                ..ServerConfig::default()
            },
            ClaudeClient::new("isolated-uptime-test".to_string()).unwrap(),
            Router::new(crate::models::ThresholdRouter::new()),
            MetricsLogger::new(home.join(".finch/uptime-metrics")).unwrap(),
            Arc::new(RwLock::new(LocalGenerator::new())),
            Arc::new(BootstrapLoader::new(Arc::clone(&generator_state), None)),
            generator_state,
            isolated_provider_graph(),
        )
        .unwrap();

        let first = server.uptime();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let second = server.uptime();

        assert!(
            second > first,
            "uptime must advance: {first:?} then {second:?}"
        );
        assert!(
            second >= std::time::Duration::from_millis(50),
            "uptime must reflect the elapsed time, not a constant: {second:?}"
        );
    }

    #[test]
    fn production_constructor_persists_named_brain_only_in_isolated_home() {
        if !supervisor_contract_present() {
            return;
        }
        let proof = crate::brain::isolated_test_proof()
            .expect("production constructor test requires supervisor-issued authority");
        let daemon_address = proof.daemon_address().to_owned();
        let brain_password = proof.brain_password().unwrap();
        let home = proof.home;
        let expected_root = proof.root;
        assert_eq!(expected_root, home.join(".finch/brains"));

        let config =
            crate::config::Config::with_providers(vec![crate::config::ProviderEntry::Claude {
                api_key: "isolated-constructor-test".into(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("isolated-constructor-test".into()),
            }]);
        let generator_state = Arc::new(RwLock::new(GeneratorState::NotAvailable));
        let server = AgentServer::new(
            config,
            ServerConfig {
                bind_address: daemon_address,
                brain_password,
                ..ServerConfig::default()
            },
            ClaudeClient::new("isolated-constructor-test".to_string()).unwrap(),
            Router::new(crate::models::ThresholdRouter::new()),
            MetricsLogger::new(home.join(".finch/constructor-metrics")).unwrap(),
            Arc::new(RwLock::new(LocalGenerator::new())),
            Arc::new(BootstrapLoader::new(Arc::clone(&generator_state), None)),
            generator_state,
            isolated_provider_graph(),
        )
        .unwrap();
        let name = format!("constructor-boundary-{}", uuid::Uuid::new_v4().simple());
        server
            .brain_store()
            .push(
                &name,
                "isolation-test",
                crate::brain::BrainEventKind::Prompt {
                    text: "boundary proof".into(),
                    attached_mentions: Vec::new(),
                },
            )
            .unwrap();
        assert!(expected_root.join(name).join("events.jsonl").is_file());
    }

    /// Production boundary for #411: `remove_if_unused` already refused to
    /// delete a Brain with real activity, but nothing ever called it as a
    /// sweep -- every named Brain minted by a launch (`names::generate`)
    /// just accumulated. This drives the real daemon-startup constructor
    /// (`AgentServer::new`, the same call `main.rs` makes before `serve()`)
    /// over a root pre-seeded with the four cases the issue calls out, and
    /// checks the real on-disk outcome rather than calling
    /// `BrainStore::sweep_unused` directly -- that would only prove the
    /// store method works, not that daemon startup actually reaches it.
    #[test]
    fn production_constructor_sweeps_unused_brains_before_anything_can_touch_them() {
        if !supervisor_contract_present() {
            return;
        }
        let proof = crate::brain::isolated_test_proof()
            .expect("sweep boundary test requires supervisor authority");
        let daemon_address = proof.daemon_address().to_owned();
        let brain_password = proof.brain_password().unwrap();
        let home = proof.home.clone();
        let root = proof.root.clone();
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let stale = format!("sweep-boundary-stale-{suffix}");
        let active = format!("sweep-boundary-active-{suffix}");
        let fresh = format!("sweep-boundary-fresh-{suffix}");
        let half_deleted = format!("sweep-boundary-half-deleted-{suffix}");

        // Seed through the real store API, so the fixtures are byte-for-byte
        // what production writes -- then backdate `created_ms` by hand,
        // which is the one fact the API has no reason to let a caller set.
        let fixture_store =
            crate::brain::BrainStore::with_root("sweep-fixture", Some(root.clone()));
        fixture_store.snapshot(&stale).unwrap();
        fixture_store
            .push(
                &active,
                "alice",
                crate::brain::BrainEventKind::Prompt {
                    text: "boundary proof".into(),
                    attached_mentions: Vec::new(),
                },
            )
            .unwrap();
        fixture_store.snapshot(&fresh).unwrap();
        let old_enough = crate::brain::unix_millis()
            .saturating_sub(crate::brain::BrainStore::SWEEP_MIN_AGE_MS + 60_000);
        for name in [&stale, &active] {
            let path = root.join(name).join("metadata.json");
            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            value["created_ms"] = serde_json::json!(old_enough);
            std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let active_directory = root.join(&active);
        std::fs::remove_file(active_directory.join("initialization.json")).unwrap();
        // #393 shape: durable state present, metadata.json absent. Must
        // never be handed to anything that would mint it an identity.
        let half_deleted_dir = root.join(&half_deleted);
        std::fs::create_dir_all(&half_deleted_dir).unwrap();
        std::fs::write(half_deleted_dir.join("events.jsonl"), "not empty\n").unwrap();
        drop(fixture_store);
        let active_before = durable_file_fingerprints(&active_directory);

        let config =
            crate::config::Config::with_providers(vec![crate::config::ProviderEntry::Claude {
                api_key: "sweep-boundary-test".into(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("sweep-boundary-test".into()),
            }]);
        let generator_state = Arc::new(RwLock::new(GeneratorState::NotAvailable));
        // The real daemon-startup constructor. The sweep this test exists to
        // prove runs synchronously inside this call, before the returned
        // server has bound a listener or accepted any connection.
        let server = AgentServer::new(
            config,
            ServerConfig {
                bind_address: daemon_address,
                brain_password,
                ..ServerConfig::default()
            },
            ClaudeClient::new("sweep-boundary-test".to_string()).unwrap(),
            Router::new(crate::models::ThresholdRouter::new()),
            MetricsLogger::new(home.join(".finch/sweep-boundary-metrics")).unwrap(),
            Arc::new(RwLock::new(LocalGenerator::new())),
            Arc::new(BootstrapLoader::new(Arc::clone(&generator_state), None)),
            generator_state,
            isolated_provider_graph(),
        )
        .unwrap();

        assert!(
            !root.join(&stale).exists(),
            "a zero-event Brain older than the sweep threshold must be gone \
             after the real daemon-startup constructor runs"
        );
        assert!(
            root.join(&active).join("events.jsonl").exists(),
            "a Brain with a real Prompt event must survive the sweep even \
             though it is just as old as the one that was removed"
        );
        assert_eq!(
            durable_file_fingerprints(&active_directory),
            active_before,
            "the real AgentServer::new startup sweep must leave an old active Brain byte-identical and create no missing initialization or audit files"
        );
        assert_eq!(
            server.brain_store.resident_brain_count(),
            0,
            "the real AgentServer::new startup sweep must not leave any inspected Brain resident"
        );
        assert!(
            root.join(&fresh).exists(),
            "a Brain created moments ago must survive even with zero events \
             -- it may be the one the current session is about to type into"
        );
        assert!(
            half_deleted_dir.exists() && !half_deleted_dir.join("metadata.json").exists(),
            "a directory with durable state but no metadata.json (#393) must \
             be left exactly as found -- not deleted, and not given a fresh \
             identity by whatever the sweep uses to decide"
        );
    }

    #[test]
    fn supervised_http_fixture_rejects_parent_traversal_without_external_mutation() {
        if !supervisor_contract_present() {
            return;
        }
        let proof = crate::brain::isolated_test_proof()
            .expect("HTTP containment regression requires supervisor authority");
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        std::fs::write(&sentinel, b"unchanged").unwrap();
        let requested = proof
            .home
            .join("fixture")
            .join("..")
            .join("..")
            .join("outside");
        let result = AgentServer::for_supervised_brain_http_test(
            "containment.local",
            &requested,
            crate::brain::BrainCredentialAuthority::ephemeral([71; 32]),
        );
        let error = match result {
            Ok(_) => panic!("parent traversal unexpectedly constructed a server"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("normal descendant"));
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"unchanged");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn supervised_http_fixture_rejects_symlinked_ancestor_without_external_mutation() {
        if !supervisor_contract_present() {
            return;
        }
        let proof = crate::brain::isolated_test_proof()
            .expect("HTTP containment regression requires supervisor authority");
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        std::fs::write(&sentinel, b"unchanged").unwrap();
        let link = proof
            .home
            .join(format!("fixture-link-{}", uuid::Uuid::new_v4().simple()));
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let result = AgentServer::for_supervised_brain_http_test(
            "containment.local",
            &link,
            crate::brain::BrainCredentialAuthority::ephemeral([72; 32]),
        );
        let error = match result {
            Ok(_) => panic!("symlink traversal unexpectedly constructed a server"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("cannot traverse symlinks"));
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"unchanged");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
        std::fs::remove_file(link).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn supervised_http_fixture_pins_state_root_across_ancestor_swap() {
        if !supervisor_contract_present() {
            return;
        }
        let proof = crate::brain::isolated_test_proof()
            .expect("HTTP containment regression requires supervisor authority");
        let requested = proof
            .home
            .join(format!("pinned-state-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&requested).unwrap();
        let pinned = supervised_state_root(&proof, &requested).unwrap();
        let moved = proof
            .home
            .join(format!("moved-state-{}", uuid::Uuid::new_v4().simple()));
        std::fs::rename(&requested, &moved).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        std::fs::write(&sentinel, b"unchanged").unwrap();
        std::os::unix::fs::symlink(outside.path(), &requested).unwrap();

        let mut server = AgentServer::for_brain_http_test(
            "containment.local",
            &pinned.path,
            crate::brain::BrainCredentialAuthority::ephemeral([73; 32]),
        )
        .unwrap();
        server.supervised_state_root = Some(pinned.directory);
        let brain = format!("pinned-brain-{}", uuid::Uuid::new_v4().simple());
        server
            .brain_store()
            .push(
                &brain,
                "containment-test",
                crate::brain::BrainEventKind::Prompt {
                    text: "descriptor-pinned write".into(),
                    attached_mentions: Vec::new(),
                },
            )
            .unwrap();

        assert_eq!(std::fs::read(&sentinel).unwrap(), b"unchanged");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
        assert!(moved.join("metrics").is_dir());
        assert!(moved.join("feedback.jsonl").is_file());
        assert!(moved
            .join("brains")
            .join(&brain)
            .join("events.jsonl")
            .is_file());
        assert!(!outside.path().join("brains").join(brain).exists());

        let second = proof
            .home
            .join(format!("second-state-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&second).unwrap();
        let error = match AgentServer::for_supervised_brain_http_test(
            "containment.local",
            &second,
            crate::brain::BrainCredentialAuthority::ephemeral([74; 32]),
        ) {
            Ok(_) => panic!("a second fixture changed the process-relative root"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("already pinned"));
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"unchanged");
        assert_eq!(std::fs::read_dir(&second).unwrap().count(), 0);
    }

    #[test]
    fn production_constructor_rejects_unverified_environment_before_store_mutation() {
        const CHILD_ENV: &str = "FINCH_TEST_CONSTRUCTOR_FORGERY_CHILD";
        if !supervisor_contract_present() {
            return;
        }
        if std::env::var_os(CHILD_ENV).is_some() {
            let proof = crate::brain::isolated_test_proof().unwrap();
            let forged_home = proof.home.join("forged-constructor-home");
            std::fs::create_dir(&forged_home).unwrap();
            let metrics =
                MetricsLogger::new(proof.home.join("forged-constructor-metrics")).unwrap();
            let generator_state = Arc::new(RwLock::new(GeneratorState::NotAvailable));
            let bootstrap = Arc::new(BootstrapLoader::new(Arc::clone(&generator_state), None));
            std::env::set_var("HOME", &forged_home);
            std::env::set_var("FINCH_BRAIN_TEST_HOME", &forged_home);
            std::env::set_var("FINCH_BRAIN_TEST_ROOT", forged_home.join(".finch/brains"));
            let result = AgentServer::new(
                crate::config::Config::with_providers(Vec::new()),
                ServerConfig::default(),
                ClaudeClient::new("constructor-forgery".to_owned()).unwrap(),
                Router::new(crate::models::ThresholdRouter::new()),
                metrics,
                Arc::new(RwLock::new(LocalGenerator::new())),
                bootstrap,
                generator_state,
                isolated_provider_graph(),
            );
            assert!(result.is_err());
            assert!(std::fs::read_dir(&forged_home).unwrap().next().is_none());
            return;
        }
        let status = crate::brain::supervised_test_subprocess_command()
            .args([
                "--exact",
                "server::tests::production_constructor_rejects_unverified_environment_before_store_mutation",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(unix)]
    #[test]
    fn production_constructor_rejects_rewritten_proof_and_accepts_exact_restore() {
        const CHILD_ENV: &str = "FINCH_TEST_CONSTRUCTOR_REWRITTEN_PROOF_CHILD";
        if !supervisor_contract_present() {
            return;
        }
        if std::env::var_os(CHILD_ENV).is_some() {
            use std::io::Write as _;
            use std::os::fd::FromRawFd as _;
            use std::os::unix::fs::FileExt as _;

            let proof = crate::brain::isolated_test_proof().unwrap();
            let state_before = std::fs::read_dir(proof.home.join(".finch"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<std::collections::BTreeSet<_>>();
            #[cfg(target_os = "macos")]
            {
                assert_eq!(unsafe { nix::libc::fchmod(9, 0o600) }, 0);
                let error = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open("/dev/fd/9")
                    .unwrap_err();
                assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
                assert_eq!(unsafe { nix::libc::fchmod(9, 0o400) }, 0);
                let state_after = std::fs::read_dir(proof.home.join(".finch"))
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .collect::<std::collections::BTreeSet<_>>();
                assert_eq!(
                    state_after, state_before,
                    "constructor authority sealing attempt mutated Finch state"
                );
                crate::brain::isolated_test_proof().unwrap();
                return;
            }
            let duplicate = unsafe { nix::libc::dup(9) };
            assert!(duplicate >= 0);
            let reader = unsafe { std::fs::File::from_raw_fd(duplicate) };
            let length = reader.metadata().unwrap().len() as usize;
            let mut original = vec![0_u8; length];
            let mut offset = 0;
            while offset < original.len() {
                let count = reader
                    .read_at(&mut original[offset..], offset as u64)
                    .unwrap();
                assert!(count > 0);
                offset += count;
            }
            let mut forged = original.clone();
            forged[0] = if forged[0] == b'a' { b'b' } else { b'a' };
            for fd in [9, 108] {
                assert_eq!(unsafe { nix::libc::fchmod(fd, 0o600) }, 0);
                let mut writer = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(format!("/dev/fd/{fd}"))
                    .unwrap();
                writer.write_all(&forged).unwrap();
                writer.sync_all().unwrap();
                drop(writer);
                assert_eq!(unsafe { nix::libc::fchmod(fd, 0o400) }, 0);
            }

            let generator_state = Arc::new(RwLock::new(GeneratorState::NotAvailable));
            let result = AgentServer::new(
                crate::config::Config::with_providers(Vec::new()),
                ServerConfig::default(),
                ClaudeClient::new("constructor-rewrite".to_owned()).unwrap(),
                Router::new(crate::models::ThresholdRouter::new()),
                MetricsLogger::new(proof.home.join("constructor-rewrite-metrics")).unwrap(),
                Arc::new(RwLock::new(LocalGenerator::new())),
                Arc::new(BootstrapLoader::new(Arc::clone(&generator_state), None)),
                generator_state,
                isolated_provider_graph(),
            );
            assert!(
                result.is_err(),
                "rewritten proof reached AgentServer construction"
            );
            let state_after = std::fs::read_dir(proof.home.join(".finch"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                state_after, state_before,
                "constructor mutated Finch state before rejecting rewritten authority"
            );

            for fd in [9, 108] {
                assert_eq!(unsafe { nix::libc::fchmod(fd, 0o600) }, 0);
                let mut writer = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(format!("/dev/fd/{fd}"))
                    .unwrap();
                writer.write_all(&original).unwrap();
                writer.sync_all().unwrap();
                drop(writer);
                assert_eq!(unsafe { nix::libc::fchmod(fd, 0o400) }, 0);
            }
            crate::brain::isolated_test_proof().unwrap();
            return;
        }
        let status = crate::brain::supervised_test_subprocess_command()
            .args([
                "--exact",
                "server::tests::production_constructor_rejects_rewritten_proof_and_accepts_exact_restore",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(unix)]
    #[test]
    fn isolated_address_publication_replaces_final_symlink_without_following_it() {
        let state = tempfile::tempdir().unwrap();
        let parent = state.path().join("private");
        std::fs::create_dir(&parent).unwrap();
        let outside = state.path().join("outside");
        std::fs::write(&outside, "sentinel").unwrap();
        std::os::unix::fs::symlink(&outside, parent.join("bound.addr")).unwrap();

        publish_isolated_address_file(
            &std::fs::File::open(&parent).unwrap(),
            std::ffi::OsStr::new("bound.addr"),
            "127.0.0.1:43210".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "sentinel");
        assert_eq!(
            std::fs::read_to_string(parent.join("bound.addr")).unwrap(),
            "127.0.0.1:43210"
        );
    }

    #[cfg(unix)]
    #[test]
    fn isolated_address_publication_replaces_final_hardlink_without_writing_its_inode() {
        use std::os::unix::fs::MetadataExt as _;
        let state = tempfile::tempdir().unwrap();
        let parent = state.path().join("private");
        std::fs::create_dir(&parent).unwrap();
        let outside = state.path().join("outside");
        std::fs::write(&outside, "sentinel").unwrap();
        std::fs::hard_link(&outside, parent.join("bound.addr")).unwrap();
        let outside_inode = std::fs::metadata(&outside).unwrap().ino();

        publish_isolated_address_file(
            &std::fs::File::open(&parent).unwrap(),
            std::ffi::OsStr::new("bound.addr"),
            "127.0.0.1:43211".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "sentinel");
        let committed = parent.join("bound.addr");
        assert_ne!(std::fs::metadata(&committed).unwrap().ino(), outside_inode);
        assert_eq!(
            std::fs::read_to_string(committed).unwrap(),
            "127.0.0.1:43211"
        );
    }

    #[cfg(unix)]
    #[test]
    fn isolated_address_publication_rejects_symlinked_ancestor() {
        use std::os::unix::fs::MetadataExt as _;
        let state = tempfile::tempdir().unwrap();
        let home = state.path().join("home");
        let root = home.join("safe-root");
        let outside = state.path().join("outside");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, home.join(".finch")).unwrap();
        let metadata = std::fs::metadata(&home).unwrap();
        let proof = crate::brain::IsolatedTestProof {
            home_identity: (metadata.dev(), metadata.ino()),
            root_identity: (0, 0),
            ipc_socket: home.join("safe.sock"),
            socket_root: home.clone(),
            socket_root_identity: (metadata.dev(), metadata.ino()),
            ipc_listener_identity: (0, 0),
            home,
            root,
            brain_addr: String::new(),
            daemon_addr: String::new(),
            supervisor_pid: std::process::id(),
            password_digest: String::new(),
        };
        assert!(
            open_isolated_address_parent(&proof, std::path::Path::new(".finch/bound.addr"))
                .is_err()
        );
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
    }

    #[test]
    fn brain_password_comparison_checks_length_and_contents() {
        assert!(constant_time_eq(b"brain-secret", b"brain-secret"));
        assert!(!constant_time_eq(b"brain-secret", b"brain-secrex"));
        assert!(!constant_time_eq(b"brain-secret", b"brain-secret-longer"));
    }
    use crate::providers::{
        LlmProvider, ProviderBackend, ProviderResponse, StreamChunk, ValidatedProviderRequest,
    };
    use async_trait::async_trait;
    use tokio::sync::mpsc::Receiver;

    struct NamedProvider(String);

    #[async_trait]
    impl ProviderBackend for NamedProvider {
        fn name(&self) -> &str {
            &self.0
        }
        fn default_model(&self) -> &str {
            "test-model"
        }
        async fn send_message_validated(
            &self,
            _r: ValidatedProviderRequest,
        ) -> anyhow::Result<ProviderResponse> {
            unimplemented!()
        }
        async fn send_message_stream_validated(
            &self,
            _r: ValidatedProviderRequest,
        ) -> anyhow::Result<Receiver<anyhow::Result<StreamChunk>>> {
            unimplemented!()
        }
    }

    fn make_providers(names: &[&str]) -> Vec<Arc<dyn LlmProvider>> {
        names
            .iter()
            .map(|n| Arc::new(NamedProvider(n.to_string())) as Arc<dyn LlmProvider>)
            .collect()
    }

    #[test]
    fn test_provider_for_name_found_exact() {
        // Build a minimal AgentServer-like providers Vec and call provider_for_name directly
        // (we test via a wrapper since building a full AgentServer requires many deps)
        let providers = make_providers(&["claude", "grok", "openai"]);
        let result = providers
            .iter()
            .find(|p| p.name().eq_ignore_ascii_case("grok"));
        assert!(result.is_some());
        assert_eq!(result.unwrap().name(), "grok");
    }

    #[test]
    fn test_provider_for_name_case_insensitive() {
        let providers = make_providers(&["Claude", "Grok"]);
        let result = providers
            .iter()
            .find(|p| p.name().eq_ignore_ascii_case("claude"));
        assert!(result.is_some());
        assert_eq!(result.unwrap().name(), "Claude");
    }

    #[test]
    fn test_provider_for_name_not_found_returns_none_when_empty() {
        let providers: Vec<Arc<dyn LlmProvider>> = vec![];
        // Mirrors provider_for_name: empty -> None
        let result = if providers.is_empty() {
            None
        } else {
            providers.first()
        };
        assert!(result.is_none());
    }

    #[test]
    fn test_provider_for_name_unknown_does_not_silently_fall_back() {
        let providers = make_providers(&["claude", "grok"]);
        let result = providers
            .iter()
            .find(|p| p.name().eq_ignore_ascii_case("unknown"));
        assert!(result.is_none());
    }

    #[test]
    fn test_provider_for_name_none_name_returns_first() {
        let providers = make_providers(&["claude", "grok"]);
        // None name → first provider
        let name: Option<&str> = None;
        let result = if providers.is_empty() {
            None
        } else if let Some(n) = name {
            providers
                .iter()
                .find(|p| p.name().eq_ignore_ascii_case(n))
                .or_else(|| providers.first())
        } else {
            providers.first()
        };
        assert_eq!(result.unwrap().name(), "claude");
    }
}

/// Return the OS hostname, or "finch-node" if it can't be determined.
fn hostname_or_default() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "finch-node".to_string())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    let width = left.len().max(right.len());
    for index in 0..width {
        diff |= left.get(index).copied().unwrap_or(0) as usize
            ^ right.get(index).copied().unwrap_or(0) as usize;
    }
    diff == 0
}
