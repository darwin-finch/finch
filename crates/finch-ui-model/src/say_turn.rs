//! Pure WorkUnit say-turn projection (stage 2 of docs/TUI_DESIGN.md, #882).
//!
//! One say turn renders as **one** representation per state — never a card
//! stacked beside a legacy source group:
//!
//! - **Generating** (model working, no program yet): one animated progress
//!   line in the spinner style, showing the model is producing the response.
//! - **Running** (program exists, executing): the program source inline —
//!   `(say "Hi, Shammah! …")` — no chrome row, no card, no glyph. Output
//!   bytes that have already arrived (streaming `say` chunks, wire-error
//!   diagnostics, transient status) render beneath the source; hiding arrived
//!   say bytes is the pre-#350 defect class, so they are never suppressed.
//! - **Completed**: the output prose inline, followed by a labelled
//!   `Show program` / `Hide program` control carrying the `(ran Ns)`
//!   annotation. No Program source row, Brain run row, result row, or card
//!   chrome. The answer remains visible while `show_program` reveals the
//!   exact source beneath it.
//!
//! The ViewModel (status, program, output, `show_program`) lives on the
//! message behind its own lock; subwidgets are constructed
//! from it each frame and choose to render or not, so a subwidget with
//! nothing to show contributes zero lines and claims zero rows. The engine
//! never matches on the message type: it asks the `Message` trait for
//! [`SayTurnView`] and hands the snapshot here; clicks on the labelled control
//! resolve to `(RowId, action)` and route to the component's handle, which
//! toggles `show_program` under the message's lock. Repaints stay
//! pull-per-frame — the next frame re-renders from the mutated ViewModel.
//!
//! The stage-1 chrome (`chrome_line`, status glyph, the `[0]` disclosure
//! hitbox) is deleted. The completed turn's explicit labelled control owns
//! the stable `[1]` target; answer and source text are never click targets.

use crate::{MessageId, NodeRole, RenderedTranscriptLine, RowId};

/// Status of a component-owned say turn. The completion path transitions it
/// exactly once; a finished turn therefore cannot keep wearing `running`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SayTurnStatus {
    #[default]
    Running,
    Completed,
}

/// The program-source part of a say turn's ViewModel: the exact wire text the
/// provider produced, retained so the reader can reveal it on demand.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgramSourceVm {
    pub language: String,
    pub lines: Vec<String>,
}

/// The output part of a say turn's ViewModel, set when the program produces
/// output and updated live as `say` chunks stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputVm {
    pub lines: Vec<String>,
}

/// The retained ViewModel of one say turn, living on the WorkUnit behind the
/// message's existing lock. Holds presentation state (status, program,
/// output) and the ephemeral UI state (`show_program`, default hidden for say
/// turns — #350's prose ruling). Because it is retained, component state needs
/// no renderer-side map.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkUnitViewModel {
    pub status: SayTurnStatus,
    pub program: ProgramSourceVm,
    pub output: Option<OutputVm>,
    pub show_program: bool,
}

/// One frame's component snapshot: the retained ViewModel plus the chrome
/// timing, captured under the same lock read. The full-resolution elapsed
/// drives the component's animated generating state; completed turns read the
/// captured value, so the annotation is stable for scrollback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SayTurnView {
    pub message_id: MessageId,
    pub vm: WorkUnitViewModel,
    pub elapsed: std::time::Duration,
}

/// Semantic path of the say turn's program-disclosure control. The chrome's
/// `[0]` retired in stage 2 and is never reused.
pub(crate) const PROGRAM_CONTROL_PATH: &[u32] = &[1];

/// Braille spinner frames for the animated generating state; the blit tick
/// re-snapshots every frame, so sub-second elapsed animates the indicator.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Spinner rotation speed in milliseconds per frame.
const SPINNER_TICK_MS: u64 = 80;

/// Which single representation the turn renders this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SayTurnState {
    Generating,
    Running,
    Completed,
}

fn say_state(vm: &WorkUnitViewModel) -> SayTurnState {
    if vm.status == SayTurnStatus::Completed {
        // Status transitions win: a completed turn renders its output
        // (however empty, #1185) and can never wear the spinner again.
        SayTurnState::Completed
    } else if vm.program.lines.is_empty() {
        // No producer reaches a say turn before its program is known today
        // (`begin_say_turn` carries the source); the state stays total for a
        // future producer that creates the card during model generation.
        SayTurnState::Generating
    } else {
        SayTurnState::Running
    }
}

fn spinner_frame(elapsed: std::time::Duration) -> &'static str {
    SPINNER_FRAMES
        [(elapsed.as_millis() / u128::from(SPINNER_TICK_MS)) as usize % SPINNER_FRAMES.len()]
}

fn fmt_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

fn body_line(text: String) -> RenderedTranscriptLine {
    RenderedTranscriptLine {
        text,
        spans: Vec::new(),
        row_id: None,
        row_expanded: None,
        role: Some(NodeRole::Output),
        body_of: None,
        component_owned: false,
    }
}

fn program_line(text: String) -> RenderedTranscriptLine {
    RenderedTranscriptLine {
        role: Some(NodeRole::Program),
        ..body_line(text)
    }
}

/// A completed turn's labelled toggle line. `row_expanded` carries the same
/// disclosure state as its visible Show/Hide label for assistive consumers.
fn toggle_line(text: String, target: &RowId, show_program: bool) -> RenderedTranscriptLine {
    RenderedTranscriptLine {
        text,
        spans: Vec::new(),
        row_id: Some(target.clone()),
        row_expanded: Some(show_program),
        role: Some(NodeRole::Output),
        body_of: None,
        component_owned: true,
    }
}

fn program_control(view: &SayTurnView) -> RowId {
    RowId {
        message_id: view.message_id,
        path: PROGRAM_CONTROL_PATH.to_vec(),
    }
}

/// The animated generating line: spinner frame + phase + elapsed. No source
/// exists yet, so none renders and nothing is a hit target.
fn generating_lines(view: &SayTurnView) -> Vec<RenderedTranscriptLine> {
    vec![body_line(format!(
        "{} Generating… ({})",
        spinner_frame(view.elapsed),
        fmt_elapsed(view.elapsed.as_secs())
    ))]
}

/// `ProgramSource`: the exact wire text the turn ran, inline while it
/// executes and beneath the answer while a completed turn is toggled.
/// Constructed from the outer ViewModel each frame; with nothing to show it
/// renders nothing and claims zero rows.
pub(crate) struct ProgramSource<'a> {
    lines: &'a [String],
    shown: bool,
}

impl<'a> ProgramSource<'a> {
    pub(crate) fn from_vm(vm: &'a WorkUnitViewModel) -> Self {
        Self {
            lines: &vm.program.lines,
            shown: vm.show_program,
        }
    }

    /// Render for the Running state: the source is the representation.
    pub(crate) fn render_inline(&self) -> Vec<String> {
        self.lines.to_vec()
    }

    /// Render for a toggled completed turn: only while `show_program`.
    pub(crate) fn render_toggled(&self) -> Vec<String> {
        if !self.shown {
            return Vec::new();
        }
        self.render_inline()
    }
}

/// `Output`: what the program said. Constructed from the outer ViewModel
/// each frame; an absent or still-empty output renders nothing.
pub(crate) struct Output<'a> {
    lines: &'a [String],
}

impl<'a> Output<'a> {
    pub(crate) fn from_vm(output: &'a OutputVm) -> Self {
        Self {
            lines: &output.lines,
        }
    }

    pub(crate) fn render(&self) -> Vec<String> {
        self.lines.to_vec()
    }
}

/// Running: the program source inline; arrived output bytes render beneath
/// it and are never hidden.
fn running_lines(view: &SayTurnView) -> Vec<RenderedTranscriptLine> {
    let program = ProgramSource::from_vm(&view.vm);
    let mut lines: Vec<RenderedTranscriptLine> =
        program.render_inline().into_iter().map(body_line).collect();
    if let Some(output) = &view.vm.output {
        lines.extend(Output::from_vm(output).render().into_iter().map(body_line));
    }
    lines
}

/// Completed: output prose remains visible, followed immediately by the
/// labelled program-disclosure control and, while open, the exact source.
/// Only the control is interactive. `Show program` and `Hide program` have
/// equal display width, so its wrapped hit region is stable on narrow
/// terminals. Even an empty program keeps the control (#1185).
fn completed_lines(view: &SayTurnView) -> Vec<RenderedTranscriptLine> {
    let target = program_control(view);
    let mut lines: Vec<RenderedTranscriptLine> = view
        .vm
        .output
        .as_ref()
        .map(|output| Output::from_vm(output).render())
        .unwrap_or_default()
        .into_iter()
        .map(body_line)
        .collect();
    let label = if view.vm.show_program {
        "Hide program"
    } else {
        "Show program"
    };
    lines.push(toggle_line(
        format!("{label} · (ran {})", fmt_elapsed(view.elapsed.as_secs())),
        &target,
        view.vm.show_program,
    ));
    lines.extend(
        ProgramSource::from_vm(&view.vm)
            .render_toggled()
            .into_iter()
            .map(program_line),
    );
    lines
}

/// Render the say turn's lines for one frame: exactly one representation for
/// the turn's current state. The transcript viewport's claiming pass turns
/// the completed labelled control into one stable toggle hitbox; a hidden
/// subwidget claims nothing.
pub fn say_turn_lines(view: &SayTurnView) -> Vec<RenderedTranscriptLine> {
    match say_state(&view.vm) {
        SayTurnState::Generating => generating_lines(view),
        SayTurnState::Running => running_lines(view),
        SayTurnState::Completed => completed_lines(view),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Axis, Rect, Track, Widget};

    fn say_view(vm: WorkUnitViewModel) -> SayTurnView {
        SayTurnView {
            message_id: MessageId::new(),
            vm,
            elapsed: std::time::Duration::from_millis(2350),
        }
    }

    fn running_vm() -> WorkUnitViewModel {
        WorkUnitViewModel {
            status: SayTurnStatus::Running,
            program: ProgramSourceVm {
                language: "Co-Forth".into(),
                lines: vec!["(say \"hello\")".to_string()],
            },
            output: None,
            show_program: false,
        }
    }

    fn completed_vm() -> WorkUnitViewModel {
        WorkUnitViewModel {
            status: SayTurnStatus::Completed,
            program: ProgramSourceVm {
                language: "Co-Forth".into(),
                lines: vec!["(say \"hello\")".to_string()],
            },
            output: Some(OutputVm {
                lines: vec!["hello".to_string()],
            }),
            show_program: false,
        }
    }

    fn texts(lines: &[RenderedTranscriptLine]) -> Vec<String> {
        lines.iter().map(|line| line.text.clone()).collect()
    }

    fn viewport_layout(lines: Vec<RenderedTranscriptLine>) -> crate::Layout {
        crate::layout(
            &Widget::Viewport { lines },
            Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 40,
            },
        )
    }

    fn hit_rect_count(lines: Vec<RenderedTranscriptLine>) -> usize {
        viewport_layout(lines).hit_rects().count()
    }

    // ── Generating ──────────────────────────────────────────────────────────

    #[test]
    fn test_generating_state_renders_an_animated_line_and_no_source() {
        // INVARIANT (stage 2): while the model is producing the response and
        // no program exists yet, the turn is one animated progress line —
        // never the source (there is none) and never chrome furniture.
        let view = say_view(WorkUnitViewModel::default());
        let lines = say_turn_lines(&view);
        let rendered = texts(&lines);
        assert_eq!(
            rendered.len(),
            1,
            "generating renders exactly one animated line; got {rendered:?}"
        );
        assert!(
            rendered[0].contains("Generating…") && rendered[0].contains("(2s)"),
            "the animated line names the phase and the elapsed time; got {rendered:?}"
        );
        assert!(
            SPINNER_FRAMES.contains(&rendered[0].split(' ').next().unwrap_or_default()),
            "the line leads with a spinner frame; got {:?}",
            rendered[0]
        );
        assert_eq!(
            hit_rect_count(lines),
            0,
            "nothing is toggleable while generating"
        );
    }

    #[test]
    fn test_generating_spinner_frame_advances_with_sub_second_elapsed() {
        // The indicator animates: the frame is a pure function of elapsed
        // time, so successive blit frames rotate it without any observer.
        let early = spinner_frame(std::time::Duration::from_millis(0));
        let later = spinner_frame(std::time::Duration::from_millis(SPINNER_TICK_MS * 3));
        assert_ne!(
            early, later,
            "the spinner frame must advance with elapsed time"
        );
        assert_eq!(
            spinner_frame(std::time::Duration::from_millis(SPINNER_TICK_MS * 3)),
            spinner_frame(std::time::Duration::from_millis(SPINNER_TICK_MS * 3 + 17)),
            "frames are stable within one tick"
        );
    }

    // ── Running ─────────────────────────────────────────────────────────────

    #[test]
    fn test_running_state_renders_the_source_inline_with_no_chrome_or_legacy_rows() {
        // INVARIANT (stage 2, the maintainer's verbatim target): a running say
        // turn IS its program source, inline — no chrome row, no glyph, no
        // card, no elapsed-on-chrome, no legacy `Program source` label.
        let view = say_view(running_vm());
        let lines = say_turn_lines(&view);
        let rendered = texts(&lines);
        assert_eq!(
            rendered,
            vec!["(say \"hello\")"],
            "the running turn renders only its program source, flush-left; got {rendered:?}"
        );
        for line in &rendered {
            assert!(
                !line.contains('\u{25cb}')
                    && !line.contains('\u{23fa}')
                    && !line.contains('\u{25b6}')
                    && !line.contains('\u{25bc}')
                    && !line.contains("(ran")
                    && !line.contains("Program source"),
                "the running state must not carry chrome, a glyph, an arrow, or a legacy \
                 label; got {line:?}"
            );
        }
        assert_eq!(
            hit_rect_count(say_turn_lines(&view)),
            0,
            "nothing is toggleable while the program is still executing"
        );
    }

    #[test]
    fn test_running_never_hides_output_bytes_that_have_already_arrived() {
        // INVARIANT (#350 class): say chunks that streamed in while the
        // program is still executing render beneath the source. Hiding them
        // made chunks appear and then vanish during longer programs.
        let mut vm = running_vm();
        vm.output = Some(OutputVm {
            lines: vec!["partial greeting".to_string()],
        });
        let view = say_view(vm);
        let lines = say_turn_lines(&view);
        let rendered = texts(&lines);
        assert_eq!(
            rendered,
            vec!["(say \"hello\")", "partial greeting"],
            "source inline, arrived output beneath, nothing hidden; got {rendered:?}"
        );
    }

    // ── Completed ───────────────────────────────────────────────────────────

    #[test]
    fn test_completed_state_renders_prose_then_the_labelled_control_and_no_legacy_rows() {
        // INVARIANT (stage 2, the maintainer's verbatim target): the completed
        // say turn is the prose inline plus `(ran Ns)` — no Program source
        // row, no Brain run row, no UUID, no result row, no card chrome.
        let view = say_view(completed_vm());
        let lines = say_turn_lines(&view);
        let rendered = texts(&lines);
        assert_eq!(
            rendered,
            vec!["hello", "Show program · (ran 2s)"],
            "completed renders prose followed immediately by an explicit labelled control \
             carrying the elapsed annotation; got {rendered:?}"
        );
        for line in &rendered {
            assert!(
                !line.contains("Program source")
                    && !line.contains("Brain run")
                    && !line.contains("(say \"hello\")")
                    && !line.contains('\u{23fa}')
                    && !line.contains('\u{25b6}'),
                "the completed state must not render the legacy source group or chrome; \
                 got {line:?}"
            );
        }
    }

    #[test]
    fn test_completed_elapsed_annotation_always_renders_and_reads_aloud() {
        // `(ran 0s)` is the annotation in the maintainer's spec — it renders
        // even for a same-second turn, and long turns stay readable.
        let mut vm = completed_vm();
        vm.output = Some(OutputVm {
            lines: vec!["hi".to_string()],
        });
        let quick = say_view(vm.clone());
        assert!(
            texts(&say_turn_lines(&quick))
                .last()
                .is_some_and(|line| *line == "Show program · (ran 2s)"),
            "the annotation always renders on the labelled control; got {:?}",
            texts(&say_turn_lines(&quick))
        );
        let long = SayTurnView {
            elapsed: std::time::Duration::from_secs(75),
            ..say_view(vm)
        };
        assert!(
            texts(&say_turn_lines(&long))
                .last()
                .is_some_and(|line| *line == "Show program · (ran 1m 15s)"),
            "minutes render readably; got {:?}",
            texts(&say_turn_lines(&long))
        );
    }

    #[test]
    fn test_completed_turn_with_empty_program_does_not_stay_generating_forever() {
        // INVARIANT (#1185): a wire turn whose raw source trims to "" begins
        // a say turn with an empty program and still completes. Status
        // transitions win over the empty-program heuristic: the completed
        // turn renders its (empty) output region plus the `(ran Ns)`
        // annotation, never the generating spinner. A still-running turn
        // with no program yet keeps the generating representation.
        let running = say_view(WorkUnitViewModel::default());
        let running_rendered = texts(&say_turn_lines(&running));
        assert!(
            running_rendered[0].contains("Generating…"),
            "a running turn with no program yet still renders the animated \
             generating line; got {running_rendered:?}"
        );

        let completed = say_view(WorkUnitViewModel {
            status: SayTurnStatus::Completed,
            program: ProgramSourceVm {
                language: "forth".into(),
                lines: Vec::new(),
            },
            output: None,
            show_program: false,
        });
        let lines = say_turn_lines(&completed);
        let rendered = texts(&lines);
        assert!(
            !rendered.iter().any(|line| line.contains("Generating")),
            "a completed turn must not wear the generating spinner regardless \
             of its program being empty; got {rendered:?}"
        );
        assert_eq!(
            rendered,
            vec!["Show program · (ran 2s)"],
            "the completed empty turn renders its empty output region plus the \
             labelled elapsed control; got {rendered:?}"
        );
        assert_eq!(
            lines.iter().filter(|line| line.row_id.is_some()).count(),
            1,
            "only the labelled control is the toggle hit target; got {lines:?}"
        );
    }

    #[test]
    fn test_completed_turn_with_no_prose_still_has_its_annotation_and_target() {
        // A completed turn whose output never arrived (empty successful say)
        // still carries the annotation and a focusable toggle target.
        let mut vm = completed_vm();
        vm.output = None;
        let view = say_view(vm);
        let lines = say_turn_lines(&view);
        let rendered = texts(&lines);
        assert_eq!(
            rendered,
            vec!["Show program · (ran 2s)"],
            "got {rendered:?}"
        );
        assert_eq!(lines.iter().filter(|line| line.row_id.is_some()).count(), 1);
    }

    // ── Toggle ──────────────────────────────────────────────────────────────

    #[test]
    fn test_labelled_toggle_preserves_answer_and_reveals_exact_source_beneath_it() {
        // INVARIANT (#350): the answer is never the disclosure control and
        // never disappears. One labelled row owns the stable action path.
        let view = say_view(completed_vm());
        let lines = say_turn_lines(&view);
        let target = program_control(&view);
        assert!(
            lines.first().is_some_and(|line| line.row_id.is_none()),
            "the answer stays ordinary readable prose, not a click target; got {lines:?}"
        );
        assert!(
            lines.iter().filter(|line| line.component_owned).count() == 1,
            "exactly one labelled control owns component routing; got {lines:?}"
        );
        assert!(
            lines.last().is_some_and(|line| {
                line.row_id.as_ref() == Some(&target)
                    && line.row_expanded == Some(false)
                    && line.text == "Show program · (ran 2s)"
            }),
            "the labelled control exposes its closed structured state; got {lines:?}"
        );
        assert_eq!(
            hit_rect_count(say_turn_lines(&view)),
            1,
            "only the explicit Show program control is clickable"
        );

        // Toggle through the component handle path: answer persists, exact
        // source appears beneath it, and the same control reports open.
        let mut vm = completed_vm();
        vm.show_program = true;
        let toggled = say_view(vm);
        let toggled_target = program_control(&toggled);
        let toggled_lines = say_turn_lines(&toggled);
        let rendered = texts(&toggled_lines);
        assert_eq!(
            rendered,
            vec!["hello", "Hide program · (ran 2s)", "(say \"hello\")"],
            "toggled on, the answer stays visible and exact program source appears beneath \
             the labelled inverse control; got {rendered:?}"
        );
        assert!(
            say_turn_lines(&toggled).iter().any(|line| {
                line.row_id.as_ref() == Some(&toggled_target)
                    && line.row_expanded == Some(true)
                    && line.text == "Hide program · (ran 2s)"
            }),
            "the same control reports the opened state and inverse label"
        );
        assert_eq!(
            hit_rect_count(say_turn_lines(&view)),
            hit_rect_count(say_turn_lines(&toggled)),
            "opening source must not add click targets or move focus identity"
        );
    }

    #[test]
    fn test_labelled_control_wraps_to_the_same_hit_region_in_both_states() {
        let closed = say_view(completed_vm());
        let mut open_vm = completed_vm();
        open_vm.show_program = true;
        let open = say_view(open_vm);
        let layout_at = |view: &SayTurnView| {
            crate::layout(
                &Widget::Viewport {
                    lines: say_turn_lines(view),
                },
                Rect {
                    x: 0,
                    y: 0,
                    width: 12,
                    height: 40,
                },
            )
            .hit_rects()
            .map(|(_, rect)| rect)
            .collect::<Vec<_>>()
        };
        assert_eq!(
            layout_at(&closed),
            layout_at(&open),
            "Show/Hide controls have equal display width, so narrow wrapping keeps the \
             same focusable hit region while source is revealed below"
        );
    }

    #[test]
    fn test_program_control_path_is_derived_and_never_reuses_the_retired_chrome_path() {
        let view = say_view(completed_vm());
        let target = program_control(&view);
        assert_eq!(
            target.path, PROGRAM_CONTROL_PATH,
            "the target's path is the declared PROGRAM_CONTROL_PATH"
        );
        assert_ne!(
            target.path,
            vec![0],
            "the chrome's [0] path retired with the chrome; the output region never \
             reuses a path segment"
        );
    }

    // ── Subwidgets ──────────────────────────────────────────────────────────

    #[test]
    fn test_hidden_subwidget_claims_zero_rows_and_stays_constructible() {
        // INVARIANT (#882): a subwidget with nothing to show claims zero rows
        // and stays in the tree — constructed from the outer VM each frame.
        let vm = running_vm();
        let hidden = ProgramSource::from_vm(&vm);
        assert!(
            hidden.render_toggled().is_empty(),
            "show_program=false renders no program lines behind the toggle"
        );
        let shown_vm = WorkUnitViewModel {
            show_program: true,
            ..vm.clone()
        };
        assert_eq!(
            ProgramSource::from_vm(&shown_vm).render_toggled(),
            vm.program.lines,
            "show_program=true renders the exact wire text"
        );
        assert_eq!(
            ProgramSource::from_vm(&shown_vm).render_inline(),
            vm.program.lines,
            "the running representation is the source regardless of the toggle"
        );
    }

    #[test]
    fn test_output_subwidget_renders_only_when_the_output_part_is_set() {
        let view = say_view(running_vm());
        assert!(view.vm.output.is_none());
        assert_eq!(
            texts(&say_turn_lines(&view)),
            vec!["(say \"hello\")"],
            "no output part means no output lines beneath the source"
        );
    }

    #[test]
    fn test_streaming_vm_updates_render_on_the_next_frame() {
        // Streaming lands under the message's lock; every frame repaints from
        // the current VM. The component is a pure function of the snapshot,
        // so a growing output part grows the completed card.
        let mut vm = completed_vm();
        vm.output = Some(OutputVm {
            lines: vec!["hel".to_string()],
        });
        let partial_view = say_view(vm.clone());
        let partial = texts(&say_turn_lines(&partial_view));
        vm.output = Some(OutputVm {
            lines: vec!["hel".to_string(), "lo".to_string()],
        });
        let streamed_view = say_view(vm);
        let streamed = texts(&say_turn_lines(&streamed_view));
        assert_eq!(partial.len() + 1, streamed.len());
        assert!(
            streamed.iter().any(|line| *line == "lo"),
            "the streamed chunk renders; got {streamed:?}"
        );
    }

    #[test]
    fn test_completion_transitions_exactly_once_in_the_view_model() {
        // The completion path owns the transition; the component renders
        // whatever the VM says. Re-running completion re-renders identically.
        let mut vm = running_vm();
        let running_view = say_view(vm.clone());
        let running = say_turn_lines(&running_view);
        vm.status = SayTurnStatus::Completed;
        vm.output = Some(OutputVm {
            lines: vec!["hello".to_string()],
        });
        let completed_view = say_view(vm.clone());
        let completed = say_turn_lines(&completed_view);
        vm.status = SayTurnStatus::Completed;
        assert_ne!(
            texts(&running),
            texts(&completed),
            "the completed representation differs from the running one"
        );
        assert_eq!(
            texts(&say_turn_lines(&completed_view)),
            texts(&completed),
            "an already-completed status re-renders identically — the transition \
             happened once"
        );
    }

    #[test]
    fn test_the_completed_card_participates_in_a_claiming_frame_as_a_subtree() {
        // The engine's claiming pass offers a box; the completed card claims
        // prose + labelled control rows inside it.
        let view = say_view(completed_vm());
        const CARD: u16 = 7;
        let tree = Widget::Stack {
            axis: Axis::Column,
            children: vec![(
                Track::Natural,
                Widget::Marked(
                    CARD,
                    Box::new(Widget::Text {
                        lines: say_turn_lines(&view)
                            .into_iter()
                            .map(|line| line.text)
                            .collect(),
                    }),
                ),
            )],
        };
        let layout = crate::layout(
            &tree,
            Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 20,
            },
        );
        let rect = layout.keyed(CARD).expect("the card claims a rect");
        assert_eq!(
            rect.height, 2,
            "prose + labelled control claim two rows; got {rect:?}"
        );
    }
}
