// Scrollback Buffer - Internal message storage with trait-based messages
//
// Provides full control over conversation history with:
// - Trait-based messages (polymorphic, reactive)
// - Live updates (messages can change via Arc<RwLock<>>)
// - Scroll position tracking
// - Efficient viewport rendering

use std::collections::VecDeque;

use crate::cli::messages::{MessageId, MessageRef};

/// Calculate display height for a message (number of terminal lines needed)
fn calculate_display_height(content: &str, terminal_width: usize) -> usize {
    // Simple line counting - will be refined with proper wrapping
    let lines = content.lines().count();
    if lines == 0 {
        1
    } else {
        // Account for wrapping (approximate)
        let wrapped_lines: usize = content.lines()
            .map(|line| {
                let visible_len = strip_ansi_codes(line).len();
                (visible_len + terminal_width - 1) / terminal_width.max(1)
            })
            .sum();
        wrapped_lines.max(1)
    }
}

/// Strip ANSI escape codes for length calculation
fn strip_ansi_codes(s: &str) -> String {
    // Simple ANSI stripping (will use a proper library later)
    let mut result = String::new();
    let mut in_escape = false;

    for ch in s.chars() {
        if ch == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if ch == 'm' {
                in_escape = false;
            }
        } else {
            result.push(ch);
        }
    }

    result
}

/// Reference to a line within a message
#[derive(Debug, Clone)]
struct LineRef {
    message_id: MessageId,
    line_offset: usize,  // 0-indexed line within message
}

/// Scrollback buffer for conversation history
pub struct ScrollbackBuffer {
    /// All messages in chronological order (trait objects)
    messages: Vec<MessageRef>,

    /// Current scroll position (0 = bottom/latest, higher = scrolled up)
    scroll_offset: usize,

    /// Height of the viewport (lines available for messages)
    pub viewport_height: usize,

    /// Terminal width (for wrapping calculations)
    pub terminal_width: usize,

    /// Auto-scroll to bottom on new messages
    auto_scroll: bool,

    /// Ring buffer of line references (bounded memory)
    ring_buffer: VecDeque<LineRef>,

    /// Maximum lines in ring buffer
    max_lines: usize,

    /// Position of most recent line (renders at viewport bottom)
    most_recent_line: usize,
}

impl ScrollbackBuffer {
    /// Create a new scrollback buffer
    pub fn new(viewport_height: usize, terminal_width: usize) -> Self {
        Self {
            messages: Vec::new(),
            scroll_offset: 0,
            viewport_height,
            terminal_width,
            auto_scroll: true,
            ring_buffer: VecDeque::new(),
            max_lines: 1000,  // Configurable
            most_recent_line: 0,
        }
    }

    /// Add a new message to the buffer
    pub fn add_message(&mut self, message: MessageRef) -> MessageId {
        let id = message.id();
        self.messages.push(message);

        // Auto-scroll to bottom if enabled
        if self.auto_scroll {
            self.scroll_to_bottom();
        }

        id
    }

    /// Get a message by ID (returns cloned Arc)
    pub fn get_message(&self, id: MessageId) -> Option<MessageRef> {
        self.messages.iter().find(|m| m.id() == id).cloned()
    }

    /// Get the last message (most recent)
    pub fn get_last_message(&self) -> Option<MessageRef> {
        self.messages.last().cloned()
    }

    /// Get total number of messages
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Get visible messages for current viewport
    pub fn get_visible_messages(&self) -> Vec<MessageRef> {
        if self.messages.is_empty() {
            return Vec::new();
        }

        // Calculate total height of all messages
        let message_heights: Vec<usize> = self.messages
            .iter()
            .map(|m| calculate_display_height(&m.format(&crate::config::ColorScheme::default()), self.terminal_width))
            .collect();

        let total_height: usize = message_heights.iter().sum();

        // If all messages fit in viewport, show them all
        if total_height <= self.viewport_height {
            return self.messages.clone();
        }

        // Calculate which messages are visible based on scroll offset
        let mut lines_from_bottom = self.scroll_offset;
        let mut visible = Vec::new();

        // Walk backwards from the end
        for (idx, height) in message_heights.iter().enumerate().rev() {
            let msg_idx = idx;

            if lines_from_bottom < self.viewport_height {
                visible.push(self.messages[msg_idx].clone());
            }

            lines_from_bottom += height;

            if lines_from_bottom >= self.viewport_height + self.scroll_offset {
                break;
            }
        }

        visible.reverse();
        visible
    }

    /// Scroll up (away from bottom) by N lines
    pub fn scroll_up(&mut self, lines: usize) {
        let total_height: usize = self.messages
            .iter()
            .map(|m| calculate_display_height(&m.format(&crate::config::ColorScheme::default()), self.terminal_width))
            .sum();

        let max_scroll = total_height.saturating_sub(self.viewport_height);
        self.scroll_offset = (self.scroll_offset + lines).min(max_scroll);

        // Disable auto-scroll when user scrolls up
        if self.scroll_offset > 0 {
            self.auto_scroll = false;
        }
    }

    /// Scroll down (toward bottom) by N lines
    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);

        // Re-enable auto-scroll when scrolled to bottom
        if self.scroll_offset == 0 {
            self.auto_scroll = true;
        }
    }

    /// Scroll to the very bottom (most recent messages)
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }

    /// Scroll to the very top (oldest messages)
    pub fn scroll_to_top(&mut self) {
        let total_height: usize = self.messages
            .iter()
            .map(|m| calculate_display_height(&m.format(&crate::config::ColorScheme::default()), self.terminal_width))
            .sum();

        self.scroll_offset = total_height.saturating_sub(self.viewport_height);
        self.auto_scroll = false;
    }

    /// Update viewport dimensions (called on terminal resize)
    pub fn update_viewport(&mut self, height: usize, width: usize) {
        self.viewport_height = height;
        self.terminal_width = width;
    }

    /// Check if currently at bottom (for UI indicators)
    pub fn is_at_bottom(&self) -> bool {
        self.scroll_offset == 0
    }

    /// Get scroll position as percentage (0-100)
    pub fn scroll_percentage(&self) -> u8 {
        let total_height: usize = self.messages
            .iter()
            .map(|m| calculate_display_height(&m.format(&crate::config::ColorScheme::default()), self.terminal_width))
            .sum();

        if total_height <= self.viewport_height {
            return 100; // Everything visible
        }

        let max_scroll = total_height.saturating_sub(self.viewport_height);
        if max_scroll == 0 {
            return 100;
        }

        let position_from_top = max_scroll.saturating_sub(self.scroll_offset);
        ((position_from_top * 100) / max_scroll).min(100) as u8
    }

    /// Clear all messages
    pub fn clear(&mut self) {
        self.messages.clear();
        self.scroll_offset = 0;
        self.auto_scroll = true;
        self.ring_buffer.clear();
        self.most_recent_line = 0;
    }

    /// Add a line to ring buffer (when rendering new content)
    pub fn push_line(&mut self, message_id: MessageId, line_offset: usize) {
        if self.ring_buffer.len() >= self.max_lines {
            self.ring_buffer.pop_front(); // Drop oldest
            // Adjust most_recent_line if needed
            if self.most_recent_line > 0 {
                self.most_recent_line -= 1;
            }
        }
        self.ring_buffer.push_back(LineRef {
            message_id,
            line_offset,
        });
        self.most_recent_line = self.ring_buffer.len().saturating_sub(1);
    }

    /// Get lines for viewport rendering (from ring buffer)
    pub fn get_viewport_lines(&self) -> Vec<(MessageId, usize)> {
        if self.ring_buffer.is_empty() {
            return Vec::new();
        }

        let start_idx = self.most_recent_line.saturating_sub(self.viewport_height.saturating_sub(1));
        let end_idx = self.most_recent_line + 1;

        self.ring_buffer
            .iter()
            .skip(start_idx)
            .take(end_idx - start_idx)
            .map(|line_ref| (line_ref.message_id, line_ref.line_offset))
            .collect()
    }

    /// Rebuild ring buffer from messages (e.g., after resize)
    pub fn rebuild_ring_buffer(&mut self) {
        self.ring_buffer.clear();
        self.most_recent_line = 0;

        // Collect message data first to avoid borrow checker issues
        let message_data: Vec<(MessageId, usize)> = self.messages
            .iter()
            .map(|msg| (msg.id(), calculate_display_height(&msg.format(&crate::config::ColorScheme::default()), self.terminal_width)))
            .collect();

        for (message_id, height) in message_data {
            for line_offset in 0..height {
                self.push_line(message_id, line_offset);
            }
        }
    }
}

// Tests temporarily disabled during trait migration
// Will be updated in task #17 to use trait-based messages
#[cfg(test)]
mod tests {
    // TODO: Update tests to use trait-based message system
    // use super::*;
    // use crate::cli::messages::{UserQueryMessage, StreamingResponseMessage};
}
