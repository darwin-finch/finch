//! Test-only application ports for renderer unit tests.
//!
//! Production adapters live in the root CLI. These fixtures exercise the
//! renderer's stateful port contract without making its unit tests depend on
//! the application crate that will consume the extracted renderer.

use std::sync::Mutex;

use finch_messages::{MessageRef, StaticMessage, WorkUnit};
use finch_theme::ColorScheme;

use super::{ActivityUsage, TuiOutputPort, TuiStatusPort};

pub(super) struct OutputManager {
    messages: Mutex<Vec<MessageRef>>,
}

impl OutputManager {
    pub(super) fn new(_colors: ColorScheme) -> Self {
        Self {
            messages: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn get_messages(&self) -> Vec<MessageRef> {
        self.messages.lock().unwrap().clone()
    }

    pub(super) fn add_trait_message(&self, message: MessageRef) {
        self.messages.lock().unwrap().push(message);
    }

    pub(super) fn start_work_unit(&self, verb: impl Into<String>) -> std::sync::Arc<WorkUnit> {
        let unit = std::sync::Arc::new(WorkUnit::new(verb));
        self.add_trait_message(unit.clone());
        unit
    }

    pub(super) fn disable_stdout(&self) {}
}

impl TuiOutputPort for OutputManager {
    fn get_messages(&self) -> Vec<MessageRef> {
        self.get_messages()
    }

    fn add_trait_message(&self, message: MessageRef) {
        self.add_trait_message(message);
    }

    fn write_tool_raw(&self, content: String) {
        self.add_trait_message(std::sync::Arc::new(StaticMessage::plain(content)));
    }

    fn enable_stdout(&self) {}

    fn disable_stdout(&self) {}
}

#[derive(Clone, Copy)]
pub(super) enum StatusLineType {
    AgentActivity,
}

#[derive(Default)]
pub(super) struct StatusBar {
    activity_line: Mutex<Option<String>>,
    operation: Mutex<Option<String>>,
}

impl StatusBar {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn get_line(&self, line_type: &StatusLineType) -> Option<String> {
        match line_type {
            StatusLineType::AgentActivity => self.activity_line.lock().unwrap().clone(),
        }
    }

    pub(super) fn get_status(&self) -> String {
        let activity = self.activity_line.lock().unwrap().clone();
        let operation = self.operation.lock().unwrap().clone();
        [activity, operation]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(super) fn update_agent_activity(&self, active_children: usize, usage: &ActivityUsage) {
        if active_children == 0 {
            *self.activity_line.lock().unwrap() = None;
            return;
        }
        let input = usage
            .input_tokens
            .map(|tokens| tokens.to_string())
            .unwrap_or_else(|| "unavailable".to_string());
        let output = usage
            .output_tokens
            .map(|tokens| tokens.to_string())
            .unwrap_or_else(|| "unavailable".to_string());
        let state = match usage.state {
            super::activity::ActivityUsageState::Complete => "complete",
            super::activity::ActivityUsageState::Partial => "partial",
            super::activity::ActivityUsageState::Unavailable => "unavailable",
        };
        *self.activity_line.lock().unwrap() = Some(format!(
            "Children: {active_children} active | Tokens: {input} input, {output} output | Usage: {state} ({}/{} attempts reported)",
            usage.reported_attempts, usage.started_attempts
        ));
    }
}

impl TuiStatusPort for StatusBar {
    fn status_without_session(&self) -> String {
        self.get_status()
    }

    fn session_label(&self) -> Option<String> {
        None
    }

    fn update_agent_activity(&self, active_children: usize, usage: &ActivityUsage) {
        self.update_agent_activity(active_children, usage);
    }

    fn set_operation(&self, operation: String) {
        *self.operation.lock().unwrap() = Some(operation);
    }

    fn clear_operation(&self) {
        *self.operation.lock().unwrap() = None;
    }
}
