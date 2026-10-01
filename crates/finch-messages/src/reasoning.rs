//! Bounded, stateful sanitisation for provider reasoning shown only in the live UI.

pub(crate) const MAX_REASONING_BYTES: usize = 128 * 1024;
pub(crate) const MAX_REASONING_LINES: usize = 1024;
pub(crate) const MAX_REASONING_SCALARS_PER_LINE: usize = 512;
pub(crate) const REASONING_TRUNCATION_MARKER: &str = "[Provider reasoning truncated]";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum EscapeState {
    #[default]
    Ground,
    Escape,
    Csi,
    Osc,
    OscEscape,
}

/// Incremental sanitizer whose parser state survives provider delta boundaries.
#[derive(Clone, Debug, Default)]
pub(crate) struct ReasoningSanitizer {
    state: EscapeState,
    lines: Vec<String>,
    input_bytes: usize,
    truncated: bool,
}

impl ReasoningSanitizer {
    pub(crate) fn push(&mut self, chunk: &str) {
        if self.truncated {
            return;
        }
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        for ch in chunk.chars() {
            let bytes = ch.len_utf8();
            if self.input_bytes.saturating_add(bytes) > MAX_REASONING_BYTES {
                self.truncated = true;
                break;
            }
            self.input_bytes += bytes;
            self.push_char(ch);
            if self.truncated {
                break;
            }
        }
    }

    fn push_char(&mut self, ch: char) {
        match self.state {
            EscapeState::Ground => match ch {
                '\u{1b}' => self.state = EscapeState::Escape,
                '\u{9b}' => self.state = EscapeState::Csi,
                '\u{9d}' => self.state = EscapeState::Osc,
                '\n' => self.newline(),
                '\t' => {
                    for _ in 0..4 {
                        self.push_printable(' ');
                        if self.truncated {
                            break;
                        }
                    }
                }
                _ if ch.is_control() || ('\u{80}'..='\u{9f}').contains(&ch) => {}
                _ => self.push_printable(ch),
            },
            EscapeState::Escape => {
                self.state = match ch {
                    '[' => EscapeState::Csi,
                    ']' => EscapeState::Osc,
                    '\u{1b}' => EscapeState::Escape,
                    _ => EscapeState::Ground,
                };
            }
            EscapeState::Csi => {
                if ('\u{40}'..='\u{7e}').contains(&ch) {
                    self.state = EscapeState::Ground;
                }
            }
            EscapeState::Osc => match ch {
                '\u{7}' => self.state = EscapeState::Ground,
                '\u{1b}' => self.state = EscapeState::OscEscape,
                _ => {}
            },
            EscapeState::OscEscape => {
                self.state = if ch == '\\' {
                    EscapeState::Ground
                } else if ch == '\u{1b}' {
                    EscapeState::OscEscape
                } else {
                    EscapeState::Osc
                };
            }
        }
    }

    fn newline(&mut self) {
        if self.lines.len() >= MAX_REASONING_LINES {
            self.truncated = true;
            return;
        }
        self.lines.push(String::new());
    }

    fn push_printable(&mut self, ch: char) {
        let line = self.lines.last_mut().expect("reasoning has an active line");
        if line.chars().count() >= MAX_REASONING_SCALARS_PER_LINE {
            self.truncated = true;
            return;
        }
        line.push(ch);
    }

    pub(crate) fn lines(&self) -> Vec<String> {
        let mut lines = self.lines.clone();
        if self.truncated {
            lines.push(REASONING_TRUNCATION_MARKER.to_string());
        }
        lines
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.lines.iter().all(|line| line.trim().is_empty()) && !self.truncated
    }

    pub(crate) fn word_count(&self) -> usize {
        self.lines
            .iter()
            .flat_map(|line| line.split_whitespace())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reasoning_sanitizer_hides_split_controls_and_expands_tabs() {
        let mut sanitizer = ReasoningSanitizer::default();
        for chunk in ["safe\u{1b}", "[31", "mred\u{1b}]0;title\u{1b}", "\\\tend"] {
            sanitizer.push(chunk);
        }
        assert_eq!(sanitizer.lines(), vec!["safered    end"]);
    }

    #[test]
    fn test_reasoning_sanitizer_discards_unterminated_sequence() {
        let mut sanitizer = ReasoningSanitizer::default();
        sanitizer.push("visible\u{1b}]hidden");
        assert_eq!(sanitizer.lines(), vec!["visible"]);
    }

    #[test]
    fn test_reasoning_sanitizer_adds_one_marker_and_rejects_late_input_at_limit() {
        let mut sanitizer = ReasoningSanitizer::default();
        sanitizer.push(&"x".repeat(MAX_REASONING_SCALARS_PER_LINE + 1));
        sanitizer.push("late");
        let lines = sanitizer.lines();
        assert_eq!(lines[0].chars().count(), MAX_REASONING_SCALARS_PER_LINE);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.as_str() == REASONING_TRUNCATION_MARKER)
                .count(),
            1
        );
        assert!(!lines.join("\n").contains("late"));
    }

    #[test]
    fn test_reasoning_sanitizer_enforces_byte_and_line_bounds_with_one_marker() {
        let mut bytes = ReasoningSanitizer::default();
        let byte_heavy = format!("{}\n", "🦜".repeat(400)).repeat(100);
        bytes.push(&byte_heavy);
        assert_eq!(
            bytes
                .lines()
                .iter()
                .filter(|line| line.as_str() == REASONING_TRUNCATION_MARKER)
                .count(),
            1,
            "byte truncation must append exactly one marker"
        );

        let mut lines = ReasoningSanitizer::default();
        lines.push(&"\n".repeat(MAX_REASONING_LINES));
        let projected = lines.lines();
        assert_eq!(
            projected
                .iter()
                .filter(|line| line.as_str() == REASONING_TRUNCATION_MARKER)
                .count(),
            1,
            "line truncation must append exactly one marker"
        );
        assert_eq!(
            projected.len(),
            MAX_REASONING_LINES + 1,
            "bounded lines plus the single marker are the only projection"
        );
    }
}
