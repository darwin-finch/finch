// Tabbed Dialog - Multi-question dialog with tab navigation
//
// Allows Claude to ask multiple questions simultaneously with tab-based navigation
// similar to Claude Code's implementation.

use crate::cli::llm_dialogs::{Question, QuestionOption};
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
            answered: false,
            answer: None,
        }
    }

    /// Get the current answer (either selected option or custom text)
    fn get_answer(&self) -> Option<String> {
        if self.custom_mode_active {
            // In custom mode, return custom text if non-empty
            self.custom_input.as_ref()
                .filter(|s| !s.trim().is_empty())
                .cloned()
        } else if self.question.multi_select {
            // Multi-select: join selected labels
            let labels: Vec<String> = self.selected_indices.iter()
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
        self.tabs.iter()
            .filter_map(|tab| {
                tab.answer.as_ref().map(|answer| {
                    (tab.question.question.clone(), answer.clone())
                })
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
                tab.selected_index = (tab.selected_index + 1)
                    .min(tab.question.options.len().saturating_sub(1));
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
                tab.selected_index = (tab.selected_index + 1)
                    .min(tab.question.options.len().saturating_sub(1));
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
                // Insert character
                if let Some(ref mut input) = tab.custom_input {
                    input.push(c);
                }
                None
            }
            KeyCode::Backspace => {
                // Delete last character
                if let Some(ref mut input) = tab.custom_input {
                    input.pop();
                }
                None
            }
            KeyCode::Enter => {
                // Save custom text and move to next tab
                if let Some(answer) = tab.get_answer() {
                    tab.answer = Some(answer);
                    tab.answered = true;
                    tab.custom_mode_active = false;

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
                None
            }
            _ => None,
        }
    }
}
