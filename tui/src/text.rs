//! Text on its way to the terminal. Anything drawn goes through `sanitize` (no escape sequences or
//! control characters, tabs expanded), with `scrub` as the last line of defence over the finished
//! frame; a stray `\r`, tab or `ESC[` in a cell makes the real terminal disagree with ratatui about
//! where the cursor is, and the screen falls apart. Pastes are normalised, large ones held aside
//! behind a placeholder token, and keystroke pastes (no bracketed paste) are told from typing by
//! how close together their keys arrive.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::text::Span;
use std::iter::Peekable;
use std::str::Chars;
use std::time::{Duration, Instant};

/// Tab stops every this many columns.
pub const TAB: usize = 4;

/// Text as the composer (and the server) should get it: CRLF and lone CR become LF, terminal
/// escape sequences and every other control character except LF and tab are dropped. Only 7-bit
/// (ESC) sequences are parsed: a C1 character on its own is dropped alone, since one turns up in
/// ordinary text far more often than as an escape (UTF-8 read as Latin-1: ” is "â€\u{9d}"), and
/// what would follow it in a sequence is harmless once it is gone.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\r' => {
                it.next_if_eq(&'\n');
                out.push('\n');
            }
            '\n' | '\t' => out.push(c),
            '\x1b' => skip_escape(&mut it),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// After ESC: a CSI, a string command (OSC, DCS, SOS, PM, APC), or a short escape (intermediate
/// bytes and one final byte, like `ESC 7` or `ESC ( B`).
fn skip_escape(it: &mut Peekable<Chars>) {
    match it.peek() {
        Some('[') => {
            it.next();
            skip_csi(it);
        }
        Some(']' | 'P' | 'X' | '^' | '_') => {
            it.next();
            skip_string(it);
        }
        _ => {
            while it.next_if(|c| ('\x20'..='\x2f').contains(c)).is_some() {}
            it.next_if(|c| ('\x30'..='\x7e').contains(c));
        }
    }
}

/// CSI parameters and intermediates, then the final byte; anything else ends it early.
fn skip_csi(it: &mut Peekable<Chars>) {
    while it.next_if(|c| ('\x20'..='\x3f').contains(c)).is_some() {}
    it.next_if(|c| ('\x40'..='\x7e').contains(c));
}

/// A string command runs to BEL or ST (`ESC \` or U+009C). A line break ends it too, so an
/// unterminated one can't swallow the rest of the text.
fn skip_string(it: &mut Peekable<Chars>) {
    while let Some(c) = it.next_if(|c| *c != '\n' && *c != '\r') {
        match c {
            '\x07' | '\u{9c}' => return,
            '\x1b' => {
                it.next_if_eq(&'\\');
                return;
            }
            _ => {}
        }
    }
}

/// Columns a string takes in the terminal: what each character takes, the way ratatui places text
/// (a grapheme at a time; unicode-width's whole-string rules count some pairs, like Arabic
/// lam-alef, as one column where ratatui uses two). Meant for drawable text, after `sanitize`.
pub fn width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

fn char_width(c: char) -> usize {
    Span::raw(&*c.encode_utf8(&mut [0; 4])).width()
}

// ------------------------------------------------------------------- emoji

/// Emoji in a form ratatui and the terminal agree on the width of. Their width tables part ways
/// on emoji sequences: ✍️ (with VS16) is two columns to ratatui and one to many terminals (and to
/// wcwidth), a ZWJ family is two to ratatui and six to others, a flag two or four, 1️⃣ two or one.
/// Each disagreement shifts the rest of the row, and ratatui's incremental drawing then leaves
/// stale cells behind. So drawn text keeps only what everyone measures alike: variation
/// selectors, tags, keycap marks and other invisible format characters go, a ZWJ sequence keeps
/// its first emoji, a skin tone on an emoji goes, and a flag's regional indicators become the
/// two letters they stand for.
pub fn plain_emoji(s: &str) -> String {
    if !s.chars().any(|c| invisible(c) || matches!(c, '\u{1F1E6}'..='\u{1F1FF}' | '\u{1F3FB}'..='\u{1F3FF}')) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut last: Option<char> = None; // the last character kept
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            // 👨‍👩‍👧 keeps 👨: the joined emoji goes with its joiner (and its own modifiers after)
            '\u{200D}' => {
                if last.is_some_and(is_emoji) {
                    it.next_if(|n| is_emoji(*n));
                }
            }
            '\u{1F3FB}'..='\u{1F3FF}' if last.is_some_and(is_emoji) => {}
            '\u{1F1E6}'..='\u{1F1FF}' => {
                let l = char::from(b'A' + (c as u32 - 0x1F1E6) as u8);
                out.push(l);
                last = Some(l);
            }
            c if invisible(c) => {}
            c => {
                out.push(c);
                last = Some(c);
            }
        }
    }
    out
}

/// Zero-width format characters that join, select or steer how the characters around them are
/// shown (ZWJ, ZWNJ, variation selectors, tags, the keycap and other enclosing marks, direction
/// marks, word joiners, BOM): terminals and ratatui treat them differently, and they draw nothing.
fn invisible(c: char) -> bool {
    matches!(c, '\u{AD}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{20DD}'..='\u{20E4}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}' | '\u{E0000}'..='\u{E0FFF}')
}

/// Pictographs: emoji and the symbol blocks emoji come from (enough to tell an emoji ZWJ sequence
/// from a joiner between letters in a script that uses them).
fn is_emoji(c: char) -> bool {
    matches!(c, '\u{1F000}'..='\u{1FAFF}' | '\u{2600}'..='\u{27BF}' | '\u{2300}'..='\u{23FF}' | '\u{2B00}'..='\u{2BFF}' | '\u{2190}'..='\u{21FF}' | '\u{2100}'..='\u{214F}' | '\u{25A0}'..='\u{25FF}' | '\u{2934}' | '\u{2935}' | '\u{3030}' | '\u{303D}' | '\u{3297}' | '\u{3299}' | '\u{A9}' | '\u{AE}' | '\u{203C}' | '\u{2049}')
}

/// An agent's icon in exactly two columns (one emoji, or whatever of the icon fits), so the names
/// after it line up whichever icon it is: ✍ is one column, 🤖 two.
pub fn icon(s: &str) -> String {
    pad_cut(line(s).trim(), 2, false)
}

// ---------------------------------------------------------------- columns

/// The longest start of `s` that fits in `cols` columns (combining marks stay with their letter).
pub fn fit(s: &str, cols: usize) -> &str {
    let mut used = 0;
    for (i, c) in s.char_indices() {
        used += char_width(c);
        if used > cols {
            return &s[..i];
        }
    }
    s
}

/// The longest end of `s` that fits in `cols` columns.
pub fn fit_end(s: &str, cols: usize) -> &str {
    let mut used = 0;
    let mut start = s.len();
    for (i, c) in s.char_indices().rev() {
        used += char_width(c);
        if used > cols {
            break;
        }
        start = i;
    }
    &s[start..]
}

/// `s` in at most `cols` columns, ending in … when it had to be cut.
pub fn cut(s: &str, cols: usize) -> String {
    if width(s) <= cols {
        s.to_string()
    } else if cols == 0 {
        String::new()
    } else {
        format!("{}…", fit(s, cols - 1))
    }
}

/// `s` cut (with … when `dots`) and padded with spaces to exactly `cols` columns: columns, not
/// characters, so CJK and emoji line up in lists and tables.
pub fn pad_cut(s: &str, cols: usize, dots: bool) -> String {
    let s = if dots { cut(s, cols) } else { fit(s, cols).to_string() };
    let w = width(&s);
    s + &" ".repeat(cols.saturating_sub(w))
}

/// `s` cut into pieces of at most `cols` columns each (a character wider than that gets a piece
/// of its own), for text that wraps anywhere, like code. Combining marks stay with their letter.
pub fn chunks(s: &str, cols: usize) -> Vec<&str> {
    let mut out = vec![];
    let (mut start, mut used) = (0, 0);
    for (i, c) in s.char_indices() {
        let w = char_width(c);
        if w > 0 && used + w > cols && i > start {
            out.push(&s[start..i]);
            (start, used) = (i, 0);
        }
        used += w;
    }
    if start < s.len() || out.is_empty() {
        out.push(&s[start..]);
    }
    out
}

/// Text that is safe to draw: `normalize`, emoji in their `plain_emoji` form, then tabs expanded
/// to the next stop, counted from the start of each line. Line breaks stay (as `\n`) for callers
/// that split lines; wide characters and combining marks pass through untouched.
pub fn sanitize(s: &str) -> String {
    let s = plain_emoji(&normalize(s));
    if !s.contains('\t') {
        return s;
    }
    let mut out = String::with_capacity(s.len() + 16);
    let mut col = 0;
    for c in s.chars() {
        match c {
            '\t' => {
                let n = TAB - col % TAB;
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            '\n' => {
                out.push('\n');
                col = 0;
            }
            c => {
                out.push(c);
                col += char_width(c);
            }
        }
    }
    out
}

/// `sanitize` for one row of the screen: line breaks become spaces.
pub fn line(s: &str) -> String {
    sanitize(s).replace('\n', " ")
}

/// The safety net over a finished frame, whatever path put text there (the message box and form
/// text areas draw what they hold): no cell may carry a control character to the terminal, and
/// an emoji sequence the terminal may measure differently is swapped for its `plain_emoji` form,
/// spread over the columns ratatui gave it (a flag's two letters take one each, a blank fills
/// what is left over) so nothing after it moves.
pub fn scrub(buf: &mut Buffer) {
    let cols = buf.area.width as usize;
    for i in 0..buf.content.len() {
        let sym = buf.content[i].symbol();
        if sym.chars().any(char::is_control) {
            let kept: String = sym.chars().filter(|c| !c.is_control()).collect();
            buf.content[i].set_symbol(if width(&kept) == 0 { " " } else { &kept });
        } else if sym.chars().any(|c| invisible(c) || matches!(c, '\u{1F1E6}'..='\u{1F1FF}' | '\u{1F3FB}'..='\u{1F3FF}')) {
            let room = Span::raw(sym).width().max(1).min(cols - i % cols);
            let plain = plain_emoji(sym);
            // characters with the marks that ride on them, in the room there is
            let mut pieces: Vec<String> = vec![];
            let mut used = 0;
            for c in plain.chars() {
                let w = char_width(c);
                match pieces.last_mut() {
                    Some(p) if w == 0 => p.push(c),
                    _ if w > 0 && used + w <= room => {
                        pieces.push(c.to_string());
                        used += w;
                    }
                    _ if w > 0 => break,
                    _ => {}
                }
            }
            if pieces.is_empty() {
                pieces.push(if width(&plain) > room { "?".into() } else { " ".into() });
            }
            let like = buf.content[i].clone();
            let mut x = 0;
            for p in &pieces {
                buf.content[i + x] = like.clone();
                buf.content[i + x].set_symbol(p);
                x += width(p).max(1);
            }
            for k in x..room {
                buf.content[i + k] = like.clone();
                buf.content[i + k].set_symbol(" ");
            }
        }
    }
}

// ------------------------------------------------------------------ pastes

/// A paste longer than this (lines or characters) goes into the composer as a placeholder token.
pub const PASTE_LINES: usize = 10;
pub const PASTE_CHARS: usize = 1000;

/// Large pastes held aside while the composer shows a token for each, like
/// `[Pasted text #1 +200 lines]`. Numbering restarts whenever it is cleared (per message).
#[derive(Clone, Debug, Default)]
pub struct Pastes {
    items: Vec<(String, String)>, // (token, text)
}

impl Pastes {
    /// What to insert for an already normalised paste: the text itself when it is small,
    /// otherwise a new token (the text is kept here).
    pub fn add(&mut self, text: &str) -> String {
        let lines = text.lines().count();
        let chars = text.chars().count();
        if lines <= PASTE_LINES && chars <= PASTE_CHARS {
            return text.to_string();
        }
        let n = self.items.len() + 1;
        let token = if lines > 1 { format!("[Pasted text #{n} +{lines} lines]") } else { format!("[Pasted text #{n}, {chars} chars]") };
        self.items.push((token.clone(), text.to_string()));
        token
    }

    /// `s` with every intact token replaced by its text, in one pass (a paste that itself contains
    /// token-like text stays as pasted). A token the user edited no longer matches and is sent as
    /// typed.
    pub fn expand(&self, s: &str) -> String {
        if self.items.is_empty() {
            return s.to_string();
        }
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(i) = rest.find("[Pasted text #") {
            out.push_str(&rest[..i]);
            let tail = &rest[i..];
            match self.items.iter().find(|(t, _)| tail.starts_with(t.as_str())) {
                Some((t, text)) => {
                    out.push_str(text);
                    rest = &tail[t.len()..];
                }
                None => {
                    out.push('[');
                    rest = &tail[1..];
                }
            }
        }
        out.push_str(rest);
        out
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

// ------------------------------------------------------------- keystroke pastes

/// Keys closer together than this are a burst: faster than anyone types, the way a terminal
/// without bracketed paste delivers a paste.
pub const BURST_GAP: Duration = Duration::from_millis(10);
/// Once a burst is open it survives slightly longer gaps, since a long paste reaches the app in
/// pieces; it ends (and is handled as one paste) after this long without a key.
pub const BURST_LINGER: Duration = Duration::from_millis(30);

/// Whether a key that arrived `at` belongs to a keystroke paste, given when the key before it
/// arrived, when the next one did if it is already waiting, and whether a burst is open.
pub fn joins_burst(prev: Option<Instant>, at: Instant, next: Option<Instant>, open: bool) -> bool {
    let limit = if open { BURST_LINGER } else { BURST_GAP };
    prev.is_some_and(|p| at.saturating_duration_since(p) <= limit) || next.is_some_and(|n| n.saturating_duration_since(at) <= BURST_GAP)
}

/// The text a key stands for inside a burst: printable characters, Enter (and Ctrl+J, which is
/// how a raw LF arrives) as a line break, Tab as a tab. Anything else (arrows, Backspace,
/// shortcuts) is not paste material: it ends the burst and acts as usual.
pub fn burst_char(k: &KeyEvent) -> Option<char> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    match k.code {
        KeyCode::Char('j') if ctrl && !alt => Some('\n'),
        KeyCode::Char(c) if !ctrl && !alt => Some(c),
        KeyCode::Enter if !ctrl && !alt => Some('\n'),
        KeyCode::Tab if k.modifiers.is_empty() => Some('\t'),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn normalize_line_endings_and_controls() {
        assert_eq!(normalize("a\r\nb\rc\nd"), "a\nb\nc\nd");
        assert_eq!(normalize("a\r\r\nb"), "a\n\nb");
        assert_eq!(normalize("tab\there"), "tab\there");
        assert_eq!(normalize("bell\x07 nul\0 bs\x08 del\x7f ff\x0c"), "bell nul bs del ff");
        assert_eq!(normalize("c1 \u{85}\u{8d}x"), "c1 x");
    }

    #[test]
    fn normalize_strips_escape_sequences() {
        assert_eq!(normalize("\x1b[31mred\x1b[0m plain"), "red plain");
        assert_eq!(normalize("\x1b[1;38;5;208mbold\x1b[m"), "bold");
        assert_eq!(normalize("\x1b[?2004h\x1b[2J\x1b[Hx"), "x");
        assert_eq!(normalize("\x1b]0;window title\x07after"), "after");
        assert_eq!(normalize("\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\"), "link");
        assert_eq!(normalize("\x1bPdcs stuff\x1b\\ok"), "ok");
        assert_eq!(normalize("\x1b7saved\x1b8 \x1b(Bascii \x1b=k"), "saved ascii k");
        // 8-bit C1 introducers are dropped on their own: the text after one stays (what would be
        // its parameters can't do anything without it)
        assert_eq!(normalize("\u{9b}31mc1 csi\u{9b}0m \u{9d}0;t\u{9c}osc"), "31mc1 csi0m 0;tosc");
        // UTF-8 read as Latin-1 is full of them: ” is "â€\u{9d}", 😀 "ð\u{9f}\u{98}\u{80}", ě "Ä\u{9b}"
        assert_eq!(normalize("He said â€œhiâ€\u{9d} and then left\nsecond ð\u{9f}\u{98}\u{80} still here"), "He said â€œhiâ€ and then left\nsecond ð still here");
        assert_eq!(normalize("dÄ\u{9b}kuji"), "dÄkuji");
        assert_eq!(normalize("lone esc\x1b"), "lone esc");
        assert_eq!(normalize("\x1b\x1b[31mx"), "x");
        // an unterminated string command stops at the line break instead of eating everything
        assert_eq!(normalize("a\x1b]0;oops\nnext line"), "a\nnext line");
        // a CSI cut short by something that can't be in one keeps that character
        assert_eq!(normalize("\x1b[12é"), "é");
    }

    #[test]
    fn sanitize_expands_tabs_per_line() {
        assert_eq!(sanitize("a\tb"), "a   b");
        assert_eq!(sanitize("\tx"), "    x");
        assert_eq!(sanitize("abcd\te"), "abcd    e");
        assert_eq!(sanitize("ab\tc\nd\te"), "ab  c\nd   e");
        assert_eq!(sanitize("\x1b[31m\tred\r\n\tz"), "    red\n    z");
        // wide characters count two columns toward the next stop
        assert_eq!(sanitize("日\tx"), "日  x");
        assert_eq!(line("one\ntwo\tthree"), "one two three");
    }

    #[test]
    fn sanitize_leaves_wide_text_alone() {
        for s in ["日本語のテキスト", "emoji 👋 🧑 💻 ❤ ✍ ⌚ 🎉", "combining e\u{301} a\u{308}", "arabic مرحبا لا", "hangul 한국어", "plain ascii ~!@#$%^&*()"] {
            assert_eq!(sanitize(s), s);
            assert_eq!(normalize(s), s);
        }
        assert!(sanitize("日本").chars().all(|c| !c.is_control()));
        // content keeps every sequence as typed; only drawing simplifies them
        assert_eq!(normalize("✍️ 👨‍👩‍👧 🇦🇪"), "✍️ 👨‍👩‍👧 🇦🇪");
    }

    #[test]
    fn emoji_are_drawn_in_a_form_every_terminal_measures_alike() {
        let cases = [
            ("✍\u{FE0F} Writer", "✍ Writer"),                         // VS16 on a text-style symbol: 2 columns to ratatui, 1 to wcwidth
            ("❤\u{FE0F} ☀\u{FE0F} ⚠\u{FE0F}", "❤ ☀ ⚠"),
            ("⌚\u{FE0E}", "⌚"),                                       // VS15 on a wide one: 1 to ratatui, 2 to wcwidth
            ("👨\u{200D}👩\u{200D}👧 family", "👨 family"),             // a ZWJ sequence keeps its first emoji
            ("🧑\u{1F3FD}\u{200D}💻 dev", "🧑 dev"),
            ("🏳\u{FE0F}\u{200D}🌈", "🏳"),
            ("❤\u{FE0F}\u{200D}🔥", "❤"),
            ("🏃\u{1F3FD}\u{200D}♀\u{FE0F}", "🏃"),
            ("👋\u{1F3FD} hi ✋\u{1F3FB}", "👋 hi ✋"),               // skin tones go
            ("🇦🇪 🇯🇵 🇦", "AE JP A"),                                  // flags spelled out
            ("1\u{FE0F}\u{20E3} #\u{FE0F}\u{20E3} *\u{20E3}", "1 # *"), // keycaps
            ("🏴\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}", "🏴"), // tag sequences
            ("zwj a\u{200D}b zwnj\u{200C}x soft\u{AD}hy\u{200B}phen \u{202E}rtl\u{202C} \u{FEFF}bom", "zwj ab zwnjx softhyphen rtl bom"),
            ("🏽 alone", "🏽 alone"),                                  // a lone skin swatch is an emoji of its own
        ];
        for (raw, want) in cases {
            assert_eq!(sanitize(raw), want, "{raw:?}");
            assert_eq!(line(raw), want);
            // what is left measures the same per character (wcwidth, terminals) as per string (ratatui)
            assert_eq!(width(want), Span::raw(want).width(), "{want:?}");
        }
        assert_eq!(sanitize("\t✍\u{FE0F}\tx"), "    ✍   x", "tab stops count the simplified form");
    }

    #[test]
    fn agent_icons_take_two_columns() {
        for (raw, want) in [("✍\u{FE0F}", "✍ "), ("🤖", "🤖"), ("👨\u{200D}👩\u{200D}👧", "👨"), ("🇦🇪", "AE"), ("", "  "), ("🤖🤖", "🤖"), ("abc", "ab"), (" \t🧭 ", "🧭"), ("\x1b[31m❔", "❔")] {
            assert_eq!(icon(raw), want, "{raw:?}");
            assert_eq!(width(&icon(raw)), 2);
        }
    }

    #[test]
    fn columns_not_characters() {
        assert_eq!(fit("日本語", 5), "日本");
        assert_eq!(fit("e\u{301}x", 1), "e\u{301}", "a combining mark stays with its letter");
        assert_eq!(fit_end("abc日本", 5), "c日本");
        assert_eq!(cut("日本語テキスト", 7), "日本語…");
        assert_eq!(cut("short", 9), "short");
        assert_eq!(cut("abc", 0), "");
        assert_eq!(pad_cut("日本語", 5, true), "日本…");
        assert_eq!(pad_cut("日本語", 5, false), "日本 ");
        assert_eq!(pad_cut("ab", 4, true), "ab  ");
        assert_eq!(width("لا"), 2, "two graphemes, two cells, as ratatui places them");
        assert_eq!(chunks("abcdefg", 3), vec!["abc", "def", "g"]);
        assert_eq!(chunks("日本語", 3), vec!["日", "本", "語"]);
        assert_eq!(chunks("日本", 1), vec!["日", "本"], "too wide for a piece: one each");
        assert_eq!(chunks("", 4), vec![""]);
        assert_eq!(chunks("ab\u{301}c", 1), vec!["a", "b\u{301}", "c"]);
        assert_eq!(chunks(&"x".repeat(200_000), 80).len(), 2500, "one pass, even over a minified file's line");
    }

    #[test]
    fn scrub_clears_control_cells() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 1));
        buf[(0, 0)].set_symbol("a");
        buf[(1, 0)].set_symbol("\t");
        buf[(2, 0)].set_symbol("\r");
        buf[(3, 0)].set_symbol("\x1b");
        buf[(4, 0)].set_symbol("b\u{7}");
        buf[(5, 0)].set_symbol("日");
        scrub(&mut buf);
        let syms: Vec<&str> = buf.content.iter().map(|c| c.symbol()).collect();
        assert_eq!(syms, vec!["a", " ", " ", " ", "b", "日"]);
    }

    #[test]
    fn scrub_redraws_fragile_emoji_in_the_columns_they_were_given() {
        use ratatui::style::{Color, Style};
        // what ratatui lays out from text that never went through `sanitize` (the message box)
        let mut buf = Buffer::empty(Rect::new(0, 0, 16, 1));
        buf.set_string(0, 0, "✍\u{FE0F}|🇦🇪|👨\u{200D}👩\u{200D}👧|⌚\u{FE0E}|1\u{FE0F}\u{20E3}|", Style::default().bg(Color::Blue));
        scrub(&mut buf);
        let syms: Vec<&str> = buf.content.iter().map(|c| c.symbol()).collect();
        assert_eq!(syms, vec!["✍", " ", "|", "A", "E", "|", "👨", " ", "|", "?", "|", "1", " ", "|", " ", " "]);
        assert!(buf.content[1].bg == Color::Blue, "the blank left over keeps the colors");
        // every cell now measures the same everywhere, so the separators stay where ratatui put them
        for c in &buf.content {
            assert!(!c.symbol().chars().any(|c| invisible(c) || ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)), "{:?}", c.symbol());
        }
    }

    #[test]
    fn small_pastes_go_inline() {
        let mut p = Pastes::default();
        let ten = (0..10).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
        assert_eq!(p.add(&ten), ten);
        assert_eq!(p.add(&"x".repeat(1000)), "x".repeat(1000));
        assert!(p.items.is_empty());
    }

    #[test]
    fn large_pastes_become_tokens_and_expand_back() {
        let mut p = Pastes::default();
        let many = (0..200).map(|i| format!("line {i}\tcol")).collect::<Vec<_>>().join("\n");
        let long = "y".repeat(5000);
        let t1 = p.add(&many);
        let t2 = p.add(&long);
        assert_eq!(t1, "[Pasted text #1 +200 lines]");
        assert_eq!(t2, "[Pasted text #2, 5000 chars]");
        assert_eq!(p.expand(&format!("see {t1} and {t2}.")), format!("see {many} and {long}."));
        // the same token twice expands twice; text around and between is kept
        assert_eq!(p.expand(&format!("{t1}{t1}")), format!("{many}{many}"));
        // eleven lines is already large
        let eleven = vec!["a"; 11].join("\n");
        assert_eq!(p.add(&eleven), "[Pasted text #3 +11 lines]");
        p.clear();
        assert_eq!(p.add(&many), "[Pasted text #1 +200 lines]", "numbering restarts per message");
    }

    #[test]
    fn edited_tokens_stay_literal() {
        let mut p = Pastes::default();
        let many = vec!["z"; 50].join("\n");
        let t = p.add(&many);
        assert_eq!(p.expand("[Pasted text #1 +50 line]"), "[Pasted text #1 +50 line]");
        assert_eq!(p.expand("[Pasted text #1 +50 lines"), "[Pasted text #1 +50 lines");
        assert_eq!(p.expand("[Pasted text #2 +50 lines]"), "[Pasted text #2 +50 lines]");
        assert_eq!(p.expand(&format!("[[{t}]]")), format!("[[{many}]]"));
        // a pasted text that looks like a token is not expanded again
        let mut q = Pastes::default();
        let tricky = format!("{}\n[Pasted text #1 +12 lines]", vec!["w"; 11].join("\n"));
        let tok = q.add(&tricky);
        assert_eq!(q.expand(&tok), tricky);
    }

    #[test]
    fn burst_classifier() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        // a human: keys far apart, nothing waiting
        assert!(!joins_burst(Some(t0), ms(120), None, false));
        assert!(!joins_burst(None, t0, None, false));
        // the first key of a paste: the next one is already waiting
        assert!(joins_burst(Some(t0), ms(500), Some(ms(500)), false));
        // inside a paste: the previous key was a moment ago
        assert!(joins_burst(Some(ms(500)), ms(501), None, false));
        // an open burst survives a short gap between pieces, a closed one needs a tight gap
        assert!(joins_burst(Some(ms(500)), ms(520), None, true));
        assert!(!joins_burst(Some(ms(500)), ms(520), None, false));
        assert!(!joins_burst(Some(ms(500)), ms(560), None, true));
        // a key waiting a while later doesn't make this one a paste
        assert!(!joins_burst(Some(t0), ms(300), Some(ms(400)), false));
    }

    #[test]
    fn burst_chars() {
        let k = |code, m| KeyEvent::new(code, m);
        assert_eq!(burst_char(&k(KeyCode::Char('a'), KeyModifiers::NONE)), Some('a'));
        assert_eq!(burst_char(&k(KeyCode::Char('A'), KeyModifiers::SHIFT)), Some('A'));
        assert_eq!(burst_char(&k(KeyCode::Enter, KeyModifiers::NONE)), Some('\n'));
        assert_eq!(burst_char(&k(KeyCode::Char('j'), KeyModifiers::CONTROL)), Some('\n'));
        assert_eq!(burst_char(&k(KeyCode::Tab, KeyModifiers::NONE)), Some('\t'));
        assert_eq!(burst_char(&k(KeyCode::Char('c'), KeyModifiers::CONTROL)), None);
        assert_eq!(burst_char(&k(KeyCode::Char('o'), KeyModifiers::ALT)), None);
        assert_eq!(burst_char(&k(KeyCode::Up, KeyModifiers::NONE)), None);
        assert_eq!(burst_char(&k(KeyCode::Backspace, KeyModifiers::NONE)), None);
        assert_eq!(burst_char(&k(KeyCode::Esc, KeyModifiers::NONE)), None);
    }

    /// xorshift64*: random enough for property loops, and the same on every run, so a failing
    /// case is named by its seed and number.
    pub(crate) struct Rng(pub u64);

    impl Rng {
        pub(crate) fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        pub(crate) fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    /// What drawn text meets: escape sequences whole and in parts, every kind of control
    /// character, line ends and tabs, wide and zero-width characters, emoji sequences, format
    /// characters, scripts that join, combine or reorder.
    const PIECES: &[&str] = &[
        "a", "Z", "0", " ", "~", "-", "[", "]", "\t", "\r", "\n", "\r\n", "\x1b", "\x1b[", "\x1b]", "\x1bP", "\x1b(", "[31m", "[0m", "[?2004h", "]0;",
        ";", "\x07", "\x1b\\", "\u{9b}", "\u{9c}", "\u{9d}", "\u{90}", "\0", "\x08", "\x0b", "\x0c", "\x7f", "\u{85}", "\u{8d}", "日", "本", "語", "한",
        "\u{1100}\u{1161}\u{11A8}", "\u{1161}", "e\u{301}", "\u{301}", "\u{20DD}", "\u{20E3}", "👋", "🏽", "👨\u{200D}👩\u{200D}👧", "\u{200D}", "\u{200C}",
        "\u{FE0F}", "\u{FE0E}", "✍", "❤", "⌚", "☺", "🇦", "🇪", "1\u{FE0F}\u{20E3}", "🏴\u{E0067}\u{E0062}\u{E007F}", "\u{E0100}", "\u{AD}", "\u{200B}",
        "\u{202E}", "\u{2066}", "\u{2060}", "\u{FEFF}", "ل", "ا", "مرحبا", "\u{600}", "\u{2028}", "\u{2029}", "\u{180E}", "\u{FFF9}", "\u{1D173}",
        "\u{A4F8}\u{A4FC}", "\u{1A15}\u{1A17}", "\u{2D7F}", "ॐ", "कि", "\u{903}", "\u{E000}", "\u{FFFD}", "\u{10FFFD}", "…", "─", "▎",
    ];

    /// Up to `n` random pieces: from PIECES, a random character of the first plane, or any
    /// Unicode scalar value at all.
    pub(crate) fn random_text(rng: &mut Rng, n: usize) -> String {
        let mut s = String::new();
        for _ in 0..rng.below(n + 1) {
            match rng.below(4) {
                0 | 1 => s.push_str(PIECES[rng.below(PIECES.len())]),
                2 => s.extend(char::from_u32(rng.below(0x1_0000) as u32)),
                _ => s.extend(char::from_u32(rng.below(0x11_0000) as u32)),
            }
        }
        s
    }

    #[test]
    fn random_text_draws_no_control_characters_in_the_columns_it_measures() {
        let mut rng = Rng(0x5eed);
        for case in 0..5000 {
            let raw = random_text(&mut rng, 24);
            let n = normalize(&raw);
            assert!(!n.chars().any(|c| c.is_control() && c != '\n' && c != '\t'), "case {case}: {raw:?} normalizes to {n:?}");
            assert_eq!(normalize(&n), n, "case {case}: normalizing twice changes {raw:?}");
            let s = sanitize(&raw);
            assert!(!s.chars().any(|c| c.is_control() && c != '\n'), "case {case}: {raw:?} sanitizes to {s:?}");
            assert_eq!(sanitize(&s), s, "case {case}: sanitizing twice changes {raw:?}");
            assert_eq!(s.matches('\n').count(), n.matches('\n').count(), "case {case}: lines come and go in {raw:?}");
            let l = line(&raw);
            assert!(!l.chars().any(char::is_control), "case {case}: {raw:?} as a line is {l:?}");
            // drawn, it takes exactly the columns `width` says, and every cell measures the same
            // per character (a terminal) as per grapheme (ratatui): nothing after it can shift
            let w = width(&l);
            let mut buf = Buffer::empty(Rect::new(0, 0, w as u16 + 4, 1));
            let (end, _) = buf.set_stringn(0, 0, &l, usize::MAX, ratatui::style::Style::default());
            assert_eq!(end as usize, w, "case {case}: {l:?} ({}) is {w} columns to `width`, {end} to ratatui", l.escape_unicode());
            let mut x = 0;
            while x < end {
                let sym = buf[(x, 0)].symbol();
                let cell = Span::raw(sym).width();
                assert_eq!(cell, width(sym), "case {case}: the cell {sym:?} ({}) from {raw:?}", sym.escape_unicode());
                x += cell.max(1) as u16;
            }
            assert_eq!(x, end, "case {case}: the cells add up to the line");
        }
    }

    #[test]
    fn random_text_fits_cuts_and_pads_by_columns() {
        let mut rng = Rng(7);
        for case in 0..5000 {
            let s = line(&random_text(&mut rng, 16));
            let cols = rng.below(14);
            let f = fit(&s, cols);
            assert!(s.starts_with(f) && width(f) <= cols, "case {case}: fit({s:?}, {cols}) = {f:?}");
            if let Some(next) = s[f.len()..].chars().next() {
                assert!(width(f) + char_width(next) > cols, "case {case}: fit({s:?}, {cols}) stopped early at {f:?}");
            }
            let e = fit_end(&s, cols);
            assert!(s.ends_with(e) && width(e) <= cols, "case {case}: fit_end({s:?}, {cols}) = {e:?}");
            if let Some(prev) = s[..s.len() - e.len()].chars().next_back() {
                assert!(width(e) + char_width(prev) > cols, "case {case}: fit_end({s:?}, {cols}) stopped early at {e:?}");
            }
            let c = cut(&s, cols);
            assert!(width(&c) <= cols, "case {case}: cut({s:?}, {cols}) = {c:?}");
            if width(&s) <= cols {
                assert_eq!(c, s);
            } else if cols > 0 {
                assert!(c.ends_with('…') && s.starts_with(c.trim_end_matches('…')), "case {case}: cut({s:?}, {cols}) = {c:?}");
            }
            for dots in [true, false] {
                assert_eq!(width(&pad_cut(&s, cols, dots)), cols, "case {case}: pad_cut({s:?}, {cols}, {dots})");
            }
            assert_eq!(width(&icon(&s)), 2, "case {case}: icon({s:?})");
            // pieces: back together they are the text; one is wider than asked only when it is a
            // single character too wide for any piece (with the marks on it)
            let pieces = chunks(&s, cols);
            assert_eq!(pieces.concat(), s, "case {case}");
            for p in &pieces {
                let first = p.chars().next().map_or(0, char_width);
                assert!(width(p) <= cols.max(first), "case {case}: chunks({s:?}, {cols}) made {p:?}");
            }
            if cols >= 2 {
                assert!(pieces.iter().skip(1).all(|p| p.chars().next().is_none_or(|c| char_width(c) > 0)), "case {case}: a mark split from its letter: chunks({s:?}, {cols}) = {pieces:?}");
            }
        }
    }

    #[test]
    fn a_mark_stays_with_a_wide_letter_in_a_narrow_piece() {
        // a character wider than the piece gets one of its own, and its marks go with it (the
        // way `fit` keeps them): a mark starting a piece is drawn on nothing
        assert_eq!(chunks("日\u{301}x", 1), vec!["日\u{301}", "x"]);
        assert_eq!(chunks("a日\u{20DD}b", 1), vec!["a", "日\u{20DD}", "b"]);
    }

    #[test]
    fn scrub_leaves_no_cell_a_terminal_measures_differently() {
        let mut rng = Rng(23);
        for case in 0..2000 {
            // what a text area draws: the raw text, never sanitized
            let raw = random_text(&mut rng, 16);
            let mut buf = Buffer::empty(Rect::new(0, 0, 24, 2));
            buf.set_string(0, 0, &raw, ratatui::style::Style::default());
            buf.set_string(5, 1, raw.replace(['\n', '\r'], " "), ratatui::style::Style::default());
            scrub(&mut buf);
            for (i, c) in buf.content.iter().enumerate() {
                let sym = c.symbol();
                assert!(!sym.chars().any(|ch| ch.is_control() || invisible(ch) || ('\u{1F1E6}'..='\u{1F1FF}').contains(&ch)), "case {case}: cell {i} holds {sym:?} from {raw:?}");
                assert_eq!(Span::raw(sym).width(), width(sym), "case {case}: cell {i} {sym:?} ({}) from {raw:?}", sym.escape_unicode());
            }
        }
    }

    #[test]
    fn random_pastes_expand_back_to_exactly_what_was_pasted() {
        let mut rng = Rng(11);
        for case in 0..400 {
            let mut p = Pastes::default();
            let (mut typed, mut want, mut tokens) = (String::new(), String::new(), 0);
            for _ in 0..rng.below(6) {
                // what the user typed around the pastes ('[' can't start a token there)
                let filler = normalize(&random_text(&mut rng, 3)).replace('[', "(");
                typed.push_str(&filler);
                want.push_str(&filler);
                let rows: Vec<String> = (0..rng.below(25)).map(|_| normalize(&random_text(&mut rng, 8))).collect();
                let mut text = rows.join("\n");
                if rng.below(3) == 0 {
                    text.push_str(&"x".repeat(rng.below(1600)));
                }
                let ins = p.add(&text);
                let large = text.lines().count() > PASTE_LINES || text.chars().count() > PASTE_CHARS;
                if large {
                    tokens += 1;
                    assert!(ins.starts_with(&format!("[Pasted text #{tokens}")) && ins.ends_with(']') && !ins.contains('\n'), "case {case}: {ins:?}");
                } else {
                    assert_eq!(ins, text, "case {case}: a small paste goes in as it is");
                }
                typed.push_str(&ins);
                want.push_str(&text);
            }
            assert_eq!(p.expand(&typed), want, "case {case}");
            p.clear();
            assert_eq!(p.expand(&typed), typed, "case {case}: cleared, the tokens are just text");
        }
    }

    #[test]
    fn a_burst_is_keys_closer_than_typing() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        // the edges: within the gap (or the linger, once open) joins; a millisecond more does not
        let gap = BURST_GAP.as_millis() as u64;
        let linger = BURST_LINGER.as_millis() as u64;
        assert!(joins_burst(Some(at(100)), at(100 + gap), None, false));
        assert!(!joins_burst(Some(at(100)), at(101 + gap), None, false));
        assert!(joins_burst(Some(at(100)), at(100 + linger), None, true));
        assert!(!joins_burst(Some(at(100)), at(101 + linger), None, true));
        assert!(joins_burst(None, at(5), Some(at(5 + gap)), false), "the first key of a paste, its next one waiting");
        assert!(!joins_burst(None, at(5), Some(at(6 + gap)), false));
        // a clock that seems to run backwards (keys stamped out of order) is no burst breaker
        assert!(joins_burst(Some(at(50)), at(40), None, false));
        // keys typed 150 ms apart, a hundred of them, never make one
        let mut prev = None;
        for i in 0..100 {
            assert!(!joins_burst(prev, at(150 * i), None, false));
            prev = Some(at(150 * i));
        }
    }
}
