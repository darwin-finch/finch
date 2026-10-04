//! The in-progress indicator of one assistant turn, at the production
//! boundary: a real `EventLoop` on its real LLM worker, a provider whose
//! stream the test advances one chunk at a time, and the production render
//! tick painting into a modelled terminal (`LiveFrameProbe`).
//!
//! Issue #1664 (the waiting indicator was a frozen hollow circle with no
//! elapsed time or token count, while a second, animated indicator existed
//! on a path the live terminal never painted).
//!
//! Time is a clock the test advances by hand (`WorkClock`), so the pulse
//! frame and the elapsed seconds on screen are exact; no assertion compares
//! wall-clock durations. The only real-time bound is `settle`'s liveness
//! guard, whose failure says the turn hung.

use super::*;

use crate::generators::StreamChunk;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

type ChunkSender = tokio::sync::mpsc::Sender<anyhow::Result<StreamChunk>>;
type ChunkReceiver = tokio::sync::mpsc::Receiver<anyhow::Result<StreamChunk>>;

const WIDTH: usize = 80;
const HEIGHT: usize = 24;
const PROMPT: &str = "what is the plan PROMPT_SENTINEL_1664";
const ANSWER: &str = "ANSWER_SENTINEL_1664";
const TOOL_NAME: &str = "indicator_probe";

fn provenance() -> crate::providers::EventProvenance {
    crate::providers::EventProvenance {
        provider: "stepped-stream-provider".into(),
        model: "stepped-stream-provider".into(),
        event: "scripted".into(),
        sequence: 1,
        opaque_replay: None,
    }
}

/// A streaming provider the test steps by hand. Each provider call blocks
/// until the test releases it (the "request sent, nothing back yet" stage),
/// then hands back a stream whose sender the test holds.
struct SteppedStreamProvider {
    streams: std::sync::Mutex<VecDeque<ChunkReceiver>>,
    calls_started: AtomicUsize,
    calls_released: tokio::sync::Semaphore,
}

impl SteppedStreamProvider {
    /// A provider scripted for `calls` provider requests, with their senders.
    fn new(calls: usize) -> (Arc<Self>, Vec<ChunkSender>) {
        let mut senders = Vec::new();
        let mut streams = VecDeque::new();
        for _ in 0..calls {
            let (sender, receiver) = tokio::sync::mpsc::channel(16);
            senders.push(sender);
            streams.push_back(receiver);
        }
        let provider = Arc::new(Self {
            streams: std::sync::Mutex::new(streams),
            calls_started: AtomicUsize::new(0),
            calls_released: tokio::sync::Semaphore::new(0),
        });
        (provider, senders)
    }
}

#[async_trait::async_trait]
impl crate::generators::Generator for SteppedStreamProvider {
    async fn generate(
        &self,
        _messages: Vec<crate::providers::Message>,
        _tools: Option<Vec<crate::tools::ToolDefinition>>,
    ) -> anyhow::Result<crate::generators::GeneratorResponse> {
        anyhow::bail!("the stepped provider only streams")
    }

    async fn generate_stream(
        &self,
        _messages: Vec<crate::providers::Message>,
        _tools: Option<Vec<crate::tools::ToolDefinition>>,
    ) -> anyhow::Result<Option<ChunkReceiver>> {
        self.calls_started.fetch_add(1, Ordering::SeqCst);
        self.calls_released
            .acquire()
            .await
            .expect("the release semaphore is never closed")
            .forget();
        let stream = self
            .streams
            .lock()
            .expect("stepped provider stream lock poisoned")
            .pop_front()
            .expect("the turn made more provider calls than the test scripted");
        Ok(Some(stream))
    }

    fn capabilities(&self) -> &crate::generators::GeneratorCapabilities {
        static CAPABILITIES: crate::generators::GeneratorCapabilities =
            crate::generators::GeneratorCapabilities {
                supports_streaming: true,
                supports_tools: true,
                supports_conversation: true,
                max_context_messages: None,
            };
        &CAPABILITIES
    }

    fn name(&self) -> &str {
        "stepped-stream-provider"
    }
}

/// A read-only tool that blocks until the test releases it, so a frame can
/// be captured while a tool row is running.
struct GatedProbeTool {
    started: Arc<AtomicUsize>,
    release: Arc<tokio::sync::Semaphore>,
}

#[async_trait::async_trait]
impl crate::tools::Tool for GatedProbeTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn effect(&self) -> finch_programs::ExecutionEffect {
        finch_programs::ExecutionEffect::WorkspaceRead
    }

    fn description(&self) -> &str {
        "blocks until the indicator test releases it"
    }

    fn input_schema(&self) -> crate::tools::ToolInputSchema {
        crate::tools::ToolInputSchema::simple(Vec::new())
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        _context: &crate::tools::ToolContext<'_>,
    ) -> anyhow::Result<String> {
        self.started.fetch_add(1, Ordering::SeqCst);
        self.release
            .acquire()
            .await
            .expect("the tool release semaphore is never closed")
            .forget();
        Ok("probe finished".into())
    }
}

/// One real turn: the event loop, its provider's stream senders, and the
/// terminal the production render tick paints into.
struct IndicatorTurn {
    event_loop: EventLoop,
    provider: Arc<SteppedStreamProvider>,
    senders: VecDeque<ChunkSender>,
    probe: finch_tui::LiveFrameProbe,
    tool_started: Arc<AtomicUsize>,
    tool_release: Arc<tokio::sync::Semaphore>,
    /// The screen at each stage the test inspected, for diagnostics.
    frames: Vec<(String, Vec<String>)>,
    terminal_seen: bool,
    /// The hand-advanced clock every work unit of the turn reads.
    now: Arc<std::sync::Mutex<Duration>>,
}

impl IndicatorTurn {
    fn start(provider_calls: usize) -> Self {
        let (provider, senders) = SteppedStreamProvider::new(provider_calls);
        let tool_started = Arc::new(AtomicUsize::new(0));
        let tool_release = Arc::new(tokio::sync::Semaphore::new(0));
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register(Box::new(GatedProbeTool {
            started: Arc::clone(&tool_started),
            release: Arc::clone(&tool_release),
        }));
        let definitions = registry.definitions();
        let patterns = tempfile::tempdir()
            .expect("isolated indicator-test tool state")
            .keep()
            .join("patterns.json");
        let executor = crate::tools::ToolExecutor::new(
            registry,
            crate::tools::PermissionManager::new(),
            patterns,
        )
        .expect("construct the indicator-test tool executor");
        let mut event_loop = EventLoop::new_test_runner(
            "turn-indicator-test",
            Arc::clone(&provider) as Arc<dyn crate::generators::Generator>,
            definitions,
            Arc::new(tokio::sync::Mutex::new(executor)),
            Arc::new(crate::runtime::ProgramRuntime::new()),
            Vec::new(),
            0,
            None,
        );
        event_loop.streaming_enabled = true;
        event_loop.output_manager.disable_stdout();
        let now = Arc::new(std::sync::Mutex::new(Duration::ZERO));
        let clock_now = Arc::clone(&now);
        event_loop
            .output_manager
            .set_work_clock(crate::cli::messages::WorkClock::from_fn(move || {
                *clock_now
                    .lock()
                    .expect("indicator-test clock lock poisoned")
            }));
        event_loop.start_llm_worker();
        let probe = finch_tui::LiveFrameProbe::attach(
            &mut event_loop
                .tui_renderer
                .try_lock()
                .expect("nothing else holds the renderer before the turn starts"),
            WIDTH,
            HEIGHT,
        );
        Self {
            event_loop,
            provider,
            senders: senders.into(),
            probe,
            tool_started,
            tool_release,
            frames: Vec::new(),
            terminal_seen: false,
            now,
        }
    }

    /// Move the turn's clock forward by exactly `by`.
    fn advance(&self, by: Duration) {
        *self.now.lock().expect("indicator-test clock lock poisoned") += by;
    }

    /// How many frames the production render tick has painted so far.
    fn painted(&self) -> usize {
        self.probe.painted_frames().len()
    }

    /// The token count of the turn's generation unit.
    fn tokens(&self) -> usize {
        self.work_units()
            .iter()
            .map(|unit| unit.token_count)
            .max()
            .unwrap_or(0)
    }

    /// Submit the user turn through the real input path.
    async fn submit(&mut self) -> uuid::Uuid {
        self.event_loop
            .handle_user_input(PROMPT.into())
            .await
            .expect("the indicator turn must dispatch");
        self.event_loop
            .active_query_id
            .read()
            .await
            .expect("the indicator turn must own the active slot")
    }

    /// Dispatch every event already queued, without waiting for more.
    async fn pump(&mut self) {
        while let Ok(event) = self.event_loop.event_rx.try_recv() {
            if matches!(
                &event,
                ReplEvent::StreamingComplete { .. } | ReplEvent::QueryFailed { .. }
            ) {
                self.terminal_seen = true;
            }
            self.event_loop
                .handle_event(event)
                .await
                .expect("indicator-turn events must dispatch");
        }
    }

    /// Dispatch events until `ready` holds. The bound is a liveness guard:
    /// its failure means the turn hung, not that it was slow.
    async fn settle(&mut self, what: &str, mut ready: impl FnMut(&Self) -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            self.pump().await;
            if ready(self) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the indicator turn hung waiting for: {what}\n{}",
                self.report()
            );
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// The live work units, oldest first.
    fn work_units(&self) -> Vec<finch_ui_model::WorkUnitView> {
        let colors = crate::theme::ColorScheme::default();
        self.event_loop
            .output_manager
            .get_messages()
            .iter()
            .filter_map(|message| message.work_unit_view(&colors))
            .collect()
    }

    /// Run one production render tick and record the painted screen.
    async fn frame(&mut self, stage: &str) -> Vec<String> {
        self.event_loop
            .render_tui()
            .await
            .expect("the production render tick must succeed");
        let rows = self.probe.rows();
        self.frames.push((stage.to_string(), rows.clone()));
        rows
    }

    async fn send(&mut self, chunk: StreamChunk) {
        self.senders
            .front()
            .expect("a provider stream is open")
            .send(Ok(chunk))
            .await
            .expect("the worker is reading the provider stream");
    }

    /// Close the current provider stream.
    fn end_stream(&mut self) {
        self.senders.pop_front();
    }

    fn release_provider_call(&self) {
        self.provider.calls_released.add_permits(1);
    }

    fn provider_calls_started(&self) -> usize {
        self.provider.calls_started.load(Ordering::SeqCst)
    }

    /// Every inspected stage and every frame the tick painted, for
    /// assertion messages.
    fn report(&self) -> String {
        let mut report = String::new();
        for (stage, rows) in &self.frames {
            report.push_str(&format!("── stage: {stage} ──\n{}\n", screen(rows)));
        }
        for (index, rows) in self.probe.painted_frames().iter().enumerate() {
            report.push_str(&format!("── painted frame {index} ──\n{}\n", screen(rows)));
        }
        report
    }
}

/// The non-blank rows of one screen.
fn screen(rows: &[String]) -> String {
    rows.iter()
        .filter(|row| !row.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

/// The pulse frames of the turn indicator.
const PULSE: [char; 3] = ['✦', '✳', '✼'];

/// Rows that say "this turn is still working": a marker, a verb, and an
/// ellipsis. The marker set covers the animated pulse, the hollow circle the
/// frozen label used, and the braille frames of the say card's generating
/// line, so a second indicator of any of the three kinds is counted.
fn indicator_rows(rows: &[String]) -> Vec<String> {
    rows.iter()
        .filter(|row| {
            let text = row.trim_start();
            let mut chars = text.chars();
            let Some(marker) = chars.next() else {
                return false;
            };
            let is_marker = PULSE.contains(&marker)
                || marker == '○'
                || ('\u{2800}'..='\u{28ff}').contains(&marker);
            let rest = chars.as_str();
            is_marker && rest.starts_with(' ') && rest.contains('…')
        })
        .cloned()
        .collect()
}

/// The single indicator row of `rows`, or a failure naming the invariant.
fn the_indicator(turn: &IndicatorTurn, stage: &str, rows: &[String]) -> String {
    let found = indicator_rows(rows);
    assert_eq!(
        found.len(),
        1,
        "INVARIANT: a turn in progress shows exactly one in-progress indicator; at stage \
         `{stage}` the screen had {found:?}\n{}",
        turn.report()
    );
    found[0].trim().to_string()
}

fn pulse_of(indicator: &str) -> char {
    indicator
        .chars()
        .next()
        .expect("an indicator row is not empty")
}

/// (a) From the moment the request is sent, before anything comes back, the
/// screen shows one animated indicator with elapsed time and `thinking`.
///
/// Failed before the fix: the waiting row was the fixed label `○ Analyzing…`
/// (`assistant_prose_label` in `crates/finch-ui-model/src/work_unit.rs`),
/// with no pulse frame, no elapsed time, and no `thinking`; a later tick
/// painted nothing because nothing in the row could change.
#[tokio::test]
async fn test_waiting_turn_shows_an_animated_indicator_with_elapsed_time_and_thinking() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut turn = IndicatorTurn::start(1);
            turn.submit().await;
            turn.settle("the provider request to be sent", |turn| {
                turn.provider_calls_started() == 1
            })
            .await;

            let rows = turn.frame("waiting, 0 ms").await;
            let first = the_indicator(&turn, "waiting, 0 ms", &rows);
            assert!(
                PULSE.contains(&pulse_of(&first)) && first.ends_with("… (0s · thinking)"),
                "INVARIANT: while the request is waiting the indicator is a pulse marker, a \
                 verb, elapsed seconds, and `thinking`; got {first:?}\n{}",
                turn.report()
            );
            assert!(
                first.chars().any(|character| character.is_alphabetic()),
                "INVARIANT: the indicator names what is happening in words, so it reads as \
                 in-progress without the animation; got {first:?}\n{}",
                turn.report()
            );

            // No time passes: the tick must not repaint a row that says the
            // same thing, or terminal text selection is destroyed.
            let painted_before = turn.painted();
            turn.frame("waiting, still 0 ms").await;
            assert_eq!(
                turn.painted(),
                painted_before,
                "INVARIANT: a render tick repaints the indicator only when its text changes; \
                 an unchanged indicator was repainted\n{}",
                turn.report()
            );

            // One pulse step later the same tick repaints with the next frame.
            turn.advance(Duration::from_millis(200));
            let rows = turn.frame("waiting, 200 ms").await;
            let second = the_indicator(&turn, "waiting, 200 ms", &rows);
            assert!(
                turn.painted() > painted_before,
                "INVARIANT: the production render tick repaints the indicator when its pulse \
                 frame advances; no frame was painted\n{}",
                turn.report()
            );
            assert_ne!(
                pulse_of(&first),
                pulse_of(&second),
                "INVARIANT: the waiting indicator animates — its marker differs one pulse step \
                 (200 ms) later; first={first:?} second={second:?}\n{}",
                turn.report()
            );
            assert_eq!(
                first.chars().skip(1).collect::<String>(),
                second.chars().skip(1).collect::<String>(),
                "INVARIANT: only the marker changes within one second; first={first:?} \
                 second={second:?}\n{}",
                turn.report()
            );

            // Elapsed seconds come from the turn's clock.
            turn.advance(Duration::from_millis(3_300));
            let rows = turn.frame("waiting, 3500 ms").await;
            let later = the_indicator(&turn, "waiting, 3500 ms", &rows);
            assert!(
                later.ends_with("… (3s · thinking)"),
                "INVARIANT: the waiting indicator shows the turn's elapsed seconds; after \
                 3.5 s it read {later:?}\n{}",
                turn.report()
            );
        })
        .await;
}

/// (b) Once tokens arrive the same indicator shows the token count, and it
/// stays — still exactly one — while the reply streams.
///
/// Failed before the fix: the token count was held on the work unit
/// (`WorkUnitInner::token_count`) but never carried into `WorkUnitView`, so
/// the live row stayed `○ Analyzing…` after reasoning tokens arrived, and
/// once reply text streamed the screen showed the program source with no
/// in-progress indicator at all.
#[tokio::test]
async fn test_turn_indicator_shows_the_token_count_once_tokens_arrive() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut turn = IndicatorTurn::start(1);
            turn.submit().await;
            turn.settle("the provider request to be sent", |turn| {
                turn.provider_calls_started() == 1
            })
            .await;
            let rows = turn.frame("waiting").await;
            let waiting = the_indicator(&turn, "waiting", &rows);
            turn.release_provider_call();

            turn.send(StreamChunk::ThinkingDelta {
                text: "weigh the two approaches first".into(),
                provenance: provenance(),
            })
            .await;
            turn.settle("the reasoning tokens to be counted", |turn| {
                turn.tokens() == 5
            })
            .await;
            turn.advance(Duration::from_secs(2));
            let rows = turn.frame("reasoning tokens arrived").await;
            let reasoning = the_indicator(&turn, "reasoning tokens arrived", &rows);
            assert!(
                reasoning.ends_with("… (2s · ↓ 5 tokens)") && !reasoning.contains("thinking"),
                "INVARIANT: once tokens arrive the indicator shows the token count in place \
                 of `thinking`; got {reasoning:?}\n{}",
                turn.report()
            );
            let verb = |indicator: &str| {
                indicator
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string()
            };
            assert_eq!(
                verb(&waiting),
                verb(&reasoning),
                "INVARIANT: it is the same indicator before and after the first token — the \
                 verb does not change; waiting={waiting:?} reasoning={reasoning:?}\n{}",
                turn.report()
            );

            turn.send(StreamChunk::TextDelta(format!("(say \"{ANSWER}\")")))
                .await;
            turn.settle("the reply text to be counted", |turn| turn.tokens() == 7)
                .await;
            let rows = turn.frame("reply streaming").await;
            let streaming = the_indicator(&turn, "reply streaming", &rows);
            assert!(
                streaming.ends_with("… (2s · ↓ 7 tokens)"),
                "INVARIANT: the indicator keeps counting while the reply streams; got \
                 {streaming:?}\n{}",
                turn.report()
            );
            assert!(
                rows.iter().any(|row| row.contains(ANSWER)),
                "INVARIANT: the streamed reply is visible above the indicator while it \
                 arrives\n{}",
                turn.report()
            );
        })
        .await;
}

/// (c) and (d) across a whole turn with a tool round: every stage that is in
/// progress shows exactly one indicator, no painted frame ever shows two,
/// and the completed turn leaves none behind — on screen or on later ticks.
///
/// Failed before the fix at the continuation stages: once the tool round
/// finished, the screen showed only `Tools (1 call)` while the second
/// provider request waited and streamed, with no indicator of any kind
/// (stages `tool running` through `continuation streaming` had zero).
#[tokio::test]
async fn test_a_turn_has_exactly_one_in_progress_indicator_and_leaves_none_behind() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut turn = IndicatorTurn::start(2);
            turn.submit().await;
            turn.settle("the provider request to be sent", |turn| {
                turn.provider_calls_started() == 1
            })
            .await;
            let rows = turn.frame("waiting").await;
            the_indicator(&turn, "waiting", &rows);

            turn.release_provider_call();
            turn.send(StreamChunk::ToolCallComplete {
                id: "call-1".into(),
                name: TOOL_NAME.into(),
                input: serde_json::json!({}),
                provenance: provenance(),
            })
            .await;
            turn.end_stream();
            turn.settle("the tool to start", |turn| {
                turn.tool_started.load(Ordering::SeqCst) == 1
            })
            .await;
            turn.advance(Duration::from_secs(1));
            let rows = turn.frame("tool running").await;
            the_indicator(&turn, "tool running", &rows);
            assert!(
                rows.iter().any(|row| row.contains(TOOL_NAME)),
                "the tool row must be on screen while the tool runs\n{}",
                turn.report()
            );

            turn.tool_release.add_permits(1);
            turn.settle("the continuation request to be sent", |turn| {
                turn.provider_calls_started() == 2
            })
            .await;
            turn.advance(Duration::from_secs(1));
            let rows = turn.frame("continuation waiting").await;
            let continuation = the_indicator(&turn, "continuation waiting", &rows);
            assert!(
                continuation.contains("2s"),
                "INVARIANT: the indicator keeps the turn's elapsed time across a tool round; \
                 got {continuation:?}\n{}",
                turn.report()
            );

            turn.release_provider_call();
            turn.send(StreamChunk::TextDelta(format!("(say \"{ANSWER}\")")))
                .await;
            turn.settle("the continuation text to be counted", |turn| {
                turn.tokens() == 2
            })
            .await;
            turn.advance(Duration::from_millis(200));
            let rows = turn.frame("continuation streaming").await;
            the_indicator(&turn, "continuation streaming", &rows);

            turn.end_stream();
            turn.settle("the turn's terminal event", |turn| turn.terminal_seen)
                .await;
            let rows = turn.frame("complete").await;
            assert!(
                rows.iter().any(|row| row.contains(ANSWER)),
                "the completed turn's answer must be on screen\n{}",
                turn.report()
            );
            assert_eq!(
                indicator_rows(&rows),
                Vec::<String>::new(),
                "INVARIANT: a completed turn leaves no in-progress indicator on screen\n{}",
                turn.report()
            );

            // Time moving on after the turn ended must not repaint anything:
            // a finished turn has no clock on screen.
            let painted_at_completion = turn.painted();
            turn.advance(Duration::from_secs(5));
            let rows = turn.frame("complete, 5 s later").await;
            assert_eq!(
                (turn.painted(), indicator_rows(&rows)),
                (painted_at_completion, Vec::new()),
                "INVARIANT: after the turn completes no tick repaints and no indicator \
                 returns\n{}",
                turn.report()
            );

            for (index, rows) in turn.probe.painted_frames().iter().enumerate() {
                let found = indicator_rows(rows);
                assert!(
                    found.len() <= 1,
                    "INVARIANT: no frame of a turn contains two in-progress indicators; \
                     painted frame {index} had {found:?}\n{}",
                    turn.report()
                );
            }
        })
        .await;
}
