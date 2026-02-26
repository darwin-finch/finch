// Async input handler for TUI - non-blocking keyboard polling

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex};

use super::TuiRenderer;

// ---------------------------------------------------------------------------
// InputEvent — discriminated input events sent to the event loop
// ---------------------------------------------------------------------------

/// Events produced by the async input task and consumed by the event loop.
#[derive(Debug)]
pub enum InputEvent {
    /// User pressed Enter and submitted a complete query / command.
    Submitted(String),
    /// User is actively typing (debounced, fired at most once every 300 ms).
    TypingStarted(String),
}

/// Check the system clipboard for image data and return it as (base64, media_type) if found.
/// Uses the `arboard` crate for cross-platform clipboard access.
fn try_grab_clipboard_image() -> Option<(String, String)> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let img = clipboard.get_image().ok()?;

    // Convert RGBA pixels to PNG bytes
    let png_bytes = encode_rgba_to_png(img.width, img.height, img.bytes.as_ref())?;
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png_bytes);
    Some((b64, "image/png".to_string()))
}

/// Encode raw RGBA bytes to PNG format.
fn encode_rgba_to_png(width: usize, height: usize, rgba: &[u8]) -> Option<Vec<u8>> {
    use std::io::Cursor;
    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(Cursor::new(&mut buf), width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
    }
    Some(buf)
}

/// Sanitize pasted content to prevent TUI breakage
/// Filters out:
/// - Image escape sequences (kitty, iTerm2, sixel)
/// - Non-printable control characters (except newlines/tabs)
/// - Invalid UTF-8 sequences
fn sanitize_paste_char(c: char) -> bool {
    match c {
        // Allow printable ASCII
        ' '..='~' => true,
        // Allow common whitespace
        '\t' | '\n' | '\r' => true,
        // Allow extended Unicode (for international text)
        '\u{0080}'..='\u{10FFFF}' => {
            // Block private use areas (often used for images)
            !matches!(c, '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}')
        }
        // Block everything else (control chars, escape sequences)
        _ => false,
    }
}

/// Check if a key event should be accepted during paste
/// Filters out problematic characters while allowing normal input
fn should_accept_key_event(key: &KeyEvent) -> bool {
    match &key.code {
        KeyCode::Char(c) => {
            // Apply sanitization filter
            sanitize_paste_char(*c)
        }
        // Allow all other key codes (Enter, Backspace, arrows, etc.)
        _ => true,
    }
}

/// Spawn a background task that polls keyboard input and sends to channel
///
/// This enables non-blocking input handling in the event loop:
/// - Polls keyboard with 100ms timeout (non-blocking)
/// - Sends completed lines to channel
/// - Handles Enter key to submit input
/// - Handles all other keys via TextArea
/// - Renders TUI periodically
/// - Sends `InputEvent::TypingStarted` (debounced, 300 ms) when input changes
pub fn spawn_input_task(tui_renderer: Arc<Mutex<TuiRenderer>>) -> mpsc::UnboundedReceiver<InputEvent> {
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        // Track last time we sent a TypingStarted event for debouncing.
        let mut last_typing_signal: Option<Instant> = None;

        loop {
            // The lock block returns (input_result, typing_hint).
            // typing_hint is Some(content) when text was modified but not submitted.
            let (input_result, typing_hint): (Result<Option<String>>, Option<String>) = {
                let mut tui = tui_renderer.lock().await;

                // Poll with short timeout (100ms) to avoid blocking
                if crossterm::event::poll(Duration::from_millis(100)).unwrap_or(false) {
                    // Track if we need to render after processing first event
                    let mut first_event_modified_input = false;

                    // Process first event
                    let first_event_result = match crossterm::event::read() {
                        Ok(Event::Key(key)) => {
                            // Priority 1: Handle active dialog (if any)
                            if tui.active_dialog.is_some() {
                                let dialog_result = if let Some(dialog) = tui.active_dialog.as_mut()
                                {
                                    dialog.handle_key_event(key)
                                } else {
                                    None
                                };

                                if let Some(result) = dialog_result {
                                    // Dialog completed, clear it and store result
                                    tui.active_dialog = None;
                                    tui.pending_dialog_result = Some(result);
                                }

                                // Mark for render so dialog updates are shown
                                first_event_modified_input = true;

                                Ok(None) // Don't submit input while dialog is active
                            } else if key.code == KeyCode::Enter {
                                // Check if Shift or Alt is held (inserts newline, Enter submits).
                                // Standard VT100 raw mode never sets SHIFT for Enter on macOS
                                // Terminal/iTerm2 — Option+Enter sends \x1b\r, reported as
                                // KeyCode::Enter + KeyModifiers::ALT, which is what we check.
                                // SHIFT is also accepted for terminals that implement it.
                                if key
                                    .modifiers
                                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
                                {
                                    // Shift+Enter / Alt(Option)+Enter: Insert newline (pass to textarea)
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
                                let _input_empty =
                                    tui.input_textarea.lines().join("").trim().is_empty();

                                // Check for special shortcuts and navigation (Ctrl+C, Ctrl+G, Ctrl+B, Up/Down)
                                match (key.code, key.modifiers) {
                                    (KeyCode::Char('c'), m)
                                        if m.contains(KeyModifiers::CONTROL) =>
                                    {
                                        // Ctrl+C: Cancel query
                                        tui.pending_cancellation = true;
                                        Ok(None)
                                    }
                                    // Cmd+V on macOS / Ctrl+V: check clipboard for images
                                    (KeyCode::Char('v'), m)
                                        if m.contains(KeyModifiers::SUPER)
                                            || m.contains(KeyModifiers::CONTROL) =>
                                    {
                                        // Try to grab image from clipboard first
                                        if let Some((b64, media_type)) = try_grab_clipboard_image()
                                        {
                                            tui.image_counter += 1;
                                            let idx = tui.image_counter;
                                            tui.pending_images.push((idx, b64, media_type));

                                            // Insert marker into textarea
                                            let marker = format!("[Image #{}]", idx);
                                            let current = tui.input_textarea.lines().join("\n");
                                            let new_text = if current.trim().is_empty() {
                                                marker
                                            } else {
                                                format!("{}\n{}", current, marker)
                                            };
                                            tui.input_textarea =
                                                TuiRenderer::create_clean_textarea_with_text(
                                                    &new_text,
                                                );
                                            first_event_modified_input = true;
                                        } else {
                                            // No image - pass V to textarea for text paste
                                            tui.input_textarea.input(Event::Key(key));
                                            first_event_modified_input = true;
                                        }
                                        Ok(None)
                                    }
                                    (KeyCode::Char('g'), m)
                                        if m.contains(KeyModifiers::CONTROL) =>
                                    {
                                        // Ctrl+G: Good feedback
                                        tui.pending_feedback =
                                            Some(crate::feedback::FeedbackRating::Good);
                                        Ok(None)
                                    }
                                    (KeyCode::Char('b'), m)
                                        if m.contains(KeyModifiers::CONTROL) =>
                                    {
                                        // Ctrl+B: Bad feedback
                                        tui.pending_feedback =
                                            Some(crate::feedback::FeedbackRating::Bad);
                                        Ok(None)
                                    }
                                    (KeyCode::Char('/'), m)
                                        if m.contains(KeyModifiers::CONTROL) =>
                                    {
                                        // Ctrl+/: Show help (send as command)
                                        Ok(Some("/help".to_string()))
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
                                                let current_text =
                                                    tui.input_textarea.lines().join("\n");
                                                if !current_text.trim().is_empty() {
                                                    tui.history_draft = Some(current_text);
                                                }

                                                tui.history_index =
                                                    Some(tui.command_history.len() - 1);
                                                let cmd = &tui.command_history
                                                    [tui.command_history.len() - 1];
                                                tui.input_textarea =
                                                    TuiRenderer::create_clean_textarea_with_text(
                                                        cmd,
                                                    );
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
                                                        tui.input_textarea =
                                                            TuiRenderer::create_clean_textarea();
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
                                    (KeyCode::Tab, KeyModifiers::NONE) => {
                                        // Tab: Accept ghost text suggestion if available
                                        if let Some(ghost) = tui.ghost_text.take() {
                                            // Append ghost text to current input
                                            let current = tui.input_textarea.lines().join("\n");
                                            let completed = format!("{}{}", current, ghost);
                                            tui.input_textarea =
                                                TuiRenderer::create_clean_textarea_with_text(
                                                    &completed,
                                                );
                                            first_event_modified_input = true;
                                        } else {
                                            // No ghost text - pass Tab to textarea (insert tab char)
                                            tui.input_textarea.input(Event::Key(key));
                                            first_event_modified_input = true;
                                        }
                                        Ok(None)
                                    }
                                    _ => {
                                        // Pass key event to textarea (with sanitization)
                                        if should_accept_key_event(&key) {
                                            tui.input_textarea.input(Event::Key(key));
                                            first_event_modified_input = true; // Mark for render
                                        }
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
                                if key
                                    .modifiers
                                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
                                {
                                    // Shift+Enter / Alt(Option)+Enter: Insert newline
                                    tui.input_textarea.input(Event::Key(key));
                                    had_input = true;
                                } else {
                                    // Enter without Shift: Stop batch, will be processed next iteration
                                    break;
                                }
                            }
                            Ok(Event::Key(key)) => {
                                // Sanitize pasted content (filter images/control chars)
                                if should_accept_key_event(&key) {
                                    tui.input_textarea.input(Event::Key(key));
                                    had_input = true;
                                }
                                // Silently ignore problematic characters
                            }
                            Ok(_) => {}      // Ignore other events
                            Err(_) => break, // Error, stop batching
                        }
                    }

                    // Render immediately after input (event-driven, not polled)
                    // Capture typing hint BEFORE releasing lock, only when
                    // text was modified but not submitted (had_input && no submit).
                    let typing_hint = if had_input {
                        // Update ghost text suggestion based on new input
                        tui.update_ghost_text();

                        if let Err(e) = tui.render() {
                            tracing::error!("Async input render failed: {}", e);
                            // Signal event loop that render failed - recovery needed
                            tui.needs_full_refresh = true;
                            tui.last_render_error = Some(e.to_string());
                        }

                        // Capture current content for typing hint.
                        Some(tui.input_textarea.lines().join("\n"))
                    } else {
                        None
                    };

                    (first_event_result, typing_hint)
                } else {
                    // No input available, just render
                    (Ok(None), None)
                }
            };

            match input_result {
                Ok(Some(input)) => {
                    // Submit: reset typing signal, send Submitted event.
                    last_typing_signal = None;
                    if tx.send(InputEvent::Submitted(input)).is_err() {
                        // Channel closed, exit task
                        break;
                    }
                }
                Ok(None) => {
                    // No submit — check if we should fire a TypingStarted event.
                    if let Some(content) = typing_hint {
                        if !content.trim().is_empty() {
                            let now = Instant::now();
                            let should_signal = last_typing_signal
                                .map(|t| now.duration_since(t).as_millis() >= 300)
                                .unwrap_or(true);
                            if should_signal {
                                if tx.send(InputEvent::TypingStarted(content)).is_err() {
                                    break;
                                }
                                last_typing_signal = Some(now);
                            }
                        }
                    }
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

            // Small delay to prevent CPU spinning
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });

    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // --- sanitize_paste_char ---

    #[test]
    fn test_allows_printable_ascii() {
        // All printable ASCII 0x20–0x7E should be allowed
        for c in ' '..='~' {
            assert!(
                sanitize_paste_char(c),
                "printable ASCII {c:?} should be allowed"
            );
        }
    }

    #[test]
    fn test_allows_common_whitespace() {
        assert!(sanitize_paste_char('\t'), "tab should be allowed");
        assert!(sanitize_paste_char('\n'), "newline should be allowed");
        assert!(
            sanitize_paste_char('\r'),
            "carriage return should be allowed"
        );
    }

    #[test]
    fn test_blocks_control_characters() {
        // Control chars below 0x20 (except \t 0x09, \n 0x0A, \r 0x0D) should be blocked
        let allowed_whitespace = ['\t', '\n', '\r'];
        for byte in 0x00u8..0x20u8 {
            let c = byte as char;
            if allowed_whitespace.contains(&c) {
                assert!(sanitize_paste_char(c), "whitespace {c:?} should be allowed");
            } else {
                assert!(
                    !sanitize_paste_char(c),
                    "control char {c:?} should be blocked"
                );
            }
        }
    }

    #[test]
    fn test_blocks_private_use_area_unicode() {
        // Private use area E000–F8FF is used for image rendering
        assert!(
            !sanitize_paste_char('\u{E000}'),
            "private use start should be blocked"
        );
        assert!(
            !sanitize_paste_char('\u{F8FF}'),
            "private use end should be blocked"
        );
        assert!(
            !sanitize_paste_char('\u{E100}'),
            "mid private use should be blocked"
        );
    }

    #[test]
    fn test_allows_normal_unicode_text() {
        // Common international characters should be allowed
        for c in ['é', 'ñ', 'ü', '中', '日', '한', '🦀'] {
            // Note: emoji may or may not be in private use range — just check no panic
            let _ = sanitize_paste_char(c);
        }
        assert!(sanitize_paste_char('é'));
        assert!(sanitize_paste_char('ñ'));
        assert!(sanitize_paste_char('中'));
    }

    #[test]
    fn test_allows_del_char_as_printable() {
        // 0x7E '~' is the last printable ASCII; 0x7F DEL is NOT in ' '..='~'
        assert!(!sanitize_paste_char('\x7F'), "DEL should be blocked");
    }

    // --- Enter modifier: newline vs submit ---

    /// Helper that mirrors the runtime condition: should this Enter key event
    /// insert a newline (true) rather than submit the input (false)?
    fn enter_should_insert_newline(modifiers: KeyModifiers) -> bool {
        modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
    }

    #[test]
    fn enter_without_modifier_submits() {
        assert!(
            !enter_should_insert_newline(KeyModifiers::NONE),
            "plain Enter should submit (not insert newline)"
        );
    }

    #[test]
    fn shift_enter_inserts_newline() {
        // Some terminals DO send SHIFT for Shift+Enter — honour it.
        assert!(
            enter_should_insert_newline(KeyModifiers::SHIFT),
            "Shift+Enter should insert newline"
        );
    }

    #[test]
    fn alt_enter_inserts_newline() {
        // macOS Terminal/iTerm2 standard raw mode: Option+Enter → \x1b\r,
        // reported by crossterm as KeyCode::Enter + KeyModifiers::ALT.
        assert!(
            enter_should_insert_newline(KeyModifiers::ALT),
            "Alt/Option+Enter should insert newline"
        );
    }

    #[test]
    fn ctrl_enter_does_not_insert_newline() {
        // Ctrl+Enter is not a newline shortcut.
        assert!(
            !enter_should_insert_newline(KeyModifiers::CONTROL),
            "Ctrl+Enter should not insert newline (submits instead)"
        );
    }

    // --- should_accept_key_event ---

    #[test]
    fn test_accepts_printable_char_keys() {
        // Normal alphanumeric keys should all be accepted
        for c in 'a'..='z' {
            let event = key(KeyCode::Char(c));
            assert!(
                should_accept_key_event(&event),
                "char {c} should be accepted"
            );
        }
    }

    #[test]
    fn test_accepts_non_char_key_codes() {
        // Structural keys (Enter, Backspace, arrows) are always accepted
        let enter = key(KeyCode::Enter);
        let backspace = key(KeyCode::Backspace);
        let up = key(KeyCode::Up);
        let down = key(KeyCode::Down);
        assert!(should_accept_key_event(&enter));
        assert!(should_accept_key_event(&backspace));
        assert!(should_accept_key_event(&up));
        assert!(should_accept_key_event(&down));
    }

    #[test]
    fn test_rejects_private_use_unicode_in_key_event() {
        // A key event carrying a private-use-area character should be rejected
        let event = key(KeyCode::Char('\u{E000}'));
        assert!(!should_accept_key_event(&event));
    }

    // --- encode_rgba_to_png ---

    #[test]
    fn test_encode_rgba_to_png_produces_png_signature() {
        // 2x2 red pixels (RGBA)
        let rgba = vec![
            255u8, 0, 0, 255, // pixel 0
            255, 0, 0, 255, // pixel 1
            255, 0, 0, 255, // pixel 2
            255, 0, 0, 255,
        ]; // pixel 3
        let png = encode_rgba_to_png(2, 2, &rgba).unwrap();

        // PNG files start with the 8-byte PNG signature
        assert_eq!(
            &png[..8],
            b"\x89PNG\r\n\x1a\n",
            "output should start with PNG signature"
        );
    }

    #[test]
    fn test_encode_rgba_to_png_nonempty_output() {
        let rgba = vec![0u8; 4]; // 1x1 black pixel
        let png = encode_rgba_to_png(1, 1, &rgba).unwrap();
        assert!(!png.is_empty());
    }

    // --- InputEvent ---

    #[test]
    fn test_input_event_submitted_variant_holds_string() {
        let event = InputEvent::Submitted("hello world".to_string());
        match event {
            InputEvent::Submitted(s) => assert_eq!(s, "hello world"),
            _ => panic!("Expected Submitted variant"),
        }
    }

    #[test]
    fn test_input_event_typing_started_variant_holds_string() {
        let event = InputEvent::TypingStarted("how do I use lifetimes".to_string());
        match event {
            InputEvent::TypingStarted(s) => assert_eq!(s, "how do I use lifetimes"),
            _ => panic!("Expected TypingStarted variant"),
        }
    }
}

/// Helper to create a clean text area (needs to be accessible)
#[allow(dead_code)]
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
