// Color Scheme Configuration - Customizable TUI colors
//
// Allows users to customize terminal UI colors for accessibility
// and personal preference.

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

/// Color scheme for TUI elements
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorScheme {
    /// Status bar colors
    #[serde(default = "default_status_colors")]
    pub status: StatusColors,

    /// Message colors
    #[serde(default = "default_message_colors")]
    pub messages: MessageColors,

    /// Border and UI element colors
    #[serde(default = "default_ui_colors")]
    pub ui: UiColors,

    /// Dialog colors
    #[serde(default = "default_dialog_colors")]
    pub dialog: DialogColors,
}

impl Default for ColorScheme {
    fn default() -> Self {
        Self {
            status: default_status_colors(),
            messages: default_message_colors(),
            ui: default_ui_colors(),
            dialog: default_dialog_colors(),
        }
    }
}

/// Status bar color configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusColors {
    /// Live stats (tokens, latency, etc.)
    #[serde(default = "default_green")]
    pub live_stats: ColorSpec,

    /// Training statistics
    #[serde(default = "default_dark_gray")]
    pub training: ColorSpec,

    /// Download progress
    #[serde(default = "default_cyan")]
    pub download: ColorSpec,

    /// Operation status
    #[serde(default = "default_yellow")]
    pub operation: ColorSpec,

    /// Border color
    #[serde(default = "default_gray")]
    pub border: ColorSpec,
}

fn default_status_colors() -> StatusColors {
    StatusColors {
        live_stats: default_green(),
        training: default_dark_gray(),
        download: default_cyan(),
        operation: default_yellow(),
        border: default_gray(),
    }
}

/// Message display colors
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageColors {
    /// User messages
    #[serde(default = "default_cyan")]
    pub user: ColorSpec,

    /// Assistant messages
    #[serde(default = "default_white")]
    pub assistant: ColorSpec,

    /// System messages
    #[serde(default = "default_dark_gray")]
    pub system: ColorSpec,

    /// Error messages
    #[serde(default = "default_red")]
    pub error: ColorSpec,

    /// Tool use markers
    #[serde(default = "default_yellow")]
    pub tool: ColorSpec,
}

fn default_message_colors() -> MessageColors {
    MessageColors {
        user: default_cyan(),
        assistant: default_white(),
        system: default_dark_gray(),
        error: default_red(),
        tool: default_yellow(),
    }
}

/// UI element colors
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiColors {
    /// Borders
    #[serde(default = "default_gray")]
    pub border: ColorSpec,

    /// Separator lines
    #[serde(default = "default_dark_gray")]
    pub separator: ColorSpec,

    /// Input text
    #[serde(default = "default_white")]
    pub input: ColorSpec,

    /// Cursor
    #[serde(default = "default_cyan")]
    pub cursor: ColorSpec,
}

fn default_ui_colors() -> UiColors {
    UiColors {
        border: default_gray(),
        separator: default_dark_gray(),
        input: default_white(),
        cursor: default_cyan(),
    }
}

/// Dialog color configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialogColors {
    /// Dialog border
    #[serde(default = "default_cyan")]
    pub border: ColorSpec,

    /// Dialog title
    #[serde(default = "default_cyan")]
    pub title: ColorSpec,

    /// Selected option background
    #[serde(default = "default_cyan")]
    pub selected_bg: ColorSpec,

    /// Selected option text
    #[serde(default = "default_black")]
    pub selected_fg: ColorSpec,

    /// Normal option text
    #[serde(default = "default_white")]
    pub option: ColorSpec,
}

fn default_dialog_colors() -> DialogColors {
    DialogColors {
        border: default_cyan(),
        title: default_cyan(),
        selected_bg: default_cyan(),
        selected_fg: default_black(),
        option: default_white(),
    }
}

/// Color specification - supports named colors and RGB
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ColorSpec {
    /// Named color (e.g., "red", "green", "cyan")
    Named(String),
    /// RGB color (e.g., [255, 0, 0])
    Rgb(u8, u8, u8),
}

impl ColorSpec {
    /// Convert to ratatui Color
    pub fn to_color(&self) -> Color {
        match self {
            ColorSpec::Named(name) => parse_named_color(name),
            ColorSpec::Rgb(r, g, b) => Color::Rgb(*r, *g, *b),
        }
    }
}

/// Parse named color string to ratatui Color
fn parse_named_color(name: &str) -> Color {
    match name.to_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "white" => Color::White,
        _ => Color::White, // Default fallback
    }
}

// Default color constructors
fn default_green() -> ColorSpec {
    ColorSpec::Named("green".to_string())
}

fn default_dark_gray() -> ColorSpec {
    ColorSpec::Named("darkgray".to_string())
}

fn default_cyan() -> ColorSpec {
    ColorSpec::Named("cyan".to_string())
}

fn default_yellow() -> ColorSpec {
    ColorSpec::Named("yellow".to_string())
}

fn default_gray() -> ColorSpec {
    ColorSpec::Named("gray".to_string())
}

fn default_white() -> ColorSpec {
    ColorSpec::Named("white".to_string())
}

fn default_red() -> ColorSpec {
    ColorSpec::Named("red".to_string())
}

fn default_black() -> ColorSpec {
    ColorSpec::Named("black".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_color_scheme() {
        let scheme = ColorScheme::default();

        // Check status colors
        assert!(matches!(scheme.status.live_stats, ColorSpec::Named(_)));

        // Check message colors
        assert!(matches!(scheme.messages.user, ColorSpec::Named(_)));

        // Check UI colors
        assert!(matches!(scheme.ui.border, ColorSpec::Named(_)));
    }

    #[test]
    fn test_named_color_parsing() {
        let color = parse_named_color("cyan");
        assert_eq!(color, Color::Cyan);

        let color = parse_named_color("darkgray");
        assert_eq!(color, Color::DarkGray);

        let color = parse_named_color("unknown");
        assert_eq!(color, Color::White); // Fallback
    }

    #[test]
    fn test_rgb_color() {
        let spec = ColorSpec::Rgb(255, 0, 0);
        let color = spec.to_color();
        assert_eq!(color, Color::Rgb(255, 0, 0));
    }

    #[test]
    fn test_color_spec_to_color() {
        let spec = ColorSpec::Named("green".to_string());
        assert_eq!(spec.to_color(), Color::Green);

        let spec = ColorSpec::Rgb(128, 128, 128);
        assert_eq!(spec.to_color(), Color::Rgb(128, 128, 128));
    }
}
