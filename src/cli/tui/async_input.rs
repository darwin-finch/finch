// Async input handler for TUI - non-blocking keyboard polling

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
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
                    // Track if we need to render after processing first event
                    let mut first_event_modified_input = false;

                    // Process first event
                    let first_event_result = match crossterm::event::read() {
                        Ok(Event::Key(key)) => {
                            // Priority 1: Handle active dialog (if any)
                            if tui.active_dialog.is_some() {
                                let dialog_result = if let Some(dialog) = tui.active_dialog.as_mut() {
                                    dialog.handle_key_event(key)
                                } else {
                                    None
                                };

                                if let Some(result) = dialog_result {
                                    // Dialog completed, clear it and store result
                                    tui.active_dialog = None;
                                    tui.pending_dialog_result = Some(result);
                                }
                                Ok(None) // Don't submit input while dialog is active
                            } else if key.code == KeyCode::Enter {
                            // Check if Shift is held (Shift+Enter inserts newline, Enter submits)
                            if key.modifiers.contains(KeyModifiers::SHIFT) {
                                // Shift+Enter: Insert newline (pass to textarea)
                                tui.input_textarea.input(Event::Key(key));
                                first_event_modified_input = true; // Mark for render
                                Ok(None)
                            } else {
                                // Enter without Shift: Submit input
                                let input = tui.input_textarea.lines().join("\n");
                                if !input.trim().is_empty() {
                                    // Add to command history
                                    tui.command_history.push(input.clone());
                                    tui.history_index = None;
                                    tui.history_draft = None; // Clear any saved draft

                                    // Clear textarea for next input
                                    tui.input_textarea = TuiRenderer::create_clean_textarea();
                                    Ok(Some(input))
                                } else {
                                    Ok(None) // Empty input, ignore
                                }
                            }
                            } else {
                            // Priority 3: Handle other keys (feedback shortcuts, history, input)
                            // Check for feedback shortcuts when input is empty
                            let input_empty = tui.input_textarea.lines().join("").trim().is_empty();

                            // Check for special shortcuts and navigation (Ctrl+C, Ctrl+G, Ctrl+B, Up/Down)
                            match (key.code, key.modifiers) {
                                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                                    // Ctrl+C: Cancel query
                                    tui.pending_cancellation = true;
                                    Ok(None)
                                }
                                (KeyCode::Char('g'), m) if m.contains(KeyModifiers::CONTROL) => {
                                    // Ctrl+G: Good feedback
                                    tui.pending_feedback = Some(crate::feedback::FeedbackRating::Good);
                                    Ok(None)
                                }
                                (KeyCode::Char('b'), m) if m.contains(KeyModifiers::CONTROL) => {
                                    // Ctrl+B: Bad feedback
                                    tui.pending_feedback = Some(crate::feedback::FeedbackRating::Bad);
                                    Ok(None)
                                }
                                (KeyCode::BackTab, _) => {
                                    // Shift+Tab: Toggle plan mode (send as command)
                                    Ok(Some("/plan".to_string()))
                                }
                                (KeyCode::Up, KeyModifiers::NONE) => {
                                    // Check cursor position - only navigate history if at top line
                                    let (cursor_row, _cursor_col) = tui.input_textarea.cursor();

                                    if cursor_row == 0 {
                                        // At top line - navigate history backwards (older commands)
                                        if let Some(idx) = tui.history_index {
                                            if idx > 0 {
                                                tui.history_index = Some(idx - 1);
                                                let cmd = &tui.command_history[idx - 1];
                                                tui.input_textarea = TuiRenderer::create_clean_textarea_with_text(cmd);
                                            }
                                        } else if !tui.command_history.is_empty() {
                                            // Save current input as draft before entering history
                                            let current_text = tui.input_textarea.lines().join("\n");
                                            if !current_text.trim().is_empty() {
                                                tui.history_draft = Some(current_text);
                                            }

                                            tui.history_index = Some(tui.command_history.len() - 1);
                                            let cmd = &tui.command_history[tui.command_history.len() - 1];
                                            tui.input_textarea = TuiRenderer::create_clean_textarea_with_text(cmd);
                                        }
                                    } else {
                                        // Not at top - move cursor up within textarea
                                        tui.input_textarea.input(Event::Key(key));
                                        first_event_modified_input = true;
                                    }
                                    Ok(None)
                                }
                                (KeyCode::Down, KeyModifiers::NONE) => {
                                    // Check cursor position - only navigate history if at bottom line
                                    let (cursor_row, _cursor_col) = tui.input_textarea.cursor();
                                    let num_lines = tui.input_textarea.lines().len();
                                    let last_line = num_lines.saturating_sub(1);

                                    if cursor_row >= last_line {
                                        // At bottom line - navigate history forwards (newer commands)
                                        if let Some(idx) = tui.history_index {
                                            if idx < tui.command_history.len() - 1 {
                                                tui.history_index = Some(idx + 1);
                                                let cmd = &tui.command_history[idx + 1];
                                                tui.input_textarea = TuiRenderer::create_clean_textarea_with_text(cmd);
                                            } else {
                                                // At newest entry - restore draft or clear
                                                tui.history_index = None;
                                                if let Some(draft) = tui.history_draft.take() {
                                                    // Restore the saved draft
                                                    tui.input_textarea = TuiRenderer::create_clean_textarea_with_text(&draft);
                                                } else {
                                                    // No draft - clear input
                                                    tui.input_textarea = TuiRenderer::create_clean_textarea();
                                                }
                                            }
                                        }
                                    } else {
                                        // Not at bottom - move cursor down within textarea
                                        tui.input_textarea.input(Event::Key(key));
                                        first_event_modified_input = true;
                                    }
                                    Ok(None)
                                }
                                _ => {
                                    // Pass key event to textarea
                                    tui.input_textarea.input(Event::Key(key));
                                    first_event_modified_input = true; // Mark for render
                                    Ok(None)
                                }
                            }
                            }
                        }
                        Ok(_) => Ok(None), // Ignore other events (mouse, resize, etc.)
                        Err(e) => Err(anyhow::anyhow!("Failed to read input: {}", e)),
                    };

                    // Fast path: Check if more events are immediately available (for paste operations)
                    // Process all available events without delay to make pasting instant
                    let mut had_input = first_event_modified_input;
                    while crossterm::event::poll(Duration::from_millis(0)).unwrap_or(false) {
                        match crossterm::event::read() {
                            Ok(Event::Key(key)) if key.code == KeyCode::Enter => {
                                if key.modifiers.contains(KeyModifiers::SHIFT) {
                                    // Shift+Enter: Insert newline
                                    tui.input_textarea.input(Event::Key(key));
                                    had_input = true;
                                } else {
                                    // Enter without Shift: Stop batch, will be processed next iteration
                                    break;
                                }
                            }
                            Ok(Event::Key(key)) => {
                                // Pass key event to textarea
                                tui.input_textarea.input(Event::Key(key));
                                had_input = true;
                            }
                            Ok(_) => {} // Ignore other events
                            Err(_) => break, // Error, stop batching
                        }
                    }

                    // Render immediately after input (event-driven, not polled)
                    if had_input {
                        if let Err(e) = tui.render() {
                            eprintln!("Render error: {}", e);
                        }
                    }

                    first_event_result
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
                    // Check if channel is closed (event loop exited)
                    if tx.is_closed() {
                        break;
                    }
                }
                Err(e) => {
                    // Error reading input, log and continue
                    eprintln!("Input error: {}", e);
                }
            }

            // Only render when input actually changed (event-driven, not polled)
            // The render will be triggered by event_loop when needed

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
