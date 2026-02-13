// Prompt Suggestions - Contextual help for users
//
// Provides Claude Code-style suggestions to help users discover features
// and improve query quality based on current state.
//
// Architecture:
// - Hardcoded suggestions: Basic fallbacks when LLM unavailable
// - LLM-generated suggestions: Uses teacher model to analyze conversation
//   and generate contextual suggestions (like Claude Code)

use std::time::{Duration, Instant};

/// Context for generating suggestions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionContext {
    /// User just started the session
    FirstRun,
    /// Idle - no recent activity
    Idle,
    /// Just completed a successful query
    QueryComplete,
    /// Query resulted in an error
    QueryError,
    /// Model is currently streaming
    Streaming,
    /// Model is loading/downloading
    ModelLoading,
    /// Tool execution in progress
    ToolExecution,
}

/// A single suggestion with optional keyboard shortcut
#[derive(Debug, Clone)]
pub struct Suggestion {
    /// The suggestion text
    pub text: String,
    /// Optional keyboard shortcut (e.g., "Ctrl+C")
    pub shortcut: Option<String>,
    /// Priority (higher = more important, shown first)
    pub priority: u8,
}

impl Suggestion {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            shortcut: None,
            priority: 50, // Default medium priority
        }
    }

    pub fn with_shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Format suggestion for display
    pub fn format(&self) -> String {
        if let Some(ref shortcut) = self.shortcut {
            format!("{} [{}]", self.text, shortcut)
        } else {
            self.text.clone()
        }
    }
}

/// Source of suggestions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionSource {
    /// Hardcoded suggestions based on context
    Hardcoded,
    /// LLM-generated suggestions (like Claude Code)
    LLM,
}

/// Manages contextual suggestions
pub struct SuggestionManager {
    /// Current context
    context: SuggestionContext,
    /// Time when context last changed
    context_changed: Instant,
    /// Whether suggestions are enabled
    enabled: bool,
    /// Number of queries completed in this session
    query_count: usize,
    /// Preferred suggestion source
    source: SuggestionSource,
    /// Cached LLM-generated suggestions
    llm_suggestions: Vec<Suggestion>,
    /// Time when LLM suggestions were last generated
    llm_suggestions_time: Option<Instant>,
}

impl SuggestionManager {
    pub fn new() -> Self {
        Self {
            context: SuggestionContext::FirstRun,
            context_changed: Instant::now(),
            enabled: true,
            query_count: 0,
            source: SuggestionSource::Hardcoded, // Default to hardcoded, can be upgraded to LLM
            llm_suggestions: Vec::new(),
            llm_suggestions_time: None,
        }
    }

    /// Set the preferred suggestion source
    pub fn set_source(&mut self, source: SuggestionSource) {
        self.source = source;
    }

    /// Get the system prompt for generating LLM-based suggestions
    ///
    /// This prompt is inspired by Claude Code's suggestion generators
    pub fn get_suggestion_prompt(
        conversation_history: &str,
        user_stated_intent: Option<&str>,
    ) -> String {
        let base_prompt = "Based on the conversation history, generate 2-3 helpful next action suggestions for the user.\n\n\
            Each suggestion should be:\n\
            - Actionable and specific\n\
            - Relevant to the current context\n\
            - Helpful for discovering features or improving workflow\n\n\
            Format each suggestion on a new line starting with a dash (-).\n\n\
            Conversation history:\n";

        let mut prompt = base_prompt.to_string();
        prompt.push_str(conversation_history);

        if let Some(intent) = user_stated_intent {
            prompt.push_str("\n\nUser's stated next steps: ");
            prompt.push_str(intent);
        }

        prompt.push_str("\n\nSuggestions:");
        prompt
    }

    /// Parse LLM response into Suggestions
    pub fn parse_llm_suggestions(response: &str) -> Vec<Suggestion> {
        response
            .lines()
            .filter(|line| line.trim().starts_with('-'))
            .map(|line| {
                let text = line.trim().trim_start_matches('-').trim();
                Suggestion::new(text).with_priority(70)
            })
            .collect()
    }

    /// Update LLM-generated suggestions (call this when you get response from LLM)
    pub fn update_llm_suggestions(&mut self, suggestions: Vec<Suggestion>) {
        self.llm_suggestions = suggestions;
        self.llm_suggestions_time = Some(Instant::now());
    }

    /// Check if LLM suggestions are stale (older than 60 seconds)
    fn are_llm_suggestions_stale(&self) -> bool {
        match self.llm_suggestions_time {
            None => true,
            Some(time) => time.elapsed() > Duration::from_secs(60),
        }
    }

    /// Update the current context
    pub fn set_context(&mut self, context: SuggestionContext) {
        if context != self.context {
            self.context = context;
            self.context_changed = Instant::now();
        }
    }

    /// Increment query count
    pub fn increment_query_count(&mut self) {
        self.query_count += 1;
    }

    /// Get current suggestions based on context and source
    pub fn get_suggestions(&self) -> Vec<Suggestion> {
        if !self.enabled {
            return vec![];
        }

        let mut suggestions = match self.source {
            SuggestionSource::LLM => {
                // Use LLM suggestions if available and not stale
                if !self.llm_suggestions.is_empty() && !self.are_llm_suggestions_stale() {
                    self.llm_suggestions.clone()
                } else {
                    // Fall back to hardcoded if LLM suggestions unavailable/stale
                    self.get_hardcoded_suggestions()
                }
            }
            SuggestionSource::Hardcoded => {
                self.get_hardcoded_suggestions()
            }
        };

        // Sort by priority (highest first)
        suggestions.sort_by(|a, b| b.priority.cmp(&a.priority));

        // Limit to top 3 suggestions
        suggestions.truncate(3);

        suggestions
    }

    /// Get hardcoded suggestions based on current context
    fn get_hardcoded_suggestions(&self) -> Vec<Suggestion> {
        match self.context {
            SuggestionContext::FirstRun => self.first_run_suggestions(),
            SuggestionContext::Idle => self.idle_suggestions(),
            SuggestionContext::QueryComplete => self.query_complete_suggestions(),
            SuggestionContext::QueryError => self.error_suggestions(),
            SuggestionContext::Streaming => self.streaming_suggestions(),
            SuggestionContext::ModelLoading => self.model_loading_suggestions(),
            SuggestionContext::ToolExecution => self.tool_execution_suggestions(),
        }
    }

    /// Get a single formatted suggestion line for status bar
    pub fn get_suggestion_line(&self) -> Option<String> {
        let suggestions = self.get_suggestions();
        if suggestions.is_empty() {
            return None;
        }

        // Join first 2-3 suggestions with " • "
        let formatted: Vec<String> = suggestions
            .iter()
            .take(2)
            .map(|s| s.format())
            .collect();

        Some(format!("💡 {}", formatted.join(" • ")))
    }

    /// Enable or disable suggestions
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Check if suggestions are enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    // ========================================================================
    // Context-specific suggestion generators
    // ========================================================================

    fn first_run_suggestions(&self) -> Vec<Suggestion> {
        vec![
            Suggestion::new("Type a question to get started")
                .with_priority(100),
            Suggestion::new("Try /help for available commands")
                .with_shortcut("Ctrl+/")
                .with_priority(90),
            Suggestion::new("Use /local to check local model status")
                .with_priority(80),
        ]
    }

    fn idle_suggestions(&self) -> Vec<Suggestion> {
        let time_idle = self.context_changed.elapsed();

        if time_idle > Duration::from_secs(30) {
            // User idle for 30+ seconds
            vec![
                Suggestion::new("Ask a coding question")
                    .with_priority(70),
                Suggestion::new("Try /help to see available commands")
                    .with_shortcut("Ctrl+/")
                    .with_priority(60),
                Suggestion::new("Press Shift+Enter for multi-line input")
                    .with_priority(50),
            ]
        } else {
            // Recently idle
            vec![
                Suggestion::new("Press Shift+Enter for multi-line input")
                    .with_priority(60),
                Suggestion::new("Use ↑/↓ to navigate command history")
                    .with_priority(50),
            ]
        }
    }

    fn query_complete_suggestions(&self) -> Vec<Suggestion> {
        vec![
            Suggestion::new("Rate response with 'g' (good) or 'b' (bad)")
                .with_priority(80),
            Suggestion::new("Ask a follow-up question")
                .with_priority(70),
            Suggestion::new("Use /clear to start a new conversation")
                .with_shortcut("Ctrl+L")
                .with_priority(60),
        ]
    }

    fn error_suggestions(&self) -> Vec<Suggestion> {
        vec![
            Suggestion::new("Try rephrasing your query")
                .with_priority(90),
            Suggestion::new("Check /local status if using local model")
                .with_priority(80),
            Suggestion::new("Use /help for troubleshooting tips")
                .with_shortcut("Ctrl+/")
                .with_priority(70),
        ]
    }

    fn streaming_suggestions(&self) -> Vec<Suggestion> {
        vec![
            Suggestion::new("Press Ctrl+C to cancel streaming")
                .with_shortcut("Ctrl+C")
                .with_priority(100),
            Suggestion::new("Scroll up to view earlier messages")
                .with_shortcut("Shift+PgUp")
                .with_priority(70),
        ]
    }

    fn model_loading_suggestions(&self) -> Vec<Suggestion> {
        vec![
            Suggestion::new("Model is loading... queries will use teacher API")
                .with_priority(100),
            Suggestion::new("First download may take 5-30 minutes")
                .with_priority(90),
            Suggestion::new("You can still ask questions while loading")
                .with_priority(80),
        ]
    }

    fn tool_execution_suggestions(&self) -> Vec<Suggestion> {
        vec![
            Suggestion::new("Tools are executing... please wait")
                .with_priority(90),
            Suggestion::new("Press Ctrl+C to cancel if needed")
                .with_shortcut("Ctrl+C")
                .with_priority(80),
        ]
    }
}

impl Default for SuggestionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_suggestion_creation() {
        let suggestion = Suggestion::new("Test suggestion")
            .with_shortcut("Ctrl+T")
            .with_priority(75);

        assert_eq!(suggestion.text, "Test suggestion");
        assert_eq!(suggestion.shortcut, Some("Ctrl+T".to_string()));
        assert_eq!(suggestion.priority, 75);
        assert_eq!(suggestion.format(), "Test suggestion [Ctrl+T]");
    }

    #[test]
    fn test_suggestion_without_shortcut() {
        let suggestion = Suggestion::new("No shortcut");
        assert_eq!(suggestion.format(), "No shortcut");
    }

    #[test]
    fn test_manager_creation() {
        let manager = SuggestionManager::new();
        assert_eq!(manager.context, SuggestionContext::FirstRun);
        assert!(manager.enabled);
        assert_eq!(manager.query_count, 0);
    }

    #[test]
    fn test_context_switching() {
        let mut manager = SuggestionManager::new();

        manager.set_context(SuggestionContext::Idle);
        assert_eq!(manager.context, SuggestionContext::Idle);

        manager.set_context(SuggestionContext::QueryComplete);
        assert_eq!(manager.context, SuggestionContext::QueryComplete);
    }

    #[test]
    fn test_first_run_suggestions() {
        let manager = SuggestionManager::new();
        let suggestions = manager.get_suggestions();

        assert!(!suggestions.is_empty());
        assert!(suggestions.len() <= 3); // Max 3 suggestions

        // Check that suggestions are sorted by priority
        for i in 1..suggestions.len() {
            assert!(suggestions[i-1].priority >= suggestions[i].priority);
        }
    }

    #[test]
    fn test_suggestion_line_formatting() {
        let manager = SuggestionManager::new();
        let line = manager.get_suggestion_line();

        assert!(line.is_some());
        assert!(line.unwrap().starts_with("💡 "));
    }

    #[test]
    fn test_disable_suggestions() {
        let mut manager = SuggestionManager::new();
        manager.set_enabled(false);

        let suggestions = manager.get_suggestions();
        assert!(suggestions.is_empty());

        let line = manager.get_suggestion_line();
        assert!(line.is_none());
    }

    #[test]
    fn test_query_count_increment() {
        let mut manager = SuggestionManager::new();
        assert_eq!(manager.query_count, 0);

        manager.increment_query_count();
        assert_eq!(manager.query_count, 1);

        manager.increment_query_count();
        assert_eq!(manager.query_count, 2);
    }

    #[test]
    fn test_all_contexts_have_suggestions() {
        let manager = SuggestionManager::new();

        let contexts = vec![
            SuggestionContext::FirstRun,
            SuggestionContext::Idle,
            SuggestionContext::QueryComplete,
            SuggestionContext::QueryError,
            SuggestionContext::Streaming,
            SuggestionContext::ModelLoading,
            SuggestionContext::ToolExecution,
        ];

        for context in contexts {
            let mut test_manager = manager.clone();
            test_manager.set_context(context);
            let suggestions = test_manager.get_suggestions();

            assert!(!suggestions.is_empty(), "Context {:?} should have suggestions", context);
        }
    }
}
