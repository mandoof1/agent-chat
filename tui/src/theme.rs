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
    pub name: &'static str,
}

/// The palettes `/theme` cycles through, in order.
pub const NAMES: [&str; 3] = ["dark", "light", "plain"];

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
            name: "dark",
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
            name: "light",
        }
    }

    /// A terminal that keeps its own background: no bg fills, default fg.
    pub fn plain() -> Self {
        Self { bg: Color::Reset, surface: Color::Reset, surface2: Color::Reset, surface3: Color::DarkGray, line: Color::DarkGray, text: Color::Reset, muted: Color::Gray, faint: Color::DarkGray, accent: Color::Blue, amber: Color::Yellow, danger: Color::Red, ok: Color::Green, code: Color::Reset, light: false, name: "plain" }
    }

    pub fn named(name: &str) -> Option<Self> {
        match name {
            "dark" => Some(Self::dark()),
            "light" => Some(Self::light()),
            "plain" => Some(Self::plain()),
            _ => None,
        }
    }
}

/// "#rrggbb" to a terminal color (the agent colors from the server).
pub fn hex(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    // (from_str_radix alone would take "+fffff")
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_palette_is_named_and_readable() {
        let all = [Theme::dark(), Theme::light(), Theme::plain()];
        assert_eq!(all.iter().map(|t| t.name).collect::<Vec<_>>(), NAMES, "/theme cycles through them in this order");
        for t in &all {
            assert_eq!(Theme::named(t.name).map(|n| n.name), Some(t.name));
            // text and the accents stand out from what they are drawn on
            for fg in [t.text, t.muted, t.accent, t.danger, t.ok, t.amber] {
                assert!(fg != t.surface || fg == Color::Reset, "{}: {fg:?} on {:?}", t.name, t.surface);
                assert!(fg != t.bg || fg == Color::Reset, "{}: {fg:?} on {:?}", t.name, t.bg);
            }
            assert_ne!(t.text, t.faint, "{}", t.name);
        }
        assert!(Theme::light().light && !Theme::dark().light && !Theme::plain().light);
        let p = Theme::plain();
        assert_eq!((p.bg, p.surface, p.surface2, p.text), (Color::Reset, Color::Reset, Color::Reset, Color::Reset), "plain keeps the terminal's own colors");
        for bad in ["", "Dark", "solarized", " dark"] {
            assert!(Theme::named(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn agent_colors_parse_from_hex() {
        for (s, want) in [
            ("#3fb27f", Some(Color::Rgb(0x3f, 0xb2, 0x7f))),
            ("#FFFFFF", Some(Color::Rgb(255, 255, 255))),
            ("000000", Some(Color::Rgb(0, 0, 0))),
            ("  #7c6cff \n", Some(Color::Rgb(0x7c, 0x6c, 0xff))),
            ("#abc", None),
            ("#1234567", None),
            ("", None),
            ("#", None),
            ("#gggggg", None),
            ("#12 456", None),
            ("#日本", None),
            ("rgb(1,2,3)", None),
            ("#-12345", None),
            // six characters, but not six hex digits (u32::from_str_radix takes a leading +)
            ("#+fffff", None),
            ("+12345", None),
        ] {
            assert_eq!(hex(s), want, "{s:?}");
        }
    }

    #[test]
    fn dimming_divides_true_colors_and_leaves_the_rest() {
        assert_eq!(dim(Color::Rgb(200, 101, 3), 2), Color::Rgb(100, 50, 1));
        assert_eq!(dim(Color::Rgb(9, 9, 9), 1), Color::Rgb(9, 9, 9));
        for c in [Color::Reset, Color::Gray, Color::Indexed(42), Color::Blue] {
            assert_eq!(dim(c, 2), c);
        }
    }
}
