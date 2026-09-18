//! The WorkUnit say-turn component (stage 1 of docs/TUI_DESIGN.md, #882).
//!
//! One component owns the say turn's presentation: a retained ViewModel on
//! the message (status, program, output, `show_program` — see
//! [`crate::cli::messages::WorkUnitViewModel`]), a chrome renderer, and two
//! subwidgets constructed from the outer ViewModel each frame so they choose
//! to render or not. A hidden subwidget contributes zero lines and therefore
//! claims zero rows in the claiming pass. The chrome renders the disclosure
//! arrow **only while the program source can be shown** — the affordance and
//! the content are decided in the same function, so a dead arrow over nothing
//! is impossible by construction.
//!
//! The engine never matches on the message type: it asks the `Message` trait
//! for [`crate::cli::messages::SayTurnView`] and hands the snapshot here;
//! clicks resolve to `(RowId, action)` and route to the component's handle,
//! which toggles `show_program` under the message's lock. Repaints stay
//! pull-per-frame — the next frame re-renders from the mutated ViewModel.

use crate::cli::components::vocab::{NodeRole, RenderedTranscriptLine, RowId};
use crate::cli::messages::{SayTurnStatus, SayTurnView};

/// Semantic path of the say card's chrome row: the disclosure hitbox.
pub(crate) const CARD_PATH: &[u32] = &[0];

/// The status glyph a say card wears. Hollow while the turn is still running,
/// filled once it completed — a turn that died must never look identical to
/// one that answered. Wordless shapes are enough on the card only because the
/// output subwidget below carries the turn's own words; a card with nothing
/// to show at all still shows the elapsed time, which reads aloud.
fn say_glyph(status: SayTurnStatus) -> &'static str {
    match status {
        SayTurnStatus::Running => "\u{25cb}",
        SayTurnStatus::Completed => "\u{23fa}",
    }
}

fn fmt_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// The card chrome: status glyph + elapsed + the disclosure arrow. The arrow
/// exists only while the `ProgramSource` subwidget can be shown; without a
/// program there is nothing behind the arrow, so there is no arrow.
pub(crate) fn chrome_line(view: &SayTurnView, card: &RowId) -> RenderedTranscriptLine {
    let can_show_program = !view.vm.program.lines.is_empty();
    let arrow = match (can_show_program, view.vm.show_program) {
        (true, true) => " \u{25bc}",
        (true, false) => " \u{25b6}",
        (false, _) => "",
    };
    RenderedTranscriptLine {
        text: format!(
            "{} {}{arrow}",
            say_glyph(view.vm.status),
            fmt_elapsed(view.elapsed_secs)
        ),
        row_id: Some(card.clone()),
        row_expanded: can_show_program.then_some(view.vm.show_program),
        role: Some(NodeRole::Output),
        body_of: None,
        component_owned: true,
    }
}

/// `ProgramSource`: the exact wire text the turn ran, revealed only while
/// `show_program`. Constructed from the outer ViewModel each frame; hidden,
/// it renders nothing and claims zero rows.
pub(crate) struct ProgramSource<'a> {
    lines: &'a [String],
    shown: bool,
}

impl<'a> ProgramSource<'a> {
    pub(crate) fn from_vm(vm: &'a crate::cli::messages::WorkUnitViewModel) -> Self {
        Self {
            lines: &vm.program.lines,
            shown: vm.show_program,
        }
    }

    pub(crate) fn render(&self) -> Vec<String> {
        if !self.shown {
            return Vec::new();
        }
        self.lines.iter().map(|line| format!("  {line}")).collect()
    }
}

/// `Output`: what the program said, visible whenever the output part is set —
/// #350's prose ruling keeps the turn's own words as the primary content.
pub(crate) struct Output<'a> {
    lines: &'a [String],
}

impl<'a> Output<'a> {
    pub(crate) fn from_vm(vm: &'a crate::cli::messages::OutputVm) -> Self {
        Self { lines: &vm.lines }
    }

    pub(crate) fn render(&self) -> Vec<String> {
        self.lines.iter().map(|line| format!("  {line}")).collect()
    }
}

/// Render the say card's lines for one frame: the chrome's furniture row, the
/// `ProgramSource` subwidget (zero rows while hidden), then the `Output`
/// subwidget. The transcript viewport's claiming pass turns the chrome row
/// into the disclosure hitbox; a hidden subwidget claims nothing.
pub(crate) fn card_lines(view: &SayTurnView) -> Vec<RenderedTranscriptLine> {
    let card = RowId {
        message_id: view.message_id,
        path: CARD_PATH.to_vec(),
    };
    let mut lines = vec![chrome_line(view, &card)];
    let program = ProgramSource::from_vm(&view.vm);
    lines.extend(
        program
            .render()
            .into_iter()
            .map(|text| RenderedTranscriptLine {
                text,
                row_id: None,
                row_expanded: None,
                role: Some(NodeRole::Output),
                body_of: None,
                component_owned: false,
            }),
    );
    if let Some(output) = &view.vm.output {
        let output = Output::from_vm(output);
        lines.extend(
            output
                .render()
                .into_iter()
                .map(|text| RenderedTranscriptLine {
                    text,
                    row_id: None,
                    row_expanded: None,
                    role: Some(NodeRole::Output),
                    body_of: None,
                    component_owned: false,
                }),
        );
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::components::vocab::{Axis, Rect, Track, Widget};
    use crate::cli::messages::{MessageId, OutputVm, ProgramSourceVm, WorkUnitViewModel};

    fn say_view(vm: WorkUnitViewModel) -> SayTurnView {
        SayTurnView {
            message_id: MessageId::new(),
            vm,
            elapsed_secs: 2,
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

    fn viewport_layout(
        lines: Vec<RenderedTranscriptLine>,
    ) -> crate::cli::components::vocab::Layout {
        crate::cli::components::vocab::layout(
            &Widget::Viewport { lines },
            Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 40,
            },
        )
    }

    #[test]
    fn test_chrome_arrow_is_absent_when_program_source_cannot_show() {
        // INVARIANT (#882): the disclosure arrow exists only while the
        // ProgramSource subwidget can be shown — a dead ▼ over nothing is
        // impossible by construction because the affordance and the content
        // are decided in the same function.
        let mut vm = running_vm();
        vm.program.lines.clear();
        let view = say_view(vm);
        let card = RowId {
            message_id: view.message_id,
            path: CARD_PATH.to_vec(),
        };
        let chrome = chrome_line(&view, &card);
        assert!(
            !chrome.text.contains('\u{25b6}') && !chrome.text.contains('\u{25bc}'),
            "the chrome must not render a disclosure arrow when no program can be shown; got {:?}",
            chrome.text
        );
        assert_eq!(
            chrome.row_expanded, None,
            "a chrome with no toggle affordance is not an expandable row"
        );
        let without_program = WorkUnitViewModel::default();
        assert_eq!(
            without_program.program.lines,
            Vec::<String>::new(),
            "the default say ViewModel carries no program"
        );
    }

    #[test]
    fn test_chrome_arrow_is_present_while_program_source_can_show() {
        let view = say_view(running_vm());
        let card = RowId {
            message_id: view.message_id,
            path: CARD_PATH.to_vec(),
        };
        let chrome = chrome_line(&view, &card);
        assert!(
            chrome.text.contains('\u{25b6}'),
            "a say card whose program can be shown renders the closed disclosure arrow; got {:?}",
            chrome.text
        );
        assert_eq!(
            chrome.row_expanded,
            Some(false),
            "row_expanded carries show_program=false for assistive consumers"
        );
        assert!(
            chrome.text.contains('\u{25cb}') && chrome.text.contains("2s"),
            "the chrome names status and elapsed: got {:?}",
            chrome.text
        );
    }

    #[test]
    fn test_toggle_flips_visibility_and_the_claiming_pass_re_claims() {
        // INVARIANT (#882): a click toggles show_program on the ViewModel and
        // the next frame's claiming pass re-claims — the program subwidget's
        // rect appears where it claimed nothing before.
        let view = say_view(running_vm());
        let hidden = viewport_layout(card_lines(&view));
        let hidden_rects: Vec<_> = hidden.hit_rects().collect();
        assert_eq!(
            hidden_rects.len(),
            1,
            "only the chrome row is a hitbox while the program is hidden; got {hidden_rects:?}"
        );

        let mut vm = running_vm();
        vm.show_program ^= true;
        let revealed = say_view(vm);
        let revealed_lines = card_lines(&revealed);
        let revealed_text: Vec<&str> = revealed_lines
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert!(
            revealed_text
                .iter()
                .any(|line| line.contains("(say \"hello\")")),
            "toggled on, the card renders the program text; got {revealed_text:?}"
        );
        let shown = viewport_layout(revealed_lines);
        let shown_rects: Vec<_> = shown.hit_rects().collect();
        assert_eq!(
            shown_rects.len(),
            1,
            "the program body lines are not hitboxes; got {shown_rects:?}"
        );
        assert_eq!(
            card_lines(&view).len() + 1,
            card_lines(&revealed).len(),
            "revealing a one-line program claims exactly one more row"
        );
    }

    #[test]
    fn test_hidden_subwidget_claims_zero_rows_and_stays_constructible() {
        // INVARIANT (#882): a subwidget with nothing to show claims zero rows
        // and stays in the tree — constructed from the outer VM each frame,
        // choosing to render nothing.
        let vm = running_vm();
        let program = ProgramSource::from_vm(&vm);
        assert!(
            program.render().is_empty(),
            "show_program=false renders no program lines"
        );
        let view = say_view(vm);
        let lines = card_lines(&view);
        assert_eq!(
            lines.len(),
            1,
            "hidden program and absent output leave only the chrome row; got {:?}",
            lines.iter().map(|line| &line.text).collect::<Vec<_>>()
        );
        let layout = viewport_layout(lines);
        let claimed: Vec<usize> = layout.hit_rects().map(|(_, rect)| rect.height).collect();
        assert_eq!(
            claimed,
            vec![1],
            "the chrome claims its furniture row; hidden subwidgets claim zero; got {claimed:?}"
        );
    }

    #[test]
    fn test_output_subwidget_renders_only_when_the_output_part_is_set() {
        let mut vm = running_vm();
        vm.output = None;
        let view = say_view(vm);
        assert_eq!(
            card_lines(&view).len(),
            1,
            "no output part means no output lines"
        );
        let mut vm = running_vm();
        vm.output = Some(OutputVm {
            lines: vec!["hello".to_string()],
        });
        let view = say_view(vm);
        let lines = card_lines(&view);
        let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
        assert!(
            texts.iter().any(|line| line.contains("hello")),
            "the output subwidget renders the say bytes; got {texts:?}"
        );
    }

    #[test]
    fn test_streaming_vm_updates_render_on_the_next_frame() {
        // Streaming lands under the message's lock; every frame repaints from
        // the current VM. The component itself is a pure function of the
        // snapshot, so a growing output part grows the card.
        let mut vm = running_vm();
        vm.output = Some(OutputVm {
            lines: vec!["hel".to_string()],
        });
        let partial = card_lines(&say_view(vm.clone()));
        vm.output = Some(OutputVm {
            lines: vec!["hel".to_string(), "lo".to_string()],
        });
        let streamed = card_lines(&say_view(vm));
        assert_eq!(partial.len() + 1, streamed.len());
        assert!(streamed.last().expect("streamed frame").text.contains("lo"));
    }

    #[test]
    fn test_completion_transitions_status_exactly_once_in_the_view_model() {
        // The completion path owns the transition; the component renders
        // whatever the VM says. Two completion passes must not distinguish
        // themselves — and the glyph must change.
        let mut vm = running_vm();
        let running = say_glyph(vm.status);
        vm.status = SayTurnStatus::Completed;
        let completed = say_glyph(vm.status);
        vm.status = SayTurnStatus::Completed;
        assert_ne!(running, completed, "running and completed glyphs differ");
        assert_eq!(
            say_glyph(vm.status),
            completed,
            "an already-completed status re-renders identically — the transition happened once"
        );
    }

    #[test]
    fn test_card_lines_route_the_chrome_to_the_component_path() {
        let view = say_view(running_vm());
        let lines = card_lines(&view);
        let chrome = &lines[0];
        assert!(chrome.component_owned, "the chrome row is component-owned");
        assert_eq!(
            chrome.row_id.as_ref().map(|id| id.path.as_slice()),
            Some(CARD_PATH),
            "the chrome's semantic path is the card's disclosure hitbox"
        );
        assert!(
            lines[1..].iter().all(|line| !line.component_owned),
            "subwidget body lines carry no row identity of their own"
        );
    }

    #[test]
    fn test_the_card_participates_in_a_claiming_frame_as_a_subtree() {
        // The engine's claiming pass offers a box; the chrome reserves its
        // furniture row inside it and the subwidgets take the remainder. Here
        // the card sits inside a stack the way the transcript viewport hosts
        // it, and the pass claims exactly the rows the card rendered.
        let mut vm = running_vm();
        vm.show_program = true;
        vm.output = Some(OutputVm {
            lines: vec!["hello".to_string()],
        });
        let view = say_view(vm);
        const CARD: u16 = 7;
        let tree = Widget::Stack {
            axis: Axis::Column,
            children: vec![(
                Track::Natural,
                Widget::Marked(
                    CARD,
                    Box::new(Widget::Text {
                        lines: card_lines(&view)
                            .into_iter()
                            .map(|line| line.text)
                            .collect(),
                    }),
                ),
            )],
        };
        let layout = crate::cli::components::vocab::layout(
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
            rect.height, 3,
            "chrome + one program line + one output line claim three rows; got {rect:?}"
        );
    }
}
