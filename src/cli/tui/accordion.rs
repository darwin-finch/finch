//! Presentation-only disclosure state for retained transcript rows.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::cli::messages::{MessageRef, TranscriptRow, TranscriptRowId};
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

#[derive(Debug, Default)]
pub struct AccordionState {
    expanded: HashMap<TranscriptRowId, bool>,
    pub focused: Option<TranscriptRowId>,
    pub hit_regions: Vec<TranscriptHitRegion>,
    visible_order: Vec<TranscriptRowId>,
    visible_expanded: HashMap<TranscriptRowId, bool>,
}

impl AccordionState {
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
        self.render_row(&root, 0, &mut lines);
        lines
    }

    fn render_row(
        &self,
        row: &TranscriptRow,
        depth: usize,
        lines: &mut Vec<RenderedTranscriptLine>,
    ) {
        let expandable = !row.body.is_empty() || !row.children.is_empty();
        let expanded = expandable && self.is_expanded(row);
        let marker = match (expandable, expanded) {
            (true, true) => "▼",
            (true, false) => "▶",
            (false, _) => "•",
        };
        let state = if expandable {
            if expanded {
                " [expanded]"
            } else {
                " [collapsed]"
            }
        } else {
            ""
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
            self.render_row(child, depth + 1, lines);
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
                self.expanded.insert(focused, !current);
                true
            }
            KeyCode::Left => {
                self.expanded.insert(focused, false);
                true
            }
            KeyCode::Right => {
                self.expanded.insert(focused, true);
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
        self.focused = Some(row_id.clone());
        let current = self.visible_expanded.get(&row_id).copied().unwrap_or(false);
        self.expanded.insert(row_id, !current);
        true
    }
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
        state.focused = Some(call_id.clone());
        state.visible_order = vec![call_id.clone()];
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
    fn test_mouse_uses_reflowed_region_and_matches_keyboard_toggle() {
        let work = Arc::new(WorkUnit::new("response"));
        work.set_response("one\ntwo");
        work.set_complete();
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
        assert!(state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(state
            .render_message(&message, &colors)
            .iter()
            .any(|line| line.text.contains("one")));
    }

    #[test]
    fn test_semantic_defaults_collapse_long_completed_source_but_not_output() {
        let source = Arc::new(WorkUnit::new("program"));
        source.set_program_source("forth");
        source.set_response("a\nb\nc\nd");
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
        assert!(collapsed[0].text.contains("[collapsed]"));
        assert_eq!(collapsed.len(), 1);
        assert!(source.complete_transcript(&colors).contains("a\nb\nc\nd"));
        let visible = state.render_message(&output_message, &colors);
        assert!(visible[0].text.contains("[expanded]"));
        assert!(visible
            .iter()
            .any(|line| line.text.contains("visible output")));
    }
}
