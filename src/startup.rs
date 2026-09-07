//! Startup phase timing for the interactive path (#364, "Instrument and reduce
//! Finch interactive TUI time-to-ready").
//!
//! Nothing in Finch measured its own startup. There was no `Instant::now()` on
//! the path from `main` to the first interactive frame, no tracing span, and no
//! diagnostic that reported a duration -- only two `SHAMMAH_DEBUG` `eprintln!`s
//! bracketing the entire event loop. Every latency claim about startup was
//! therefore unfalsifiable -- which is `AGENTS.md`'s first key principle,
//! "Evidence before claims: configuration or design intent is not
//! conformance", applied to latency.
//!
//! # What "time-to-ready" means here
//!
//! Time-to-ready is [`begin`] through [`ready`]. [`ready`] is called
//! immediately before the event loop's `select!` begins consuming keystrokes,
//! which is **the first instant at which a typed key is acted upon**. It is not
//! the first painted frame: [`MARK_HEADER_QUEUED`] hands the header to the
//! output manager, and in TUI mode the first actual paint happens on a render
//! tick *after* the loop this mark precedes. Anything that calls this number
//! "the first fully rendered frame" is overclaiming it.
//!
//! [`begin`] is the first statement of `main`'s async body. Two things happen
//! before it and are therefore *not* measured, and the report names both
//! rather than letting the reader assume the number is complete: pre-`main`
//! dynamic loader work, and the construction of the multi-threaded Tokio
//! runtime, which `#[tokio::main]` performs inside the real `fn main` before
//! it polls the body at all. This is a proxy for process entry, not process
//! entry.
//!
//! Three earlier instants are recorded separately because they are routinely
//! confused with readiness and are not the same:
//!
//! * [`MARK_TERMINAL_OWNED`] -- raw mode entered, the terminal is ours.
//! * [`MARK_INPUT_CAPTURED`] -- the reader task is running, so keys are
//!   buffered and not lost, but nothing acts on them yet.
//! * [`MARK_HEADER_QUEUED`] -- the header has been handed to the output
//!   manager. Note the name: in TUI mode stdout is disabled and `write_info`
//!   only appends to an in-memory buffer, so the first actual paint happens on
//!   the render tick *after* the event loop starts. Calling this "painted"
//!   would be the exact conflation this module exists to end.
//!
//! # Accounted and unaccounted time
//!
//! Phases nest, so the `ms` column does not sum to anything. The report
//! therefore prints `accounted_ms` -- the sum of the *outermost* phases -- and
//! `unaccounted_ms`, the remainder of time-to-ready that no phase covers. A
//! reader who does not diff the `at_ms` column by hand still sees how much of
//! the total is un-attributed, which is the number that decides whether the
//! instrumentation is good enough to optimise from.
//!
//! # Privacy
//!
//! A phase carries a name, a duration, and [`PhaseDetail`] -- counts, byte
//! totals, and a `&'static str` category. That is the whole vocabulary. There
//! is no field of any type that can hold a Brain name, an event payload, a
//! prompt, a path, or a credential, so a report cannot leak one by mistake
//! rather than merely by convention.
//!
//! # Two phases have no `warn` sink yet
//!
//! [`PHASE_ARGS`] and the first [`PHASE_CONFIG`] (`category=debug_logging_probe`)
//! both open **and close** before [`PHASE_TRACING`] installs a subscriber, so
//! when their guards drop there is nothing to receive a `tracing` event. They
//! are recorded in the timeline and appear in the report exactly like every
//! other phase; what they cannot do is emit the over-budget `warn!` that #364
//! requires of a phase that exceeds its budget, because no subscriber exists
//! to render it. This is a real, named gap rather than a silent one.
//!
//! It is not fixable by moving the boundary: what verbosity to install is
//! decided by the parsed arguments and by that first configuration read, so
//! initialising tracing before them would mean initialising it before knowing
//! what to initialise it to. The two phases sit at 0.272 ms and 0.419 ms on
//! the reference host; a regression in either shows up in the report's
//! `at_ms` column, which is the surface that still works.
//!
//! # Cost when nobody asked
//!
//! `FINCH_STARTUP_TIMINGS` unset is the common case and must be nearly free:
//! #364 requires that "the measurement itself must not become a startup
//! phase". Each phase is a lock, a push and a `tracing::debug!` that the
//! default filter discards. The expensive part -- rendering the report, which
//! sorts, runs an O(n^2) depth pass and formats several dozen `f64`s -- is
//! skipped entirely unless a destination is set. [`REPORTS_RENDERED`] makes
//! that observable to a test rather than merely intended.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Declare phase or mark names, and the slice of every name declared.
///
/// The slice and the constants are generated from one list, so they cannot
/// drift apart. That is the point: `tests/startup_time_to_ready.rs` derives
/// what it expects the report to contain from [`ALL_PHASES`] and
/// [`ALL_MARKS`], so deleting an instrumented phase can no longer be made to
/// pass by also deleting the line that expected it. Before this existed, a
/// mutant that dropped `program_runtime` from both the production guard and
/// the test's expected order left the suite entirely green: nothing tied the
/// expected set to the instrumented set.
macro_rules! declare_names {
    ($slice:ident: $($(#[$attribute:meta])* $konst:ident = $name:literal;)*) => {
        $($(#[$attribute])* pub const $konst: &str = $name;)*
        /// Every name declared above, in declaration order.
        pub const $slice: &[&str] = &[$($name),*];
    };
}

// Phase names. `&'static str` so no caller can smuggle content into one.
declare_names! {
    ALL_PHASES:

    PHASE_ARGS = "args_parse";
    PHASE_CONFIG = "config_load";
    PHASE_TRACING = "tracing_init";
    PHASE_THRESHOLD_ROUTER = "threshold_router_load";
    PHASE_PROVIDER_GRAPH = "provider_graph_build";
    PHASE_METRICS = "metrics_logger_init";
    /// The whole `DaemonClient::connect`, including everything below it.
    PHASE_DAEMON_CONNECT = "daemon_http_connect";
    /// One `GET /health` probe under its 500 ms client timeout.
    ///
    /// Nested inside [`PHASE_DAEMON_CONNECT`]. Issue #364, "Instrument and
    /// reduce Finch interactive TUI time-to-ready", requires the daemon
    /// connect to separate "probe latency from the `sleep(2s)` fallback",
    /// because the two need different fixes and an undivided
    /// `daemon_http_connect ms=2503.1` tells a maintainer which one it was.
    /// `category` says how the probe ended: `healthy`, `unhealthy` (a reply
    /// that was not 2xx) or `unreachable` (no reply inside the timeout).
    PHASE_DAEMON_HEALTH_PROBE = "daemon_health_probe";
    /// The unconditional `sleep(2s)` between a failed first probe and the one
    /// retry, taken whenever the daemon pid file names a live process.
    ///
    /// Nested inside [`PHASE_DAEMON_CONNECT`]. This is the flat two-second
    /// floor #364 names as the plausible per-launch amplifier: a cold
    /// `load_all` that overruns the 500 ms probe timeout costs the *next*
    /// launch this wait rather than its own duration. It is a constant, so a
    /// report that shows it is telling the reader the daemon was not slow --
    /// it was merely late once.
    PHASE_DAEMON_RETRY_BACKOFF = "daemon_retry_backoff";
    PHASE_REPL_NEW = "repl_construct";
    PHASE_SESSION_RESTORE = "session_restore";
    PHASE_IPC_CONNECT = "ipc_connect";
    PHASE_TERMINAL_INIT = "terminal_init";
    PHASE_MEMORY_OPEN = "memory_open";
    PHASE_PROGRAM_SYNC = "program_sync";
    PHASE_TOOL_REGISTRY = "tool_registry";
    /// Typed-runtime construction and, on macOS, the Accessibility availability
    /// probe. Separate from tool registration because it is the one phase here
    /// that can stall on an OS permission check, and attributing that to registry
    /// construction sends a reader to the wrong code.
    PHASE_PROGRAM_RUNTIME = "program_runtime";
    PHASE_MCP_CONNECT = "mcp_connect";
    PHASE_BRAIN_REGISTER = "brain_register";
    PHASE_BRAIN_ATTACH = "brain_attach";

    // The phases below cover what the first report measured and could not name: at
    // that tip 6.4 ms of a 13.99 ms total sat between `input_captured` and
    // `brain_register` with nothing over it, and the commit message attributed the
    // gap to serial daemon IPC. That was wrong -- `register_home_brain` is wholly
    // inside `brain_register`, at 0.003 ms, and runs *after* the gap. This is what
    // was actually in there.

    /// Selecting and constructing the generators the event loop will use, which
    /// includes the second full provider-graph build of the process.
    PHASE_GENERATOR_SELECT = "generator_select";
    /// `ProviderResolver` and `AgentScheduler` construction.
    PHASE_SCHEDULER_INIT = "scheduler_init";
    /// Registering the agent tools and materialising the model-visible tool list.
    PHASE_TOOL_DEFINITIONS = "tool_definitions";
    /// `EventLoop::new`: channels, watcher tasks, the terminal reader, the tool
    /// coordinator and the memtree console. Encloses [`MARK_INPUT_CAPTURED`].
    PHASE_EVENT_LOOP_NEW = "event_loop_construct";
    /// Handing the terminal to the TUI and clearing accumulated startup noise out
    /// of the output manager.
    PHASE_TUI_HANDOFF = "tui_handoff";
    /// Resolving the selected generator through the provider handle so the header
    /// can name the model.
    PHASE_GENERATOR_RESOLVE = "generator_resolve";
    /// The weekly licence notice, which performs the third full `config.toml` read
    /// and TOML parse of the process (#76, "Keep ordinary Finch startup
    /// byte-for-byte read-only on user configuration").
    PHASE_LICENSE_NOTICE = "license_notice";
    /// Priming the status bar: compaction state, plan mode, memory engine and the
    /// context strip.
    PHASE_STATUS_PRIME = "status_prime";
    /// Projecting the home-runner state onto the session label and building the
    /// startup header. Ends at [`MARK_HEADER_QUEUED`].
    PHASE_STARTUP_HEADER = "startup_header";
    /// Spawning the LLM worker task and creating the render/cleanup intervals.
    PHASE_LLM_WORKER = "llm_worker_spawn";
}

// Instant marks, recorded as zero-duration entries in timeline order.
declare_names! {
    ALL_MARKS:


    MARK_TERMINAL_OWNED = "terminal_owned";
    MARK_INPUT_CAPTURED = "input_captured";
    MARK_HEADER_QUEUED = "header_queued";
    MARK_INPUT_READY = "input_ready";
}

/// A phase slower than this is reported as `SLOW` and logged at `warn`.
///
/// This governs *reporting only*, and no test asserts that a phase came in
/// under it. Issue #364, "Instrument and reduce Finch interactive TUI
/// time-to-ready", requires of this work's coverage: "Synchronization and
/// structural assertions, not absolute wall-clock thresholds." The precedent
/// it cites is `a0ea2c64` ("assert hydration state, not a wall-clock ratio"),
/// which replaced the last of four attempts at a timing assertion on #242,
/// "Make ordinary TUI startup prompt-first and lazily hydrate MemTree" -- one
/// of which reached 35/35 green CI while depending on the machine being busy.
///
/// Note what is and is not being cited. That requirement is written in #364
/// and the precedent is in `a0ea2c64`; it is **not** a rule in `AGENTS.md`.
/// PR #391, "docs(agents): write down the no-wall-clock-assertion rule",
/// proposed adding it and was closed DO NOT MERGE on the ground that a
/// blanket prohibition is not what `a0ea2c64` established and conflicts with
/// coarse liveness bounds Finch does accept elsewhere. Two earlier revisions
/// of this module cited this rule to a document that does not contain it,
/// which in the one module whose subject is falsifiability is worth saying
/// out loud rather than quietly correcting a third time.
///
/// A budget makes a slow phase visible and nameable; it is not a gate.
const DEFAULT_SLOW_PHASE: Duration = Duration::from_millis(150);

/// The budget in force, honouring `FINCH_STARTUP_SLOW_BUDGET_MS`.
///
/// The override exists so the over-budget warning -- a user-visible terminal
/// surface that #364 requires to say "which phase", by name -- can be driven
/// at the production boundary instead of only in a unit test. Without it the
/// only way to make a real `finch` launch exceed 150 ms in a phase is to load
/// the machine, which is precisely the load-dependent test this module
/// refuses to write. Read once: an environment lookup per phase drop would be
/// the measurement becoming a startup cost.
fn slow_phase_budget() -> Duration {
    static BUDGET: OnceLock<Duration> = OnceLock::new();
    *BUDGET.get_or_init(|| {
        let Some(value) = std::env::var_os("FINCH_STARTUP_SLOW_BUDGET_MS") else {
            return DEFAULT_SLOW_PHASE;
        };
        value
            .to_string_lossy()
            .trim()
            .parse::<u64>()
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_SLOW_PHASE)
    })
}

/// How many times a report has been rendered in this process.
///
/// One relaxed increment inside [`Timeline::report`]. It exists so a test can
/// hold the module to #364's "the measurement itself must not become a startup
/// phase": with no destination set, [`ready`] must not render at all, and
/// without this counter that property is unobservable and stays true only by
/// inspection.
pub static REPORTS_RENDERED: AtomicU64 = AtomicU64::new(0);

/// Content-free measurements attached to a phase.
///
/// Every field is a number or a `&'static str`. There is deliberately no
/// `String` and no `PathBuf`: the type is what keeps Brain names, prompts, and
/// credentials out of the report, not the discipline of each call site.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhaseDetail {
    /// How many things the phase carried to completion -- roots actually
    /// synced, servers actually connected, Brains actually registered.
    ///
    /// An outcome, never an attempt. A count of what was *tried* reads as a
    /// count of what worked, and a phase that failed every one of five servers
    /// then reports `count=5` is worse than reporting nothing. Whether the
    /// phase succeeded at all belongs in [`PhaseDetail::category`].
    pub count: Option<u64>,
    /// How many bytes it read or wrote.
    pub bytes: Option<u64>,
    /// A fixed classification, e.g. `"cached"`, `"spawned"`, `"absent"`.
    pub category: Option<&'static str>,
}

impl PhaseDetail {
    pub fn count(count: u64) -> Self {
        Self {
            count: Some(count),
            ..Self::default()
        }
    }

    pub fn category(category: &'static str) -> Self {
        Self {
            category: Some(category),
            ..Self::default()
        }
    }

    pub fn with_count(mut self, count: u64) -> Self {
        self.count = Some(count);
        self
    }

    pub fn with_bytes(mut self, bytes: u64) -> Self {
        self.bytes = Some(bytes);
        self
    }

    pub fn with_category(mut self, category: &'static str) -> Self {
        self.category = Some(category);
        self
    }

    fn render(&self, out: &mut String) {
        if let Some(count) = self.count {
            let _ = write!(out, " count={count}");
        }
        if let Some(bytes) = self.bytes {
            let _ = write!(out, " bytes={bytes}");
        }
        if let Some(category) = self.category {
            let _ = write!(out, " category={category}");
        }
    }
}

/// Whether an entry spans time or names an instant.
///
/// Explicit rather than inferred. Deriving "mark" from a zero duration would
/// make a phase that happened to measure zero render as an instant, which is a
/// clock-dependent property in the one module whose premise is that startup
/// facts must not depend on the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseKind {
    /// Spans time: a start and an end.
    Phase,
    /// Names a single instant.
    Mark,
}

impl PhaseKind {
    fn label(self) -> &'static str {
        match self {
            PhaseKind::Phase => "phase",
            PhaseKind::Mark => "mark",
        }
    }
}

/// One completed phase or instant mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseRecord {
    pub name: &'static str,
    pub kind: PhaseKind,
    /// Offset from [`begin`] at which the phase started.
    pub started_at: Duration,
    /// Zero for an instant mark.
    pub duration: Duration,
    pub detail: PhaseDetail,
}

impl PhaseRecord {
    pub fn is_mark(&self) -> bool {
        matches!(self.kind, PhaseKind::Mark)
    }

    pub fn is_slow(&self) -> bool {
        self.duration >= slow_phase_budget()
    }

    fn ends_at(&self) -> Duration {
        self.started_at + self.duration
    }
}

/// The ordered record of one process's startup.
#[derive(Debug)]
pub struct Timeline {
    entry: Instant,
    records: Vec<PhaseRecord>,
    ready_at: Option<Duration>,
}

impl Timeline {
    fn new(entry: Instant) -> Self {
        Self {
            entry,
            records: Vec::new(),
            ready_at: None,
        }
    }

    pub fn records(&self) -> &[PhaseRecord] {
        &self.records
    }

    /// Total time-to-ready, once [`ready`] has been called.
    pub fn time_to_ready(&self) -> Option<Duration> {
        self.ready_at
    }

    /// The report, as written to `FINCH_STARTUP_TIMINGS`.
    ///
    /// Line-oriented and stable, because a test parses it. The first token of
    /// an entry line is its name; a slow phase carries a trailing `SLOW`.
    ///
    /// Entries are ordered by when they *started*, not by when they were
    /// recorded. A phase records itself on drop, so the raw order is completion
    /// order and an enclosing phase lands after everything it contains --
    /// which reads as "`repl_construct` began after `mcp_connect` finished"
    /// when the truth is the opposite.
    ///
    /// Phases nest, so the `ms` column does not sum to the total. Each entry
    /// carries `depth=N`, the number of phases that enclose it, so a reader can
    /// see which durations are already counted inside another; and the report
    /// ends with `accounted_ms` (the depth-0 phases summed) and
    /// `unaccounted_ms` (what is left of the total), so a reader is told how
    /// much of startup this instrumentation does not name.
    pub fn report(&self) -> String {
        REPORTS_RENDERED.fetch_add(1, Ordering::Relaxed);

        // Start ascending, then *longest first* so that when an enclosing
        // phase and its first child start in the same tick the enclosing one
        // still precedes. Sorting both ascending would put the child first and
        // invert the nesting the depth column then reports.
        let mut ordered: Vec<&PhaseRecord> = self.records.iter().collect();
        ordered.sort_by_key(|record| (record.started_at, std::cmp::Reverse(record.ends_at())));

        let mut out = String::with_capacity(128 * (self.records.len() + 6));
        out.push_str(
            "finch startup timings -- #364, instrument and reduce interactive TUI \
             time-to-ready\n",
        );
        out.push_str(
            "t0 is the first statement of main's async body; pre-main loader \
             work and the tokio runtime construction that #[tokio::main] \
             performs before polling it are excluded\n",
        );
        out.push_str(
            "input_ready is the first instant a typed key is acted upon, not \
             the first painted frame -- the first paint happens on a render \
             tick after it\n",
        );
        out.push_str(
            "entries are in start order, phases nest, and the ms column does \
             not sum to the total -- see depth, accounted_ms and \
             unaccounted_ms\n",
        );
        let _ = writeln!(out, "phases={}", ordered.len());
        let mut accounted = Duration::ZERO;
        for (index, record) in ordered.iter().enumerate() {
            // A phase encloses this entry when it precedes it in the order
            // above -- so it started no later -- and ends no earlier. Using
            // position rather than `started_at <= started_at` keeps two phases
            // with identical spans from each counting the other as its parent.
            let depth = ordered[..index]
                .iter()
                .filter(|other| {
                    matches!(other.kind, PhaseKind::Phase) && other.ends_at() >= record.ends_at()
                })
                .count();
            if depth == 0 && matches!(record.kind, PhaseKind::Phase) {
                accounted += record.duration;
            }
            let _ = write!(
                out,
                "{kind} {name} at_ms={at:.3} ms={ms:.3} depth={depth}",
                kind = record.kind.label(),
                name = record.name,
                at = record.started_at.as_secs_f64() * 1000.0,
                ms = record.duration.as_secs_f64() * 1000.0,
            );
            record.detail.render(&mut out);
            if record.is_slow() {
                out.push_str(" SLOW");
            }
            out.push('\n');
        }
        let accounted_ms = accounted.as_secs_f64() * 1000.0;
        match self.ready_at {
            Some(ready) => {
                let total_ms = ready.as_secs_f64() * 1000.0;
                let _ = writeln!(
                    out,
                    "accounted_ms={accounted_ms:.3} unaccounted_ms={:.3}",
                    total_ms - accounted_ms
                );
                let _ = writeln!(out, "time_to_ready_ms={total_ms:.3}");
            }
            None => {
                let _ = writeln!(out, "accounted_ms={accounted_ms:.3} unaccounted_ms=none");
                out.push_str("time_to_ready_ms=none\n");
            }
        }
        out
    }
}

fn timeline() -> &'static Mutex<Timeline> {
    static TIMELINE: OnceLock<Mutex<Timeline>> = OnceLock::new();
    TIMELINE.get_or_init(|| Mutex::new(Timeline::new(Instant::now())))
}

fn with_timeline<T>(operation: impl FnOnce(&mut Timeline) -> T) -> T {
    // A poisoned startup timeline must never take the process down: this is
    // diagnostics. Recover the guard and keep going.
    let mut guard = timeline()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    operation(&mut guard)
}

/// Start the clock. Call from the first statement of `main`.
///
/// Idempotent: a second call is a no-op, so a test harness that touches the
/// timeline first does not move t0.
pub fn begin() {
    let _ = timeline();
}

/// Record a zero-duration instant.
pub fn mark(name: &'static str) {
    with_timeline(|timeline| {
        let started_at = timeline.entry.elapsed();
        timeline.records.push(PhaseRecord {
            name,
            kind: PhaseKind::Mark,
            started_at,
            duration: Duration::ZERO,
            detail: PhaseDetail::default(),
        });
        tracing::debug!(target: "finch::startup", phase = name, at_ms = started_at.as_secs_f64() * 1000.0, "startup mark");
    });
}

/// Open a phase. It is recorded when the returned guard is dropped.
///
/// Dropping on an error path still records the phase, which is the point: a
/// phase that failed took time too, and the report should say so.
#[must_use = "a phase is recorded when its guard drops; binding it to `_` records an empty phase"]
pub fn phase(name: &'static str) -> PhaseGuard {
    let (entry, started_at) = with_timeline(|timeline| (timeline.entry, timeline.entry.elapsed()));
    PhaseGuard {
        name,
        started_at,
        start: Instant::now(),
        detail: PhaseDetail::default(),
        entry,
    }
}

/// An open phase. Records itself on drop.
pub struct PhaseGuard {
    name: &'static str,
    started_at: Duration,
    start: Instant,
    detail: PhaseDetail,
    entry: Instant,
}

impl PhaseGuard {
    /// Attach content-free measurements before the guard drops.
    pub fn detail(&mut self, detail: PhaseDetail) -> &mut Self {
        self.detail = detail;
        self
    }

    /// Record now rather than at end of scope, with a detail.
    pub fn finish(mut self, detail: PhaseDetail) {
        self.detail = detail;
        drop(self);
    }

    /// Offset from t0 at which this phase started, for callers that need to
    /// order their own work against the timeline.
    pub fn started_at(&self) -> Duration {
        self.started_at
    }

    /// t0, so a caller can compute its own offsets consistently.
    pub fn entry(&self) -> Instant {
        self.entry
    }

    /// Pretend the phase started `by` earlier.
    ///
    /// Test-only, and the alternative is worse: the over-budget path is a
    /// user-visible surface that has to be asserted, and asserting it by
    /// sleeping past a 150 ms budget makes the suite both slower and
    /// load-dependent -- which is the failure this module's own budget comment
    /// is about.
    #[cfg(test)]
    fn backdate(mut self, by: Duration) -> Self {
        self.start = self.start.checked_sub(by).unwrap_or(self.start);
        self
    }
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        let duration = self.start.elapsed();
        let record = PhaseRecord {
            name: self.name,
            kind: PhaseKind::Phase,
            started_at: self.started_at,
            duration,
            detail: self.detail,
        };
        let slow = record.is_slow();
        with_timeline(|timeline| timeline.records.push(record));
        let phase_name = self.name;
        let ms = duration.as_secs_f64() * 1000.0;
        if slow {
            // The name and the duration are in the *message*, not only in the
            // fields. `OutputManagerLayer`'s visitor keeps `message` and drops
            // every other field, so a warning whose name lives in a field
            // reaches the operator as "startup phase exceeded its budget" --
            // "startup was slow", which is what #364 exists to stop. The
            // fields stay for structured sinks that do read them.
            let budget_ms = slow_phase_budget().as_secs_f64() * 1000.0;
            tracing::warn!(
                target: "finch::startup",
                phase = phase_name,
                ms,
                budget_ms,
                count = self.detail.count,
                bytes = self.detail.bytes,
                category = self.detail.category,
                "startup phase {phase_name} took {ms:.3} ms, over its {budget_ms:.0} ms budget"
            );
            return;
        }
        tracing::debug!(
            target: "finch::startup",
            phase = phase_name,
            ms,
            count = self.detail.count,
            bytes = self.detail.bytes,
            category = self.detail.category,
            "startup phase"
        );
    }
}

/// Record [`MARK_INPUT_READY`], close the timeline, and publish the report.
///
/// Call immediately before the loop that consumes keystrokes -- both the TUI
/// event loop and the plain readline REPL, so that `--raw` and `--no-tui` are
/// measured rather than silently producing no report at all.
///
/// Idempotent: the first call wins. The two REPL paths are exclusive today,
/// but a second call must not append a second `input_ready` and overwrite a
/// smaller total with a larger one.
///
/// The destination is resolved *before* the timeline is touched, and the
/// report is rendered only if there is one. Rendering allocates a few
/// kilobytes, sorts, runs an O(n^2) depth pass and formats several dozen
/// `f64`s; doing that on every start with the feature off would make the
/// measurement a startup phase, which #364 forbids -- and it could never
/// appear in its own report, because `ready_at` is stamped before it.
pub fn ready() {
    // Resolved before the timeline is touched, and used only to decide whether
    // to render at all; `publish` re-reads it for the write itself.
    let wanted = destination().is_some();
    let Some((report, total)) = with_timeline(|timeline| {
        if timeline.ready_at.is_some() {
            return None;
        }
        let ready_at = timeline.entry.elapsed();
        timeline.records.push(PhaseRecord {
            name: MARK_INPUT_READY,
            kind: PhaseKind::Mark,
            started_at: ready_at,
            duration: Duration::ZERO,
            detail: PhaseDetail::default(),
        });
        timeline.ready_at = Some(ready_at);
        // Rendered under the lock, so the published report cannot contain a
        // half-appended record; only the first caller past the guard above
        // ever gets here.
        Some((wanted.then(|| timeline.report()), ready_at))
    }) else {
        return;
    };
    if let Some(report) = report {
        publish(&report);
    }
    // `debug!`, not `info!`. The default filter is `info`, and
    // `OutputManagerLayer` suppresses internal INFO only for the module
    // prefixes it lists -- `finch::startup` is not one of them, and could not
    // be without also suppressing the over-budget warning above, which is the
    // one thing here the operator must see. An `info!` therefore reached every
    // user's terminal as a contentless `[startup] finch interactive startup
    // ready`, painted after `EventLoop::run`'s `output_manager.clear()` so
    // nothing removed it. #364: "No new always-on cost."
    tracing::debug!(
        target: "finch::startup",
        time_to_ready_ms = total.as_secs_f64() * 1000.0,
        "finch interactive startup ready"
    );
}

/// A snapshot of everything recorded so far.
///
/// Test-facing. Used by `src/daemon/spawn.rs` to assert that the daemon
/// connect records the health probe and the two-second fallback as separate
/// phases, which the report string alone cannot show without also depending
/// on whatever else the test binary has pushed.
#[cfg(test)]
pub(crate) fn recorded() -> Vec<PhaseRecord> {
    with_timeline(|timeline| timeline.records.clone())
}

/// The current report, whether or not [`ready`] has been reached.
pub fn report() -> String {
    with_timeline(|timeline| timeline.report())
}

/// Where the report goes, per `FINCH_STARTUP_TIMINGS`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Destination {
    /// `1` or `stderr`.
    Stderr,
    /// Any other non-empty value: a file path.
    File(std::path::PathBuf),
}

/// Resolve `FINCH_STARTUP_TIMINGS`.
///
/// Unset or empty: `None` -- nowhere. The tracing events above are then the
/// only surface, reachable with `RUST_LOG=finch::startup=debug`. `1` or
/// `stderr`: standard error. Anything else is a file path, which is how a test
/// observes the real interactive path without parsing a live TUI frame.
fn destination() -> Option<Destination> {
    let value = std::env::var_os("FINCH_STARTUP_TIMINGS")?;
    let text = value.to_string_lossy();
    if text.is_empty() {
        return None;
    }
    if text == "1" || text == "stderr" {
        return Some(Destination::Stderr);
    }
    Some(Destination::File(std::path::PathBuf::from(&value)))
}

/// The report with CRLF line endings, for a terminal in raw mode.
///
/// By the time a startup report is written the TUI has entered raw mode and
/// cleared `OPOST`/`ONLCR`, so a bare `\n` moves the cursor down without
/// returning it to column 0: the report staircases off the right edge of the
/// screen and is unreadable in the one mode it exists to measure.
fn for_raw_terminal(report: &str) -> String {
    let mut out = String::with_capacity(report.len() + report.len() / 32 + 8);
    for line in report.lines() {
        out.push_str(line);
        out.push_str("\r\n");
    }
    out
}

/// Send `report` wherever `FINCH_STARTUP_TIMINGS` names, if anywhere.
fn publish(report: &str) {
    let Some(destination) = destination() else {
        return;
    };
    publish_to(&destination, report);
}

/// The scratch path `publish_to` writes before renaming onto `path`.
///
/// **Per-process, and it has to be.** A fixed `<path>.partial` is shared the
/// moment two `finch` processes inherit one `FINCH_STARTUP_TIMINGS` from a
/// shell profile: one truncates the other's half-written buffer, and the
/// survivor's rename publishes that torn buffer as a complete report. Nothing
/// downstream can detect this, because the result parses -- it is simply a
/// different process's startup, or half of one.
///
/// A process killed between the write and the rename leaves its scratch file
/// behind, and nothing inside a dead process can clean that up. The leak is
/// bounded rather than removed: the name is per-pid and `fs::write`
/// truncates, so the next process handed that pid reuses the file instead of
/// adding another.
fn temporary_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut temporary = path.to_path_buf().into_os_string();
    temporary.push(format!(".{}.partial", std::process::id()));
    std::path::PathBuf::from(temporary)
}

fn publish_to(destination: &Destination, report: &str) {
    let path = match destination {
        Destination::Stderr => {
            // The renderer still owns the screen and will paint over this; a
            // file destination is the one that survives, and the module docs
            // say so.
            eprint!("{}", for_raw_terminal(report));
            return;
        }
        Destination::File(path) => path,
    };
    // Write-then-rename, so a reader polling for the file never observes a
    // partial report and misparses it as a startup that produced no total.
    // `fs::write` alone truncates in place and a poller can read the hole.
    // Best effort otherwise: a diagnostic that cannot write its file must not
    // break the startup it is measuring, but it must not fail silently either.
    //
    let temporary = temporary_path(path);
    if let Err(error) =
        std::fs::write(&temporary, report).and_then(|()| std::fs::rename(&temporary, path))
    {
        let _ = std::fs::remove_file(&temporary);
        tracing::warn!(
            target: "finch::startup",
            error = %error,
            "could not write the startup timing report"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// Serialise the two pieces of process-global state these tests touch:
    /// `FINCH_STARTUP_TIMINGS`, and [`REPORTS_RENDERED`]. libtest runs these on
    /// parallel threads in one process, so every test that reads or writes the
    /// variable, or that renders a report, holds this first.
    ///
    /// The previous version claimed in a `// SAFETY:` comment that "this test
    /// is the only mutator" and that "std serialises `set_var` against its own
    /// readers". Both clauses were false: two tests mutated it, and
    /// `std::env::set_var` is `unsafe` precisely because it serialises
    /// nothing.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Set `FINCH_STARTUP_TIMINGS` for the life of the returned guard.
    struct TimingsVar;

    impl TimingsVar {
        fn set(value: &std::path::Path) -> Self {
            // SAFETY: every reader and writer of this variable in this test
            // binary holds `env_lock()`, and the caller holds it now.
            unsafe { std::env::set_var("FINCH_STARTUP_TIMINGS", value) };
            Self
        }

        fn unset() -> Self {
            // SAFETY: as above.
            unsafe { std::env::remove_var("FINCH_STARTUP_TIMINGS") };
            Self
        }
    }

    impl Drop for TimingsVar {
        fn drop(&mut self) {
            // SAFETY: as above.
            unsafe { std::env::remove_var("FINCH_STARTUP_TIMINGS") };
        }
    }

    /// A fresh timeline, independent of the process-global one, so these tests
    /// do not depend on each other's ordering.
    fn fixture() -> Timeline {
        Timeline::new(Instant::now())
    }

    /// Push a phase starting at `at_ms` and lasting `ms`, so a test can build
    /// nested and out-of-order records deliberately.
    fn push_at(
        timeline: &mut Timeline,
        name: &'static str,
        at_ms: u64,
        ms: u64,
        detail: PhaseDetail,
    ) {
        timeline.records.push(PhaseRecord {
            name,
            kind: PhaseKind::Phase,
            started_at: Duration::from_millis(at_ms),
            duration: Duration::from_millis(ms),
            detail,
        });
    }

    fn push(timeline: &mut Timeline, name: &'static str, ms: u64, detail: PhaseDetail) {
        push_at(timeline, name, 1, ms, detail);
    }

    fn field(report: &str, key: &str) -> Option<String> {
        report.split_whitespace().find_map(|token| {
            token
                .strip_prefix(key)
                .filter(|_| token.starts_with(key))
                .map(str::to_string)
        })
    }

    #[test]
    fn test_report_names_every_phase_in_order() {
        // Renders a report; see `env_lock`.
        let _lock = env_lock();
        let mut timeline = fixture();
        push_at(&mut timeline, PHASE_CONFIG, 1, 1, PhaseDetail::default());
        push_at(
            &mut timeline,
            PHASE_DAEMON_CONNECT,
            4,
            2,
            PhaseDetail::count(3),
        );
        timeline.ready_at = Some(Duration::from_millis(9));

        let report = timeline.report();
        let phases: Vec<&str> = report
            .lines()
            .filter(|line| line.starts_with("phase "))
            .map(|line| line.split_whitespace().nth(1).expect("phase name"))
            .collect();
        assert_eq!(
            phases,
            vec![PHASE_CONFIG, PHASE_DAEMON_CONNECT],
            "the report must preserve the order phases were recorded in, so a \
             reader can see what blocked what; report was:\n{report}"
        );
        assert!(
            report.contains("time_to_ready_ms=9.000"),
            "the report must carry total time-to-ready; report was:\n{report}"
        );
        assert!(
            report.contains("count=3"),
            "a phase's count must survive into the report; report was:\n{report}"
        );
    }

    #[test]
    fn test_the_report_states_how_much_of_the_total_no_phase_covers() {
        // Renders a report; see `env_lock`.
        let _lock = env_lock();
        // The defect this closes: at `2e383b52` roughly 6.4 ms of a 13.99 ms
        // total was covered by no phase, and the report emitted no gap column,
        // so the only way to see it was to diff `at_ms` by hand. Nobody did,
        // and the commit message confidently attributed the gap to code that
        // runs *after* it.
        let mut timeline = fixture();
        push_at(&mut timeline, PHASE_REPL_NEW, 1, 4, PhaseDetail::default());
        // Nested: already inside repl_construct, so it must not be counted
        // again or the "accounted" total exceeds the real one.
        push_at(
            &mut timeline,
            PHASE_MEMORY_OPEN,
            2,
            2,
            PhaseDetail::default(),
        );
        push_at(
            &mut timeline,
            PHASE_DAEMON_CONNECT,
            6,
            2,
            PhaseDetail::default(),
        );
        timeline.ready_at = Some(Duration::from_millis(10));

        let report = timeline.report();
        assert_eq!(
            field(&report, "accounted_ms=").as_deref(),
            Some("6.000"),
            "accounted time is the sum of the outermost phases only -- \
             repl_construct 4 ms plus daemon_http_connect 2 ms, with the \
             nested memory_open 2 ms already inside the first. Report \
             was:\n{report}"
        );
        assert_eq!(
            field(&report, "unaccounted_ms=").as_deref(),
            Some("4.000"),
            "and the report must say outright how much of the 10 ms total no \
             phase covers, rather than leaving a reader to diff the at_ms \
             column. Report was:\n{report}"
        );
    }

    #[test]
    fn test_a_slow_phase_is_named_in_the_report() {
        // Renders a report; see `env_lock`.
        let _lock = env_lock();
        let mut timeline = fixture();
        push_at(&mut timeline, PHASE_CONFIG, 1, 1, PhaseDetail::default());
        push_at(
            &mut timeline,
            PHASE_DAEMON_CONNECT,
            4,
            slow_phase_budget().as_millis() as u64 + 5,
            PhaseDetail::default(),
        );

        let report = timeline.report();
        let slow: Vec<&str> = report
            .lines()
            .filter(|line| line.ends_with(" SLOW"))
            .map(|line| line.split_whitespace().nth(1).expect("phase name"))
            .collect();
        assert_eq!(
            slow,
            vec![PHASE_DAEMON_CONNECT],
            "a phase over budget must be visible and named -- 'startup was \
             slow' is not actionable, 'daemon_http_connect was slow' is; \
             budget is {:?} and the report was:\n{report}",
            slow_phase_budget(),
        );
    }

    #[test]
    fn test_a_slow_phase_names_itself_in_the_only_field_the_terminal_shows() {
        // #364: "A phase that exceeds its budget says so, by name." The
        // operator's surface is `OutputManagerLayer`, whose `MessageVisitor`
        // records the `message` field and discards every other -- so a
        // warning that carries the name in `phase = ...` reaches the terminal
        // as "⚠️ [startup] startup phase exceeded its budget". This asserts
        // through that exact visitor.
        use crate::cli::output_layer::MessageVisitor;
        use std::sync::Arc;
        use tracing_subscriber::layer::SubscriberExt as _;

        #[derive(Clone, Default)]
        struct Captured(Arc<Mutex<Vec<String>>>);

        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Captured {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                if *event.metadata().level() != tracing::Level::WARN {
                    return;
                }
                let mut visitor = MessageVisitor::new();
                event.record(&mut visitor);
                if let Some(message) = visitor.message() {
                    self.0
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push(message.to_string());
                }
            }
        }

        let budget = slow_phase_budget();
        let overrun = budget + Duration::from_millis(61);
        let captured = Captured::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        tracing::subscriber::with_default(subscriber, || {
            drop(phase(PHASE_MEMORY_OPEN).backdate(overrun));
        });

        let messages = captured.0.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let named: Vec<&String> = messages
            .iter()
            .filter(|message| message.contains(PHASE_MEMORY_OPEN))
            .collect();
        assert!(
            !named.is_empty(),
            "the over-budget warning must name the phase in its message, \
             because the message is the only field that reaches the operator's \
             terminal -- everything else is dropped by MessageVisitor, and \
             '⚠️ [startup] startup phase exceeded its budget' tells an \
             operator only that startup was slow. Warnings captured through \
             the real MessageVisitor were {messages:?}"
        );
        // The duration too, parsed rather than substring-matched, so a message
        // that merely happens to contain the digits does not pass.
        let reported: Vec<f64> = named
            .iter()
            .filter_map(|message| {
                let (head, _) = message.split_once(" ms, over its ")?;
                head.rsplit(' ').next()?.parse::<f64>().ok()
            })
            .collect();
        assert!(
            reported
                .iter()
                .any(|ms| *ms >= overrun.as_secs_f64() * 1000.0),
            "and it must carry how long the phase took, or the operator is \
             told a phase was slow without being told how slow. The phase ran \
             {overrun:?} against a {budget:?} budget; durations parsed out \
             of the messages were {reported:?} and the messages were {named:?}"
        );
        assert!(
            named
                .iter()
                .any(|message| message.contains("over its 150 ms budget")),
            "and it must name the budget it exceeded; messages were {named:?}"
        );
    }

    #[test]
    fn test_the_report_carries_no_content_only_names_and_numbers() {
        // Renders a report; see `env_lock`.
        let _lock = env_lock();
        // The detail type has no String field, so this is checking the render
        // path honours that rather than re-checking the type system. Every
        // token in a phase line must be a known-static name or a number.
        let mut timeline = fixture();
        push(
            &mut timeline,
            PHASE_BRAIN_ATTACH,
            1,
            PhaseDetail::count(113)
                .with_bytes(1_348_944)
                .with_category("cached"),
        );
        timeline.ready_at = Some(Duration::from_millis(2));

        let report = timeline.report();
        let line = report
            .lines()
            .find(|line| line.starts_with("phase "))
            .expect("one phase line");
        let allowed_names = [PHASE_BRAIN_ATTACH, "cached"];
        for token in line.split_whitespace().skip(1) {
            let value = token.split_once('=').map(|(_, v)| v).unwrap_or(token);
            let acceptable = allowed_names.contains(&value)
                || value.chars().all(|c| c.is_ascii_digit() || c == '.')
                || value == "SLOW";
            assert!(
                acceptable,
                "the startup report must contain only static phase names, \
                 static categories and numbers -- a token that is neither is \
                 how a Brain name or a prompt would leak into diagnostics; \
                 offending token {token:?} in line {line:?}"
            );
        }
    }

    #[test]
    fn test_a_mark_is_distinguishable_from_a_phase() {
        // Renders a report; see `env_lock`.
        let _lock = env_lock();
        let mut timeline = fixture();
        timeline.records.push(PhaseRecord {
            name: MARK_INPUT_READY,
            kind: PhaseKind::Mark,
            started_at: Duration::from_millis(3),
            duration: Duration::ZERO,
            detail: PhaseDetail::default(),
        });
        push(&mut timeline, PHASE_CONFIG, 1, PhaseDetail::default());

        let report = timeline.report();
        assert!(
            report.contains(&format!("mark {MARK_INPUT_READY} ")),
            "an instant must be reported as a mark, not as a zero-length \
             phase, so a reader is not told that becoming ready took 0 ms; \
             report was:\n{report}"
        );
        assert!(
            report.contains(&format!("phase {PHASE_CONFIG} ")),
            "a real phase must still be reported as a phase; report was:\n{report}"
        );
    }

    #[test]
    fn test_the_report_is_in_start_order_not_completion_order() {
        // Renders a report; see `env_lock`.
        let _lock = env_lock();
        // A phase records on drop, so an enclosing phase is pushed *after*
        // everything it contains. Rendering in push order says
        // "repl_construct began after mcp_connect finished", which is the
        // opposite of the truth.
        let mut timeline = fixture();
        push_at(
            &mut timeline,
            PHASE_MCP_CONNECT,
            40,
            10,
            PhaseDetail::default(),
        );
        push_at(
            &mut timeline,
            PHASE_REPL_NEW,
            10,
            90,
            PhaseDetail::default(),
        );

        let report = timeline.report();
        let order: Vec<&str> = report
            .lines()
            .filter(|line| line.starts_with("phase "))
            .map(|line| line.split_whitespace().nth(1).expect("name"))
            .collect();
        assert_eq!(
            order,
            vec![PHASE_REPL_NEW, PHASE_MCP_CONNECT],
            "the enclosing phase started first and must be reported first, \
             whatever order the guards happened to drop in; report was:\n{report}"
        );
    }

    #[test]
    fn test_a_nested_phase_reports_its_depth() {
        // Renders a report; see `env_lock`.
        let _lock = env_lock();
        // Durations nest, so the ms column does not sum to the total. Depth is
        // how a reader knows which durations are already counted inside
        // another rather than silently double-counting them.
        let mut timeline = fixture();
        push_at(
            &mut timeline,
            PHASE_REPL_NEW,
            10,
            90,
            PhaseDetail::default(),
        );
        push_at(
            &mut timeline,
            PHASE_MCP_CONNECT,
            40,
            10,
            PhaseDetail::default(),
        );

        let report = timeline.report();
        let depth_of = |name: &str| -> Option<String> {
            report
                .lines()
                .find(|line| line.split_whitespace().nth(1) == Some(name))
                .and_then(|line| {
                    line.split_whitespace()
                        .find_map(|token| token.strip_prefix("depth=").map(str::to_string))
                })
        };
        assert_eq!(
            depth_of(PHASE_REPL_NEW).as_deref(),
            Some("0"),
            "the outermost phase is enclosed by nothing; report was:\n{report}"
        );
        assert_eq!(
            depth_of(PHASE_MCP_CONNECT).as_deref(),
            Some("1"),
            "a phase that runs entirely inside another must say so, or a \
             reader sums the column and double-counts it; report was:\n{report}"
        );
    }

    #[test]
    fn test_publish_replaces_the_destination_by_rename_not_by_truncation() {
        // The report is published while a reader may be polling for it, so a
        // reader must never see a partial file and misread it as a startup
        // that produced no total. `fs::write` truncates in place; this writes a
        // per-process temporary and renames.
        //
        // Asserted on the inode, because "the content arrived" is equally true
        // of a plain `fs::write(destination)` -- which is exactly the mutant
        // that has to fail. A rename swaps the directory entry for a new file;
        // a truncating write keeps the inode and exposes the hole.
        use std::os::unix::fs::MetadataExt as _;

        let _lock = env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let destination = dir.path().join("timings.txt");
        std::fs::write(&destination, "stale report from a previous launch\n")
            .expect("seed a destination a reader could already be polling");
        let before = std::fs::metadata(&destination)
            .expect("stale metadata")
            .ino();
        let _var = TimingsVar::set(&destination);

        publish("finch startup timings (#364)\ntime_to_ready_ms=1.000\n");

        let written = std::fs::read_to_string(&destination).expect("report published");
        assert!(
            written.contains("time_to_ready_ms=1.000"),
            "the report must reach its destination; got {written:?}"
        );
        let after = std::fs::metadata(&destination)
            .expect("published metadata")
            .ino();
        assert_ne!(
            before, after,
            "the destination must be replaced by rename, not truncated in \
             place: a reader polling this path during a truncating write reads \
             a partial report and parses it as a startup with no total. inode \
             was {before} before publishing and {after} after, so the file was \
             overwritten where it stood"
        );
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read tempdir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("partial"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "the rename must consume the temporary; a per-process `.partial` \
             left behind accumulates one file per launch. Found {leftovers:?}"
        );
    }

    #[test]
    fn test_publish_writes_nowhere_at_all_when_the_destination_is_unset() {
        // The default, and the case that must stay free. The previous version
        // of this test had no assertion in it: `publish` growing a default
        // destination -- writing into `~/.finch` on every start -- left it
        // green.
        let _lock = env_lock();
        let home = tempfile::tempdir().expect("disposable HOME");
        let _var = TimingsVar::unset();

        assert_eq!(
            destination(),
            None,
            "with FINCH_STARTUP_TIMINGS unset the report has nowhere to go. A \
             default destination here would write a file into every user's \
             home on every start, unasked"
        );

        // And nothing is written even so. `publish` is what a future default
        // would be added to, and `destination()` is what it would be added to
        // *through*, so assert on both.
        let previous_home = std::env::var_os("HOME");
        // SAFETY: `env_lock` is held; HOME is restored below before it is
        // released.
        unsafe { std::env::set_var("HOME", home.path()) };
        publish("finch startup timings (#364)\ntime_to_ready_ms=1.000\n");
        match previous_home {
            // SAFETY: as above.
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }

        let created: Vec<String> = std::fs::read_dir(home.path())
            .expect("read the disposable HOME")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            created.is_empty(),
            "publishing with no destination must create nothing anywhere, and \
             a home directory is where a default would land. Found {created:?}"
        );
    }

    #[test]
    fn test_ready_is_idempotent_renders_nothing_unasked_and_the_first_call_wins() {
        // Three properties of the real `ready`, asserted by calling it rather
        // than by re-implementing its guard inline and asserting on the
        // re-implementation, which is what the previous version did -- it
        // never called `ready` at all, so deleting the production guard left
        // the unit suite entirely green.
        //
        // `ready` closes the process-global timeline, which is why this is one
        // test and not three: it can only be called for the first time once.
        let _lock = env_lock();
        let _var = TimingsVar::unset();
        begin();

        let rendered_before = REPORTS_RENDERED.load(Ordering::Relaxed);
        ready();
        let (first_total, first_marks) = with_timeline(|timeline| {
            (
                timeline.ready_at,
                timeline
                    .records
                    .iter()
                    .filter(|record| record.name == MARK_INPUT_READY)
                    .count(),
            )
        });
        ready();
        let (second_total, second_marks) = with_timeline(|timeline| {
            (
                timeline.ready_at,
                timeline
                    .records
                    .iter()
                    .filter(|record| record.name == MARK_INPUT_READY)
                    .count(),
            )
        });
        let rendered_after = REPORTS_RENDERED.load(Ordering::Relaxed);

        assert!(
            first_total.is_some(),
            "the first call must stamp a total, or nothing defines \
             time-to-ready"
        );
        assert_eq!(
            first_marks, 1,
            "the first call records exactly one input_ready mark; recorded \
             {first_marks}"
        );
        assert_eq!(
            second_marks, 1,
            "a second call must not append a second input_ready mark. The two \
             REPL entry points are exclusive today, but a report with two \
             readiness instants has no defined total. Marks after the second \
             call: {second_marks}"
        );
        assert_eq!(
            second_total, first_total,
            "and the first call's total is the one that stands, or \
             time-to-ready silently becomes 'time until the last thing that \
             called ready'"
        );
        assert_eq!(
            rendered_before, rendered_after,
            "with no destination set, `ready` must not render a report at \
             all. Rendering allocates kilobytes, sorts, runs an O(n^2) depth \
             pass and formats dozens of f64s -- on every start, before the \
             first frame, for a report nobody asked for. #364: the \
             measurement itself must not become a startup phase. It could not \
             even appear in its own report, because ready_at is stamped \
             before it. Renders went from {rendered_before} to {rendered_after}"
        );
    }

    #[test]
    fn test_begin_does_not_move_t0_once_the_timeline_exists() {
        // `begin` is idempotent through `OnceLock`. If a second call reset t0,
        // every phase recorded before it would report a negative or truncated
        // offset, and the total would understate startup by whatever ran first.
        begin();
        let first = with_timeline(|timeline| timeline.entry);
        begin();
        let second = with_timeline(|timeline| timeline.entry);
        assert_eq!(
            first, second,
            "t0 must be fixed at the first call; moving it would make every \
             earlier phase's offset wrong and understate the total"
        );
    }

    #[test]
    fn test_time_to_ready_is_absent_until_ready_is_recorded() {
        // Renders a report; see `env_lock`.
        let _lock = env_lock();
        let timeline = fixture();
        assert_eq!(
            timeline.time_to_ready(),
            None,
            "a timeline that never reached the event loop must not report a \
             time-to-ready; reporting one would make an aborted startup look \
             like a fast one"
        );
        assert!(
            timeline.report().contains("time_to_ready_ms=none"),
            "and the report must say so explicitly; report was:\n{}",
            timeline.report()
        );
    }

    #[test]
    fn test_a_stderr_report_uses_terminal_line_endings() {
        // Renders a report; see `env_lock`.
        let _lock = env_lock();
        // `FINCH_STARTUP_TIMINGS=1` is read in the mode this module measures,
        // and by then raw mode has cleared OPOST/ONLCR. A bare `\n` moves down
        // without a carriage return, so the report staircases off the right
        // edge of the screen and is unreadable in the one mode it exists for.
        let mut timeline = fixture();
        push_at(&mut timeline, PHASE_CONFIG, 1, 1, PhaseDetail::default());
        timeline.ready_at = Some(Duration::from_millis(2));
        let report = timeline.report();
        assert!(
            report.contains('\n') && !report.contains('\r'),
            "the report itself is LF-terminated; it is the stderr sink that \
             converts. Report was:\n{report:?}"
        );

        // The production conversion, not a copy of it. Capturing the process's
        // real stderr is not available here -- libtest shares one stderr
        // between parallel tests -- so this asserts on the function `publish_to`
        // actually calls.
        let converted = for_raw_terminal(&report);
        let lines = report.lines().count();
        assert_eq!(
            converted.matches("\r\n").count(),
            lines,
            "every one of the report's {lines} lines must end CRLF before it \
             reaches a raw-mode terminal, or the report staircases across the \
             screen in the mode it exists to measure. Converted form was \
             {converted:?}"
        );
        assert!(
            !converted.contains("\n\n") && !converted.replace("\r\n", "").contains('\n'),
            "and no bare LF may survive the conversion; converted form was \
             {converted:?}"
        );
    }

    /// N5's mutant: a fixed `<path>.partial` instead of a per-pid one.
    ///
    /// Neither suite could catch it before. The unit tests write to their own
    /// destinations and the integration fixtures each get their own
    /// `FINCH_STARTUP_TIMINGS` path, so nothing ever had two writers on one
    /// destination -- which is exactly the situation the per-pid name exists
    /// for, and exactly the situation a test cannot stage without spawning a
    /// second process racing the first. Asserting the name is the property
    /// that survives review: two processes cannot collide on a scratch file
    /// whose name contains their pid.
    #[test]
    fn test_the_scratch_file_is_named_per_process_so_two_finches_cannot_collide() {
        let destination = std::path::Path::new("/tmp/finch-startup-timings.txt");
        let scratch = temporary_path(destination);
        let rendered = scratch.to_string_lossy().into_owned();
        let pid = std::process::id();

        assert!(
            rendered.contains(&pid.to_string()),
            "the scratch file `publish_to` writes before renaming must carry \
             this process's pid ({pid}). Two `finch` processes that inherit \
             one FINCH_STARTUP_TIMINGS from a shell profile otherwise share \
             one scratch path: `fs::write` truncates the other's half-written \
             buffer and the survivor's rename publishes that torn buffer as a \
             complete report, which parses, so nothing downstream can detect \
             it. Scratch path was {rendered:?}"
        );
        assert_ne!(
            rendered,
            format!("{}.partial", destination.display()),
            "a fixed `.partial` name is the collision above; the name must \
             be per-process"
        );
        assert!(
            rendered.starts_with(&*destination.to_string_lossy()),
            "the scratch file must sit beside its destination so the publish \
             is a rename within one filesystem rather than a copy that can \
             tear. Scratch path was {rendered:?}"
        );
    }

    /// N1b's mutant deleted an instrumented phase from the production guard
    /// and from the test's expected order, and left the suite green because
    /// nothing tied the two together. `ALL_PHASES` is now generated from the
    /// same list as the constants, and the integration suite derives what it
    /// expects from it. This holds up the generated list itself.
    #[test]
    fn test_every_declared_phase_and_mark_name_is_unique_and_non_empty() {
        for (label, names) in [("ALL_PHASES", ALL_PHASES), ("ALL_MARKS", ALL_MARKS)] {
            let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
            assert_eq!(
                unique.len(),
                names.len(),
                "{label} has a duplicate name. Report entries are matched by \
                 name, so two phases sharing one would make the expected-order \
                 assertion satisfiable by the wrong entry. {label} was {names:?}"
            );
            assert!(
                names.iter().all(|name| !name.is_empty()),
                "{label} contains an empty name, which renders as a report \
                 line with no subject. {label} was {names:?}"
            );
        }
        let overlap: Vec<&&str> = ALL_PHASES
            .iter()
            .filter(|name| ALL_MARKS.contains(name))
            .collect();
        assert!(
            overlap.is_empty(),
            "a name cannot be both a phase and a mark: the report's first \
             token distinguishes them and a reader would see the same name \
             under both kinds. Overlapping names were {overlap:?}"
        );
        assert!(
            ALL_PHASES.contains(&PHASE_DAEMON_HEALTH_PROBE)
                && ALL_PHASES.contains(&PHASE_DAEMON_RETRY_BACKOFF),
            "the daemon connect's sub-phases must be declared through the \
             same list as every other phase, or the integration suite's \
             derivation cannot see them. ALL_PHASES was {ALL_PHASES:?}"
        );
    }

    /// The budget override that lets the over-budget warning -- a user-visible
    /// terminal surface -- be driven at the production boundary instead of
    /// only here. `slow_phase_budget` caches on first read, so this asserts
    /// the parse rather than re-reading the environment.
    #[test]
    fn test_an_unparseable_budget_override_falls_back_rather_than_disabling_the_warning() {
        // The parse `slow_phase_budget` performs, applied to the values a
        // profile can realistically hold. A `0` means "warn about everything",
        // which is what the production-boundary test uses; anything
        // unparseable must leave the default in force rather than silently
        // becoming zero (warn about everything) or `u64::MAX` (warn about
        // nothing).
        fn parse(value: &str) -> Duration {
            value
                .trim()
                .parse::<u64>()
                .map(Duration::from_millis)
                .unwrap_or(DEFAULT_SLOW_PHASE)
        }
        assert_eq!(parse("0"), Duration::ZERO);
        assert_eq!(parse(" 250 "), Duration::from_millis(250));
        assert_eq!(
            parse("soon"),
            DEFAULT_SLOW_PHASE,
            "an unparseable FINCH_STARTUP_SLOW_BUDGET_MS must leave the \
             default budget in force. Reading it as zero would warn on every \
             phase of every launch, and reading it as no budget at all would \
             silently remove the one startup line #364 requires a user to see."
        );
        assert_eq!(parse(""), DEFAULT_SLOW_PHASE);
    }
}
