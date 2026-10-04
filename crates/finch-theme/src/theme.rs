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
            background: default_black(),
            foreground: default_white(),
            highlight_bg: default_blue(),
            highlight_fg: default_white(),
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

    /// Light preset. Every role is an explicit RGB value from the GitHub
    /// Primer light palette rather than an ANSI name: named colours resolve
    /// through the terminal's own 16-colour table, so "white" and "gray" land
    /// on whatever the profile maps them to and cannot guarantee contrast
    /// against the canvas this scheme paints.
    fn light_scheme() -> ColorScheme {
        let canvas = ColorSpec::Rgb(255, 255, 255); // #ffffff
        let ink = ColorSpec::Rgb(31, 35, 40); // fg.default #1f2328
        let muted = ColorSpec::Rgb(89, 99, 110); // fg.muted #59636e
        let accent = ColorSpec::Rgb(9, 105, 218); // accent.fg #0969da
        let success = ColorSpec::Rgb(26, 127, 55); // success.fg #1a7f37
        let attention = ColorSpec::Rgb(154, 103, 0); // attention.fg #9a6700
        let danger = ColorSpec::Rgb(207, 34, 46); // danger.fg #cf222e
        ColorScheme {
            background: canvas.clone(),
            foreground: ink.clone(),
            highlight_bg: accent.clone(),
            highlight_fg: canvas.clone(),
            status: StatusColors {
                live_stats: success,
                training: muted.clone(),
                download: accent.clone(),
                operation: attention.clone(),
                border: muted.clone(),
            },
            messages: MessageColors {
                user: accent.clone(),
                assistant: ink.clone(),
                system: muted.clone(),
                error: danger,
                tool: attention,
            },
            ui: UiColors {
                border: muted.clone(),
                separator: muted,
                input: ink,
                cursor: accent.clone(),
            },
            dialog: DialogColors {
                border: accent.clone(),
                title: accent.clone(),
                selected_bg: accent.clone(),
                selected_fg: canvas,
                option: accent,
            },
        }
    }

    fn high_contrast_scheme() -> ColorScheme {
        ColorScheme {
            background: default_black(),
            foreground: default_white(),
            highlight_bg: ColorSpec::Named("yellow".to_string()),
            highlight_fg: default_black(),
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
            background: ColorSpec::Rgb(0, 43, 54),      // Solarized base03
            foreground: ColorSpec::Rgb(147, 161, 161),  // Solarized base1
            highlight_bg: ColorSpec::Rgb(38, 139, 210), // Solarized blue
            highlight_fg: ColorSpec::Rgb(0, 43, 54),    // Solarized base03
            status: StatusColors {
                live_stats: ColorSpec::Rgb(133, 153, 0), // Solarized green
                training: ColorSpec::Rgb(88, 110, 117),  // Solarized base01
                download: ColorSpec::Rgb(38, 139, 210),  // Solarized blue
                operation: ColorSpec::Rgb(181, 137, 0),  // Solarized yellow
                border: ColorSpec::Rgb(131, 148, 150),   // Solarized base0
            },
            messages: MessageColors {
                user: ColorSpec::Rgb(38, 139, 210),       // Solarized blue
                assistant: ColorSpec::Rgb(147, 161, 161), // Solarized base1
                system: ColorSpec::Rgb(88, 110, 117),     // Solarized base01
                error: ColorSpec::Rgb(220, 50, 47),       // Solarized red
                tool: ColorSpec::Rgb(181, 137, 0),        // Solarized yellow
            },
            ui: UiColors {
                border: ColorSpec::Rgb(131, 148, 150),   // Solarized base0
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

    /// Resolve a saved theme name (`active_theme` in the config, or a
    /// display name) to its preset. Case, spaces, hyphens and underscores are
    /// ignored so `high-contrast`, `high contrast` and `HighContrast` agree.
    pub fn from_name(name: &str) -> Option<Self> {
        let key: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        match key.as_str() {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            "highcontrast" => Some(Self::HighContrast),
            "solarized" | "solarizeddark" => Some(Self::Solarized),
            _ => None,
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
            Self::Light => "Dark text on a white background",
            Self::HighContrast => "Yellow/white on black (accessibility)",
            Self::Solarized => "Solarized Dark color palette",
        }
    }
}

/// Color scheme for TUI elements
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColorScheme {
    /// Terminal canvas background.
    #[serde(default = "default_black")]
    pub background: ColorSpec,

    /// Default text colour on the canvas.
    #[serde(default = "default_white")]
    pub foreground: ColorSpec,

    /// Background of highlighted (drag-selected) transcript text.
    #[serde(default = "default_blue")]
    pub highlight_bg: ColorSpec,

    /// Text colour of highlighted (drag-selected) transcript text.
    #[serde(default = "default_white")]
    pub highlight_fg: ColorSpec,

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
            background: default_black(),
            foreground: default_white(),
            highlight_bg: default_blue(),
            highlight_fg: default_white(),
            status: default_status_colors(),
            messages: default_message_colors(),
            ui: default_ui_colors(),
            dialog: default_dialog_colors(),
        }
    }
}

impl ColorScheme {
    /// True when this scheme is exactly one of the built-in presets, i.e. it
    /// carries no user customisation worth saving or honouring over a theme.
    pub fn is_builtin_preset(&self) -> bool {
        ColorTheme::all()
            .iter()
            .any(|theme| theme.to_scheme() == *self)
    }

    /// This scheme's rendering of one of the 16 ANSI palette colours
    /// (`index` 0–15: black, red, green, yellow, blue, magenta, cyan, gray,
    /// then dark gray and the bright set, ending in white).
    ///
    /// Text that was styled with a bare ANSI colour rather than a scheme role
    /// is mapped through this at paint, so it still follows the theme. A
    /// colour the scheme itself uses by name keeps meaning itself; any other
    /// maps to the role that colour conventionally signals (red is an error,
    /// green success, yellow an operation in progress, cyan and blue the
    /// accent, dark gray muted text, white and black the canvas pair).
    pub fn ansi_color(&self, index: u8) -> ColorSpec {
        let index = index % 16;
        let own = self
            .roles()
            .into_iter()
            .find(|spec| ansi_index(spec.to_color()) == Some(index));
        if let Some(spec) = own {
            return spec.clone();
        }
        match index {
            0 => &self.background,
            1 | 9 => &self.messages.error,
            2 | 10 => &self.status.live_stats,
            3 | 11 => &self.status.operation,
            4 | 12 => &self.dialog.border,
            5 | 13 => &self.dialog.title,
            6 | 14 => &self.ui.cursor,
            8 => &self.messages.system,
            _ => &self.foreground,
        }
        .clone()
    }

    /// Every colour role this scheme defines.
    fn roles(&self) -> Vec<&ColorSpec> {
        vec![
            &self.background,
            &self.foreground,
            &self.highlight_bg,
            &self.highlight_fg,
            &self.status.live_stats,
            &self.status.training,
            &self.status.download,
            &self.status.operation,
            &self.status.border,
            &self.messages.user,
            &self.messages.assistant,
            &self.messages.system,
            &self.messages.error,
            &self.messages.tool,
            &self.ui.border,
            &self.ui.separator,
            &self.ui.input,
            &self.ui.cursor,
            &self.dialog.border,
            &self.dialog.title,
            &self.dialog.selected_bg,
            &self.dialog.selected_fg,
            &self.dialog.option,
        ]
    }

    /// Returns true if this color scheme represents a dark terminal palette.
    pub fn is_dark(&self) -> bool {
        color_luminance(&self.foreground) >= 0.5
    }

    /// Return the subtle background used to identify interactive rows on hover.
    pub fn hover_background(&self) -> Color {
        if self.is_dark() {
            Color::Rgb(36, 38, 42)
        } else {
            Color::Rgb(240, 240, 243)
        }
    }

    /// Return a subtle, contrast-safe full-row style for a transcript role.
    /// A light assistant foreground indicates a dark terminal palette (and
    /// vice versa), so custom schemes need no second theme discriminator.
    pub fn message_band_style(&self, band: MessageBand) -> Style {
        const LIGHT_PARTICIPANTS: [(u8, u8, u8); 8] = [
            (222, 235, 252),
            (232, 246, 230),
            (255, 239, 219),
            (239, 229, 252),
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

        let dark_terminal = self.is_dark();
        let (foreground, background) = if dark_terminal {
            let background = match band {
                MessageBand::LocalUser => (24, 24, 27),
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
                MessageBand::LocalUser => (247, 247, 249),
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// The ANSI palette index (0–15) of a named colour, `None` for RGB.
fn ansi_index(color: Color) -> Option<u8> {
    Some(match color {
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
        _ => return None,
    })
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

/// Muted text/chrome that stays WCAG-AA against the product background `#080808`.
/// Named `darkgray` is ANSI bright-black; terminals render it as dim grey that
/// disappears on that background.
fn default_dark_gray() -> ColorSpec {
    ColorSpec::Rgb(180, 180, 180)
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

fn default_blue() -> ColorSpec {
    ColorSpec::Named("blue".to_string())
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

    /// A bare ANSI colour follows the theme: it keeps meaning itself in a
    /// scheme that uses that named colour, and otherwise takes the role the
    /// colour conventionally signals.
    #[test]
    fn test_ansi_color_keeps_a_schemes_own_named_colours_and_maps_the_rest_to_roles() {
        let dark = ColorTheme::Dark.to_scheme();
        assert_eq!(
            dark.ansi_color(6),
            ColorSpec::Named("cyan".to_string()),
            "Dark uses cyan by name, so ANSI cyan stays cyan"
        );
        assert_eq!(
            dark.ansi_color(8),
            dark.messages.system,
            "ANSI dark grey is the muted role; Dark's is the AA-contrast RGB grey"
        );
        assert_eq!(
            dark.ansi_color(14),
            dark.ui.cursor,
            "bright cyan, the old fixed accent, is the scheme accent"
        );

        let light = ColorTheme::Light.to_scheme();
        for (index, role, why) in [
            (0u8, &light.background, "black is the canvas"),
            (1, &light.messages.error, "red is an error"),
            (2, &light.status.live_stats, "green is success"),
            (3, &light.status.operation, "yellow is an operation"),
            (6, &light.ui.cursor, "cyan is the accent"),
            (8, &light.messages.system, "dark grey is muted text"),
            (15, &light.foreground, "white is the default ink"),
        ] {
            assert_eq!(&light.ansi_color(index), role, "Light: ANSI {index}: {why}");
        }
        assert_ne!(
            light.ansi_color(15),
            light.background,
            "Light: text styled 'white' must not vanish into the white canvas"
        );
    }

    /// Product `theme-color` from the shipped site (`#080808`).
    const PRODUCT_BACKGROUND: (u8, u8, u8) = (8, 8, 8);

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

    fn contrast_rgb(foreground: (u8, u8, u8), background: (u8, u8, u8)) -> f32 {
        let fg = relative(foreground);
        let bg = relative(background);
        (fg.max(bg) + 0.05) / (fg.min(bg) + 0.05)
    }

    fn contrast(style: Style) -> f32 {
        contrast_rgb(
            rgb(style.fg.expect("band foreground")),
            rgb(style.bg.expect("band background")),
        )
    }

    /// Map a spec to RGB using typical 16-color terminal values, not the
    /// theoretical 50% grey. ANSI bright-black (`DarkGray`) is commonly
    /// ~`#666666`, which is what makes the shipped `darkgray` default vanish
    /// on `#080808`.
    fn spec_terminal_rgb(spec: &ColorSpec) -> (u8, u8, u8) {
        match spec.to_color() {
            Color::Black => (0, 0, 0),
            Color::Red | Color::LightRed => (255, 0, 0),
            Color::Green | Color::LightGreen => (0, 255, 0),
            Color::Yellow | Color::LightYellow => (255, 255, 0),
            Color::Blue | Color::LightBlue => (0, 0, 255),
            Color::Magenta | Color::LightMagenta => (255, 0, 255),
            Color::Cyan | Color::LightCyan => (0, 255, 255),
            Color::White => (255, 255, 255),
            Color::Gray => (192, 192, 192),
            Color::DarkGray => (102, 102, 102),
            Color::Rgb(red, green, blue) => (red, green, blue),
            other => panic!("unmapped color {other:?} in default palette contrast check"),
        }
    }

    fn default_text_and_border_roles(scheme: &ColorScheme) -> Vec<(&'static str, &ColorSpec)> {
        vec![
            ("status.live_stats", &scheme.status.live_stats),
            ("status.training", &scheme.status.training),
            ("status.download", &scheme.status.download),
            ("status.operation", &scheme.status.operation),
            ("status.border", &scheme.status.border),
            ("messages.user", &scheme.messages.user),
            ("messages.assistant", &scheme.messages.assistant),
            ("messages.system", &scheme.messages.system),
            ("messages.error", &scheme.messages.error),
            ("messages.tool", &scheme.messages.tool),
            ("ui.border", &scheme.ui.border),
            ("ui.separator", &scheme.ui.separator),
            ("ui.input", &scheme.ui.input),
            ("ui.cursor", &scheme.ui.cursor),
            ("dialog.border", &scheme.dialog.border),
            ("dialog.title", &scheme.dialog.title),
            ("dialog.option", &scheme.dialog.option),
        ]
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
    fn test_high_contrast_theme_forces_black_canvas_background() {
        let scheme = ColorTheme::HighContrast.to_scheme();

        assert_eq!(
            scheme.background.to_color(),
            Color::Black,
            "High Contrast must force a black canvas instead of inheriting a light terminal background"
        );
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
    fn every_builtin_transcript_band_is_unique_and_legible() {
        let semantic_bands = [
            MessageBand::LocalUser,
            MessageBand::Assistant,
            MessageBand::ProgramSource,
            MessageBand::Tool,
            MessageBand::ProgramOutput,
        ];

        for theme in ColorTheme::all() {
            let scheme = theme.to_scheme();
            let mut styles = semantic_bands
                .map(|band| scheme.message_band_style(band))
                .to_vec();
            styles.extend(
                (0..8).map(|index| scheme.message_band_style(MessageBand::Participant(index))),
            );

            for style in &styles {
                assert!(
                    contrast(*style) >= 7.0,
                    "{theme:?} band contrast was too low"
                );
            }
            for (index, style) in styles.iter().enumerate() {
                assert!(
                    styles[index + 1..].iter().all(|other| style.bg != other.bg),
                    "{theme:?} reused transcript background {:?}",
                    style.bg
                );
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

    #[test]
    fn test_user_input_and_hover_backgrounds_are_subtle_and_readable() {
        let dark = ColorTheme::Dark.to_scheme();
        let light = ColorTheme::Light.to_scheme();

        assert_eq!(
            dark.message_band_style(MessageBand::LocalUser).bg,
            Some(Color::Rgb(24, 24, 27)),
            "dark user input should use a soft near-black background"
        );
        assert_eq!(
            dark.hover_background(),
            Color::Rgb(36, 38, 42),
            "dark hover should remain visible without overpowering row text"
        );
        assert_eq!(
            light.message_band_style(MessageBand::LocalUser).bg,
            Some(Color::Rgb(247, 247, 249)),
            "light user input should use a soft near-white background"
        );
        assert_eq!(
            light.hover_background(),
            Color::Rgb(240, 240, 243),
            "light hover should remain visible without overpowering row text"
        );
        assert!(
            contrast(dark.message_band_style(MessageBand::LocalUser)) >= 7.0,
            "softened dark user input must retain AAA text contrast"
        );
        assert!(
            contrast(light.message_band_style(MessageBand::LocalUser)) >= 7.0,
            "softened light user input must retain AAA text contrast"
        );
    }

    #[test]
    fn default_dark_palette_is_readable_on_product_background() {
        let schemes = [
            ("ColorScheme::default", ColorScheme::default()),
            ("ColorTheme::Dark", ColorTheme::Dark.to_scheme()),
        ];
        for (label, scheme) in &schemes {
            for (role, spec) in default_text_and_border_roles(scheme) {
                assert_ne!(
                    spec.to_color(),
                    Color::DarkGray,
                    "{label} {role} used ANSI DarkGray, which terminals render as dim grey on the product background {:?}",
                    PRODUCT_BACKGROUND
                );
                let rgb = spec_terminal_rgb(spec);
                let contrast = contrast_rgb(rgb, PRODUCT_BACKGROUND);
                assert!(
                    contrast >= 4.5,
                    "{label} {role} {rgb:?} contrast {contrast:.2} against product background {:?} is below WCAG AA 4.5",
                    PRODUCT_BACKGROUND
                );
            }
        }
    }
}
