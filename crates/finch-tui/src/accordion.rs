//! Presentation-only disclosure state for the transcript.
//!
//! The renderer owns expand/collapse and focus (#805): the open set lives in
//! [`AccordionState::expanded`], keyed by the ViewModel's stable [`RowId`], and
//! a row's default is a projection-time prop (`TranscriptNode::default_open`),
//! never state stored on domain data. Rendering consumes ViewModel props —
//! a leaf line shows exactly the label the ViewModel projected, which already
//! carries any status glyph; the disclosure never invents a bullet and never
//! sniffs glyph characters (#821).

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::view_model::{RowId, TranscriptNode};

pub use finch_ui_model::RenderedTranscriptLine;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptHitRegion {
    pub row_id: RowId,
    pub top: u16,
    pub bottom: u16,
    pub left: u16,
    pub right: u16,
}

/// A disclosure hit rect the layout pass claimed inside the transcript
/// viewport, with the row's painted disclosure state for the renderer's
/// open-set cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedDisclosureRect {
    pub region: TranscriptHitRegion,
    pub row_expanded: bool,
    /// Component-owned rows (#882): disclosure lives on the component's
    /// ViewModel, so the rect registers for component routing instead of the
    /// RowId-keyed open-set maps.
    pub component_owned: bool,
}

#[derive(Debug, Default)]
pub struct AccordionState {
    /// The single owner of open/closed. Completing a run re-projects the row
    /// with the same stable id, so a recorded choice survives it. Migrated
    /// say-turn rows (#882) never enter this map — their disclosure state
    /// lives on the component ViewModel.
    expanded: HashMap<RowId, bool>,
    pub focused: Option<RowId>,
    pub hit_regions: Vec<TranscriptHitRegion>,
    visible_order: Vec<RowId>,
    visible_expanded: HashMap<RowId, bool>,
    /// Hit rects of component-owned rows. Clicks and keyboard toggles on
    /// these route to the component's handle, not to the maps above.
    component_regions: Vec<TranscriptHitRegion>,
}

impl AccordionState {
    pub fn is_expanded(&self, row: &TranscriptNode) -> bool {
        self.expanded
            .get(&row.id)
            .copied()
            .unwrap_or(row.default_open)
    }

    /// What a row is currently showing as, for a toggle that has only the row's
    /// identity to work from.
    ///
    /// A choice the user already made is authoritative — it is exactly what
    /// [`Self::is_expanded`] will resolve to on the next projection.
    /// `visible_expanded` is only the last painted frame's resolution, so it is
    /// consulted second, and `false` is reached only for a row that has never
    /// been painted and never been toggled.
    ///
    /// Both toggle paths used to read `visible_expanded` alone, which
    /// `rebuild_hit_regions` wiped on every frame. A row the viewport clipped —
    /// and every row on the ≤3-row path, which rebuilds from an empty slice —
    /// was reported collapsed whatever it was really showing, so toggling an
    /// open work unit opened it again instead of closing it.
    fn resolved_expanded(&self, row_id: &RowId) -> bool {
        self.expanded
            .get(row_id)
            .or_else(|| self.visible_expanded.get(row_id))
            .copied()
            .unwrap_or(false)
    }

    /// Render one ViewModel-projected transcript node under this state's
    /// disclosure choices.
    pub fn render_node(&self, node: &TranscriptNode) -> Vec<RenderedTranscriptLine> {
        let mut lines = Vec::new();
        self.render_row(node, 0, false, &mut lines);
        lines
    }

    /// Render one node with every disclosure forced open, for the canonical
    /// transcript commit. The commit renders the node's RAW source body when
    /// one exists (#756): native scrollback is the copyable record, so a
    /// markdown-rendered viewport body never replaces it there.
    pub fn render_node_fully_expanded(&self, node: &TranscriptNode) -> Vec<RenderedTranscriptLine> {
        let mut lines = Vec::new();
        self.render_row(node, 0, true, &mut lines);
        lines
    }

    /// Render a message that is not a WorkUnit run as its formatted lines.
    pub fn render_plain(&self, formatted: &str) -> Vec<RenderedTranscriptLine> {
        formatted
            .split('\n')
            .map(|text| RenderedTranscriptLine {
                text: text.to_owned(),
                ..RenderedTranscriptLine::default()
            })
            .collect()
    }

    fn render_row(
        &self,
        row: &TranscriptNode,
        depth: usize,
        force_expanded: bool,
        lines: &mut Vec<RenderedTranscriptLine>,
    ) {
        let expandable = !row.body.is_empty() || !row.children.is_empty();
        let expanded = expandable && (force_expanded || self.is_expanded(row));
        // A leaf shows the label alone: the ViewModel's label already carries
        // the status glyph, and a second invented bullet is chrome in the
        // wrong layer (#821).
        let marker = match (expandable, expanded) {
            (true, true) => "▼ ",
            (true, false) => "▶ ",
            (false, _) => "",
        };
        let focus = if self.focused.as_ref() == Some(&row.id) {
            "> "
        } else {
            "  "
        };
        lines.push(RenderedTranscriptLine {
            text: format!("{focus}{}{}{}", "  ".repeat(depth), marker, row.label),
            spans: Vec::new(),
            row_id: expandable.then(|| row.id.clone()),
            row_expanded: expandable.then_some(expanded),
            role: Some(row.role),
            body_of: None,
            component_owned: false,
        });
        if !expanded {
            return;
        }
        // The canonical commit (force_expanded) renders the raw source body —
        // the copyable record (#756). Every other projection renders the
        // viewport body, which is a markdown rendering when the node carries
        // a raw body, and the same text otherwise.
        let body = match (force_expanded, &row.raw_body) {
            (true, Some(raw)) => raw,
            _ => &row.body,
        };
        for body in body {
            lines.push(RenderedTranscriptLine {
                text: format!("{}  {}", "  ".repeat(depth), body),
                spans: Vec::new(),
                row_id: None,
                row_expanded: None,
                role: Some(row.role),
                body_of: expandable.then(|| row.id.clone()),
                component_owned: false,
            });
        }
        for child in &row.children {
            self.render_row(child, depth + 1, force_expanded, lines);
        }
    }

    /// Register the hit regions of the retained transcript region above the
    /// live frame, recounting physical rows from the lines that were painted.
    pub fn rebuild_retained_hit_regions(
        &mut self,
        lines: &[RenderedTranscriptLine],
        top: usize,
        width: usize,
    ) {
        self.hit_regions.clear();
        self.component_regions.clear();
        self.visible_order.clear();
        // `visible_expanded` is not cleared: it is the last resolved state per
        // row, and a row that is merely off-screen this frame has not changed.
        // Wiping it made a clipped row indistinguishable from a collapsed one.
        self.register_line_regions(lines, top, width);
    }

    /// Adopt the disclosure hit rects the layout pass claimed for the live
    /// frame's transcript viewport. The pass owns these rects — they are the
    /// expandable rows' claimed sub-rects of the viewport box, not a recount.
    /// `row_offset` shifts frame-relative rows into terminal rows.
    pub fn adopt_claimed_hitboxes(
        &mut self,
        claimed: &[ClaimedDisclosureRect],
        row_offset: usize,
        width: usize,
    ) {
        for rect in claimed {
            let top = rect.region.top as usize + row_offset;
            let rows = rect.region.bottom as usize - rect.region.top as usize + 1;
            self.visible_order.push(rect.region.row_id.clone());
            if rect.component_owned {
                // Migrated say-turn rows: no open-set entry, component routing
                // instead (#882).
                self.component_regions.push(TranscriptHitRegion {
                    top: top as u16,
                    bottom: top.saturating_add(rows).saturating_sub(1) as u16,
                    ..rect.region.clone()
                });
                continue;
            }
            self.visible_expanded
                .insert(rect.region.row_id.clone(), rect.row_expanded);
            self.hit_regions.push(TranscriptHitRegion {
                row_id: rect.region.row_id.clone(),
                top: top as u16,
                bottom: top.saturating_add(rows).saturating_sub(1) as u16,
                left: 0,
                right: width.saturating_sub(1) as u16,
            });
        }
    }

    fn register_line_regions(
        &mut self,
        lines: &[RenderedTranscriptLine],
        top: usize,
        width: usize,
    ) {
        let mut y = top;
        for line in lines {
            let rows = super::shadow_buffer::physical_rows(&line.text, width.max(1));
            if let Some(row_id) = &line.row_id {
                self.visible_order.push(row_id.clone());
                let region = TranscriptHitRegion {
                    row_id: row_id.clone(),
                    top: y as u16,
                    bottom: y.saturating_add(rows).saturating_sub(1) as u16,
                    left: 0,
                    right: width.saturating_sub(1) as u16,
                };
                if line.component_owned {
                    // Component-owned rows: disclosure state lives on the
                    // component ViewModel, never in the maps (#882).
                    self.component_regions.push(region);
                } else {
                    self.visible_expanded
                        .insert(row_id.clone(), line.row_expanded.unwrap_or(false));
                    self.hit_regions.push(region);
                }
            }
            y = y.saturating_add(rows);
        }
    }

    /// The component-owned row whose hit rect contains this pointer position,
    /// if any. The caller routes the click to the component's handle; the
    /// open-set maps are never consulted for these rows.
    pub fn component_region_at(&self, column: u16, row: u16) -> Option<RowId> {
        self.component_regions
            .iter()
            .find(|region| {
                row >= region.top
                    && row <= region.bottom
                    && column >= region.left
                    && column <= region.right
            })
            .map(|region| region.row_id.clone())
    }

    /// Whether this focused row is component-owned, so keyboard disclosure
    /// routes to the component's handle.
    pub fn is_component_row(&self, row: &RowId) -> bool {
        self.component_regions
            .iter()
            .any(|region| &region.row_id == row)
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
                let current = self.resolved_expanded(&focused);
                self.expanded.insert(focused.clone(), !current);
                self.visible_expanded.insert(focused.clone(), !current);
                true
            }
            KeyCode::Left => {
                self.expanded.insert(focused.clone(), false);
                self.visible_expanded.insert(focused.clone(), false);
                true
            }
            KeyCode::Right => {
                self.expanded.insert(focused.clone(), true);
                self.visible_expanded.insert(focused.clone(), true);
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
        let current = self.resolved_expanded(&row_id);
        self.expanded.insert(row_id.clone(), !current);
        self.visible_expanded.insert(row_id.clone(), !current);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view_model::project_work_unit;
    use finch_messages::{Message, MessageRef, WorkUnit};
    use finch_theme::ColorScheme;
    use std::sync::Arc;

    fn colors() -> ColorScheme {
        ColorScheme::default()
    }

    /// Project a WorkUnit message into ViewModel widget props.
    fn projected_node(message: &MessageRef, colors: &ColorScheme) -> TranscriptNode {
        let view = message
            .work_unit_view(colors)
            .expect("accordion tests project WorkUnit runs");
        project_work_unit(&view)
    }

    #[test]
    fn test_nested_rows_keep_ids_and_hidden_content_across_toggle() {
        let work = Arc::new(WorkUnit::new("Tools"));
        let call = work.add_row("bash(echo 世界)");
        work.complete_row_with_body(call, "2 lines", vec!["世界".into(), "done".into()]);
        work.set_complete();
        let message: MessageRef = work.clone();
        let colors = ColorScheme::default();
        let mut state = AccordionState::default();
        let first = state.render_node(&projected_node(
            &(Arc::clone(&message) as MessageRef),
            &colors,
        ));
        let call_id = projected_node(&(Arc::clone(&work) as MessageRef), &colors).children[0]
            .id
            .clone();
        state.rebuild_retained_hit_regions(&first, 0, 80);
        assert!(state.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
        assert!(state.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)));
        let root_expanded = state.render_node(&projected_node(
            &(Arc::clone(&message) as MessageRef),
            &colors,
        ));
        state.rebuild_retained_hit_regions(&root_expanded, 0, 80);
        assert!(state.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
        assert_eq!(state.focused.as_ref(), Some(&call_id));
        assert!(state.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)));
        let expanded = state.render_node(&projected_node(
            &(Arc::clone(&message) as MessageRef),
            &colors,
        ));
        assert!(expanded.iter().any(|line| line.text.contains("Input")));
        assert_eq!(work.content(), "");
        assert!(work.complete_transcript(&colors).contains("世界"));
        assert_eq!(
            call_id,
            projected_node(&(Arc::clone(&work) as MessageRef), &colors).children[0].id
        );
        assert_ne!(first, expanded);
    }

    /// A completed work unit whose tool reported a summary: collapsed by default.
    fn collapsed_work_unit() -> (Arc<WorkUnit>, MessageRef) {
        let work = Arc::new(WorkUnit::new("Tools"));
        let call = work.add_row("bash(echo hi)");
        work.complete_row_with_body(call, "1 line", vec!["hi".into()]);
        work.set_complete();
        let message: MessageRef = work.clone();
        (work, message)
    }

    #[test]
    fn test_toggle_inverts_the_state_the_row_is_actually_in() {
        // Regression: both toggle paths read `visible_expanded`, a cache that
        // `rebuild_hit_regions` wipes on every frame and repopulates only from
        // the lines that were painted. A row the viewport clipped — or any row
        // at all on the ≤3-row path, which rebuilds from an empty slice — was
        // therefore absent, and `unwrap_or(false)` claimed it was collapsed.
        // Toggling an *open* work unit then opened it again, so it stayed open
        // and needed a second press to close: work units that would not shut,
        // seemingly at random, because it depended on whether the row happened
        // to be painted in the preceding frame.
        let colors = ColorScheme::default();
        let (work, message) = collapsed_work_unit();
        let mut state = AccordionState::default();
        let lines = state.render_node(&projected_node(
            &(Arc::clone(&message) as MessageRef),
            &colors,
        ));
        state.rebuild_retained_hit_regions(&lines, 0, 80);
        let root = projected_node(&(Arc::clone(&work) as MessageRef), &colors);

        state.focused = Some(root.id.clone());
        assert!(
            state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            "the first toggle must be handled; focused row was {:?}",
            state.focused
        );
        assert!(
            state.is_expanded(&root),
            "toggling a collapsed row must open it"
        );

        // A frame in which this row is not painted: clipped by the viewport
        // budget, or a terminal too short to host a live area at all.
        state.rebuild_retained_hit_regions(&[], 0, 80);

        state.focused = Some(root.id.clone());
        assert!(state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(
            !state.is_expanded(&root),
            "toggling an open row must close it, whatever the last frame happened to \
             paint; the row resolved as expanded={} before the toggle",
            true
        );
    }

    #[test]
    fn test_click_inverts_the_state_the_row_is_actually_in() {
        // Same defect through the mouse path, which is how a work unit is
        // usually opened.
        let colors = ColorScheme::default();
        let (work, message) = collapsed_work_unit();
        let mut state = AccordionState::default();
        let lines = state.render_node(&projected_node(
            &(Arc::clone(&message) as MessageRef),
            &colors,
        ));
        state.rebuild_retained_hit_regions(&lines, 0, 80);
        let root = projected_node(&(Arc::clone(&work) as MessageRef), &colors);

        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: state.hit_regions[0].top,
            modifiers: KeyModifiers::NONE,
        };
        assert!(state.handle_mouse(click), "the header must be clickable");
        assert!(state.is_expanded(&root), "the click must open the row");

        // The next frame rebuilds from lines projected before the toggle was
        // applied — the renderer plans a frame from the snapshot it holds, and
        // input is drained separately. The cache then says "collapsed" about a
        // row the user just opened.
        state.rebuild_retained_hit_regions(&lines, 0, 80);

        let click_again = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: state.hit_regions[0].top,
            modifiers: KeyModifiers::NONE,
        };
        assert!(state.handle_mouse(click_again));
        assert!(
            !state.is_expanded(&root),
            "clicking an open work unit must close it; it reopened instead, which is the \
             work unit that will not stay shut"
        );
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

        let compact = state.render_node(&projected_node(
            &(Arc::clone(&message) as MessageRef),
            &colors,
        ));
        assert_eq!(compact.len(), 1);
        assert!(compact[0].text.contains("catalog.validate"));
        assert_eq!(compact[0].text.matches("catalog unavailable").count(), 1);
        assert!(!compact[0].text.contains("Output (0)"));

        state.rebuild_retained_hit_regions(&compact, 3, 24);
        assert!(state.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
        assert!(state.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)));
        let keyboard_expanded = state.render_node(&projected_node(
            &(Arc::clone(&message) as MessageRef),
            &colors,
        ));
        assert!(keyboard_expanded
            .iter()
            .any(|line| line.text.contains("catalog.validate")));

        state.rebuild_retained_hit_regions(&keyboard_expanded, 1, 9);
        let root = state.hit_regions[0].clone();
        assert!(state.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: root.left,
            row: root.top,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(
            state
                .render_node(&projected_node(
                    &(Arc::clone(&message) as MessageRef),
                    &colors
                ))
                .len(),
            1
        );

        let fully_expanded = state.render_node_fully_expanded(&projected_node(
            &(Arc::clone(&message) as MessageRef),
            &colors,
        ));
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
        let id = RowId {
            message_id: finch_messages::MessageId::new(),
            path: vec![0],
        };
        let lines = vec![RenderedTranscriptLine {
            text: "▶ 世界世界".into(),
            row_id: Some(id.clone()),
            row_expanded: Some(false),
            ..RenderedTranscriptLine::default()
        }];
        let mut state = AccordionState::default();
        state.rebuild_retained_hit_regions(&lines, 8, 8);
        assert!(state.hit_regions[0].bottom > state.hit_regions[0].top);
        state.rebuild_retained_hit_regions(&lines, 2, 80);
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

        let initial = state.render_node(&projected_node(
            &(Arc::clone(&message) as MessageRef),
            &colors,
        ));
        state.rebuild_retained_hit_regions(&initial, 0, 20);
        assert!(state.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
        let focused = state.focused.clone().unwrap();
        assert!(state.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)));

        work.append_row_body_line(call, "second 世界".into());
        state.rebuild_retained_hit_regions(&[], 0, 6);
        assert_eq!(state.focused.as_ref(), Some(&focused));
        let after_append = state.render_node(&projected_node(
            &(Arc::clone(&message) as MessageRef),
            &colors,
        ));
        assert!(after_append.iter().any(|line| line.text.contains("Input")));
        assert!(after_append
            .iter()
            .any(|line| line.row_id.as_ref() == Some(&focused)));
    }

    #[test]
    fn test_mouse_disclosure_uses_reflowed_region_without_leaking_keyboard_focus() {
        let work = Arc::new(WorkUnit::new("response"));
        work.set_response("one\ntwo");
        work.set_complete();
        let message: MessageRef = work;
        let colors = ColorScheme::default();
        let mut state = AccordionState::default();
        let lines = state.render_node(&projected_node(
            &(Arc::clone(&message) as MessageRef),
            &colors,
        ));
        state.rebuild_retained_hit_regions(&lines, 7, 10);
        let region = state.hit_regions[0].clone();
        assert!(state.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: region.right,
            row: region.bottom,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(state
            .render_node(&projected_node(
                &(Arc::clone(&message) as MessageRef),
                &colors
            ))
            .iter()
            .all(|line| !line.text.contains("one")));
        assert!(state.focused.is_none());
        assert!(!state.render_node(&projected_node(
            &(Arc::clone(&message) as MessageRef),
            &colors
        ))[0]
            .text
            .starts_with("> "));
        assert!(!state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(state.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
        assert!(state.render_node(&projected_node(
            &(Arc::clone(&message) as MessageRef),
            &colors
        ))[0]
            .text
            .starts_with("> "));
        assert!(state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(state
            .render_node(&projected_node(
                &(Arc::clone(&message) as MessageRef),
                &colors
            ))
            .iter()
            .any(|line| line.text.contains("one")));
    }

    #[test]
    fn test_completed_assistant_prose_renders_without_implementation_label() {
        let work = Arc::new(WorkUnit::new("Channeling"));
        work.set_response("Hello, Shammah! How can I help you today?");
        work.set_complete();
        let message: MessageRef = work;
        let colors = ColorScheme::default();
        let state = AccordionState::default();

        let rendered = state
            .render_node(&projected_node(
                &(Arc::clone(&message) as MessageRef),
                &colors,
            ))
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>();
        let transcript = rendered.join("\n");

        assert!(
            !rendered
                .iter()
                .any(|line| line.contains("Assistant response")),
            "invariant: a plain assistant turn is projected as the assistant's prose, \
             never as Finch's internal `Assistant response` placeholder (#350); \
             rendered transcript:\n{transcript}"
        );
        assert!(
            rendered[0].contains('\u{23fa}'),
            "invariant: a completed assistant prose row carries the filled activity \
             glyph; header was {:?}; rendered transcript:\n{transcript}",
            rendered[0]
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("Hello, Shammah! How can I help you today?")),
            "invariant: the assistant's own words stay visible by default; \
             rendered transcript:\n{transcript}"
        );
    }

    #[test]
    fn test_pending_assistant_prose_uses_hollow_activity_glyph() {
        let work = Arc::new(WorkUnit::new("Channeling"));
        work.append_response("Hello, Sha");
        let message: MessageRef = work;
        let colors = ColorScheme::default();
        let state = AccordionState::default();

        let rendered = state
            .render_node(&projected_node(
                &(Arc::clone(&message) as MessageRef),
                &colors,
            ))
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>();
        let transcript = rendered.join("\n");

        assert!(
            !rendered
                .iter()
                .any(|line| line.contains("Assistant response")),
            "invariant: a pending assistant prose row shows a compact activity glyph, \
             not the internal `Assistant response` placeholder (#350); \
             rendered transcript:\n{transcript}"
        );
        assert!(
            rendered[0].contains('\u{25cb}') && !rendered[0].contains('\u{23fa}'),
            "invariant: a pending assistant prose row uses the hollow glyph and the \
             filled glyph is reserved for the completed row; header was {:?}; \
             rendered transcript:\n{transcript}",
            rendered[0]
        );
    }

    #[test]
    fn test_successful_say_renders_as_prose_not_program_output() {
        let source = Arc::new(WorkUnit::new("typed program"));
        source.set_program_source("lisp");
        source.set_response("(say \"Hello\")");
        source.set_complete();
        let output = Arc::new(WorkUnit::new("VM program output"));
        output.set_program_output();
        output.set_response("Hello");
        output.present_as_assistant_prose();
        output.set_complete();
        let colors = ColorScheme::default();
        let state = AccordionState::default();

        let source_message: MessageRef = source;
        let source_lines = state.render_node(&projected_node(
            &(Arc::clone(&source_message) as MessageRef),
            &colors,
        ));
        let source_transcript = source_lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            (source_lines.len(), source_lines[0].row_expanded),
            (1, Some(false)),
            "invariant: successful one-line (say …) source defaults collapsed; \
             header was {:?}; rendered transcript:\n{source_transcript}",
            source_lines[0].text
        );

        let output_message: MessageRef = output;
        let rendered = state
            .render_node(&projected_node(
                &(Arc::clone(&output_message) as MessageRef),
                &colors,
            ))
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>();
        let transcript = rendered.join("\n");
        assert!(
            !transcript.contains("Program output") && !transcript.contains("Assistant response"),
            "invariant: successful say is assistant prose, not Program output chrome; \
             rendered transcript:\n{transcript}"
        );
        assert!(
            rendered[0].contains('\u{23fa}'),
            "invariant: completed say uses the filled activity glyph; header was {:?}; \
             rendered transcript:\n{transcript}",
            rendered[0]
        );
        assert!(
            rendered.iter().any(|line| line.contains("Hello")),
            "invariant: the user-facing say bytes stay visible; \
             rendered transcript:\n{transcript}"
        );
    }

    #[test]
    fn test_failed_program_output_stays_implementation_labelled() {
        let output = Arc::new(WorkUnit::new("VM program output"));
        output.set_program_output();
        output.set_response("visible first\nVM error: type error");
        output.set_complete();
        let message: MessageRef = output;
        let colors = ColorScheme::default();
        let state = AccordionState::default();
        let lines = state.render_node(&projected_node(
            &(Arc::clone(&message) as MessageRef),
            &colors,
        ));
        let rendered = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>();
        let transcript = rendered.join("\n");
        assert!(
            transcript.contains("Program output"),
            "invariant: a failed program remains labelled Program output; \
             rendered transcript:\n{transcript}"
        );
        assert_eq!(
            lines[0].row_expanded,
            Some(true),
            "invariant: failures remain expanded and actionable; header was {:?}; \
             rendered transcript:\n{transcript}",
            lines[0].text
        );
        assert!(
            transcript.contains("VM error: type error"),
            "invariant: the diagnostic stays visible; rendered transcript:\n{transcript}"
        );
        assert!(
            !transcript.contains('\u{23fa}'),
            "invariant: a failure must not wear the completed-prose glyph; \
             rendered transcript:\n{transcript}"
        );
    }

    /// Production reaches this row constantly: `query_processor` creates the
    /// query WorkUnit empty and it stays empty for the whole provider round
    /// trip, and a stream error or an unstageable tool round terminalises that
    /// same empty unit. With no body the accordion is not expandable, so the
    /// label is the row's entire text — it must be readable, not a bare glyph.
    #[test]
    fn test_assistant_row_without_words_renders_readable_text_not_a_bare_glyph() {
        let colors = ColorScheme::default();
        let state = AccordionState::default();

        let pending = Arc::new(WorkUnit::new("Channeling"));
        let message: MessageRef = pending;
        let rendered = state
            .render_node(&projected_node(
                &(Arc::clone(&message) as MessageRef),
                &colors,
            ))
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>();
        let transcript = rendered.join("\n");

        assert!(
            !transcript.contains("Assistant response"),
            "invariant: the internal `Assistant response` placeholder never reaches the \
             screen (#350); rendered transcript:\n{transcript}"
        );
        assert!(
            transcript.contains("Channeling"),
            "invariant: an assistant row with nothing to show yet still carries readable \
             text — this row is on screen for the entire provider round trip and a bare \
             glyph would leave it unspeakable (#350, Key Principle 5); \
             rendered transcript:\n{transcript}"
        );
        assert!(
            rendered[0].chars().any(char::is_alphabetic),
            "invariant: the rendered header line contains letters, not glyphs alone; \
             header was {:?}; rendered transcript:\n{transcript}",
            rendered[0]
        );

        let failed = Arc::new(WorkUnit::new("Channeling"));
        failed.set_failed();
        let failed_message: MessageRef = failed;
        let failed_rendered = state
            .render_node(&projected_node(
                &(Arc::clone(&failed_message) as MessageRef),
                &colors,
            ))
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>();
        let failed_transcript = failed_rendered.join("\n");

        assert!(
            failed_transcript.contains("Assistant turn failed"),
            "invariant: a turn that died before its first token says so in words rather \
             than presenting as an ordinary finished row (#350); \
             rendered transcript:\n{failed_transcript}"
        );
        assert!(
            failed_transcript.contains('\u{2298}')
                && !failed_transcript.contains('\u{23fa}')
                && !failed_transcript.contains('\u{25cb}'),
            "invariant: a failed assistant turn carries its own mark and never the \
             completed or in-progress glyph, so a dead query cannot be read as an \
             answered or a still-running one; rendered transcript:\n{failed_transcript}"
        );
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

        let collapsed = state.render_node(&projected_node(
            &(Arc::clone(&source_message) as MessageRef),
            &colors,
        ));
        assert_eq!(collapsed[0].row_expanded, Some(false));
        assert_eq!(collapsed.len(), 1);
        assert!(source.complete_transcript(&colors).contains("a\nb\nc\nd"));
        let visible = state.render_node(&projected_node(
            &(Arc::clone(&output_message) as MessageRef),
            &colors,
        ));
        assert_eq!(visible[0].row_expanded, Some(true));
        assert!(visible
            .iter()
            .any(|line| line.text.contains("visible output")));
    }

    #[test]
    fn test_reconnect_projection_reuses_canonical_message_identity() {
        let id = finch_messages::MessageId::from_uuid(uuid::Uuid::from_u128(69));
        let original = Arc::new(WorkUnit::with_id(id, "program"));
        original.set_program_source("forth");
        original.set_response("a\nb\nc\nd");
        original.set_complete();
        let original_message: MessageRef = original;
        let colors = ColorScheme::default();
        let mut state = AccordionState::default();
        let initial = state.render_node(&projected_node(
            &(Arc::clone(&original_message) as MessageRef),
            &colors,
        ));
        state.rebuild_retained_hit_regions(&initial, 0, 80);
        assert!(state.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
        assert!(state.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)));

        let replayed = Arc::new(WorkUnit::with_id(id, "program"));
        replayed.set_program_source("forth");
        replayed.set_response("a\nb\nc\nd\nafter reconnect");
        replayed.set_complete();
        let replayed_message: MessageRef = replayed;
        let rendered = state.render_node(&projected_node(
            &(Arc::clone(&replayed_message) as MessageRef),
            &colors,
        ));
        assert_eq!(rendered[0].row_expanded, Some(true));
        assert!(rendered
            .iter()
            .any(|line| line.text.contains("after reconnect")));
    }

    /// Production dump: Program source / Program output headers still appended
    /// a second visible `[expanded]` / `[collapsed]` token after ▼/▶ (#417).
    /// Expand/collapse belongs on `row_expanded` (role/state), not in the text.
    #[test]
    fn test_disclosure_rows_do_not_append_expanded_collapsed_suffix() {
        let source = Arc::new(WorkUnit::new("program"));
        source.set_program_source("lisp");
        source.set_response("(say \"Hello, Shammah!\")");
        source.set_complete();
        let output = Arc::new(WorkUnit::new("VM program output"));
        output.set_program_output();
        output.set_response("Hello, Shammah!");
        output.set_complete();
        let state = AccordionState::default();
        let colors = ColorScheme::default();
        let source_message: MessageRef = source;
        let output_message: MessageRef = output;
        let source_lines = state.render_node(&projected_node(
            &(Arc::clone(&source_message) as MessageRef),
            &colors,
        ));
        let output_lines = state.render_node(&projected_node(
            &(Arc::clone(&output_message) as MessageRef),
            &colors,
        ));
        let dump = source_lines
            .iter()
            .chain(output_lines.iter())
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !dump.contains("[expanded]") && !dump.contains("[collapsed]"),
            "invariant: disclosure headers must not append [expanded]/[collapsed]; \
             the dump was:\n{dump}"
        );
        assert_eq!(
            source_lines[0].row_expanded,
            Some(false),
            "invariant: collapsed Program source still reports state on row_expanded, \
             not a visible suffix; header was {:?}; dump:\n{dump}",
            source_lines[0].text
        );
        assert_eq!(
            output_lines[0].row_expanded,
            Some(true),
            "invariant: expanded Program output still reports state on row_expanded, \
             not a visible suffix; header was {:?}; dump:\n{dump}",
            output_lines[0].text
        );
        assert!(
            source_lines[0].text.contains('▶') && output_lines[0].text.contains('▼'),
            "invariant: the glyph remains the visible expand/collapse mark; dump:\n{dump}"
        );
        assert!(
            dump.contains("Program source") && dump.contains("Program output"),
            "invariant: the dump still names Program source and Program output; \
             dump:\n{dump}"
        );
    }
}
