//! Startup phase timing for the interactive path (#364).
//!
//! Nothing in Finch measured its own startup. There was no `Instant::now()` on
//! the path from `main` to the first interactive frame, no tracing span, and no
//! diagnostic that reported a duration -- only two `SHAMMAH_DEBUG` `eprintln!`s
//! bracketing the entire event loop. Every latency claim about startup was
//! therefore unfalsifiable, which is the failure mode `AGENTS.md` names first.
//!
//! # What "time-to-ready" means here
//!
//! Time-to-ready is [`begin`] through [`ready`]. [`begin`] is called from the
//! first statement of `main`, so it excludes pre-`main` runtime and dynamic
//! loader work; that is a proxy for process entry, not process entry itself,
//! and the report says so rather than overclaiming. [`ready`] is called
//! immediately before the event loop's `select!` begins consuming keystrokes,
//! which is the first instant at which a typed key is acted upon.
//!
//! Three earlier instants are recorded separately because they are routinely
//! confused with readiness and are not the same:
//!
//! * [`MARK_TERMINAL_OWNED`] -- raw mode entered, the terminal is ours.
//! * [`MARK_INPUT_CAPTURED`] -- the reader task is running, so keys are
//!   buffered and not lost, but nothing acts on them yet.
//! * [`MARK_HEADER_PAINTED`] -- the first frame's content has been written.
//!
//! # Privacy
//!
//! A phase carries a name, a duration, and [`PhaseDetail`] -- counts, byte
//! totals, and a `&'static str` category. That is the whole vocabulary. There
//! is no field of any type that can hold a Brain name, an event payload, a
//! prompt, a path, or a credential, so a report cannot leak one by mistake
//! rather than merely by convention.

use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Phase names. `&'static str` so no caller can smuggle content into one.
pub const PHASE_ARGS: &str = "args_parse";
pub const PHASE_CONFIG: &str = "config_load";
pub const PHASE_TRACING: &str = "tracing_init";
pub const PHASE_THRESHOLD_ROUTER: &str = "threshold_router_load";
pub const PHASE_PROVIDER_GRAPH: &str = "provider_graph_build";
pub const PHASE_METRICS: &str = "metrics_logger_init";
pub const PHASE_DAEMON_CONNECT: &str = "daemon_http_connect";
pub const PHASE_REPL_NEW: &str = "repl_construct";
pub const PHASE_SESSION_RESTORE: &str = "session_restore";
pub const PHASE_IPC_CONNECT: &str = "ipc_connect";
pub const PHASE_TERMINAL_INIT: &str = "terminal_init";
pub const PHASE_MEMORY_OPEN: &str = "memory_open";
pub const PHASE_PROGRAM_SYNC: &str = "program_sync";
pub const PHASE_TOOL_REGISTRY: &str = "tool_registry";
pub const PHASE_MCP_CONNECT: &str = "mcp_connect";
pub const PHASE_BRAIN_REGISTER: &str = "brain_register";
pub const PHASE_BRAIN_ATTACH: &str = "brain_attach";

/// Instant marks, recorded as zero-duration entries in timeline order.
pub const MARK_TERMINAL_OWNED: &str = "terminal_owned";
pub const MARK_INPUT_CAPTURED: &str = "input_captured";
pub const MARK_HEADER_PAINTED: &str = "header_painted";
pub const MARK_INPUT_READY: &str = "input_ready";

/// A phase slower than this is reported as `SLOW` and logged at `warn`.
///
/// This governs *reporting only*. No test asserts on it, and none should: #242
/// burned four assertions on wall-clock startup thresholds, three of which were
/// wrong and one of which reached green CI while depending on the machine being
/// busy. A budget makes a slow phase visible and nameable; it is not a gate.
const SLOW_PHASE: Duration = Duration::from_millis(150);

/// Content-free measurements attached to a phase.
///
/// Every field is a number or a `&'static str`. There is deliberately no
/// `String` and no `PathBuf`: the type is what keeps Brain names, prompts, and
/// credentials out of the report, not the discipline of each call site.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhaseDetail {
    /// How many things the phase handled -- Brains counted, files swept,
    /// servers connected, round-trips made.
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

/// One completed phase or instant mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseRecord {
    pub name: &'static str,
    /// Offset from [`begin`] at which the phase started.
    pub started_at: Duration,
    /// Zero for an instant mark.
    pub duration: Duration,
    pub detail: PhaseDetail,
}

impl PhaseRecord {
    /// An instant mark has no duration; a phase does.
    pub fn is_mark(&self) -> bool {
        self.duration.is_zero() && self.detail == PhaseDetail::default()
    }

    pub fn is_slow(&self) -> bool {
        self.duration >= SLOW_PHASE
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
    /// a phase line is the phase name; a slow phase carries a trailing `SLOW`.
    pub fn report(&self) -> String {
        let mut out = String::with_capacity(128 * (self.records.len() + 4));
        out.push_str("finch startup timings (#364)\n");
        out.push_str(
            "t0 is the first statement of main, so pre-main runtime and loader \
             work is excluded\n",
        );
        let _ = writeln!(out, "phases={}", self.records.len());
        for record in &self.records {
            let kind = if record.is_mark() { "mark" } else { "phase" };
            let _ = write!(
                out,
                "{kind} {name} at_ms={at:.3} ms={ms:.3}",
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
        match self.ready_at {
            Some(ready) => {
                let _ = writeln!(out, "time_to_ready_ms={:.3}", ready.as_secs_f64() * 1000.0);
            }
            None => out.push_str("time_to_ready_ms=none\n"),
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
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        let duration = self.start.elapsed();
        let record = PhaseRecord {
            name: self.name,
            started_at: self.started_at,
            duration,
            detail: self.detail,
        };
        let slow = record.is_slow();
        with_timeline(|timeline| timeline.records.push(record));
        let ms = duration.as_secs_f64() * 1000.0;
        if slow {
            // Named, so the operator knows which phase to look at rather than
            // being told only that startup was slow.
            tracing::warn!(
                target: "finch::startup",
                phase = self.name,
                ms,
                budget_ms = SLOW_PHASE.as_secs_f64() * 1000.0,
                count = self.detail.count,
                bytes = self.detail.bytes,
                category = self.detail.category,
                "startup phase exceeded its budget"
            );
            return;
        }
        tracing::debug!(
            target: "finch::startup",
            phase = self.name,
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
/// Call immediately before the event loop begins consuming keystrokes.
pub fn ready() {
    let (report, total) = with_timeline(|timeline| {
        let ready_at = timeline.entry.elapsed();
        timeline.records.push(PhaseRecord {
            name: MARK_INPUT_READY,
            started_at: ready_at,
            duration: Duration::ZERO,
            detail: PhaseDetail::default(),
        });
        timeline.ready_at = Some(ready_at);
        (timeline.report(), ready_at)
    });
    tracing::info!(
        target: "finch::startup",
        time_to_ready_ms = total.as_secs_f64() * 1000.0,
        "finch interactive startup ready"
    );
    publish(&report);
}

/// The current report, whether or not [`ready`] has been reached.
pub fn report() -> String {
    with_timeline(|timeline| timeline.report())
}

/// Where the report goes, per `FINCH_STARTUP_TIMINGS`.
///
/// Unset: nowhere -- the tracing events above are the only surface, reachable
/// with `RUST_LOG=finch::startup=debug`. `1` or `stderr`: standard error.
/// Anything else is a file path, which is how a test observes the real
/// interactive path without parsing a live TUI frame.
fn publish(report: &str) {
    let Some(destination) = std::env::var_os("FINCH_STARTUP_TIMINGS") else {
        return;
    };
    let destination = destination.to_string_lossy().into_owned();
    if destination.is_empty() {
        return;
    }
    if destination == "1" || destination == "stderr" {
        eprint!("{report}");
        return;
    }
    // Best effort. A diagnostic that cannot write its file must not break the
    // startup it is measuring, but it must not fail silently either.
    if let Err(error) = std::fs::write(&destination, report) {
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

    /// A fresh timeline, independent of the process-global one, so these tests
    /// do not depend on each other's ordering.
    fn fixture() -> Timeline {
        Timeline::new(Instant::now())
    }

    fn push(timeline: &mut Timeline, name: &'static str, ms: u64, detail: PhaseDetail) {
        timeline.records.push(PhaseRecord {
            name,
            started_at: Duration::from_millis(1),
            duration: Duration::from_millis(ms),
            detail,
        });
    }

    #[test]
    fn test_report_names_every_phase_in_order() {
        let mut timeline = fixture();
        push(&mut timeline, PHASE_CONFIG, 1, PhaseDetail::default());
        push(
            &mut timeline,
            PHASE_DAEMON_CONNECT,
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
    fn test_a_slow_phase_is_named_in_the_report() {
        let mut timeline = fixture();
        push(&mut timeline, PHASE_CONFIG, 1, PhaseDetail::default());
        push(
            &mut timeline,
            PHASE_DAEMON_CONNECT,
            SLOW_PHASE.as_millis() as u64 + 5,
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
             budget is {SLOW_PHASE:?} and the report was:\n{report}"
        );
    }

    #[test]
    fn test_the_report_carries_no_content_only_names_and_numbers() {
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
        let mut timeline = fixture();
        timeline.records.push(PhaseRecord {
            name: MARK_INPUT_READY,
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
    fn test_time_to_ready_is_absent_until_ready_is_recorded() {
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
}
