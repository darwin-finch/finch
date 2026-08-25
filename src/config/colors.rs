// Color Scheme Configuration - Customizable TUI colors
//
// Allows users to customize terminal UI colors for accessibility
// and personal preference.

use ratatui::style::{Color, Style};
use serde::{Deserialize, Serialize};

/// Predefined color themes for different terminal backgrounds
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum ColorTheme {
    /// White text on black background (default)
    #[default]
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
                training: ColorSpec::Rgb(88, 110, 117),  // Solarized base01
                download: ColorSpec::Rgb(38, 139, 210),  // Solarized blue
                operation: ColorSpec::Rgb(181, 137, 0),  // Solarized yellow
                border: ColorSpec::Rgb(101, 123, 131),   // Solarized base0
            },
            messages: MessageColors {
                user: ColorSpec::Rgb(38, 139, 210),       // Solarized blue
                assistant: ColorSpec::Rgb(147, 161, 161), // Solarized base1
                system: ColorSpec::Rgb(88, 110, 117),     // Solarized base01
                error: ColorSpec::Rgb(220, 50, 47),       // Solarized red
                tool: ColorSpec::Rgb(181, 137, 0),        // Solarized yellow
            },
            ui: UiColors {
                border: ColorSpec::Rgb(101, 123, 131),   // Solarized base0
                separator: ColorSpec::Rgb(88, 110, 117), // Solarized base01
                input: ColorSpec::Rgb(147, 161, 161),    // Solarized base1
                cursor: ColorSpec::Rgb(38, 139, 210),    // Solarized blue
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

/// Semantic full-row bands used by the transcript renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageBand {
    LocalUser,
    Participant(usize),
    Assistant,
    ProgramSource,
    Tool,
    ProgramOutput,
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

impl ColorScheme {
    /// Return a subtle, contrast-safe full-row style for a transcript role.
    /// A light assistant foreground indicates a dark terminal palette (and
    /// vice versa), so custom schemes need no second theme discriminator.
    pub fn message_band_style(&self, band: MessageBand) -> Style {
        const LIGHT_PARTICIPANTS: [(u8, u8, u8); 8] = [
            (226, 238, 255),
            (232, 246, 230),
            (255, 239, 219),
            (243, 231, 255),
            (224, 246, 246),
            (255, 229, 235),
            (241, 240, 218),
            (231, 235, 242),
        ];
        const DARK_PARTICIPANTS: [(u8, u8, u8); 8] = [
            (24, 49, 70),
            (27, 55, 42),
            (62, 44, 24),
            (51, 36, 66),
            (22, 53, 55),
            (65, 34, 43),
            (54, 52, 27),
            (42, 47, 58),
        ];

        let dark_terminal = color_luminance(&self.messages.assistant) >= 0.5;
        let (foreground, background) = if dark_terminal {
            let background = match band {
                MessageBand::LocalUser => (28, 45, 64),
                MessageBand::Participant(index) => {
                    DARK_PARTICIPANTS[index % DARK_PARTICIPANTS.len()]
                }
                MessageBand::Assistant => (32, 36, 43),
                MessageBand::ProgramSource => (45, 35, 55),
                MessageBand::Tool => (50, 43, 22),
                MessageBand::ProgramOutput => (24, 49, 42),
            };
            (Color::Rgb(245, 247, 250), background)
        } else {
            let background = match band {
                MessageBand::LocalUser => (226, 238, 255),
                MessageBand::Participant(index) => {
                    LIGHT_PARTICIPANTS[index % LIGHT_PARTICIPANTS.len()]
                }
                MessageBand::Assistant => (247, 247, 244),
                MessageBand::ProgramSource => (243, 231, 255),
                MessageBand::Tool => (255, 243, 214),
                MessageBand::ProgramOutput => (224, 246, 236),
            };
            (Color::Rgb(18, 22, 28), background)
        };

        Style::default()
            .fg(foreground)
            .bg(Color::Rgb(background.0, background.1, background.2))
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

fn color_luminance(color: &ColorSpec) -> f32 {
    let (red, green, blue) = match color.to_color() {
        Color::Black => (0, 0, 0),
        Color::Red | Color::LightRed => (255, 0, 0),
        Color::Green | Color::LightGreen => (0, 255, 0),
        Color::Yellow | Color::LightYellow => (255, 255, 0),
        Color::Blue | Color::LightBlue => (0, 0, 255),
        Color::Magenta | Color::LightMagenta => (255, 0, 255),
        Color::Cyan | Color::LightCyan => (0, 255, 255),
        Color::Gray | Color::White => (255, 255, 255),
        Color::DarkGray => (128, 128, 128),
        Color::Rgb(red, green, blue) => (red, green, blue),
        _ => (255, 255, 255),
    };
    (0.2126 * f32::from(red) + 0.7152 * f32::from(green) + 0.0722 * f32::from(blue)) / 255.0
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

    fn rgb(color: Color) -> (u8, u8, u8) {
        match color {
            Color::Rgb(red, green, blue) => (red, green, blue),
            other => panic!("expected RGB color, got {other:?}"),
        }
    }

    fn contrast(style: Style) -> f32 {
        fn linear(component: u8) -> f32 {
            let value = f32::from(component) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        fn relative((red, green, blue): (u8, u8, u8)) -> f32 {
            0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
        }

        let foreground = relative(rgb(style.fg.expect("band foreground")));
        let background = relative(rgb(style.bg.expect("band background")));
        (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
    }

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

    #[test]
    fn transcript_bands_are_distinct_and_legible_in_light_and_dark_themes() {
        let bands = [
            MessageBand::LocalUser,
            MessageBand::Assistant,
            MessageBand::ProgramSource,
            MessageBand::Tool,
            MessageBand::ProgramOutput,
        ];

        for theme in [ColorTheme::Dark, ColorTheme::Light] {
            let scheme = theme.to_scheme();
            let styles = bands.map(|band| scheme.message_band_style(band));
            for style in styles {
                assert!(
                    contrast(style) >= 7.0,
                    "{theme:?} band contrast was too low"
                );
            }
            for (index, style) in styles.iter().enumerate() {
                assert!(styles[index + 1..].iter().all(|other| style.bg != other.bg));
            }
        }
    }

    #[test]
    fn participant_palette_switches_with_theme_and_is_index_stable() {
        let dark = ColorTheme::Dark.to_scheme();
        let light = ColorTheme::Light.to_scheme();
        let dark_alice = dark.message_band_style(MessageBand::Participant(3));

        assert_eq!(
            dark_alice,
            dark.message_band_style(MessageBand::Participant(3))
        );
        assert_ne!(
            dark_alice.bg,
            dark.message_band_style(MessageBand::Participant(4)).bg
        );
        assert_ne!(
            dark_alice.bg,
            light.message_band_style(MessageBand::Participant(3)).bg
        );
        assert!(contrast(dark_alice) >= 7.0);
        assert!(contrast(light.message_band_style(MessageBand::Participant(3))) >= 7.0);
    }
}
