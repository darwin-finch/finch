// Tabbed Dialog - Multi-question dialog with tab navigation
//
// Allows Claude to ask multiple questions simultaneously with tab-based navigation
// similar to Claude Code's implementation.

use crate::cli::llm_dialogs::Question;
#[cfg(test)]
use crate::cli::llm_dialogs::QuestionOption;
use crossterm::event::{KeyCode, KeyEvent};
use std::collections::{HashMap, HashSet};

/// Result from a tabbed dialog
#[derive(Debug, Clone, PartialEq)]
pub enum TabbedDialogResult {
    /// All questions answered successfully
    Completed(HashMap<String, String>),
    /// User cancelled
    Cancelled,
}

/// State for a single question tab
#[derive(Debug, Clone)]
pub struct TabState {
    /// The question being asked
    pub question: Question,
    /// Current selection (for single-select) or cursor position (for multi-select)
    pub selected_index: usize,
    /// Selected indices for multi-select
    pub selected_indices: HashSet<usize>,
    /// Custom text input if active
    pub custom_input: Option<String>,
    /// Whether custom input mode is active
    pub custom_mode_active: bool,
    /// Char-index cursor position inside `custom_input`
    pub custom_cursor_pos: usize,
    /// Whether this question has been answered
    pub answered: bool,
    /// The answer provided (if answered)
    pub answer: Option<String>,
}

impl TabState {
    fn new(question: Question) -> Self {
        Self {
            question,
            selected_index: 0,
            selected_indices: HashSet::new(),
            custom_input: Some(String::new()),
            custom_mode_active: false,
            custom_cursor_pos: 0,
            answered: false,
            answer: None,
        }
    }

    /// Convert a char-index to its byte offset in `s`.
    fn char_to_byte_offset(s: &str, char_pos: usize) -> usize {
        s.char_indices()
            .nth(char_pos)
            .map(|(i, _)| i)
            .unwrap_or(s.len())
    }

    /// Get the current answer (either selected option or custom text)
    fn get_answer(&self) -> Option<String> {
        if self.custom_mode_active {
            // In custom mode, return custom text if non-empty
            self.custom_input
                .as_ref()
                .filter(|s| !s.trim().is_empty())
                .cloned()
        } else if self.question.multi_select {
            // Multi-select: join selected labels
            let labels: Vec<String> = self
                .selected_indices
                .iter()
                .filter_map(|&idx| {
                    if idx < self.question.options.len() {
                        Some(self.question.options[idx].label.clone())
                    } else {
                        None
                    }
                })
                .collect();

            if labels.is_empty() {
                None
            } else {
                Some(labels.join(", "))
            }
        } else {
            // Single-select: get selected option
            if self.selected_index < self.question.options.len() {
                Some(self.question.options[self.selected_index].label.clone())
            } else {
                None
            }
        }
    }

    /// Mark this tab as answered with the current selection
    fn mark_answered(&mut self) {
        if let Some(answer) = self.get_answer() {
            self.answer = Some(answer);
            self.answered = true;
        }
    }
}

/// Tabbed dialog for multiple questions
#[derive(Clone)]
pub struct TabbedDialog {
    /// All question tabs
    tabs: Vec<TabState>,
    /// Currently active tab index
    current_tab: usize,
    /// Title for the dialog (optional)
    title: Option<String>,
}

impl TabbedDialog {
    /// Create a new tabbed dialog from questions
    pub fn new(questions: Vec<Question>, title: Option<String>) -> Self {
        let tabs = questions.into_iter().map(TabState::new).collect();
        Self {
            tabs,
            current_tab: 0,
            title,
        }
    }

    /// Get the current tab state
    pub fn current_tab(&self) -> &TabState {
        &self.tabs[self.current_tab]
    }

    /// Get mutable reference to current tab state
    fn current_tab_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.current_tab]
    }

    /// Get all tabs (for rendering)
    pub fn tabs(&self) -> &[TabState] {
        &self.tabs
    }

    /// Get current tab index
    pub fn current_tab_index(&self) -> usize {
        self.current_tab
    }

    /// Get title
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Check if all questions have been answered
    pub fn all_answered(&self) -> bool {
        self.tabs.iter().all(|tab| tab.answered)
    }

    /// Get all answers as a HashMap
    pub fn collect_answers(&self) -> HashMap<String, String> {
        self.tabs
            .iter()
            .filter_map(|tab| {
                tab.answer
                    .as_ref()
                    .map(|answer| (tab.question.question.clone(), answer.clone()))
            })
            .collect()
    }

    /// Handle a key event and return a result if dialog should close
    pub fn handle_key_event(&mut self, key: KeyEvent) -> Option<TabbedDialogResult> {
        let tab = self.current_tab_mut();

        // Priority 1: Handle custom text input mode
        if tab.custom_mode_active {
            return self.handle_custom_input_key(key);
        }

        // Priority 2: Global navigation keys
        match key.code {
            // Tab switching
            KeyCode::Left | KeyCode::Char('h') => {
                if self.current_tab > 0 {
                    self.current_tab -= 1;
                }
                return None;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if self.current_tab < self.tabs.len() - 1 {
                    self.current_tab += 1;
                }
                return None;
            }
            // Enter: Save current answer and move to next (or submit if last)
            KeyCode::Enter => {
                // Mark current tab as answered
                let tab = self.current_tab_mut();
                if tab.get_answer().is_some() {
                    tab.mark_answered();

                    // If this is the last tab and all are answered, submit
                    if self.current_tab == self.tabs.len() - 1 {
                        if self.all_answered() {
                            return Some(TabbedDialogResult::Completed(self.collect_answers()));
                        }
                    } else {
                        // Move to next tab
                        self.current_tab += 1;
                    }
                }
                return None;
            }
            // Esc: Cancel
            KeyCode::Esc => {
                return Some(TabbedDialogResult::Cancelled);
            }
            // 'o' or 'O': Enter custom mode
            KeyCode::Char('o') | KeyCode::Char('O') => {
                let tab = self.current_tab_mut();
                tab.custom_mode_active = true;
                return None;
            }
            _ => {}
        }

        // Priority 3: Handle current tab's input
        let tab = self.current_tab_mut();
        if tab.question.multi_select {
            self.handle_multiselect_key(key)
        } else {
            self.handle_select_key(key)
        }
    }

    /// Handle key events for single-select questions
    fn handle_select_key(&mut self, key: KeyEvent) -> Option<TabbedDialogResult> {
        let tab = self.current_tab_mut();

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                tab.selected_index = tab.selected_index.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                tab.selected_index =
                    (tab.selected_index + 1).min(tab.question.options.len().saturating_sub(1));
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                let num = c.to_digit(10).unwrap() as usize;
                if num > 0 && num <= tab.question.options.len() {
                    tab.selected_index = num - 1;
                }
            }
            _ => {}
        }
        None
    }

    /// Handle key events for multi-select questions
    fn handle_multiselect_key(&mut self, key: KeyEvent) -> Option<TabbedDialogResult> {
        let tab = self.current_tab_mut();

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                tab.selected_index = tab.selected_index.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                tab.selected_index =
                    (tab.selected_index + 1).min(tab.question.options.len().saturating_sub(1));
            }
            KeyCode::Char(' ') => {
                // Toggle selection at cursor
                if tab.selected_indices.contains(&tab.selected_index) {
                    tab.selected_indices.remove(&tab.selected_index);
                } else {
                    tab.selected_indices.insert(tab.selected_index);
                }
            }
            _ => {}
        }
        None
    }

    /// Handle key events when in custom text input mode
    fn handle_custom_input_key(&mut self, key: KeyEvent) -> Option<TabbedDialogResult> {
        let tab = self.current_tab_mut();

        match key.code {
            KeyCode::Char(c) => {
                if let Some(ref mut input) = tab.custom_input {
                    let byte_pos = TabState::char_to_byte_offset(input, tab.custom_cursor_pos);
                    input.insert(byte_pos, c);
                    tab.custom_cursor_pos += 1;
                }
                None
            }
            KeyCode::Backspace => {
                if tab.custom_cursor_pos > 0 {
                    if let Some(ref mut input) = tab.custom_input {
                        tab.custom_cursor_pos -= 1;
                        let byte_pos = TabState::char_to_byte_offset(input, tab.custom_cursor_pos);
                        input.remove(byte_pos);
                    }
                }
                None
            }
            KeyCode::Delete => {
                if let Some(ref mut input) = tab.custom_input {
                    let char_count = input.chars().count();
                    if tab.custom_cursor_pos < char_count {
                        let byte_pos = TabState::char_to_byte_offset(input, tab.custom_cursor_pos);
                        input.remove(byte_pos);
                    }
                }
                None
            }
            KeyCode::Left => {
                tab.custom_cursor_pos = tab.custom_cursor_pos.saturating_sub(1);
                None
            }
            KeyCode::Right => {
                if let Some(ref input) = tab.custom_input {
                    let char_count = input.chars().count();
                    if tab.custom_cursor_pos < char_count {
                        tab.custom_cursor_pos += 1;
                    }
                }
                None
            }
            KeyCode::Home => {
                tab.custom_cursor_pos = 0;
                None
            }
            KeyCode::End => {
                if let Some(ref input) = tab.custom_input {
                    tab.custom_cursor_pos = input.chars().count();
                }
                None
            }
            KeyCode::Enter => {
                // Save custom text and move to next tab
                if let Some(answer) = tab.get_answer() {
                    tab.answer = Some(answer);
                    tab.answered = true;
                    tab.custom_mode_active = false;
                    tab.custom_cursor_pos = 0;

                    // Move to next tab or submit if last
                    if self.current_tab == self.tabs.len() - 1 {
                        if self.all_answered() {
                            return Some(TabbedDialogResult::Completed(self.collect_answers()));
                        }
                    } else {
                        self.current_tab += 1;
                    }
                }
                None
            }
            KeyCode::Esc => {
                // Exit custom mode (return to normal selection)
                tab.custom_mode_active = false;
                if let Some(ref mut input) = tab.custom_input {
                    input.clear();
                }
                tab.custom_cursor_pos = 0;
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn make_question(text: &str, options: &[&str], multi: bool) -> Question {
        Question {
            question: text.to_string(),
            header: text[..text.len().min(12)].to_string(),
            options: options
                .iter()
                .map(|&label| QuestionOption {
                    label: label.to_string(),
                    description: format!("{label} description"),
                    markdown: None,
                })
                .collect(),
            multi_select: multi,
        }
    }

    fn single_tab_dialog(options: &[&str]) -> TabbedDialog {
        TabbedDialog::new(vec![make_question("Pick one?", options, false)], None)
    }

    fn multi_tab_dialog(options: &[&str]) -> TabbedDialog {
        TabbedDialog::new(vec![make_question("Pick many?", options, true)], None)
    }

    // --- basic construction ---

    #[test]
    fn test_new_dialog_starts_at_tab_zero() {
        let d = single_tab_dialog(&["A", "B", "C"]);
        assert_eq!(d.current_tab_index(), 0);
    }

    #[test]
    fn test_new_dialog_not_answered() {
        let d = single_tab_dialog(&["A", "B"]);
        assert!(!d.all_answered());
    }

    #[test]
    fn test_title_stored_correctly() {
        let d = TabbedDialog::new(
            vec![make_question("Q?", &["A"], false)],
            Some("My Title".to_string()),
        );
        assert_eq!(d.title(), Some("My Title"));
    }

    #[test]
    fn test_no_title_is_none() {
        let d = single_tab_dialog(&["A"]);
        assert!(d.title().is_none());
    }

    // --- Esc cancels immediately ---

    #[test]
    fn test_esc_returns_cancelled() {
        let mut d = single_tab_dialog(&["A", "B"]);
        let result = d.handle_key_event(key(KeyCode::Esc));
        assert_eq!(result, Some(TabbedDialogResult::Cancelled));
    }

    // --- single-select navigation ---

    #[test]
    fn test_down_moves_selection_forward() {
        let mut d = single_tab_dialog(&["A", "B", "C"]);
        assert_eq!(d.current_tab().selected_index, 0);
        d.handle_key_event(key(KeyCode::Down));
        assert_eq!(d.current_tab().selected_index, 1);
        d.handle_key_event(key(KeyCode::Down));
        assert_eq!(d.current_tab().selected_index, 2);
    }

    #[test]
    fn test_down_stops_at_last_option() {
        let mut d = single_tab_dialog(&["A", "B"]);
        d.handle_key_event(key(KeyCode::Down));
        d.handle_key_event(key(KeyCode::Down)); // Can't go past last
        assert_eq!(d.current_tab().selected_index, 1);
    }

    #[test]
    fn test_up_moves_selection_backward() {
        let mut d = single_tab_dialog(&["A", "B", "C"]);
        d.handle_key_event(key(KeyCode::Down));
        d.handle_key_event(key(KeyCode::Down));
        assert_eq!(d.current_tab().selected_index, 2);
        d.handle_key_event(key(KeyCode::Up));
        assert_eq!(d.current_tab().selected_index, 1);
    }

    #[test]
    fn test_up_stops_at_zero() {
        let mut d = single_tab_dialog(&["A", "B"]);
        d.handle_key_event(key(KeyCode::Up)); // Already at 0
        assert_eq!(d.current_tab().selected_index, 0);
    }

    #[test]
    fn test_number_key_selects_option() {
        let mut d = single_tab_dialog(&["A", "B", "C"]);
        d.handle_key_event(key(KeyCode::Char('2')));
        assert_eq!(d.current_tab().selected_index, 1); // 0-indexed
    }

    #[test]
    fn test_number_key_out_of_range_ignored() {
        let mut d = single_tab_dialog(&["A", "B"]);
        d.handle_key_event(key(KeyCode::Char('9'))); // Out of range
        assert_eq!(d.current_tab().selected_index, 0); // No change
    }

    // --- Enter to submit single-tab ---

    #[test]
    fn test_enter_on_single_tab_completes_dialog() {
        let mut d = single_tab_dialog(&["Alpha", "Beta"]);
        let result = d.handle_key_event(key(KeyCode::Enter));
        assert!(
            matches!(result, Some(TabbedDialogResult::Completed(_))),
            "Enter on answered single-tab should complete"
        );
    }

    #[test]
    fn test_enter_result_contains_correct_answer() {
        let mut d = single_tab_dialog(&["Alpha", "Beta", "Gamma"]);
        d.handle_key_event(key(KeyCode::Down)); // Select "Beta"
        let result = d.handle_key_event(key(KeyCode::Enter)).unwrap();
        match result {
            TabbedDialogResult::Completed(answers) => {
                let answer = answers.get("Pick one?").unwrap();
                assert_eq!(answer, "Beta");
            }
            _ => panic!("Expected Completed"),
        }
    }

    // --- multi-tab navigation ---

    #[test]
    fn test_right_arrow_switches_to_next_tab() {
        let mut d = TabbedDialog::new(
            vec![
                make_question("Q1?", &["A", "B"], false),
                make_question("Q2?", &["X", "Y"], false),
            ],
            None,
        );
        assert_eq!(d.current_tab_index(), 0);
        d.handle_key_event(key(KeyCode::Right));
        assert_eq!(d.current_tab_index(), 1);
    }

    #[test]
    fn test_left_arrow_switches_to_previous_tab() {
        let mut d = TabbedDialog::new(
            vec![
                make_question("Q1?", &["A", "B"], false),
                make_question("Q2?", &["X", "Y"], false),
            ],
            None,
        );
        d.handle_key_event(key(KeyCode::Right)); // Go to tab 1
        d.handle_key_event(key(KeyCode::Left)); // Back to tab 0
        assert_eq!(d.current_tab_index(), 0);
    }

    #[test]
    fn test_right_does_not_go_past_last_tab() {
        let mut d = single_tab_dialog(&["A"]);
        d.handle_key_event(key(KeyCode::Right)); // Already at last
        assert_eq!(d.current_tab_index(), 0);
    }

    #[test]
    fn test_enter_advances_to_next_tab_if_not_last() {
        let mut d = TabbedDialog::new(
            vec![
                make_question("Q1?", &["A", "B"], false),
                make_question("Q2?", &["X", "Y"], false),
            ],
            None,
        );
        let result = d.handle_key_event(key(KeyCode::Enter)); // Answer first tab
        assert!(
            result.is_none(),
            "should advance to next tab, not complete yet"
        );
        assert_eq!(d.current_tab_index(), 1);
    }

    #[test]
    fn test_multi_tab_completes_after_all_answered() {
        let mut d = TabbedDialog::new(
            vec![
                make_question("Q1?", &["A", "B"], false),
                make_question("Q2?", &["X", "Y"], false),
            ],
            None,
        );
        d.handle_key_event(key(KeyCode::Enter)); // Answer Q1, advance to Q2
        let result = d.handle_key_event(key(KeyCode::Enter)); // Answer Q2
        assert!(
            matches!(result, Some(TabbedDialogResult::Completed(_))),
            "second Enter should complete dialog"
        );
    }

    // --- multi-select ---

    #[test]
    fn test_space_toggles_multi_select_option() {
        let mut d = multi_tab_dialog(&["A", "B", "C"]);
        d.handle_key_event(key(KeyCode::Char(' '))); // Select index 0
        assert!(d.current_tab().selected_indices.contains(&0));
        d.handle_key_event(key(KeyCode::Char(' '))); // Deselect index 0
        assert!(!d.current_tab().selected_indices.contains(&0));
    }

    #[test]
    fn test_multi_select_multiple_options() {
        let mut d = multi_tab_dialog(&["A", "B", "C"]);
        d.handle_key_event(key(KeyCode::Char(' '))); // Select A
        d.handle_key_event(key(KeyCode::Down)); // Move to B
        d.handle_key_event(key(KeyCode::Char(' '))); // Select B
        assert_eq!(d.current_tab().selected_indices.len(), 2);
    }

    #[test]
    fn test_multi_select_enter_includes_all_selected() {
        let mut d = multi_tab_dialog(&["A", "B", "C"]);
        d.handle_key_event(key(KeyCode::Char(' '))); // Select A
        d.handle_key_event(key(KeyCode::Down));
        d.handle_key_event(key(KeyCode::Char(' '))); // Select B
        let result = d.handle_key_event(key(KeyCode::Enter)).unwrap();
        match result {
            TabbedDialogResult::Completed(answers) => {
                let answer = answers.get("Pick many?").unwrap();
                // The answer should contain both A and B (order may vary)
                assert!(answer.contains('A'), "answer should contain A: {answer}");
                assert!(answer.contains('B'), "answer should contain B: {answer}");
            }
            _ => panic!("Expected Completed"),
        }
    }

    // --- custom input mode ---

    #[test]
    fn test_o_key_activates_custom_mode() {
        let mut d = single_tab_dialog(&["A", "B"]);
        d.handle_key_event(key(KeyCode::Char('o')));
        assert!(d.current_tab().custom_mode_active);
    }

    #[test]
    fn test_custom_mode_chars_append_to_input() {
        let mut d = single_tab_dialog(&["A", "B"]);
        d.handle_key_event(key(KeyCode::Char('o'))); // Enter custom mode
        d.handle_key_event(key(KeyCode::Char('h')));
        d.handle_key_event(key(KeyCode::Char('i')));
        assert_eq!(d.current_tab().custom_input.as_deref(), Some("hi"));
    }

    #[test]
    fn test_custom_mode_backspace_removes_char() {
        let mut d = single_tab_dialog(&["A", "B"]);
        d.handle_key_event(key(KeyCode::Char('o')));
        d.handle_key_event(key(KeyCode::Char('h')));
        d.handle_key_event(key(KeyCode::Char('i')));
        d.handle_key_event(key(KeyCode::Backspace));
        assert_eq!(d.current_tab().custom_input.as_deref(), Some("h"));
    }

    #[test]
    fn test_custom_mode_esc_exits_without_saving() {
        let mut d = single_tab_dialog(&["A", "B"]);
        d.handle_key_event(key(KeyCode::Char('o'))); // Enter custom mode
        d.handle_key_event(key(KeyCode::Char('x')));
        d.handle_key_event(key(KeyCode::Esc)); // Exit custom mode
        assert!(!d.current_tab().custom_mode_active);
        // Input should be cleared
        assert_eq!(d.current_tab().custom_input.as_deref(), Some(""));
    }

    #[test]
    fn test_custom_mode_cursor_movement() {
        let mut d = single_tab_dialog(&["A", "B"]);
        d.handle_key_event(key(KeyCode::Char('o'))); // enter custom mode
        d.handle_key_event(key(KeyCode::Char('a')));
        d.handle_key_event(key(KeyCode::Char('c')));
        // cursor is at 2. Move left and insert 'b' between 'a' and 'c'
        d.handle_key_event(key(KeyCode::Home));
        assert_eq!(d.current_tab().custom_cursor_pos, 0);
        d.handle_key_event(key(KeyCode::Right));
        d.handle_key_event(key(KeyCode::Char('b')));
        assert_eq!(d.current_tab().custom_input.as_deref(), Some("abc"));
    }

    #[test]
    fn test_custom_mode_delete_key() {
        let mut d = single_tab_dialog(&["A", "B"]);
        d.handle_key_event(key(KeyCode::Char('o')));
        d.handle_key_event(key(KeyCode::Char('a')));
        d.handle_key_event(key(KeyCode::Char('b')));
        d.handle_key_event(key(KeyCode::Char('c')));
        // Move to pos 1, delete 'b'
        d.handle_key_event(key(KeyCode::Home));
        d.handle_key_event(key(KeyCode::Right));
        d.handle_key_event(key(KeyCode::Delete));
        assert_eq!(d.current_tab().custom_input.as_deref(), Some("ac"));
    }

    #[test]
    fn test_custom_mode_esc_resets_cursor() {
        let mut d = single_tab_dialog(&["A", "B"]);
        d.handle_key_event(key(KeyCode::Char('o')));
        d.handle_key_event(key(KeyCode::Char('x')));
        d.handle_key_event(key(KeyCode::Char('y')));
        assert_eq!(d.current_tab().custom_cursor_pos, 2);
        d.handle_key_event(key(KeyCode::Esc));
        assert_eq!(d.current_tab().custom_cursor_pos, 0);
        assert!(!d.current_tab().custom_mode_active);
    }

    #[test]
    fn test_custom_mode_enter_saves_and_completes() {
        let mut d = single_tab_dialog(&["A", "B"]);
        d.handle_key_event(key(KeyCode::Char('o')));
        d.handle_key_event(key(KeyCode::Char('m')));
        d.handle_key_event(key(KeyCode::Char('y')));
        let result = d.handle_key_event(key(KeyCode::Enter)).unwrap();
        match result {
            TabbedDialogResult::Completed(answers) => {
                let answer = answers.get("Pick one?").unwrap();
                assert_eq!(answer, "my");
            }
            _ => panic!("Expected Completed"),
        }
    }

    // --- collect_answers ---

    #[test]
    fn test_collect_answers_empty_before_any_answered() {
        let d = single_tab_dialog(&["A"]);
        assert!(d.collect_answers().is_empty());
    }

    #[test]
    fn test_tabs_returns_all_tabs() {
        let d = TabbedDialog::new(
            vec![
                make_question("Q1?", &["A"], false),
                make_question("Q2?", &["B"], false),
                make_question("Q3?", &["C"], false),
            ],
            None,
        );
        assert_eq!(d.tabs().len(), 3);
    }
}
