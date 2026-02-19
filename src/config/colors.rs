// Color Scheme Configuration - Customizable TUI colors
//
// Allows users to customize terminal UI colors for accessibility
// and personal preference.

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

/// Predefined color themes for different terminal backgrounds
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorTheme {
    /// White text on black background (default)
    Dark,
    /// Black text on white background
    Light,
    /// High contrast yellow/white on black
    HighContrast,
    /// Solarized Dark palette
    Solarized,
}

impl ColorTheme {
    /// Convert theme to color scheme
    pub fn to_scheme(&self) -> ColorScheme {
        match self {
            Self::Dark => Self::dark_scheme(),
            Self::Light => Self::light_scheme(),
            Self::HighContrast => Self::high_contrast_scheme(),
            Self::Solarized => Self::solarized_scheme(),
        }
    }

    fn dark_scheme() -> ColorScheme {
        ColorScheme {
            status: StatusColors {
                live_stats: default_green(),
                training: default_dark_gray(),
                download: default_cyan(),
                operation: default_yellow(),
                border: default_gray(),
            },
            messages: MessageColors {
                user: default_cyan(),
                assistant: default_white(),
                system: default_dark_gray(),
                error: default_red(),
                tool: default_yellow(),
            },
            ui: UiColors {
                border: default_gray(),
                separator: default_dark_gray(),
                input: default_white(),
                cursor: default_cyan(),
            },
            dialog: DialogColors {
                border: default_cyan(),
                title: default_cyan(),
                selected_bg: default_cyan(),
                selected_fg: default_black(),
                option: default_cyan(),
            },
        }
    }

    fn light_scheme() -> ColorScheme {
        ColorScheme {
            status: StatusColors {
                live_stats: ColorSpec::Rgb(0, 128, 0), // Dark green
                training: ColorSpec::Named("gray".to_string()),
                download: ColorSpec::Rgb(0, 0, 139), // Dark blue
                operation: ColorSpec::Rgb(184, 134, 11), // Dark goldenrod
                border: ColorSpec::Named("darkgray".to_string()),
            },
            messages: MessageColors {
                user: ColorSpec::Rgb(0, 0, 255), // Blue
                assistant: ColorSpec::Named("black".to_string()),
                system: ColorSpec::Named("gray".to_string()),
                error: ColorSpec::Named("red".to_string()),
                tool: ColorSpec::Rgb(139, 69, 19), // Brown
            },
            ui: UiColors {
                border: ColorSpec::Named("darkgray".to_string()),
                separator: ColorSpec::Named("gray".to_string()),
                input: ColorSpec::Named("black".to_string()),
                cursor: ColorSpec::Rgb(0, 0, 255), // Blue
            },
            dialog: DialogColors {
                border: ColorSpec::Rgb(0, 0, 139), // Dark blue
                title: ColorSpec::Rgb(0, 0, 139),
                selected_bg: ColorSpec::Rgb(0, 0, 139),
                selected_fg: ColorSpec::Named("white".to_string()),
                option: ColorSpec::Rgb(0, 0, 139),
            },
        }
    }

    fn high_contrast_scheme() -> ColorScheme {
        ColorScheme {
            status: StatusColors {
                live_stats: ColorSpec::Named("yellow".to_string()),
                training: ColorSpec::Named("white".to_string()),
                download: ColorSpec::Named("cyan".to_string()),
                operation: ColorSpec::Named("yellow".to_string()),
                border: ColorSpec::Named("white".to_string()),
            },
            messages: MessageColors {
                user: ColorSpec::Named("yellow".to_string()),
                assistant: ColorSpec::Named("white".to_string()),
                system: ColorSpec::Named("gray".to_string()),
                error: ColorSpec::Named("red".to_string()),
                tool: ColorSpec::Named("cyan".to_string()),
            },
            ui: UiColors {
                border: ColorSpec::Named("white".to_string()),
                separator: ColorSpec::Named("gray".to_string()),
                input: ColorSpec::Named("yellow".to_string()),
                cursor: ColorSpec::Named("yellow".to_string()),
            },
            dialog: DialogColors {
                border: ColorSpec::Named("yellow".to_string()),
                title: ColorSpec::Named("yellow".to_string()),
                selected_bg: ColorSpec::Named("yellow".to_string()),
                selected_fg: ColorSpec::Named("black".to_string()),
                option: ColorSpec::Named("yellow".to_string()),
            },
        }
    }

    fn solarized_scheme() -> ColorScheme {
        // Solarized Dark color palette
        ColorScheme {
            status: StatusColors {
                live_stats: ColorSpec::Rgb(133, 153, 0), // Solarized green
                training: ColorSpec::Rgb(88, 110, 117), // Solarized base01
                download: ColorSpec::Rgb(38, 139, 210), // Solarized blue
                operation: ColorSpec::Rgb(181, 137, 0), // Solarized yellow
                border: ColorSpec::Rgb(101, 123, 131), // Solarized base0
            },
            messages: MessageColors {
                user: ColorSpec::Rgb(38, 139, 210), // Solarized blue
                assistant: ColorSpec::Rgb(147, 161, 161), // Solarized base1
                system: ColorSpec::Rgb(88, 110, 117), // Solarized base01
                error: ColorSpec::Rgb(220, 50, 47), // Solarized red
                tool: ColorSpec::Rgb(181, 137, 0), // Solarized yellow
            },
            ui: UiColors {
                border: ColorSpec::Rgb(101, 123, 131), // Solarized base0
                separator: ColorSpec::Rgb(88, 110, 117), // Solarized base01
                input: ColorSpec::Rgb(147, 161, 161), // Solarized base1
                cursor: ColorSpec::Rgb(38, 139, 210), // Solarized blue
            },
            dialog: DialogColors {
                border: ColorSpec::Rgb(38, 139, 210), // Solarized blue
                title: ColorSpec::Rgb(38, 139, 210),
                selected_bg: ColorSpec::Rgb(38, 139, 210),
                selected_fg: ColorSpec::Rgb(0, 43, 54), // Solarized base03
                option: ColorSpec::Rgb(38, 139, 210),
            },
        }
    }

    /// Get all available themes
    pub fn all() -> Vec<Self> {
        vec![Self::Dark, Self::Light, Self::HighContrast, Self::Solarized]
    }

    /// Get theme name for display
    pub fn name(&self) -> &str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::HighContrast => "High Contrast",
            Self::Solarized => "Solarized",
        }
    }

    /// Get theme description
    pub fn description(&self) -> &str {
        match self {
            Self::Dark => "White text on black background (default)",
            Self::Light => "Black text on white background",
            Self::HighContrast => "Yellow/white on black (accessibility)",
            Self::Solarized => "Solarized Dark color palette",
        }
    }
}

impl Default for ColorTheme {
    fn default() -> Self {
        Self::Dark
    }
}

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
    #[serde(default = "default_cyan")]
    pub option: ColorSpec,
}

fn default_dialog_colors() -> DialogColors {
    DialogColors {
        border: default_cyan(),
        title: default_cyan(),
        selected_bg: default_cyan(),
        selected_fg: default_black(),
        option: default_cyan(),
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
