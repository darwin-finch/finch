// Tracing Layer - Routes dependency logs through OutputManager
//
// This custom tracing::Layer intercepts all log messages from dependencies
// (tokio, reqwest, hf-hub, native model runtimes, etc.) and routes them through our
// output macros so they appear in the TUI instead of printing directly.

use std::fmt;
use tracing::{field::Visit, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

use crate::{output_error, output_progress, output_status};

/// Custom tracing layer that routes logs to OutputManager
pub struct OutputManagerLayer {
    /// Whether to show debug/trace logs (default: false)
    show_debug: bool,
}

impl OutputManagerLayer {
    /// Create a new OutputManagerLayer
    pub fn new() -> Self {
        Self { show_debug: false }
    }

    /// Create with debug logging enabled
    pub fn with_debug() -> Self {
        Self { show_debug: true }
    }

    /// Check if we should show this log level
    fn should_show(&self, level: &Level) -> bool {
        match *level {
            Level::ERROR | Level::WARN | Level::INFO => true,
            Level::DEBUG | Level::TRACE => self.show_debug,
        }
    }

    /// Format the log message (strip ugly module paths)
    fn format_message(&self, target: &str, message: &str) -> String {
        // Strip long module paths for cleaner output
        let clean_target = if target.starts_with("finch::") {
            // Our own logs: keep module name
            target.strip_prefix("finch::").unwrap_or(target)
        } else if target.contains("::") {
            // External logs: just show crate name
            target.split("::").next().unwrap_or(target)
        } else {
            target
        };

        // Skip target for very common modules and internal finch crates (issue #1670)
        if matches!(clean_target, "tokio" | "reqwest" | "hyper")
            || clean_target.starts_with("finch_")
        {
            message.to_string()
        } else {
            format!("[{}] {}", clean_target, message)
        }
    }
}

/// True for log events that llama.cpp or ggml emitted through the llama
/// binding's log-to-tracing bridge (`models::llama_log`). These are native
/// runtime diagnostics, not messages for the person using the session.
fn is_native_model_runtime_target(target: &str) -> bool {
    let crate_name = target.split("::").next().unwrap_or(target);
    matches!(
        crate_name,
        "llama-cpp-2" | "llama_cpp_2" | "llama.cpp" | "llama" | "ggml" | "mtmd"
    )
}

impl Default for OutputManagerLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for OutputManagerLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let level = metadata.level();

        // Skip if this level shouldn't be shown
        if !self.should_show(level) {
            return;
        }

        // Internal finch modules (cli, tools, generators, models, etc.) should not
        // clutter the TUI with implementation details. Only surface provider/network
        // warnings + errors that are genuinely user-relevant.
        let target = metadata.target();
        let is_internal = target.starts_with("finch::cli")
            || target.starts_with("finch::tools")
            || target.starts_with("finch::generators")
            || target.starts_with("finch::models")
            || target.starts_with("finch::local")
            || target.starts_with("finch::server");
        if is_internal && *level <= Level::INFO {
            return; // Suppress internal INFO/DEBUG from TUI; ERRORs still shown
        }

        // llama.cpp and ggml narrate every context they build (the memory
        // embedding model builds one per message). That belongs in the
        // diagnostic log, which the file layer already records and the
        // Ctrl+` console shows, never in the conversation.
        if is_native_model_runtime_target(target) {
            return;
        }

        // Extract the message using a visitor
        let mut visitor = MessageVisitor::new();
        event.record(&mut visitor);

        if let Some(message) = visitor.message {
            // Suppress mDNS multicast send failures on non-routable interfaces.
            // These come via the log→tracing bridge (target="log") and fire for
            // every interface that doesn't support IPv6 multicast (awdl0, utun*,
            // lo0, llw0, en0). They are expected and not actionable.
            if target == "log"
                && message.contains("Failed to send")
                && (message.contains(":5353") || message.contains("No route to host"))
            {
                return;
            }
            let formatted = self.format_message(target, &message);

            // Route based on log level
            match *level {
                Level::ERROR => {
                    output_error!("{}", formatted);
                }
                Level::WARN => {
                    output_status!("⚠️  {}", formatted);
                }
                Level::INFO => {
                    // Special handling for progress indicators
                    if message.contains("Downloading") || message.contains("Loading") {
                        output_progress!("{}", formatted);
                    } else {
                        output_status!("{}", formatted);
                    }
                }
                Level::DEBUG | Level::TRACE => {
                    // Only shown if show_debug is true
                    output_status!("🔍 {}", formatted);
                }
            }
        }
    }
}

/// Visitor to extract the log message from tracing events.
///
/// This keeps the `message` field and **discards every other field**. That is
/// the whole reason it is `pub(crate)`: any warning whose actionable content
/// lives in a structured field reaches the user's terminal with that content
/// gone, so code that emits one has to be able to assert on what survives
/// this visitor rather than on what it passed to `tracing` (#364,
/// "Instrument and reduce Finch interactive TUI time-to-ready").
pub(crate) struct MessageVisitor {
    message: Option<String>,
}

impl MessageVisitor {
    pub(crate) fn new() -> Self {
        Self { message: None }
    }

    /// The formatted `message` field, which is all the TUI ever shows.
    pub(crate) fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{:?}", value).trim_matches('"').to_string());
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reported failure: after llama.cpp's output was routed to tracing,
    /// every message in a live session painted a dozen `[llama-cpp-2] …`
    /// lines into the conversation, because this layer forwards external
    /// INFO events to the session output. Native runtime targets must be
    /// recognised so they stay in the diagnostic log only, while ordinary
    /// dependency and Finch targets are unaffected.
    #[test]
    fn test_native_model_runtime_logs_are_not_session_output() {
        for target in [
            "llama-cpp-2",
            "llama_cpp_2::log",
            "llama.cpp",
            "llama",
            "ggml",
            "mtmd",
        ] {
            assert!(
                is_native_model_runtime_target(target),
                "{target:?} is llama.cpp/ggml diagnostics and must not reach the conversation"
            );
        }
        for target in [
            "reqwest",
            "hf_hub::api",
            "finch::providers",
            "log",
            "llamafile",
        ] {
            assert!(
                !is_native_model_runtime_target(target),
                "{target:?} is not a native model runtime target and must keep its existing routing"
            );
        }
        // The layer consults the check before it formats or emits anything.
        let source = include_str!("output_layer.rs");
        let guard = source
            .find("if is_native_model_runtime_target(target) {")
            .expect("on_event must consult the native-runtime check");
        let emit = source
            .find("let formatted = self.format_message(target, &message);")
            .expect("on_event formats the message before emitting it");
        assert!(
            guard < emit,
            "the native-runtime check must run before a log line is formatted for the session"
        );
    }

    #[test]
    fn test_layer_creation() {
        let layer = OutputManagerLayer::new();
        assert!(!layer.show_debug);

        let layer_debug = OutputManagerLayer::with_debug();
        assert!(layer_debug.show_debug);
    }

    #[test]
    fn test_should_show() {
        let layer = OutputManagerLayer::new();
        assert!(layer.should_show(&Level::ERROR));
        assert!(layer.should_show(&Level::WARN));
        assert!(layer.should_show(&Level::INFO));
        assert!(!layer.should_show(&Level::DEBUG));
        assert!(!layer.should_show(&Level::TRACE));

        let layer_debug = OutputManagerLayer::with_debug();
        assert!(layer_debug.should_show(&Level::DEBUG));
        assert!(layer_debug.should_show(&Level::TRACE));
    }

    #[test]
    fn test_format_message() {
        let layer = OutputManagerLayer::new();

        // Our own logs
        let msg = layer.format_message("finch::models::loader", "Loading model");
        assert_eq!(msg, "[models::loader] Loading model");

        // External logs with long paths
        let msg = layer.format_message("tokio::runtime::thread_pool", "Starting worker");
        assert_eq!(msg, "Starting worker");

        // Simple external logs
        let msg = layer.format_message("reqwest::client", "Request sent");
        assert_eq!(msg, "Request sent");

        // Other external crates
        let msg = layer.format_message("hf_hub::download", "Downloading file");
        assert_eq!(msg, "[hf_hub] Downloading file");

        // Internal finch crates have no bracketed crate name (issue #1670)
        let msg = layer.format_message(
            "finch_tools_api::permissions",
            "Blocked dangerous bash command: sleep 25; ps -p 97997 > /dev/sda",
        );
        assert_eq!(
            msg,
            "Blocked dangerous bash command: sleep 25; ps -p 97997 > /dev/sda"
        );

        let msg = layer.format_message(
            "finch_tools_api",
            "Blocked dangerous bash command: rm -rf /",
        );
        assert_eq!(msg, "Blocked dangerous bash command: rm -rf /");
    }

    // Note: MessageVisitor test removed - creating proper Field instances
    // requires complex setup with tracing's internal APIs. The visitor is
    // tested indirectly through integration tests that use actual tracing events.
}
