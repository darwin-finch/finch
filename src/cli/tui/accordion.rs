//! Presentation-only disclosure state for retained transcript rows.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::cli::messages::{
    MessageRef, MessageStatus, TranscriptRow, TranscriptRowId, TranscriptRowKind,
};
use crate::config::ColorScheme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedTranscriptLine {
    pub text: String,
    pub row_id: Option<TranscriptRowId>,
    pub row_expanded: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptHitRegion {
    pub row_id: TranscriptRowId,
    pub top: u16,
    pub bottom: u16,
    pub left: u16,
    pub right: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisclosureStyle {
    Unicode,
    Text,
}

#[derive(Debug)]
pub struct AccordionState {
    expanded: HashMap<TranscriptRowId, bool>,
    pub focused: Option<TranscriptRowId>,
    pub hit_regions: Vec<TranscriptHitRegion>,
    visible_order: Vec<TranscriptRowId>,
    visible_expanded: HashMap<TranscriptRowId, bool>,
    disclosure_style: DisclosureStyle,
}

impl Default for AccordionState {
    fn default() -> Self {
        Self::with_disclosure_style(DisclosureStyle::Unicode)
    }
}

impl AccordionState {
    fn with_disclosure_style(disclosure_style: DisclosureStyle) -> Self {
        Self {
            expanded: HashMap::new(),
            focused: None,
            hit_regions: Vec::new(),
            visible_order: Vec::new(),
            visible_expanded: HashMap::new(),
            disclosure_style,
        }
    }

    /// Select textual disclosure states when the terminal cannot reliably
    /// present Unicode glyphs. The normal TUI remains compact while `TERM=dumb`
    /// and non-UTF-8 locales stay meaningful to plain-text consumers.
    pub fn for_terminal() -> Self {
        let term = std::env::var("TERM").ok();
        let locale = std::env::var("LC_ALL")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                std::env::var("LC_CTYPE")
                    .ok()
                    .filter(|value| !value.is_empty())
            })
            .or_else(|| std::env::var("LANG").ok().filter(|value| !value.is_empty()));
        let style = if terminal_supports_unicode(term.as_deref(), locale.as_deref()) {
            DisclosureStyle::Unicode
        } else {
            DisclosureStyle::Text
        };
        Self::with_disclosure_style(style)
    }

    pub fn is_expanded(&self, row: &TranscriptRow) -> bool {
        self.expanded
            .get(&row.id)
            .copied()
            .unwrap_or(row.default_expanded)
    }

    pub fn render_message(
        &self,
        message: &MessageRef,
        colors: &ColorScheme,
    ) -> Vec<RenderedTranscriptLine> {
        let Some(root) = message.transcript_row(colors) else {
            return message
                .format(colors)
                .split('\n')
                .map(|text| RenderedTranscriptLine {
                    text: text.to_owned(),
                    row_id: None,
                    row_expanded: None,
                })
                .collect();
        };
        let mut lines = Vec::new();
        self.render_row(&root, 0, false, message.status(), &mut lines);
        lines
    }

    pub fn render_message_fully_expanded(
        &self,
        message: &MessageRef,
        colors: &ColorScheme,
    ) -> Vec<RenderedTranscriptLine> {
        let Some(root) = message.transcript_row(colors) else {
            return self.render_message(message, colors);
        };
        let mut lines = Vec::new();
        self.render_row(&root, 0, true, message.status(), &mut lines);
        lines
    }

    fn render_row(
        &self,
        row: &TranscriptRow,
        depth: usize,
        force_expanded: bool,
        message_status: MessageStatus,
        lines: &mut Vec<RenderedTranscriptLine>,
    ) {
        if row.kind == TranscriptRowKind::Response {
            self.render_response(row, depth, message_status, lines);
            return;
        }
        let expandable = !row.body.is_empty() || !row.children.is_empty();
        let expanded = expandable && (force_expanded || self.is_expanded(row));
        let (marker, state) = match (self.disclosure_style, expandable, expanded) {
            (DisclosureStyle::Unicode, true, true) => ("▼", ""),
            (DisclosureStyle::Unicode, true, false) => ("▶", ""),
            (DisclosureStyle::Unicode, false, _) => ("•", ""),
            (DisclosureStyle::Text, true, true) => ("[-]", " [open]"),
            (DisclosureStyle::Text, true, false) => ("[+]", " [closed]"),
            (DisclosureStyle::Text, false, _) => ("*", ""),
        };
        let focus = if self.focused.as_ref() == Some(&row.id) {
            "> "
        } else {
            "  "
        };
        lines.push(RenderedTranscriptLine {
            text: format!(
                "{focus}{}{} {}{}",
                "  ".repeat(depth),
                marker,
                row.label,
                state
            ),
            row_id: expandable.then(|| row.id.clone()),
            row_expanded: expandable.then_some(expanded),
        });
        if !expanded {
            return;
        }
        for body in &row.body {
            lines.push(RenderedTranscriptLine {
                text: format!("{}  {}", "  ".repeat(depth), body),
                row_id: None,
                row_expanded: None,
            });
        }
        for child in &row.children {
            self.render_row(child, depth + 1, force_expanded, message_status, lines);
        }
    }

    fn render_response(
        &self,
        row: &TranscriptRow,
        depth: usize,
        status: MessageStatus,
        lines: &mut Vec<RenderedTranscriptLine>,
    ) {
        let marker = match (self.disclosure_style, status) {
            (DisclosureStyle::Unicode, MessageStatus::InProgress) => "◌".to_string(),
            (DisclosureStyle::Unicode, MessageStatus::Complete) => "●".to_string(),
            (DisclosureStyle::Unicode, MessageStatus::Failed) => {
                "✕ Assistant response failed".to_string()
            }
            (DisclosureStyle::Text, MessageStatus::InProgress) => {
                "[pending] Assistant response".to_string()
            }
            (DisclosureStyle::Text, MessageStatus::Complete) => {
                "[complete] Assistant response".to_string()
            }
            (DisclosureStyle::Text, MessageStatus::Failed) => {
                "[failed] Assistant response".to_string()
            }
        };
        let indent = "  ".repeat(depth);
        let mut body = row.body.iter();
        let first = body.next();
        lines.push(RenderedTranscriptLine {
            text: match first {
                Some(first) => format!("  {indent}{marker} {first}"),
                None => format!("  {indent}{marker}"),
            },
            row_id: None,
            row_expanded: None,
        });
        for line in body {
            lines.push(RenderedTranscriptLine {
                text: format!("    {indent}{line}"),
                row_id: None,
                row_expanded: None,
            });
        }
    }

    pub fn rebuild_hit_regions(
        &mut self,
        lines: &[RenderedTranscriptLine],
        top: usize,
        width: usize,
    ) {
        self.hit_regions.clear();
        self.visible_order.clear();
        self.visible_expanded.clear();
        let mut y = top;
        for line in lines {
            let rows = super::shadow_buffer::physical_rows(&line.text, width.max(1));
            if let Some(row_id) = &line.row_id {
                self.visible_order.push(row_id.clone());
                self.visible_expanded
                    .insert(row_id.clone(), line.row_expanded.unwrap_or(false));
                self.hit_regions.push(TranscriptHitRegion {
                    row_id: row_id.clone(),
                    top: y as u16,
                    bottom: y.saturating_add(rows).saturating_sub(1) as u16,
                    left: 0,
                    right: width.saturating_sub(1) as u16,
                });
            }
            y = y.saturating_add(rows);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::F(6) {
            if self.visible_order.is_empty() {
                return false;
            }
            let backwards = key.modifiers.contains(KeyModifiers::SHIFT);
            let current = self.focused.as_ref().and_then(|id| {
                self.visible_order
                    .iter()
                    .position(|candidate| candidate == id)
            });
            let next = if backwards {
                current
                    .unwrap_or(0)
                    .checked_sub(1)
                    .unwrap_or(self.visible_order.len() - 1)
            } else {
                current.map_or(0, |index| (index + 1) % self.visible_order.len())
            };
            self.focused = Some(self.visible_order[next].clone());
            return true;
        }
        let Some(focused) = self.focused.clone() else {
            return false;
        };
        match key.code {
            KeyCode::Enter | KeyCode::Char(' ') => {
                let current = self
                    .visible_expanded
                    .get(&focused)
                    .copied()
                    .unwrap_or(false);
                self.expanded.insert(focused.clone(), !current);
                self.visible_expanded.insert(focused, !current);
                true
            }
            KeyCode::Left => {
                self.expanded.insert(focused.clone(), false);
                self.visible_expanded.insert(focused, false);
                true
            }
            KeyCode::Right => {
                self.expanded.insert(focused.clone(), true);
                self.visible_expanded.insert(focused, true);
                true
            }
            KeyCode::Esc => {
                self.focused = None;
                true
            }
            _ => false,
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> bool {
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return false;
        }
        let Some(region) = self.hit_regions.iter().find(|region| {
            mouse.row >= region.top
                && mouse.row <= region.bottom
                && mouse.column >= region.left
                && mouse.column <= region.right
        }) else {
            return false;
        };
        let row_id = region.row_id.clone();
        // Mouse disclosure is direct manipulation, not keyboard traversal.
        // Clear keyboard focus so clicking never leaves a persistent `> `
        // marker on a differently indented row.
        self.focused = None;
        let current = self.visible_expanded.get(&row_id).copied().unwrap_or(false);
        self.expanded.insert(row_id.clone(), !current);
        self.visible_expanded.insert(row_id, !current);
        true
    }
}

fn terminal_supports_unicode(term: Option<&str>, locale: Option<&str>) -> bool {
    if term == Some("dumb") {
        return false;
    }
    let Some(locale) = locale else {
        return true;
    };
    let locale = locale.to_ascii_lowercase();
    locale.contains("utf-8") || locale.contains("utf8")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::messages::{Message, WorkUnit};
    use std::sync::Arc;

    #[test]
    fn test_nested_rows_keep_ids_and_hidden_content_across_toggle() {
        let work = Arc::new(WorkUnit::new("Tools"));
        let call = work.add_row("bash(echo 世界)");
        work.complete_row_with_body(call, "2 lines", vec!["世界".into(), "done".into()]);
        work.set_complete();
        let message: MessageRef = work.clone();
        let colors = ColorScheme::default();
        let mut state = AccordionState::default();
        let first = state.render_message(&message, &colors);
        let call_id = work.transcript_row(&colors).unwrap().children[0].id.clone();
        state.rebuild_hit_regions(&first, 0, 80);
        assert!(state.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
        assert!(state.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)));
        let root_expanded = state.render_message(&message, &colors);
        state.rebuild_hit_regions(&root_expanded, 0, 80);
        assert!(state.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
        assert_eq!(state.focused.as_ref(), Some(&call_id));
        assert!(state.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)));
        let expanded = state.render_message(&message, &colors);
        assert!(expanded.iter().any(|line| line.text.contains("Input")));
        assert_eq!(work.content(), "");
        assert!(work.complete_transcript(&colors).contains("世界"));
        assert_eq!(
            call_id,
            work.transcript_row(&colors).unwrap().children[0].id
        );
        assert_ne!(first, expanded);
    }

    #[test]
    fn test_failed_tool_defaults_to_one_summary_and_full_expansion_preserves_details() {
        let work = Arc::new(WorkUnit::new("Tools"));
        let call = work.add_row("catalog.validate provider=chatgpt");
        work.append_row_body_line(call, "raw provider detail".into());
        work.fail_row(call, "catalog unavailable");
        work.set_failed();
        let canonical_before_disclosure = work.complete_transcript(&ColorScheme::default());
        let message: MessageRef = work.clone();
        let colors = ColorScheme::default();
        let mut state = AccordionState::default();

        let compact = state.render_message(&message, &colors);
        assert_eq!(compact.len(), 1);
        assert!(compact[0].text.contains("catalog.validate"));
        assert_eq!(compact[0].text.matches("catalog unavailable").count(), 1);
        assert!(!compact[0].text.contains("Output (0)"));

        state.rebuild_hit_regions(&compact, 3, 24);
        assert!(state.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
        assert!(state.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)));
        let keyboard_expanded = state.render_message(&message, &colors);
        assert!(keyboard_expanded
            .iter()
            .any(|line| line.text.contains("catalog.validate")));

        state.rebuild_hit_regions(&keyboard_expanded, 1, 9);
        let root = state.hit_regions[0].clone();
        assert!(state.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: root.left,
            row: root.top,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(state.render_message(&message, &colors).len(), 1);

        let fully_expanded = state.render_message_fully_expanded(&message, &colors);
        assert!(fully_expanded
            .iter()
            .any(|line| line.text.contains("catalog.validate provider=chatgpt")));
        assert!(fully_expanded
            .iter()
            .any(|line| line.text.contains("raw provider detail")));
        assert!(!fully_expanded
            .iter()
            .any(|line| line.text.contains("Output (0)")));
        assert_eq!(
            work.complete_transcript(&colors),
            canonical_before_disclosure
        );
        assert!(canonical_before_disclosure.contains("catalog unavailable"));
    }

    #[test]
    fn test_unicode_wrapped_hit_region_moves_after_resize() {
        let id = TranscriptRowId {
            message_id: crate::cli::messages::MessageId::new(),
            path: vec![0],
        };
        let lines = vec![RenderedTranscriptLine {
            text: "▶ 世界世界 [collapsed]".into(),
            row_id: Some(id.clone()),
            row_expanded: Some(false),
        }];
        let mut state = AccordionState::default();
        state.rebuild_hit_regions(&lines, 8, 8);
        assert!(state.hit_regions[0].bottom > state.hit_regions[0].top);
        state.rebuild_hit_regions(&lines, 2, 80);
        assert_eq!(
            (state.hit_regions[0].top, state.hit_regions[0].bottom),
            (2, 2)
        );
        assert_eq!(state.hit_regions[0].row_id, id);
    }

    #[test]
    fn test_keyboard_focus_survives_clipping_append_and_reflow() {
        let work = Arc::new(WorkUnit::new("Tools"));
        let call = work.add_row("bash(long command)");
        work.append_row_body_line(call, "first".into());
        let message: MessageRef = work.clone();
        let colors = ColorScheme::default();
        let mut state = AccordionState::default();

        let initial = state.render_message(&message, &colors);
        state.rebuild_hit_regions(&initial, 0, 20);
        assert!(state.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
        let focused = state.focused.clone().unwrap();
        assert!(state.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)));

        work.append_row_body_line(call, "second 世界".into());
        state.rebuild_hit_regions(&[], 0, 6);
        assert_eq!(state.focused.as_ref(), Some(&focused));
        let after_append = state.render_message(&message, &colors);
        assert!(after_append.iter().any(|line| line.text.contains("Input")));
        assert!(after_append
            .iter()
            .any(|line| line.row_id.as_ref() == Some(&focused)));
    }

    #[test]
    fn test_mouse_disclosure_uses_reflowed_region_without_leaking_keyboard_focus() {
        let work = Arc::new(WorkUnit::new("program"));
        work.set_program_source("forth");
        work.set_response("one\ntwo");
        let message: MessageRef = work;
        let colors = ColorScheme::default();
        let mut state = AccordionState::default();
        let lines = state.render_message(&message, &colors);
        state.rebuild_hit_regions(&lines, 7, 10);
        let region = state.hit_regions[0].clone();
        assert!(state.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: region.right,
            row: region.bottom,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(state
            .render_message(&message, &colors)
            .iter()
            .all(|line| !line.text.contains("one")));
        assert!(state.focused.is_none());
        assert!(!state.render_message(&message, &colors)[0]
            .text
            .starts_with("> "));
        assert!(!state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(state.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
        assert!(state.render_message(&message, &colors)[0]
            .text
            .starts_with("> "));
        assert!(state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(state
            .render_message(&message, &colors)
            .iter()
            .any(|line| line.text.contains("one")));
    }

    #[test]
    fn test_completed_source_collapses_and_successful_say_is_inline_prose() {
        let source = Arc::new(WorkUnit::new("program"));
        source.set_program_source("forth");
        source.set_response("s\"visible output\" say");
        source.set_complete();
        let output = Arc::new(WorkUnit::new("output"));
        output.set_program_output();
        output.set_response("visible output");
        output.set_complete();
        let state = AccordionState::default();
        let colors = ColorScheme::default();
        let source_message: MessageRef = source.clone();
        let output_message: MessageRef = output;

        let collapsed = state.render_message(&source_message, &colors);
        assert!(collapsed[0].text.contains('▶'));
        assert!(!collapsed[0].text.contains("collapsed"));
        assert_eq!(collapsed.len(), 1);
        assert!(source
            .complete_transcript(&colors)
            .contains("s\"visible output\" say"));
        let visible = state.render_message(&output_message, &colors);
        assert_eq!(visible.len(), 1);
        assert!(visible[0].text.contains("● visible output"));
        assert!(!visible[0].text.contains("Program output"));
    }

    #[test]
    fn test_reconnect_projection_reuses_canonical_message_identity() {
        let id = crate::cli::messages::MessageId::from_uuid(uuid::Uuid::from_u128(69));
        let original = Arc::new(WorkUnit::with_id(id, "program"));
        original.set_program_source("forth");
        original.set_response("a\nb\nc\nd");
        original.set_complete();
        let original_message: MessageRef = original;
        let colors = ColorScheme::default();
        let mut state = AccordionState::default();
        let initial = state.render_message(&original_message, &colors);
        state.rebuild_hit_regions(&initial, 0, 80);
        assert!(state.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
        assert!(state.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)));

        let replayed = Arc::new(WorkUnit::with_id(id, "program"));
        replayed.set_program_source("forth");
        replayed.set_response("a\nb\nc\nd\nafter reconnect");
        replayed.set_complete();
        let replayed_message: MessageRef = replayed;
        let rendered = state.render_message(&replayed_message, &colors);
        assert!(rendered[0].text.contains('▼'));
        assert!(!rendered[0].text.contains("expanded"));
        assert!(rendered
            .iter()
            .any(|line| line.text.contains("after reconnect")));
    }

    #[test]
    fn test_text_fallback_names_response_and_disclosure_states() {
        assert!(terminal_supports_unicode(
            Some("xterm-256color"),
            Some("en_US.UTF-8")
        ));
        assert!(!terminal_supports_unicode(
            Some("dumb"),
            Some("en_US.UTF-8")
        ));
        assert!(!terminal_supports_unicode(Some("xterm"), Some("C")));

        let response = Arc::new(WorkUnit::new("response"));
        response.set_response("hello");
        let response_message: MessageRef = response;
        let source = Arc::new(WorkUnit::new("source"));
        source.set_program_source("lisp");
        source.set_response("(say \"hello\")");
        source.set_complete();
        let source_message: MessageRef = source;
        let state = AccordionState::with_disclosure_style(DisclosureStyle::Text);
        let colors = ColorScheme::default();

        let pending = state.render_message(&response_message, &colors);
        assert_eq!(
            pending[0].text.trim(),
            "[pending] Assistant response hello",
            "plain-text pending projection must name role and state: {pending:?}"
        );
        let collapsed = state.render_message(&source_message, &colors);
        assert!(
            collapsed[0].text.contains("[closed]"),
            "plain-text disclosure fallback must name closed state: {collapsed:?}"
        );
    }
}
