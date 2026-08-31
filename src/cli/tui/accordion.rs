//! Presentation-only disclosure state for retained transcript rows.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::cli::messages::{
    DisclosureLookup, MessageId, MessageRef, RenderAction, RenderCapabilities, RenderContext,
};
use crate::config::ColorScheme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedTranscriptLine {
    pub text: String,
    pub row_id: Option<MessageId>,
    pub row_expanded: Option<bool>,
    pub action: Option<RenderAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptHitRegion {
    pub row_id: MessageId,
    pub action: RenderAction,
    pub top: u16,
    pub bottom: u16,
    pub left: u16,
    pub right: u16,
}

#[derive(Debug, Default)]
pub struct AccordionState {
    expanded: HashMap<MessageId, bool>,
    pub focused: Option<MessageId>,
    pub hit_regions: Vec<TranscriptHitRegion>,
    visible_order: Vec<MessageId>,
    visible_expanded: HashMap<MessageId, bool>,
}

impl AccordionState {
    pub fn render_message(
        &self,
        message: &MessageRef,
        colors: &ColorScheme,
    ) -> Vec<RenderedTranscriptLine> {
        message
            .render(
                &RenderContext::new(colors, RenderCapabilities::terminal(usize::MAX))
                    .with_disclosure(self),
            )
            .lines
            .into_iter()
            .map(|line| RenderedTranscriptLine {
                text: line.text,
                row_id: line.row_id,
                row_expanded: line.row_expanded,
                action: line.action,
            })
            .collect()
    }

    pub fn render_message_fully_expanded(
        &self,
        message: &MessageRef,
        colors: &ColorScheme,
    ) -> Vec<RenderedTranscriptLine> {
        message
            .render(
                &RenderContext::new(colors, RenderCapabilities::terminal(usize::MAX))
                    .with_disclosure(self)
                    .fully_expanded(),
            )
            .lines
            .into_iter()
            .map(|line| RenderedTranscriptLine {
                text: line.text,
                row_id: line.row_id,
                row_expanded: line.row_expanded,
                action: line.action,
            })
            .collect()
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
            if let Some(message_id) = line.row_id {
                self.visible_order.push(message_id);
                self.visible_expanded
                    .insert(message_id, line.row_expanded.unwrap_or(false));
            }
            if let (Some(message_id), Some(action)) = (line.row_id, &line.action) {
                self.hit_regions.push(TranscriptHitRegion {
                    row_id: message_id,
                    action: action.clone(),
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
        let RenderAction::ToggleDisclosure { row_id: message_id } = region.action.clone();
        // Mouse disclosure is direct manipulation, not keyboard traversal.
        // Clear keyboard focus so clicking never leaves a persistent `> `
        // marker on a differently indented row.
        self.focused = None;
        let current = self
            .visible_expanded
            .get(&message_id)
            .copied()
            .unwrap_or(false);
        self.expanded.insert(message_id, !current);
        self.visible_expanded.insert(message_id, !current);
        true
    }
}

impl DisclosureLookup for AccordionState {
    fn expanded(&self, message_id: &MessageId) -> Option<bool> {
        self.expanded.get(message_id).copied()
    }

    fn focused(&self, message_id: &MessageId) -> bool {
        self.focused.as_ref() == Some(message_id)
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
        let call_id = work.children()[0].id();
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
        assert_eq!(call_id, work.children()[0].id());
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
        let id = crate::cli::messages::MessageId::new();
        let lines = vec![RenderedTranscriptLine {
            text: "▶ 世界世界 [collapsed]".into(),
            row_id: Some(id),
            row_expanded: Some(false),
            action: Some(RenderAction::ToggleDisclosure { row_id: id }),
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
    fn message_render_action_survives_frontend_adapter() {
        let work = Arc::new(WorkUnit::new("Tools"));
        work.add_row("submit_program");
        let message: MessageRef = work;
        let rendered = AccordionState::default().render_message(&message, &ColorScheme::default());

        assert!(matches!(
            rendered[0].action,
            Some(RenderAction::ToggleDisclosure { row_id })
                if rendered[0].row_id == Some(row_id)
        ));
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
        assert!(after_append.iter().any(|line| line.row_id == Some(focused)));
    }

    #[test]
    fn test_mouse_disclosure_uses_reflowed_region_without_leaking_keyboard_focus() {
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
        assert_eq!(collapsed[0].row_expanded, Some(false));
        assert!(!collapsed[0].text.contains("[collapsed]"));
        assert!(!collapsed[0].text.contains("[expanded]"));
        assert_eq!(collapsed.len(), 1);
        assert!(source.complete_transcript(&colors).contains("a\nb\nc\nd"));
        let visible = state.render_message(&output_message, &colors);
        assert_eq!(visible[0].row_expanded, Some(true));
        assert!(!visible[0].text.contains("[expanded]"));
        assert!(!visible[0].text.contains("[collapsed]"));
        assert!(visible
            .iter()
            .any(|line| line.text.contains("visible output")));
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
        assert_eq!(rendered[0].row_expanded, Some(true));
        assert!(!rendered[0].text.contains("[expanded]"));
        assert!(!rendered[0].text.contains("[collapsed]"));
        assert!(rendered
            .iter()
            .any(|line| line.text.contains("after reconnect")));
    }
}
