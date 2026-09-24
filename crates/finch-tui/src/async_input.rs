// Async input handler for TUI - non-blocking keyboard polling

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex};

use super::{ComposerDispatch, TuiRenderer};

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

fn dialog_owns_key(has_dialog: bool, key: &KeyEvent, input: &str) -> bool {
    has_dialog
        && !(key.code == KeyCode::Enter
            && !key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
            && !input.trim().is_empty())
}

// ---------------------------------------------------------------------------
// Keyboard binding table — shared authority for composer dispatch and /help
// ---------------------------------------------------------------------------

/// Which dispatcher owns a documented keyboard binding.
///
/// Every entry in [`KEYBOARD_SHORTCUTS`] names its real dispatcher so the
/// `/help` renderer (`cli::commands::format_help`) and the input path cannot
/// disagree about which keys exist or what they do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutAuthority {
    /// The input task's composer shortcut dispatch consumes this entry as its
    /// guard ([`KeyboardShortcut::owns`]); the key data here is authoritative.
    ComposerShortcut,
    /// `TuiRenderer::dispatch_composer_key` claims the key (completions,
    /// newline insertion, history recall).
    ComposerDispatch,
    /// `TuiRenderer::handle_accordion_key` claims the key (conversation
    /// scroll).
    ConversationScroll,
}

/// One documented keyboard binding: the key its dispatcher matches plus the
/// exact `/help` text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyboardShortcut {
    /// Key code the dispatcher matches.
    pub code: KeyCode,
    /// Modifier mask the dispatcher requires (`NONE` = no modifier needed).
    pub requires: KeyModifiers,
    /// Key label as rendered by `/help`, e.g. `"Ctrl+C"`.
    pub label: &'static str,
    /// What the binding does, as rendered by `/help`.
    pub description: &'static str,
    /// Slash command the binding submits, when it submits one.
    pub submit: Option<&'static str>,
    /// Dispatcher that owns the key.
    pub authority: ShortcutAuthority,
}

impl KeyboardShortcut {
    /// Guard predicate for [`ShortcutAuthority::ComposerShortcut`] entries:
    /// the code must match and the required modifier must be present
    /// (`requires == NONE` matches any modifier). Navigation bindings are
    /// claimed by their own dispatchers with their own conditions and must
    /// not be routed through this predicate.
    pub fn owns(&self, key: &KeyEvent) -> bool {
        if key.code != self.code {
            return false;
        }
        if self.requires == KeyModifiers::NONE {
            return true;
        }
        key.modifiers.intersects(self.requires)
    }
}

/// Ctrl+V on every platform; macOS terminals also report Cmd+V as SUPER.
const PASTE_MODIFIERS: KeyModifiers =
    KeyModifiers::from_bits_retain(KeyModifiers::CONTROL.bits() | KeyModifiers::SUPER.bits());

/// Ctrl+C: clear the draft, or cancel the running query when it is empty.
const COMPOSER_CTRL_C: KeyboardShortcut = KeyboardShortcut {
    code: KeyCode::Char('c'),
    requires: KeyModifiers::CONTROL,
    label: "Ctrl+C",
    description: "Clear the draft; cancel the query when empty",
    submit: None,
    authority: ShortcutAuthority::ComposerShortcut,
};

/// Escape: same clear-then-cancel behavior as Ctrl+C.
const COMPOSER_ESCAPE: KeyboardShortcut = KeyboardShortcut {
    code: KeyCode::Esc,
    requires: KeyModifiers::NONE,
    label: "Esc",
    description: "Clear the draft; cancel the query when empty",
    submit: None,
    authority: ShortcutAuthority::ComposerShortcut,
};

/// Cmd+V on macOS / Ctrl+V: paste a clipboard image into the draft.
const COMPOSER_PASTE_IMAGE: KeyboardShortcut = KeyboardShortcut {
    code: KeyCode::Char('v'),
    requires: PASTE_MODIFIERS,
    label: "Ctrl+V",
    description: "Paste a clipboard image into the draft (Cmd+V)",
    submit: None,
    authority: ShortcutAuthority::ComposerShortcut,
};

/// Ctrl+G: good feedback on the last response.
const COMPOSER_FEEDBACK_GOOD: KeyboardShortcut = KeyboardShortcut {
    code: KeyCode::Char('g'),
    requires: KeyModifiers::CONTROL,
    label: "Ctrl+G",
    description: "Mark last response as good (1x stored weight)",
    submit: None,
    authority: ShortcutAuthority::ComposerShortcut,
};

/// Ctrl+B: bad feedback on the last response.
const COMPOSER_FEEDBACK_BAD: KeyboardShortcut = KeyboardShortcut {
    code: KeyCode::Char('b'),
    requires: KeyModifiers::CONTROL,
    label: "Ctrl+B",
    description: "Mark last response as bad (10x stored weight)",
    submit: None,
    authority: ShortcutAuthority::ComposerShortcut,
};

/// Ctrl+Z: deliberate no-op; typed VM definitions are revisioned.
const COMPOSER_NOOP_UNDO: KeyboardShortcut = KeyboardShortcut {
    code: KeyCode::Char('z'),
    requires: KeyModifiers::CONTROL,
    label: "Ctrl+Z",
    description: "Deliberate no-op; VM definitions are revisioned",
    submit: None,
    authority: ShortcutAuthority::ComposerShortcut,
};

/// Ctrl+P: pop the top word off the vocabulary stack.
const COMPOSER_POP: KeyboardShortcut = KeyboardShortcut {
    code: KeyCode::Char('p'),
    requires: KeyModifiers::CONTROL,
    label: "Ctrl+P",
    description: "Pop top word off the vocabulary stack (/pop)",
    submit: Some("/pop"),
    authority: ShortcutAuthority::ComposerShortcut,
};

/// Ctrl+D: Readline/Emacs delete-char under the cursor.
const COMPOSER_DELETE_CHAR: KeyboardShortcut = KeyboardShortcut {
    code: KeyCode::Char('d'),
    requires: KeyModifiers::CONTROL,
    label: "Ctrl+D",
    description: "Delete the character under the cursor",
    submit: None,
    authority: ShortcutAuthority::ComposerShortcut,
};

/// Ctrl+/: show help.
const COMPOSER_HELP: KeyboardShortcut = KeyboardShortcut {
    code: KeyCode::Char('/'),
    requires: KeyModifiers::CONTROL,
    label: "Ctrl+/",
    description: "Show this help (/help)",
    submit: Some("/help"),
    authority: ShortcutAuthority::ComposerShortcut,
};

/// Shift+Tab: cycle Normal → AutoAccept → Planning.
const COMPOSER_CYCLE_MODE: KeyboardShortcut = KeyboardShortcut {
    code: KeyCode::BackTab,
    requires: KeyModifiers::NONE,
    label: "Shift+Tab",
    description: "Cycle Normal → AutoAccept → Planning",
    submit: Some("/cycle-mode"),
    authority: ShortcutAuthority::ComposerShortcut,
};

/// Tab: accept the slash-command ghost text.
const COMPOSER_TAB_COMPLETE: KeyboardShortcut = KeyboardShortcut {
    code: KeyCode::Tab,
    requires: KeyModifiers::NONE,
    label: "Tab",
    description: "Accept the /command ghost text",
    submit: None,
    authority: ShortcutAuthority::ComposerDispatch,
};

/// Shift/Option+Enter: insert an in-buffer newline.
const COMPOSER_NEWLINE: KeyboardShortcut = KeyboardShortcut {
    code: KeyCode::Enter,
    requires: KeyModifiers::SHIFT,
    label: "Shift+Enter",
    description: "Insert a newline (multi-line input)",
    submit: None,
    authority: ShortcutAuthority::ComposerDispatch,
};

/// Up: recall older command history.
const COMPOSER_HISTORY_OLDER: KeyboardShortcut = KeyboardShortcut {
    code: KeyCode::Up,
    requires: KeyModifiers::NONE,
    label: "↑",
    description: "Recall older command history",
    submit: None,
    authority: ShortcutAuthority::ComposerDispatch,
};

/// Down: recall newer command history.
const COMPOSER_HISTORY_NEWER: KeyboardShortcut = KeyboardShortcut {
    code: KeyCode::Down,
    requires: KeyModifiers::NONE,
    label: "↓",
    description: "Recall newer command history",
    submit: None,
    authority: ShortcutAuthority::ComposerDispatch,
};

/// PageUp: scroll the conversation up one page of the visible pane.
const COMPOSER_PAGE_UP: KeyboardShortcut = KeyboardShortcut {
    code: KeyCode::PageUp,
    requires: KeyModifiers::NONE,
    label: "PgUp",
    description: "Scroll the conversation up one page",
    submit: None,
    authority: ShortcutAuthority::ConversationScroll,
};

/// PageDown: scroll the conversation back toward the bottom.
const COMPOSER_PAGE_DOWN: KeyboardShortcut = KeyboardShortcut {
    code: KeyCode::PageDown,
    requires: KeyModifiers::NONE,
    label: "PgDn",
    description: "Scroll the conversation down one page",
    submit: None,
    authority: ShortcutAuthority::ConversationScroll,
};

/// The keyboard-binding catalog rendered by `/help`.
///
/// [`ShortcutAuthority::ComposerShortcut`] entries double as the dispatch
/// guards in [`handle_composer_shortcuts`], so the dispatcher and the help
/// text share one key table and cannot drift apart; the other entries are
/// pinned to their real dispatchers by tests in this module and in `lib.rs`.
pub const KEYBOARD_SHORTCUTS: &[KeyboardShortcut] = &[
    COMPOSER_CTRL_C,
    COMPOSER_ESCAPE,
    COMPOSER_PASTE_IMAGE,
    COMPOSER_FEEDBACK_GOOD,
    COMPOSER_FEEDBACK_BAD,
    COMPOSER_NOOP_UNDO,
    COMPOSER_POP,
    COMPOSER_DELETE_CHAR,
    COMPOSER_HELP,
    COMPOSER_CYCLE_MODE,
    COMPOSER_TAB_COMPLETE,
    COMPOSER_NEWLINE,
    COMPOSER_HISTORY_OLDER,
    COMPOSER_HISTORY_NEWER,
    COMPOSER_PAGE_UP,
    COMPOSER_PAGE_DOWN,
];

/// Priority-3 composer handling shared by the input task and tests: the
/// `ComposerShortcut` entries of [`KEYBOARD_SHORTCUTS`], then plain typing
/// input for the textarea.
///
/// Returns `(input_modified, submitted_line)`. `input_modified` mirrors the
/// input task's `first_event_modified_input` flag; `submitted_line` carries
/// the entry's `submit` command for the input task to submit.
fn handle_composer_shortcuts(tui: &mut TuiRenderer, key: KeyEvent) -> (bool, Option<String>) {
    if COMPOSER_CTRL_C.owns(&key) {
        // Ctrl+C: Clear input if non-empty, otherwise cancel query
        let content = tui.input_textarea.lines().join("");
        if content.trim().is_empty() {
            tui.pending_cancellation = true;
            (false, None)
        } else {
            tui.input_textarea = TuiRenderer::create_clean_textarea();
            (true, None)
        }
    } else if COMPOSER_ESCAPE.owns(&key) {
        // Escape: Clear input if non-empty, otherwise cancel query
        let content = tui.input_textarea.lines().join("");
        if content.trim().is_empty() {
            tui.pending_cancellation = true;
            (false, None)
        } else {
            tui.input_textarea = TuiRenderer::create_clean_textarea();
            (true, None)
        }
    } else if COMPOSER_PASTE_IMAGE.owns(&key) {
        // Cmd+V on macOS / Ctrl+V: check clipboard for images
        if let Some((b64, media_type)) = try_grab_clipboard_image() {
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
            tui.input_textarea = TuiRenderer::create_clean_textarea_with_text(&new_text);
            (true, None)
        } else {
            // No image - pass V to textarea for text paste
            tui.input_textarea.input(Event::Key(key));
            (true, None)
        }
    } else if COMPOSER_FEEDBACK_GOOD.owns(&key) {
        // Ctrl+G: Good feedback
        tui.pending_feedback = Some(super::Verdict::Approve);
        (false, None)
    } else if COMPOSER_FEEDBACK_BAD.owns(&key) {
        // Ctrl+B: Bad feedback
        tui.pending_feedback = Some(super::Verdict::Reject);
        (false, None)
    } else if COMPOSER_NOOP_UNDO.owns(&key) {
        // Typed VM definitions are revisioned; do not route Ctrl+Z into the
        // removed legacy-Forth undo path.
        (false, None)
    } else if COMPOSER_POP.owns(&key) {
        // Ctrl+P: Pop top word off vocabulary stack
        (false, COMPOSER_POP.submit.map(str::to_string))
    } else if COMPOSER_DELETE_CHAR.owns(&key) {
        // Readline/Emacs semantics: delete the character under the cursor. On
        // an empty buffer this is a no-op; Finch exits only through the
        // explicit `/quit` command.
        tui.input_textarea.delete_next_char();
        (true, None)
    } else if COMPOSER_HELP.owns(&key) {
        // Ctrl+/: Show help (send as command)
        (false, COMPOSER_HELP.submit.map(str::to_string))
    } else if COMPOSER_CYCLE_MODE.owns(&key) {
        // Shift+Tab: cycle Normal → AutoAccept → Planning
        (false, COMPOSER_CYCLE_MODE.submit.map(str::to_string))
    } else if should_accept_key_event(&key) {
        // Pass key event to textarea (with sanitization)
        tui.input_textarea.input(Event::Key(key));
        (true, None)
    } else {
        (false, None)
    }
}

/// Encode a Cap'n Proto `ControlMessage { quit }` into bytes.
///
/// Used to send a quit signal through the out-of-band quit channel.
/// The quit watcher task decodes and acts on it independently of the event loop.
pub fn encode_quit_message() -> Vec<u8> {
    let mut message = capnp::message::Builder::new_default();
    {
        let mut ctrl = message.init_root::<finch_ipc::finch_ipc_capnp::control_message::Builder>();
        ctrl.set_quit(());
    }
    let mut bytes = Vec::new();
    capnp::serialize::write_message(&mut bytes, &message)
        .expect("Cap'n Proto quit message serialization is infallible");
    bytes
}

/// Spawn a background task that polls keyboard input and sends to channel
///
/// This enables non-blocking input handling in the event loop:
/// - Polls keyboard with 100ms timeout (non-blocking)
/// - Sends completed lines to channel
/// - Handles Enter key to submit input
/// - Handles all other keys via TextArea
/// - Renders TUI periodically
/// - Sends `InputEvent::TypingStarted` after 300 ms of typing silence (true debounce)
///
/// `quit_tx`: binary channel for out-of-band `/quit` signals (Cap'n Proto ControlMessage).
/// The quit watcher task (spawned separately) reads this channel and exits the process.
/// `editor_active`: application-owned terminal ownership query; the input task
/// must not poll or render while an external editor owns the terminal.
pub fn spawn_input_task(
    tui_renderer: Arc<Mutex<TuiRenderer>>,
    quit_tx: mpsc::UnboundedSender<Vec<u8>>,
    editor_active: fn() -> bool,
) -> mpsc::UnboundedReceiver<InputEvent> {
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        // True debounce: fire TypingStarted only after the user has been idle for
        // 300 ms.  last_keystroke records when the user last modified the textarea.
        // pending_brain_content holds the content to fire with (captured at keystroke
        // time so we don't need to re-acquire the lock in the idle check).
        let mut last_keystroke: Option<Instant> = None;
        let mut pending_brain_content: Option<String> = None;

        loop {
            // While an external editor owns the terminal, suspend all crossterm
            // event polling.  Consuming events here would steal keystrokes from
            // the editor process and cause visible flickering / input loss.
            if editor_active() {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }

            // The lock block returns (input_result, typing_hint).
            // typing_hint is Some(content) when text was modified but not submitted.
            let (input_result, typing_hint): (Result<Option<String>>, Option<String>) = {
                let mut tui = tui_renderer.lock().await;

                // Poll with short timeout (5ms) to avoid blocking
                if crossterm::event::poll(Duration::from_millis(5)).unwrap_or(false) {
                    // Track if we need to render after processing first event
                    let mut first_event_modified_input = false;
                    let mut first_event_needs_render = false;

                    // Process first event
                    let first_event_result = match crossterm::event::read() {
                        Ok(Event::Key(key)) => {
                            // Priority 0: /quit always exits immediately.
                            // Send a Cap'n Proto binary ControlMessage { quit } to the quit
                            // watcher task — do NOT go through the event loop channel, because
                            // the loop may be blocked on streaming/tool execution and would
                            // never drain it.  The watcher task is never blocked and exits
                            // the process as soon as it receives the binary message.
                            let current_text = tui.input_textarea.lines().join("\n");
                            let is_quit_enter = key.code == KeyCode::Enter
                                && current_text.trim() == "/quit"
                                && !key
                                    .modifiers
                                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
                            if is_quit_enter {
                                let _ = quit_tx.send(encode_quit_message());
                                // Return Ok(None) — the watcher task will call process::exit(0).
                                Ok(None)
                            } else if tui.handle_accordion_key(key) {
                                first_event_needs_render = true;
                                Ok(None)
                            }
                            // Priority 1: Handle active dialog (if any).
                            // Exception: plain Enter (no modifier) submits the user's query
                            // even when a brain-question dialog is active.  The dialog is
                            // dismissed with Cancelled so the brain gets "[no answer]" and
                            // the user's input is not blocked.  Tool-approval dialogs still
                            // block submission because the input is empty while they are open.
                            else if dialog_owns_key(
                                tui.active_dialog.is_some(),
                                &key,
                                &tui.input_textarea.lines().join(""),
                            ) {
                                let dialog_result = if let Some(dialog) = tui.active_dialog.as_mut()
                                {
                                    dialog.handle_key_event(key)
                                } else {
                                    None
                                };

                                if let Some(result) = dialog_result {
                                    // Dialog completed: freeze the settled
                                    // record into the conversation, clear it,
                                    // and stage the result for the event loop
                                    // (#807).
                                    tui.complete_dialog(result);
                                }

                                // Mark for render so dialog updates are shown
                                first_event_modified_input = true;

                                Ok(None) // Don't submit input while dialog is active
                            } else {
                                // Enter without Shift/Alt still submits when a brain-question
                                // dialog is up; cancel it so the query is not blocked.
                                if key.code == KeyCode::Enter
                                    && !key
                                        .modifiers
                                        .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
                                    && tui.active_dialog.is_some()
                                {
                                    tui.complete_dialog(crate::DialogResult::Cancelled);
                                }
                                match tui.dispatch_composer_key(key) {
                                    ComposerDispatch::Submit(input) => {
                                        first_event_modified_input = true;
                                        Ok(Some(input))
                                    }
                                    ComposerDispatch::Handled { input_changed } => {
                                        if input_changed {
                                            first_event_modified_input = true;
                                        } else {
                                            first_event_needs_render = true;
                                        }
                                        Ok(None)
                                    }
                                    ComposerDispatch::Unhandled => {
                                        // Priority 3: the composer shortcut table
                                        // (KEYBOARD_SHORTCUTS), then plain typing
                                        // input.
                                        let (input_modified, submitted) =
                                            handle_composer_shortcuts(&mut tui, key);
                                        if input_modified {
                                            first_event_modified_input = true;
                                        }
                                        Ok(submitted)
                                    }
                                }
                            }
                        }
                        // Bracketed paste: insert text verbatim into the textarea.
                        // Newlines within the paste become real newlines (Shift+Enter)
                        // so they don't trigger a submit.  This is the correct Claude
                        // Code-style paste behavior.
                        Ok(Event::Paste(text)) => {
                            // Replace each \n with a manual newline insertion so
                            // tui-textarea keeps them as in-buffer newlines.
                            for ch in text.chars() {
                                if ch == '\n' || ch == '\r' {
                                    let newline_key = crossterm::event::KeyEvent::new(
                                        KeyCode::Enter,
                                        crossterm::event::KeyModifiers::SHIFT,
                                    );
                                    tui.input_textarea.input(Event::Key(newline_key));
                                } else if sanitize_paste_char(ch) {
                                    let char_key = crossterm::event::KeyEvent::new(
                                        KeyCode::Char(ch),
                                        crossterm::event::KeyModifiers::NONE,
                                    );
                                    tui.input_textarea.input(Event::Key(char_key));
                                }
                            }
                            first_event_modified_input = true;
                            Ok(None)
                        }
                        Ok(Event::Mouse(mouse)) => {
                            if tui.handle_mouse(mouse) {
                                first_event_needs_render = true;
                            }
                            Ok(None)
                        }
                        Ok(Event::Resize(w, h)) => match tui.handle_resize(w, h) {
                            Ok(()) => {
                                first_event_needs_render = true;
                                Ok(None)
                            }
                            Err(error) => Err(error),
                        },
                        Ok(_) => Ok(None), // Ignore other events (mouse, focus, etc.)
                        Err(e) => Err(anyhow::anyhow!("Failed to read input: {}", e)),
                    };

                    // Drain any immediately-available subsequent key events.
                    // With bracketed paste enabled, pasted content arrives as Event::Paste
                    // (handled above).  If a plain Enter arrives in the batch drain we must
                    // process it as a submit — the previous code read() it and then broke,
                    // silently dropping the keystroke and making Enter feel unreliable.
                    let mut had_input = first_event_modified_input;
                    let mut needs_render = first_event_needs_render;
                    let mut batch_submit: Option<String> = None;
                    if first_event_modified_input {
                        // Do not let later keys in the same terminal batch act
                        // on matches for the pre-edit draft.
                        tui.update_ghost_text();
                    }
                    while crossterm::event::poll(Duration::from_millis(0)).unwrap_or(false) {
                        match crossterm::event::read() {
                            Ok(Event::Key(key)) => {
                                if tui.handle_accordion_key(key) {
                                    needs_render = true;
                                    continue;
                                }
                                let dialog_owns_key = dialog_owns_key(
                                    tui.active_dialog.is_some(),
                                    &key,
                                    &tui.input_textarea.lines().join(""),
                                );
                                if dialog_owns_key {
                                    let result = tui
                                        .active_dialog
                                        .as_mut()
                                        .and_then(|dialog| dialog.handle_key_event(key));
                                    if let Some(result) = result {
                                        // Settled record + staged result (#807).
                                        tui.complete_dialog(result);
                                    }
                                    tui.mark_dirty();
                                    needs_render = true;
                                    continue;
                                }
                                if tui.active_dialog.is_some() {
                                    tui.complete_dialog(crate::DialogResult::Cancelled);
                                }

                                if key.code == KeyCode::Tab && key.modifiers == KeyModifiers::NONE {
                                    if tui.handle_tab_key(key) {
                                        had_input = true;
                                    }
                                    needs_render = true;
                                } else if key.modifiers == KeyModifiers::NONE
                                    && tui.handle_completion_key(key.code)
                                {
                                    needs_render = true;
                                } else if key.code == KeyCode::Enter {
                                    if key
                                        .modifiers
                                        .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
                                    {
                                        // Shift/Alt+Enter in batch: insert newline
                                        tui.input_textarea.input(Event::Key(key));
                                        had_input = true;
                                        tui.update_ghost_text();
                                    } else {
                                        // Plain Enter: collect submission and stop draining.
                                        if let Some(input) = tui.take_submitted_input() {
                                            had_input = true; // render the cleared input
                                            batch_submit = Some(input);
                                        }
                                        break;
                                    }
                                } else if should_accept_key_event(&key) {
                                    tui.input_textarea.input(Event::Key(key));
                                    had_input = true;
                                    tui.update_ghost_text();
                                }
                            }
                            Ok(Event::Resize(w, h)) => {
                                if tui.handle_resize(w, h).is_ok() {
                                    needs_render = true;
                                }
                            }
                            Ok(Event::Mouse(mouse)) => {
                                if tui.handle_mouse(mouse) {
                                    needs_render = true;
                                }
                            }
                            Ok(_) => {} // Ignore other events (mouse, focus, paste)
                            Err(_) => break,
                        }
                    }
                    // Batch drain submit overrides the first-event result (e.g. a typed
                    // character followed immediately by Enter in the same poll window).
                    let first_event_result: Result<Option<String>> =
                        if let Some(input) = batch_submit {
                            Ok(Some(input))
                        } else {
                            first_event_result
                        };

                    // Render immediately after input (event-driven, not polled)
                    // Capture typing hint BEFORE releasing lock, only when
                    // text was modified but not submitted (had_input && no submit).
                    if had_input {
                        tui.update_ghost_text();
                    }

                    if (had_input || needs_render) && !editor_active() {
                        if let Err(e) = tui.render() {
                            tracing::error!("Async input render failed: {}", e);
                            tui.needs_full_refresh = true;
                            tui.last_render_error = Some(e.to_string());
                        }
                    }

                    // A resize redraw is not typing and must not notify the Brain.
                    let typing_hint = had_input.then(|| tui.input_textarea.lines().join("\n"));

                    (first_event_result, typing_hint)
                } else {
                    // No input available, just render
                    (Ok(None), None)
                }
            };

            match input_result {
                Ok(Some(input)) => {
                    // Submit: clear pending debounce state and send Submitted event.
                    last_keystroke = None;
                    pending_brain_content = None;
                    if tx.send(InputEvent::Submitted(input)).is_err() {
                        // Channel closed, exit task
                        break;
                    }
                }
                Ok(None) => {
                    // Update debounce state if the user typed something this cycle.
                    if let Some(content) = typing_hint {
                        last_keystroke = Some(Instant::now());
                        if !content.trim().is_empty() {
                            pending_brain_content = Some(content);
                        }
                    }

                    // Fire TypingStarted after 300 ms of silence (true debounce):
                    // only trigger once per typing burst, when the user has stopped.
                    if let (Some(content), Some(kst)) = (&pending_brain_content, last_keystroke) {
                        if kst.elapsed().as_millis() >= 300 {
                            let content = content.clone();
                            if tx.send(InputEvent::TypingStarted(content)).is_err() {
                                break;
                            }
                            // Clear so we don't fire again for the same burst.
                            last_keystroke = None;
                            pending_brain_content = None;
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

            // Small delay to yield to tokio between cycles.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });

    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn quit_message_uses_the_ipc_control_wire() {
        let encoded = encode_quit_message();
        let mut cursor = encoded.as_slice();
        let message =
            capnp::serialize::read_message(&mut cursor, capnp::message::ReaderOptions::default())
                .expect("quit control message must decode");
        let control = message
            .get_root::<finch_ipc::finch_ipc_capnp::control_message::Reader>()
            .expect("quit control root must decode");
        assert!(matches!(
            control.which().expect("quit control variant must decode"),
            finch_ipc::finch_ipc_capnp::control_message::Which::Quit(_)
        ));
    }

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

    #[test]
    fn test_active_dialog_owns_batched_completion_keys() {
        for code in [KeyCode::Up, KeyCode::Down, KeyCode::Tab, KeyCode::Esc] {
            assert!(dialog_owns_key(true, &key(code), "/brain "));
        }
        assert!(!dialog_owns_key(false, &key(KeyCode::Tab), "/brain "));
        assert!(dialog_owns_key(true, &key(KeyCode::Enter), ""));
        assert!(!dialog_owns_key(true, &key(KeyCode::Enter), "draft"));
    }

    #[test]
    fn ctrl_d_deletes_the_character_under_the_prompt_cursor() {
        use tui_textarea::CursorMove;

        let mut textarea = TuiRenderer::create_clean_textarea_with_text("abc");
        textarea.move_cursor(CursorMove::Head);
        textarea.input(Event::Key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        )));

        assert_eq!(textarea.lines(), ["bc"]);
    }

    #[test]
    fn ctrl_d_is_a_noop_on_an_empty_prompt() {
        let mut textarea = TuiRenderer::create_clean_textarea();
        textarea.input(Event::Key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        )));

        assert_eq!(textarea.lines(), [""]);
    }

    // --- keyboard binding table (KEYBOARD_SHORTCUTS) ---

    fn headless_renderer() -> TuiRenderer {
        let colors = finch_theme::ColorScheme::default();
        let output = Arc::new(crate::test_support::OutputManager::new(colors.clone()));
        let status = Arc::new(crate::test_support::StatusBar::new());
        TuiRenderer::new_headless(output, status, colors)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    /// INVARIANT: the /help keyboard-shortcut section is generated from
    /// KEYBOARD_SHORTCUTS, so every table entry must match what its real
    /// dispatcher actually does with the key it declares. A table entry with
    /// no behavioral case below fails here — a new binding cannot ship
    /// documented prose without a dispatch-backed assertion.
    #[test]
    fn test_keyboard_shortcut_table_matches_the_real_dispatch_paths() {
        let mut covered = 0usize;
        for entry in KEYBOARD_SHORTCUTS {
            let event = KeyEvent::new(entry.code, entry.requires);
            let why = format!(
                "binding={:?} label={:?} code={:?} requires={:?} authority={:?}",
                entry.description, entry.label, entry.code, entry.requires, entry.authority
            );
            match (entry.label, entry.authority) {
                ("Ctrl+C", ShortcutAuthority::ComposerShortcut)
                | ("Esc", ShortcutAuthority::ComposerShortcut) => {
                    covered += 1;
                    let mut renderer = headless_renderer();
                    renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("hello");
                    let (modified, submitted) = handle_composer_shortcuts(&mut renderer, event);
                    assert!(
                        modified && submitted.is_none(),
                        "{why}: with a non-empty draft the binding must clear the \
                         draft, not cancel or submit"
                    );
                    assert_eq!(
                        renderer.input_textarea.lines(),
                        [""],
                        "{why}: the draft must be cleared"
                    );
                    assert!(
                        !renderer.pending_cancellation,
                        "{why}: a non-empty draft is cleared, never cancelled"
                    );

                    let mut renderer = headless_renderer();
                    let (modified, submitted) = handle_composer_shortcuts(&mut renderer, event);
                    assert!(
                        !modified && submitted.is_none(),
                        "{why}: with an empty draft nothing is submitted or modified"
                    );
                    assert!(
                        renderer.pending_cancellation,
                        "{why}: an empty draft must request cancellation"
                    );
                }
                ("Ctrl+V", ShortcutAuthority::ComposerShortcut) => {
                    covered += 1;
                    let mut renderer = headless_renderer();
                    let (modified, submitted) = handle_composer_shortcuts(&mut renderer, event);
                    assert_eq!(submitted, None, "{why}: paste never submits a command");
                    assert!(
                        modified,
                        "{why}: Ctrl+V always modifies the draft (image marker or \
                         text paste fallback)"
                    );
                    let pasted_image = !renderer.pending_images.is_empty();
                    if pasted_image {
                        let draft = renderer.input_textarea.lines().join("");
                        assert!(
                            draft.contains("[Image #"),
                            "{why}: a clipboard image must leave its [Image #N] \
                             marker in the draft; draft={draft:?}"
                        );
                    }
                    let cmd_v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::SUPER);
                    assert!(
                        COMPOSER_PASTE_IMAGE.owns(&cmd_v),
                        "{why}: macOS Cmd+V must own the same image-paste binding"
                    );
                }
                ("Ctrl+G", ShortcutAuthority::ComposerShortcut) => {
                    covered += 1;
                    let mut renderer = headless_renderer();
                    let (modified, submitted) = handle_composer_shortcuts(&mut renderer, event);
                    assert_eq!(
                        (modified, submitted),
                        (false, None),
                        "{why}: good feedback must not modify the draft or submit"
                    );
                    assert_eq!(
                        renderer.pending_feedback,
                        Some(crate::Verdict::Approve),
                        "{why}: Ctrl+G must record a good verdict"
                    );
                }
                ("Ctrl+B", ShortcutAuthority::ComposerShortcut) => {
                    covered += 1;
                    let mut renderer = headless_renderer();
                    let (modified, submitted) = handle_composer_shortcuts(&mut renderer, event);
                    assert_eq!(
                        (modified, submitted),
                        (false, None),
                        "{why}: bad feedback must not modify the draft or submit"
                    );
                    assert_eq!(
                        renderer.pending_feedback,
                        Some(crate::Verdict::Reject),
                        "{why}: Ctrl+B must record a bad verdict"
                    );
                }
                ("Ctrl+Z", ShortcutAuthority::ComposerShortcut) => {
                    covered += 1;
                    let mut renderer = headless_renderer();
                    renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("abc");
                    let (modified, submitted) = handle_composer_shortcuts(&mut renderer, event);
                    assert_eq!(
                        (modified, submitted),
                        (false, None),
                        "{why}: Ctrl+Z is a deliberate no-op — nothing may change"
                    );
                    assert_eq!(
                        renderer.input_textarea.lines(),
                        ["abc"],
                        "{why}: the draft must be untouched"
                    );
                    assert!(
                        renderer.pending_feedback.is_none() && !renderer.pending_cancellation,
                        "{why}: a no-op must not set feedback or cancellation state"
                    );
                }
                ("Ctrl+P", ShortcutAuthority::ComposerShortcut) => {
                    covered += 1;
                    let mut renderer = headless_renderer();
                    let (modified, submitted) = handle_composer_shortcuts(&mut renderer, event);
                    assert_eq!(
                        (modified, submitted),
                        (false, Some("/pop".to_string())),
                        "{why}: Ctrl+P must submit /pop (vocabulary pop)"
                    );
                }
                ("Ctrl+D", ShortcutAuthority::ComposerShortcut) => {
                    covered += 1;
                    let mut renderer = headless_renderer();
                    renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("abc");
                    // Compose a draft: the cursor lands after the last typed
                    // character, where Readline semantics make Ctrl+D a no-op.
                    let (modified, submitted) = handle_composer_shortcuts(&mut renderer, event);
                    assert_eq!(
                        (modified, submitted),
                        (true, None),
                        "{why}: Ctrl+D never submits; the arm marks input as \
                         touched for the render pass even when nothing is deleted"
                    );
                    assert_eq!(
                        renderer.input_textarea.lines(),
                        ["abc"],
                        "{why}: Ctrl+D after the last character must be a no-op"
                    );
                    // Cursor under a character: Readline delete-char.
                    use tui_textarea::CursorMove;
                    renderer.input_textarea.move_cursor(CursorMove::Head);
                    let (modified, submitted) = handle_composer_shortcuts(&mut renderer, event);
                    assert_eq!(
                        (modified, submitted),
                        (true, None),
                        "{why}: Ctrl+D deletes a character instead of submitting"
                    );
                    assert_eq!(
                        renderer.input_textarea.lines(),
                        ["bc"],
                        "{why}: the character under the cursor must be deleted"
                    );
                }
                ("Ctrl+/", ShortcutAuthority::ComposerShortcut) => {
                    covered += 1;
                    let mut renderer = headless_renderer();
                    let (modified, submitted) = handle_composer_shortcuts(&mut renderer, event);
                    assert_eq!(
                        (modified, submitted),
                        (false, Some("/help".to_string())),
                        "{why}: Ctrl+/ must submit /help"
                    );
                }
                ("Shift+Tab", ShortcutAuthority::ComposerShortcut) => {
                    covered += 1;
                    let mut renderer = headless_renderer();
                    let (modified, submitted) = handle_composer_shortcuts(&mut renderer, event);
                    assert_eq!(
                        (modified, submitted),
                        (false, Some("/cycle-mode".to_string())),
                        "{why}: Shift+Tab must submit /cycle-mode"
                    );
                }
                ("Tab", ShortcutAuthority::ComposerDispatch) => {
                    covered += 1;
                    let mut renderer = headless_renderer();
                    renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("/hel");
                    renderer.update_ghost_text();
                    // The real input task paints the completion pane between
                    // the keystroke that opened it and the Tab that accepts
                    // it; painting is what makes the pane keyboard-owning.
                    crate::autocomplete_widget::completion_pane_lines(
                        &mut renderer.autocomplete_state,
                        80,
                        9,
                    );
                    let dispatch = renderer.dispatch_composer_key(event);
                    assert!(
                        matches!(dispatch, ComposerDispatch::Handled { .. }),
                        "{why}: Tab over a painted completion must be consumed; \
                         got {dispatch:?}"
                    );
                    assert_eq!(
                        renderer.input_textarea.lines(),
                        ["/help"],
                        "{why}: Tab must accept the ghost text into the draft"
                    );
                }
                ("Shift+Enter", ShortcutAuthority::ComposerDispatch) => {
                    covered += 1;
                    let mut renderer = headless_renderer();
                    renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("abc");
                    let dispatch = renderer.dispatch_composer_key(event);
                    assert!(
                        matches!(
                            dispatch,
                            ComposerDispatch::Handled {
                                input_changed: true
                            }
                        ),
                        "{why}: Shift+Enter inserts a newline, not a submit; got \
                         {dispatch:?}"
                    );
                    assert_eq!(
                        renderer.input_textarea.lines(),
                        ["abc", ""],
                        "{why}: the newline must be an in-buffer line"
                    );
                }
                ("↑", ShortcutAuthority::ComposerDispatch) => {
                    covered += 1;
                    let mut renderer = headless_renderer();
                    renderer.command_history = vec!["/pop".to_string()];
                    let dispatch = renderer.dispatch_composer_key(event);
                    assert!(
                        matches!(
                            dispatch,
                            ComposerDispatch::Handled {
                                input_changed: true
                            }
                        ),
                        "{why}: Up must be claimed by history recall; got {dispatch:?}"
                    );
                    assert_eq!(
                        renderer.input_textarea.lines(),
                        ["/pop"],
                        "{why}: Up must recall the most recent history line"
                    );
                }
                ("↓", ShortcutAuthority::ComposerDispatch) => {
                    covered += 1;
                    let mut renderer = headless_renderer();
                    renderer.command_history = vec!["/one".to_string(), "/two".to_string()];
                    renderer.dispatch_composer_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
                    let dispatch = renderer.dispatch_composer_key(event);
                    assert!(
                        matches!(
                            dispatch,
                            ComposerDispatch::Handled {
                                input_changed: true
                            }
                        ),
                        "{why}: Down must be claimed by history recall; got {dispatch:?}"
                    );
                    assert_eq!(
                        renderer.input_textarea.lines(),
                        [""],
                        "{why}: Down past the newest entry must return to the empty draft"
                    );
                }
                ("PgUp", ShortcutAuthority::ConversationScroll)
                | ("PgDn", ShortcutAuthority::ConversationScroll) => {
                    covered += 1;
                    // The scroll state lives behind the renderer's private
                    // transcript field; the behavior is pinned against
                    // handle_accordion_key in lib.rs's
                    // test_page_shortcut_table_entries_scroll_the_conversation.
                    assert_eq!(
                        entry.code,
                        if entry.label == "PgUp" {
                            KeyCode::PageUp
                        } else {
                            KeyCode::PageDown
                        },
                        "{why}: the page bindings must name their scroll keys"
                    );
                }
                (label, authority) => panic!(
                    "invariant: every KEYBOARD_SHORTCUTS entry needs a behavioral \
                     case against its real dispatcher; label={label:?} \
                     authority={authority:?} — extend this test with the entry"
                ),
            }
        }
        assert_eq!(
            covered,
            KEYBOARD_SHORTCUTS.len(),
            "invariant: every binding-table entry must reach exactly one \
             behavioral case; covered={covered} table={:?}",
            KEYBOARD_SHORTCUTS
                .iter()
                .map(|b| b.label)
                .collect::<Vec<_>>()
        );
    }

    /// INVARIANT: the composer guards consume the binding table, so
    /// `owns` must reproduce exactly the pre-table match-arm predicates — a
    /// guard rewrite cannot silently change which key triggers which shortcut.
    #[test]
    fn test_composer_shortcut_guards_match_the_pre_refactor_match_arms() {
        let cases: &[(KeyEvent, &[&str])] = &[
            (ctrl(KeyCode::Char('c')), &["Ctrl+C"]),
            (key(KeyCode::Char('c')), &[]),
            (ctrl(KeyCode::Char('v')), &["Ctrl+V"]),
            (
                KeyEvent::new(KeyCode::Char('v'), KeyModifiers::SUPER),
                &["Ctrl+V"],
            ),
            (key(KeyCode::Char('v')), &[]),
            (ctrl(KeyCode::Char('g')), &["Ctrl+G"]),
            (ctrl(KeyCode::Char('b')), &["Ctrl+B"]),
            (ctrl(KeyCode::Char('z')), &["Ctrl+Z"]),
            (ctrl(KeyCode::Char('p')), &["Ctrl+P"]),
            (ctrl(KeyCode::Char('d')), &["Ctrl+D"]),
            (ctrl(KeyCode::Char('/')), &["Ctrl+/"]),
            (key(KeyCode::Esc), &["Esc"]),
            (KeyEvent::new(KeyCode::Esc, KeyModifiers::SHIFT), &["Esc"]),
            (key(KeyCode::BackTab), &["Shift+Tab"]),
            (
                KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
                &["Shift+Tab"],
            ),
            (key(KeyCode::Enter), &[]),
            (KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), &[]),
            (key(KeyCode::Tab), &[]),
            (ctrl(KeyCode::Char('x')), &[]),
        ];
        for (event, expected) in cases {
            for binding in KEYBOARD_SHORTCUTS
                .iter()
                .filter(|b| b.authority == ShortcutAuthority::ComposerShortcut)
            {
                let owns = binding.owns(event);
                assert_eq!(
                    owns,
                    expected.contains(&binding.label),
                    "invariant: the composer guard must match the pre-table match \
                     arm for its key; binding={:?} label={:?} key={:?} owns={owns}",
                    binding.description,
                    binding.label,
                    event
                );
            }
        }
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
