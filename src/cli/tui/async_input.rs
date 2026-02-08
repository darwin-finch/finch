// Async input handler for TUI - non-blocking keyboard polling

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

use super::TuiRenderer;

/// Spawn a background task that polls keyboard input and sends to channel
///
/// This enables non-blocking input handling in the event loop:
/// - Polls keyboard with 100ms timeout (non-blocking)
/// - Sends completed lines to channel
/// - Handles Enter key to submit input
/// - Handles all other keys via TextArea
/// - Renders TUI periodically
pub fn spawn_input_task(
    tui_renderer: Arc<Mutex<TuiRenderer>>,
) -> mpsc::UnboundedReceiver<String> {
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        loop {
            let input_result: Result<Option<String>> = {
                let mut tui = tui_renderer.lock().await;

                // Poll with short timeout (100ms) to avoid blocking
                if crossterm::event::poll(Duration::from_millis(100))
                    .unwrap_or(false)
                {
                    match crossterm::event::read() {
                        Ok(Event::Key(key)) if key.code == KeyCode::Enter => {
                            // User pressed Enter - extract input and send
                            let input = tui.input_textarea.lines().join("\n");
                            if !input.trim().is_empty() {
                                // Clear textarea for next input
                                tui.input_textarea = create_clean_textarea();
                                Ok(Some(input))
                            } else {
                                Ok(None) // Empty input, ignore
                            }
                        }
                        Ok(Event::Key(key)) => {
                            // Pass key event to textarea
                            tui.input_textarea.input(Event::Key(key));
                            Ok(None)
                        }
                        Ok(_) => Ok(None), // Ignore other events (mouse, resize, etc.)
                        Err(e) => Err(anyhow::anyhow!("Failed to read input: {}", e)),
                    }
                } else {
                    // No input available, just render
                    Ok(None)
                }
            };

            match input_result {
                Ok(Some(input)) => {
                    // Send input to event loop
                    if tx.send(input).is_err() {
                        // Channel closed, exit task
                        break;
                    }
                }
                Ok(None) => {
                    // No input, continue polling
                }
                Err(e) => {
                    // Error reading input, log and continue
                    eprintln!("Input error: {}", e);
                }
            }

            // Render TUI periodically
            {
                let mut tui = tui_renderer.lock().await;
                if let Err(e) = tui.render() {
                    eprintln!("Render error: {}", e);
                    // Don't break on render errors, just log them
                }
            }

            // Small delay to prevent CPU spinning
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });

    rx
}

/// Helper to create a clean text area (needs to be accessible)
fn create_clean_textarea() -> tui_textarea::TextArea<'static> {
    let mut textarea = tui_textarea::TextArea::default();
    textarea.set_placeholder_text("Type your message...");

    use ratatui::style::{Modifier, Style};

    let clean_style = Style::default();

    textarea.set_style(clean_style);
    textarea.set_cursor_line_style(clean_style);
    textarea.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
    textarea.set_selection_style(clean_style);
    textarea.set_placeholder_style(clean_style);

    textarea
}
