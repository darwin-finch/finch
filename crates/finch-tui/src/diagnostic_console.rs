use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::Arc;

/// A bounded, terminal-safe snapshot of application diagnostics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagnosticConsoleSnapshot {
    /// Monotonic source revision. It changes whenever retained output changes.
    pub revision: u64,
    /// Speakable lines, already stripped of terminal control sequences.
    pub lines: Vec<String>,
}

/// Application-owned source for the in-terminal diagnostic console.
pub trait DiagnosticConsolePort: Send + Sync {
    /// Return the latest bounded diagnostic snapshot.
    fn snapshot(&self) -> DiagnosticConsoleSnapshot;
}

#[derive(Default)]
pub(crate) struct DiagnosticConsoleState {
    source: Option<Arc<dyn DiagnosticConsolePort>>,
    snapshot: DiagnosticConsoleSnapshot,
    open: bool,
    /// Lines hidden below the current window; zero follows the newest output.
    scroll: usize,
}

impl DiagnosticConsoleState {
    pub(crate) fn set_source(&mut self, source: Arc<dyn DiagnosticConsolePort>) {
        self.source = Some(source);
        self.refresh();
    }

    pub(crate) fn refresh(&mut self) -> bool {
        let Some(source) = &self.source else {
            return false;
        };
        let next = source.snapshot();
        if next == self.snapshot {
            return false;
        }
        self.snapshot = next;
        if self.scroll > self.snapshot.lines.len().saturating_sub(1) {
            self.scroll = self.snapshot.lines.len().saturating_sub(1);
        }
        true
    }

    pub(crate) fn line_count(&self) -> usize {
        self.snapshot.lines.len()
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        if is_toggle_key(key) {
            self.open = !self.open;
            self.scroll = 0;
            if self.open {
                self.refresh();
            }
            return true;
        }
        if !self.open {
            return false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.open = false,
            KeyCode::Up => self.scroll = (self.scroll + 1).min(self.max_scroll()),
            KeyCode::Down => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageUp => self.scroll = (self.scroll + 10).min(self.max_scroll()),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Home => self.scroll = self.max_scroll(),
            KeyCode::End => self.scroll = 0,
            _ => return false,
        }
        true
    }

    /// Scroll the console in response to a vertical wheel delta. Negative
    /// deltas move toward older output; positive deltas move toward newer
    /// output, matching the renderer's other scroll surfaces.
    pub(crate) fn handle_wheel(&mut self, delta: isize) -> bool {
        if !self.open || delta == 0 {
            return false;
        }
        if delta < 0 {
            self.scroll = (self.scroll + delta.unsigned_abs()).min(self.max_scroll());
        } else {
            self.scroll = self.scroll.saturating_sub(delta as usize);
        }
        true
    }

    fn max_scroll(&self) -> usize {
        self.snapshot.lines.len().saturating_sub(1)
    }

    pub(crate) fn frame(&self, width: usize, height: usize) -> Option<Vec<String>> {
        if !self.open {
            return None;
        }
        let width = width.max(1);
        if height == 0 {
            return Some(Vec::new());
        }
        let body_rows = height.saturating_sub(2);
        let end = self
            .snapshot
            .lines
            .len()
            .saturating_sub(self.scroll)
            .max(1)
            .min(self.snapshot.lines.len());
        let start = end.saturating_sub(body_rows);
        let mut frame = Vec::with_capacity(height);
        frame.push(super::shadow_buffer::truncate_to_columns(
            &format!("Diagnostic console — {} retained lines", self.line_count()),
            width,
        ));
        if self.snapshot.lines.is_empty() && body_rows > 0 {
            frame.push("No diagnostic output yet.".to_string());
        } else {
            frame.extend(
                self.snapshot.lines[start..end]
                    .iter()
                    .map(|line| super::shadow_buffer::truncate_to_columns(line, width)),
            );
        }
        while frame.len() < height.saturating_sub(1) {
            frame.push(String::new());
        }
        if height > 1 {
            let position = if self.snapshot.lines.is_empty() {
                "lines 0 / 0".to_string()
            } else {
                format!("lines {}–{} / {}", start + 1, end, self.line_count())
            };
            frame.push(super::shadow_buffer::truncate_to_columns(
                &format!("{position} · Ctrl+` / Esc close · wheel/↑↓ PgUp/PgDn Home/End scroll"),
                width,
            ));
        }
        Some(frame)
    }
}

fn is_toggle_key(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('`') | KeyCode::Char(' '))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedSource(DiagnosticConsoleSnapshot);

    impl DiagnosticConsolePort for FixedSource {
        fn snapshot(&self) -> DiagnosticConsoleSnapshot {
            self.0.clone()
        }
    }

    #[test]
    fn test_ctrl_backtick_opens_a_bounded_latest_lines_console() {
        let mut state = DiagnosticConsoleState::default();
        state.set_source(Arc::new(FixedSource(DiagnosticConsoleSnapshot {
            revision: 1,
            lines: (0..20).map(|index| format!("line {index}")).collect(),
        })));

        assert!(state.handle_key(KeyEvent::new(KeyCode::Char('`'), KeyModifiers::CONTROL,)));
        let frame = state.frame(80, 6).expect("Ctrl+` opens the console");
        assert!(
            frame.iter().any(|line| line == "line 19"),
            "the console must follow the newest diagnostic output: {frame:?}"
        );
        assert!(
            frame.iter().all(|line| line != "line 0"),
            "the bounded console must not overflow its claimed rows: {frame:?}"
        );
        assert_eq!(
            frame.len(),
            6,
            "the console must own the entire requested frame, including blank rows: {frame:?}"
        );
        assert!(
            frame
                .last()
                .is_some_and(|line| line.contains("lines 17–20 / 20")),
            "the footer must state the visible log range and retained total: {frame:?}"
        );
    }

    #[test]
    fn test_mouse_wheel_scrolls_console_and_updates_visible_range() {
        let mut state = DiagnosticConsoleState::default();
        state.set_source(Arc::new(FixedSource(DiagnosticConsoleSnapshot {
            revision: 1,
            lines: (0..20).map(|index| format!("line {index}")).collect(),
        })));
        state.handle_key(KeyEvent::new(KeyCode::Char('`'), KeyModifiers::CONTROL));

        assert!(
            state.handle_wheel(-3),
            "an open console must claim wheel-up"
        );
        let frame = state.frame(100, 6).expect("console remains open");
        assert!(
            frame.iter().any(|line| line == "line 16"),
            "wheel-up must reveal older diagnostics: {frame:?}"
        );
        assert!(
            frame
                .last()
                .is_some_and(|line| line.contains("lines 14–17 / 20")),
            "the visible-range indicator must follow wheel scrolling: {frame:?}"
        );

        assert!(
            state.handle_wheel(3),
            "an open console must claim wheel-down"
        );
        let frame = state.frame(100, 6).expect("console remains open");
        assert!(
            frame
                .last()
                .is_some_and(|line| line.contains("lines 17–20 / 20")),
            "wheel-down must return to the newest diagnostics: {frame:?}"
        );
    }

    #[test]
    fn test_ctrl_backtick_accepts_the_legacy_terminal_nul_encoding() {
        let mut state = DiagnosticConsoleState::default();
        assert!(
            state.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL)),
            "legacy terminals encode Ctrl+` as the same NUL control event as Ctrl+Space"
        );
        assert!(state.is_open(), "the legacy encoding must open the console");
    }
}
