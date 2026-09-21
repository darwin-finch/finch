//! The blit-time ViewModel and the one domain → widget projection.
//!
//! Every blit, the live console converts domain state into one owned
//! [`LiveViewModel`] snapshot — composer draft and cursor, completion rows
//! (empty unless the draft is a slash command or mention), status lines, and
//! the live transcript — and [`project_root`] projects that snapshot into a
//! tree of standard [`widgets`](super::widgets). Widgets layout and paint the
//! snapshot; they never query `WorkUnit`, the command registry, or each other
//! to decide visibility (#805).
//!
//! Disclosure and focus are renderer state: the accordion's open set is keyed
//! by stable [`RowId`] (message id plus append-only semantic path), so
//! streamed appends, terminal reflow, and reconnects never lose a choice, and
//! completing a run cannot collapse a result by flipping a domain default.

#[cfg(test)]
use finch_messages::Message;
use finch_messages::MessageRef;
use finch_theme::ColorScheme;

use super::accordion::RenderedTranscriptLine;
use super::autocomplete_widget::{completion_pane_lines, AutocompleteState};
use super::widgets::{self, Axis, Rect, Track, Widget};

pub(crate) use finch_ui_model::{project_work_unit, NodeRole, RowId, TranscriptNode};

/// A message projected for the transcript: a WorkUnit run becomes a
/// [`TranscriptNode`]; any other message renders as its formatted lines.
pub(crate) enum ProjectedMessage {
    Node(TranscriptNode),
    Plain(Vec<String>),
}

/// Thin conversation adapter over the pure finch-ui-model projection.
pub(crate) fn project_message(message: &MessageRef, colors: &ColorScheme) -> ProjectedMessage {
    match message.work_unit_view(colors) {
        Some(view) => ProjectedMessage::Node(project_work_unit(&view)),
        None => ProjectedMessage::Plain(
            message
                .format(colors)
                .split('\n')
                .map(str::to_owned)
                .collect(),
        ),
    }
}

/// Test-only projection entry through the same message trait adapter.
#[cfg(test)]
pub(crate) fn try_project_for_test(
    message: &dyn Message,
    colors: &ColorScheme,
) -> Option<TranscriptNode> {
    message
        .work_unit_view(colors)
        .map(|view| project_work_unit(&view))
}

// ─── The live frame ViewModel ─────────────────────────────────────────────────

/// Keys the root tree's claimable regions so the layout result can hand each
/// one back by name.
pub(crate) mod frame_key {
    pub const TRANSCRIPT: u16 = 0;
    pub const COMPLETIONS: u16 = 1;
    pub const SEPARATOR: u16 = 2;
    pub const COMPOSER: u16 = 3;
    pub const STATUS_RULE: u16 = 4;
    pub const STATUS: u16 = 5;
    pub const DIALOG_CARD: u16 = 6;
    /// The session task list — furniture between the transcript viewport and
    /// the composer (#966). Zero rows when there is nothing to show.
    pub const TASKS: u16 = 7;
    /// The tracked child-agent rows — furniture alongside the session tasks.
    pub const TRACKED: u16 = 8;
}

/// The regions of one live frame, claimed by the widget tree. Rects are in the
/// live frame's own coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct FrameRects {
    pub transcript: Rect,
    /// `None` when the pane claims zero rows or a dialog owns the frame.
    pub completions: Option<Rect>,
    pub separator: Rect,
    pub composer: Rect,
    pub status_rule: Rect,
    pub status: Rect,
    /// The open dialog's inline card (#807). Empty when no dialog is open or
    /// the card was squeezed out of a tiny frame.
    pub dialog_card: Rect,
    /// The session task list's furniture claim (#966). Empty when there is
    /// nothing to show.
    pub tasks: Rect,
    /// The tracked child-agent rows' furniture claim. Empty when there is
    /// nothing to show.
    pub tracked: Rect,
}

/// Everything the live-frame planner reads: the blit-time ViewModel snapshot.
///
/// Gathering the renderer's state into one owned struct is the whole point: a
/// frame becomes computable, and therefore assertable, without a terminal.
/// `TuiRenderer::new` enables raw mode and installs a global panic hook, so no
/// test can construct a renderer — but any test can assemble this snapshot and
/// lay it out.
pub(crate) struct LiveViewModel<'a> {
    pub terminal_width: usize,
    pub terminal_height: usize,
    pub input_lines: &'a [String],
    pub input_cursor: (usize, usize),
    pub ghost_text: Option<&'a str>,
    pub effective_status: &'a str,
    pub cwd_label: &'a str,
    pub session_label: &'a str,
    pub model_identity: &'a str,
    pub dialog: Option<&'a super::Dialog>,
    /// Lines of the focused expanded tool-result surface (#656), pre-rendered
    /// by the caller so the planner stays a pure function of its inputs.
    pub expanded_lines: Option<&'a [String]>,
    /// A render error, like a dialog, owns the viewport and suppresses
    /// completions.
    pub render_error: bool,
    /// Polled session task list. A finished row claims no furniture row (the
    /// draw skips it), so the claim shrinks as the list completes.
    pub task_rows: &'a [super::ActivityRow],
    /// Child-agent rows, already ordered by depth then identity.
    pub tracked_rows: &'a [super::ActivityRow],
    pub live_rendered: &'a [RenderedTranscriptLine],
}

/// The transcript viewport and completion pane content for one frame, sized by
/// the claiming pass and painted after it.
pub(crate) struct LiveFrameContent {
    pub viewport: Vec<RenderedTranscriptLine>,
    pub completions: Vec<String>,
    /// The open dialog card's pinned lines, rendered to (and padded to) the
    /// claimed height. Empty when no dialog is open.
    pub dialog_card: Vec<String>,
}

/// The session task list's furniture lines: one bounded row per unfinished
/// task; a finished row claims nothing. Rendered from the ViewModel each
/// frame, so both claiming passes and the paint agree.
pub(crate) fn furniture_task_lines(vm: &LiveViewModel<'_>, width: usize) -> Vec<String> {
    vm.task_rows
        .iter()
        .filter_map(|row| super::session_task_line(row, width))
        .collect()
}

/// The tracked child-agent rows' furniture lines: one bounded row per tracked
/// agent, finished rows included (they render their `✓` state).
pub(crate) fn furniture_tracked_lines(vm: &LiveViewModel<'_>, width: usize) -> Vec<String> {
    vm.tracked_rows
        .iter()
        .map(|row| super::tracked_agent_line(row, width))
        .collect()
}

/// Project the root of the live frame: a single column whose chrome allocates
/// from the bottom — status, hr, input, hr, completions (0–N) — with the live
/// transcript viewport claiming the leftover (#805).
///
/// The session task list and the tracked child-agent rows are **furniture**
/// (#966): natural tracks between the transcript viewport and the completions
/// pane, always on the frame regardless of scroll position or transcript
/// length, and zero rows when there is nothing to show. The focused expanded
/// tool-result surface still owns the whole viewport and suppresses them, as
/// it suppresses the viewport itself.
///
/// When a dialog is open the card is an inline child of the same column
/// (#807): the conversation viewport stays projected above it, the card
/// claims its pinned extent below the separator, and the composer/status
/// chrome yields for the duration (the dialog owns the keys, as before). The
/// furniture keeps its place between the viewport and the separator so an
/// open dialog cannot hide it either. `dialog_card_natural` is the card's
/// pinned extent for the sizing pass; `content`, when given, is the pane,
/// viewport, and card content sized to the first pass's claims.
pub(crate) fn project_root(
    vm: &LiveViewModel<'_>,
    content: Option<&LiveFrameContent>,
    natural_completion_rows: usize,
    dialog_card_natural: usize,
) -> Widget {
    let width = vm.terminal_width.max(1);
    let separator = super::session_separator_line(width, vm.cwd_label, vm.session_label);
    let status_lines: Vec<String> = vm.effective_status.lines().map(str::to_owned).collect();
    let completion_rows = content.map_or(natural_completion_rows, |c| c.completions.len());
    let viewport_lines = content.map_or(Vec::new(), |c| c.viewport.clone());
    let completions_rows = content
        .map(|c| c.completions.clone())
        .unwrap_or_else(|| vec![String::new(); completion_rows]);
    let furniture_shown = vm.expanded_lines.is_none();
    let task_lines = furniture_shown
        .then(|| furniture_task_lines(vm, width))
        .unwrap_or_default();
    let tracked_lines = furniture_shown
        .then(|| furniture_tracked_lines(vm, width))
        .unwrap_or_default();
    let mut children: Vec<(Track, Widget)> = vec![(
        // While a dialog owns focus the card outranks the live transcript:
        // the floor yields so Yes/No keep their rows on small frames (#435).
        Track::Flex {
            weight: 1,
            min: usize::from(!vm.live_rendered.is_empty() && vm.dialog.is_none()),
        },
        Widget::Marked(
            frame_key::TRANSCRIPT,
            Box::new(Widget::Viewport {
                lines: viewport_lines,
            }),
        ),
    )];
    if vm.dialog.is_some() {
        // Paint order below is [conversation, tasks, tracked, separator, card],
        // so the tree carries the separator between the furniture and the
        // card: the card claims the frame's trailing rows, exactly where the
        // pinned lines are painted.
        children.push(furniture_track(frame_key::TASKS, task_lines));
        children.push(furniture_track(frame_key::TRACKED, tracked_lines));
        children.push((
            Track::Natural,
            Widget::Marked(
                frame_key::SEPARATOR,
                Box::new(Widget::Text {
                    lines: vec![separator],
                }),
            ),
        ));
        let card_lines = content.map_or(vec![String::new(); dialog_card_natural], |c| {
            c.dialog_card.clone()
        });
        children.push((
            Track::Natural,
            Widget::Marked(
                frame_key::DIALOG_CARD,
                Box::new(Widget::DialogCard { lines: card_lines }),
            ),
        ));
    } else {
        children.push(furniture_track(frame_key::TASKS, task_lines));
        children.push(furniture_track(frame_key::TRACKED, tracked_lines));
        children.push((
            Track::Natural,
            Widget::Marked(
                frame_key::COMPLETIONS,
                Box::new(Widget::Completions {
                    rows: completions_rows,
                }),
            ),
        ));
        children.push((
            Track::Natural,
            Widget::Marked(
                frame_key::SEPARATOR,
                Box::new(Widget::Text {
                    lines: vec![separator],
                }),
            ),
        ));
    }
    if vm.dialog.is_none() {
        children.extend([
            (
                Track::Natural,
                Widget::Marked(
                    frame_key::COMPOSER,
                    Box::new(Widget::Composer {
                        input_lines: vm.input_lines.to_vec(),
                        ghost: vm.ghost_text.map(str::to_owned),
                    }),
                ),
            ),
            (
                Track::Natural,
                Widget::Marked(frame_key::STATUS_RULE, Box::new(Widget::Rule)),
            ),
            (
                Track::Natural,
                Widget::Marked(
                    frame_key::STATUS,
                    Box::new(Widget::Text {
                        lines: status_lines,
                    }),
                ),
            ),
        ]);
    }
    Widget::Stack {
        axis: Axis::Column,
        children,
    }
}

/// One furniture track: the rendered rows at their natural extent — zero rows
/// when there is nothing to show (#966).
fn furniture_track(key: u16, lines: Vec<String>) -> (Track, Widget) {
    (
        Track::Natural,
        Widget::Marked(key, Box::new(Widget::Text { lines })),
    )
}

/// Run the claiming pass over the live frame and return the layout with the
/// marked regions' rects.
///
/// `natural_completion_rows` is the pane's unclamped extent (0 when a critical
/// surface suppresses it) and `dialog_card_natural` the open dialog card's
/// pinned extent (0 when no dialog is open); `content`, when given, is the
/// pane, viewport, and card content sized to the first pass's claims.
pub(crate) fn claim_live_frame(
    vm: &LiveViewModel<'_>,
    content: Option<&LiveFrameContent>,
    natural_completion_rows: usize,
    dialog_card_natural: usize,
) -> widgets::Layout {
    let frame = Rect {
        x: 0,
        y: 0,
        width: vm.terminal_width.max(1),
        height: vm.terminal_height,
    };
    widgets::layout(
        &project_root(vm, content, natural_completion_rows, dialog_card_natural),
        frame,
    )
}

/// The claimed regions of a layout, by name.
pub(crate) fn frame_rects(layout: &widgets::Layout) -> FrameRects {
    FrameRects {
        transcript: layout.keyed(frame_key::TRANSCRIPT).unwrap_or_default(),
        completions: layout
            .keyed(frame_key::COMPLETIONS)
            .filter(|rect| !rect.is_empty()),
        separator: layout.keyed(frame_key::SEPARATOR).unwrap_or_default(),
        composer: layout.keyed(frame_key::COMPOSER).unwrap_or_default(),
        status_rule: layout.keyed(frame_key::STATUS_RULE).unwrap_or_default(),
        status: layout.keyed(frame_key::STATUS).unwrap_or_default(),
        dialog_card: layout.keyed(frame_key::DIALOG_CARD).unwrap_or_default(),
        tasks: layout.keyed(frame_key::TASKS).unwrap_or_default(),
        tracked: layout.keyed(frame_key::TRACKED).unwrap_or_default(),
    }
}

/// The completions pane's natural (unclamped) extent, rendered so the
/// ephemeral selection window matches what a fully-afforded pane would paint.
pub(crate) fn natural_completion_pane(
    autocomplete: &mut AutocompleteState,
    width: usize,
    suppress: bool,
) -> Vec<String> {
    if suppress {
        // A zero budget clears the pane's rendered-row cache inside the
        // widget, keeping keyboard interactivity consistent with the claim.
        completion_pane_lines(autocomplete, width, 0);
        return Vec::new();
    }
    completion_pane_lines(autocomplete, width, usize::MAX)
}

/// The completion pane re-rendered to the claimed extent. A zero claim also
/// clears the widget's rendered-row cache so keyboard interactivity matches
/// what the pane painted.
pub(crate) fn completion_pane_for_claim(
    autocomplete: &mut AutocompleteState,
    width: usize,
    claimed_rows: usize,
    natural: &[String],
) -> Vec<String> {
    if claimed_rows == 0 {
        completion_pane_lines(autocomplete, width, 0);
        return Vec::new();
    }
    if claimed_rows >= natural.len() {
        return natural.to_vec();
    }
    completion_pane_lines(autocomplete, width, claimed_rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::messages::WorkUnit;
    use std::sync::Arc;

    fn colors() -> ColorScheme {
        ColorScheme::default()
    }

    /// Visible text of one rendered line: SGR and OSC sequences removed.
    fn strip_sgr(line: &str) -> String {
        let mut out = String::new();
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                match chars.peek() {
                    Some('[') => {
                        chars.next();
                        for nc in chars.by_ref() {
                            if nc.is_ascii_alphabetic() {
                                break;
                            }
                        }
                    }
                    _ => {
                        chars.next();
                    }
                }
                continue;
            }
            out.push(c);
        }
        out
    }

    fn assistant_unit(response: &str) -> WorkUnit {
        let unit = WorkUnit::new("Channeling");
        unit.set_response(response);
        unit.set_complete();
        unit
    }

    /// The rendered viewport body of one assistant response, through the real
    /// projection and disclosure path the live console draws with.
    fn rendered_viewport_body(response: &str) -> Vec<String> {
        let unit = assistant_unit(response);
        let node = try_project_for_test(&unit, &colors()).expect("assistant projects a node");
        super::super::accordion::AccordionState::default()
            .render_node(&node)
            .into_iter()
            .skip(1) // the disclosure header line
            .map(|line| line.text)
            .collect()
    }

    /// The viewport body line minus the row model's fixed two-space body
    /// indent — the presentation chrome every body line carries — so the
    /// remaining text can be compared byte-exact against the raw source.
    fn without_body_indent(line: &str) -> String {
        let plain = strip_sgr(line);
        plain.strip_prefix("  ").unwrap_or(&plain).to_owned()
    }

    #[test]
    fn test_assistant_fenced_code_block_renders_distinct_with_whitespace_exact_body() {
        // INVARIANT (#756): a fenced code block in assistant prose renders
        // visibly distinct in the viewport — dimmed fence bookends, body
        // whitespace byte-exact after SGR stripping — while the node also
        // carries the raw source lines for the canonical record.
        let response = "Use this:\n```rust\nfn main() {\n    let deep =    1;\n}\n```\nDone.\n";
        let unit = assistant_unit(response);
        let node = try_project_for_test(&unit, &colors()).expect("assistant projects a node");
        let raw = node.raw_body.as_ref().expect("markdown body carries raw");
        assert_eq!(
            raw,
            &[
                "Use this:",
                "```rust",
                "fn main() {",
                "    let deep =    1;",
                "}",
                "```",
                "Done.",
                "",
            ],
            "the raw body is the exact source the canonical commit must write; got {raw:?}"
        );
        let body: Vec<String> = rendered_viewport_body(response);
        let plain: Vec<String> = body.iter().map(|line| strip_sgr(line)).collect();
        assert_eq!(
            plain,
            &[
                "  Use this:",
                "  ```rust",
                "  fn main() {",
                "      let deep =    1;",
                "  }",
                "  ```",
                "  Done.",
                "  ",
            ],
            "stripped of SGR the viewport keeps every source line, whitespace-exact \
             (the row model's fixed two-space body indent included); got {body:?}"
        );
        assert_eq!(
            body.iter().filter(|line| line.contains("\x1b[2m")).count(),
            2,
            "the fence bookends are styled (dim SGR), which is what makes the block \
             visually distinct; got {body:?}"
        );
    }

    #[test]
    fn test_assistant_emphasis_and_lists_render_in_the_viewport() {
        let response = "**Important** run `finch test`:\n\n- first\n- second\n1. third\n";
        let body = rendered_viewport_body(response);
        let plain: Vec<String> = body.iter().map(|line| strip_sgr(line)).collect();
        assert_eq!(
            plain[0].trim(),
            "Important run finch test:",
            "emphasis markers are dropped and the words kept: {body:?}"
        );
        assert!(
            body[0].contains("\x1b[1m") && body[0].contains("\x1b[36m"),
            "bold and inline code carry their SGR attributes: {:?}",
            body[0]
        );
        assert_eq!(
            plain[2].trim(),
            "• first",
            "dash lists render as bullets: {body:?}"
        );
        assert_eq!(plain[3].trim(), "• second");
        assert_eq!(
            plain[4].trim(),
            "1. third",
            "ordered numbering is already semantic and stays: {body:?}"
        );
    }

    #[test]
    fn test_raw_and_no_color_see_the_same_text_semantics_as_the_rendered_viewport() {
        // INVARIANT (#756, accessibility authoritative): neither mode relies
        // on color to convey structure, and both are asserted on plain text,
        // never on SGR alone. Code blocks are text-identical in both modes
        // (fences kept, body whitespace-exact); emphasis is carried by SGR in
        // the rendered viewport and by its markers in the raw/no-color body.
        let response = "Before.\n```sh\necho one\n  echo two\n```\nAfter **bold**.\n";
        let body = rendered_viewport_body(response);
        let unit = assistant_unit(response);
        let node = try_project_for_test(&unit, &colors()).expect("assistant projects a node");
        let raw = node.raw_body.as_ref().expect("markdown body carries raw");
        let viewport_plain: Vec<String> = body
            .iter()
            .map(|line| without_body_indent(line))
            .filter(|line| !line.is_empty())
            .collect();
        // The code block is byte-identical text in both modes: fences kept,
        // body whitespace-exact, so no-color reading loses nothing.
        for source_line in ["```sh", "echo one", "  echo two", "```"] {
            assert!(
                raw.iter().any(|line| line == source_line),
                "INVARIANT: the raw/no-color body must keep the code block line \
                 {source_line:?} verbatim; raw was {raw:?}"
            );
            assert!(
                viewport_plain.iter().any(|line| line == source_line),
                "INVARIANT: the rendered viewport, stripped of SGR, must keep the code \
                 block line {source_line:?} byte-exact; viewport plain text was \
                 {viewport_plain:?}"
            );
        }
        // Emphasis: the marker is the no-color carrier in the raw body; the
        // rendered viewport carries it as SGR around the same words.
        assert!(
            raw.iter().any(|line| line.contains("**bold**")),
            "the raw body keeps the emphasis markers — the no-color carrier of \
             emphasis; raw was {raw:?}"
        );
        assert!(
            body.iter()
                .any(|line| line.contains("\x1b[1m") && strip_sgr(line).contains("After bold.")),
            "the rendered viewport bolds the same words the raw body marks: {body:?}"
        );
    }

    #[test]
    fn test_program_source_and_output_are_never_markdown_rendered() {
        // VM output and program source are portable side effects, not
        // assistant prose: their bodies must stay raw text with no markdown
        // rendering and no raw-body split.
        let source = WorkUnit::new("typed program");
        source.set_program_source("forth");
        source.set_response("```forth\n: greet ( -- )\n  .\" hi\" ;\n```");
        source.set_complete();
        let node = try_project_for_test(&source, &colors()).expect("program projects a node");
        assert_eq!(
            node.raw_body, None,
            "program source is not assistant prose and gains no markdown split"
        );
        assert!(
            node.body.iter().all(|line| !line.contains('\x1b')),
            "program source body carries no markdown SGR: {:?}",
            node.body
        );
        assert_eq!(
            node.body[0], "```forth",
            "fences stay literal in program source"
        );

        let output = WorkUnit::new("typed program");
        output.set_program_output();
        output.set_response("```forth\n: greet ( -- )\n  .\" hi\" ;\n```");
        output.set_complete();
        let node = try_project_for_test(&output, &colors()).expect("output projects a node");
        assert_eq!(
            node.raw_body, None,
            "VM output is never reflowed as markdown"
        );
        assert!(
            node.body.iter().all(|line| !line.contains('\x1b')),
            "program output body carries no markdown SGR: {:?}",
            node.body
        );
    }

    #[test]
    fn test_user_input_and_tool_output_are_not_reflowed() {
        // CONTROL (#756): markdown rendering is assistant prose only. A user
        // query (the Plain projection path) and a tool output row must carry
        // their text literally — no marker dropping, no bullet swaps, no SGR.
        let user: MessageRef = Arc::new(crate::cli::messages::UserQueryMessage::new(
            "show **bold** and - lists",
        ));
        assert!(
            matches!(
                project_message(&user, &colors()),
                ProjectedMessage::Plain(_)
            ),
            "user queries project on the Plain path, outside the markdown renderer"
        );
        let plain = match project_message(&user, &colors()) {
            ProjectedMessage::Plain(lines) => lines.join("\n"),
            ProjectedMessage::Node(_) => unreachable!("checked above"),
        };
        assert!(
            plain.contains("**bold**") && plain.contains("- lists"),
            "user input text is never reflowed: {plain:?}"
        );

        let unit = WorkUnit::new("Tools");
        let call = unit.add_row("bash(docs)");
        unit.complete_row_with_body(
            call,
            "",
            vec![
                "```python".to_string(),
                "print('x')".to_string(),
                "```".to_string(),
                "**literal**".to_string(),
                "- stays".to_string(),
            ],
        );
        unit.set_complete();
        let node = try_project_for_test(&unit, &colors()).expect("tool group projects");
        let output = &node.children[0].children[1];
        assert_eq!(
            output.role,
            NodeRole::ToolOutput,
            "the control must exercise a real tool output row; got {:?}",
            node.children[0]
        );
        assert_eq!(
            output.body,
            &["```python", "print('x')", "```", "**literal**", "- stays",],
            "tool output is byte-exact raw text, never markdown-rendered: {:?}",
            output.body
        );
        assert_eq!(
            output.raw_body, None,
            "tool output gains no markdown raw-body split"
        );
    }

    #[test]
    fn test_malformed_assistant_markdown_degrades_to_plain_text_without_panic() {
        let response = "dangling ** open * star\n```python\nprint('x')\n";
        let body = rendered_viewport_body(response);
        let plain: Vec<String> = body.iter().map(|line| strip_sgr(line)).collect();
        assert_eq!(
            plain[0].trim(),
            "dangling ** open * star",
            "unmatched delimiters stay literal text: {body:?}"
        );
        assert_eq!(plain[1].trim(), "```python");
        assert_eq!(plain[2].trim(), "print('x')");
        let unit = assistant_unit(response);
        let node = try_project_for_test(&unit, &colors()).expect("assistant projects a node");
        let raw = node
            .raw_body
            .as_ref()
            .expect("degraded body still carries raw");
        assert_eq!(
            raw,
            &["dangling ** open * star", "```python", "print('x')", "",],
            "the canonical raw body is the exact malformed source; got {raw:?}"
        );
    }
}
