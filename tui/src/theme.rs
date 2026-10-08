//! Colors, matching the web UI's tokens.

use ratatui::style::Color;

#[derive(Clone, Debug)]
pub struct Theme {
    pub bg: Color,
    pub surface: Color,
    pub surface2: Color,
    pub surface3: Color,
    pub line: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub accent: Color,
    pub amber: Color,
    pub danger: Color,
    pub ok: Color,
    pub code: Color,
    pub light: bool,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            bg: Color::Rgb(11, 13, 18),
            surface: Color::Rgb(17, 20, 27),
            surface2: Color::Rgb(23, 27, 36),
            surface3: Color::Rgb(30, 35, 48),
            line: Color::Rgb(50, 58, 74),
            text: Color::Rgb(230, 233, 239),
            muted: Color::Rgb(139, 147, 167),
            faint: Color::Rgb(90, 98, 116),
            accent: Color::Rgb(77, 124, 255),
            amber: Color::Rgb(255, 176, 32),
            danger: Color::Rgb(255, 92, 92),
            ok: Color::Rgb(61, 220, 151),
            code: Color::Rgb(214, 219, 230),
            light: false,
        }
    }

    pub fn light() -> Self {
        Self {
            bg: Color::Rgb(242, 243, 247),
            surface: Color::Rgb(255, 255, 255),
            surface2: Color::Rgb(236, 238, 243),
            surface3: Color::Rgb(225, 228, 235),
            line: Color::Rgb(195, 201, 214),
            text: Color::Rgb(20, 23, 31),
            muted: Color::Rgb(95, 102, 118),
            faint: Color::Rgb(143, 149, 165),
            accent: Color::Rgb(42, 91, 255),
            amber: Color::Rgb(176, 111, 5),
            danger: Color::Rgb(209, 53, 59),
            ok: Color::Rgb(23, 138, 88),
            code: Color::Rgb(40, 44, 56),
            light: true,
        }
    }

    /// A terminal that keeps its own background: no bg fills, default fg.
    pub fn plain() -> Self {
        Self { bg: Color::Reset, surface: Color::Reset, surface2: Color::Reset, surface3: Color::DarkGray, line: Color::DarkGray, text: Color::Reset, muted: Color::Gray, faint: Color::DarkGray, accent: Color::Blue, amber: Color::Yellow, danger: Color::Red, ok: Color::Green, code: Color::Reset, light: false }
    }
}

/// "#rrggbb" to a terminal color (the agent colors from the server).
pub fn hex(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(s, 16).ok()?;
    Some(Color::Rgb((n >> 16) as u8, (n >> 8 & 0xff) as u8, (n & 0xff) as u8))
}

/// A dimmer version of a color, for trace lines (mixes toward the line color).
pub fn dim(c: Color, by: u8) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Rgb(r / by, g / by, b / by),
        other => other,
    }
}
