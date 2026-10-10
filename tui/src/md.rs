//! Markdown to terminal lines: headings, emphasis, inline and fenced code, lists, quotes,
//! tables, rules and links, in the app's theme, wrapped to the width they get. Plenty for model
//! replies; images show their alt text.

use crate::text::{self, line as clean, normalize};
use crate::theme::Theme;
use crate::trace::wrap_spans;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// Columns a table cell gets at most.
const CELL: usize = 40;

#[derive(Default)]
struct Ctx {
    bold: usize,
    italic: usize,
    strike: usize,
    code: bool,
    link: usize,
    /// Open links and images: where they point, and where their text starts in the line.
    targets: Vec<(String, usize)>,
    heading: Option<HeadingLevel>,
    list: Vec<Option<u64>>, // None = bullet, Some(n) = next number
    /// The open quotes and list items, outermost first: each line is prefixed by all of them in
    /// that order (a quote's bar, an item's indent under its text, or its marker on its first line).
    nest: Vec<Nest>,
    in_code_block: bool,
    code_lang: String,
    code_buf: String,
    table_row: Vec<String>,
    table_cell: String,
    in_table: bool,
    table_rows: Vec<Vec<String>>,
}

enum Nest {
    Quote,
    /// A list item: its marker, until the item's first line takes it, and how far its other lines
    /// are indented (under its text, not its marker).
    Item { hang: usize, marker: Option<String> },
}

impl Ctx {
    /// What goes before a block's first line and before its others: for each open quote and list
    /// item, outermost first, the quote's bar, or the item's marker (on its first line) or the
    /// indent under it. A quote in an item has its bar under the item's text; a list in an item
    /// starts there too.
    fn prefixes(&mut self, theme: &Theme) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let (mut first, mut rest) = (vec![], vec![]);
        for n in self.nest.iter_mut() {
            match n {
                Nest::Quote => {
                    let bar = Span::styled("▎ ", Style::default().fg(theme.faint));
                    first.push(bar.clone());
                    rest.push(bar);
                }
                Nest::Item { hang, marker } => {
                    first.push(match marker.take() {
                        Some(m) => Span::styled(m, Style::default().fg(theme.muted)),
                        None => Span::raw(" ".repeat(*hang)),
                    });
                    rest.push(Span::raw(" ".repeat(*hang)));
                }
            }
        }
        (first, rest)
    }

    /// A gap's line inside a quote: the bars (and the indents around them), as one span; nothing
    /// outside one.
    fn gap(&self) -> String {
        if !self.nest.iter().any(|n| matches!(n, Nest::Quote)) {
            return String::new();
        }
        self.nest.iter().map(|n| match n {
            Nest::Quote => "▎ ".to_string(),
            Nest::Item { hang, .. } => " ".repeat(*hang),
        }).collect()
    }

    /// Close the innermost open quote (or item).
    fn close(&mut self, quote: bool) -> Option<Nest> {
        let i = self.nest.iter().rposition(|n| matches!(n, Nest::Quote) == quote)?;
        Some(self.nest.remove(i))
    }
}

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| text::width(&s.content)).sum()
}

/// A gap between blocks (inside a quote, its bars).
fn is_blank(l: &Line) -> bool {
    l.spans.len() <= 1 && l.spans.iter().all(|s| s.content.chars().all(|c| c == ' ' || c == '▎'))
}

/// Render `text` as styled lines at most `width` columns wide (only a character wider than what
/// is left, at absurd widths, goes over). Wrapped lines keep their block's prefix: quote bars,
/// the indent under a list item, a code block's gutter. Escape sequences and control characters
/// never reach a span (entities like `&#27;` decode to them, so each piece is cleaned again
/// after parsing).
pub fn render(text: &str, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let text = normalize(text);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut ctx = Ctx::default();
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(&text, opts);

    // a block's lines, each behind its prefix (from `Ctx::prefixes`)
    let emit = |lines: &mut Vec<Line<'static>>, (first, rest): (Vec<Span<'static>>, Vec<Span<'static>>), body: Vec<Vec<Span<'static>>>| {
        for (k, b) in body.into_iter().enumerate() {
            let mut spans = if k == 0 { first.clone() } else { rest.clone() };
            spans.extend(b);
            lines.push(Line::from(spans));
        }
    };
    // the paragraph (or item, heading, cell of text) so far, wrapped in what its prefix leaves
    let flush = |lines: &mut Vec<Line<'static>>, cur: &mut Vec<Span<'static>>, ctx: &mut Ctx| {
        if cur.is_empty() {
            return;
        }
        let pre = ctx.prefixes(theme);
        let body = wrap_spans(cur, width.saturating_sub(spans_width(&pre.0)));
        cur.clear();
        emit(lines, pre, body);
    };
    let blank = |lines: &mut Vec<Line<'static>>, ctx: &Ctx| {
        if lines.last().is_some_and(|l| !is_blank(l)) {
            let gap = ctx.gap();
            lines.push(if gap.is_empty() { Line::from("") } else { Line::from(Span::styled(gap, Style::default().fg(theme.faint))) });
        }
    };

    for ev in parser {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    if ctx.list.is_empty() {
                        blank(&mut lines, &ctx);
                    }
                }
                Tag::Heading { level, .. } => {
                    blank(&mut lines, &ctx);
                    ctx.heading = Some(level);
                }
                Tag::BlockQuote(_) => {
                    flush(&mut lines, &mut cur, &mut ctx);
                    blank(&mut lines, &ctx);
                    ctx.nest.push(Nest::Quote);
                }
                Tag::CodeBlock(kind) => {
                    flush(&mut lines, &mut cur, &mut ctx);
                    if ctx.list.is_empty() {
                        blank(&mut lines, &ctx);
                    }
                    ctx.in_code_block = true;
                    ctx.code_lang = match kind {
                        CodeBlockKind::Fenced(l) => l.to_string(),
                        _ => String::new(),
                    };
                    ctx.code_buf.clear();
                }
                Tag::List(start) => {
                    flush(&mut lines, &mut cur, &mut ctx);
                    if ctx.list.is_empty() {
                        blank(&mut lines, &ctx);
                    }
                    ctx.list.push(start);
                }
                Tag::Item => {
                    flush(&mut lines, &mut cur, &mut ctx);
                    let marker = match ctx.list.last_mut() {
                        Some(Some(n)) => {
                            let m = format!("{n}. ");
                            *n += 1;
                            m
                        }
                        _ => "• ".to_string(),
                    };
                    ctx.nest.push(Nest::Item { hang: text::width(&marker), marker: Some(marker) });
                }
                Tag::Emphasis => ctx.italic += 1,
                Tag::Strong => ctx.bold += 1,
                Tag::Strikethrough => ctx.strike += 1,
                Tag::Link { dest_url, .. } => {
                    ctx.link += 1;
                    ctx.targets.push((dest_url.to_string(), cur.len()));
                }
                Tag::Image { dest_url, .. } => ctx.targets.push((dest_url.to_string(), cur.len())),
                Tag::Table(_) => {
                    flush(&mut lines, &mut cur, &mut ctx);
                    blank(&mut lines, &ctx);
                    ctx.in_table = true;
                    ctx.table_rows.clear();
                }
                Tag::TableHead | Tag::TableRow => ctx.table_row.clear(),
                Tag::TableCell => ctx.table_cell.clear(),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::HtmlBlock => flush(&mut lines, &mut cur, &mut ctx),
                TagEnd::Heading(_) => {
                    flush(&mut lines, &mut cur, &mut ctx);
                    ctx.heading = None;
                }
                TagEnd::BlockQuote(_) => {
                    flush(&mut lines, &mut cur, &mut ctx);
                    ctx.close(true);
                }
                TagEnd::CodeBlock => {
                    ctx.in_code_block = false;
                    let faint = Style::default().fg(theme.faint);
                    let lang = clean(&ctx.code_lang);
                    // long lines wrap anywhere (it's code), each piece behind the gutter; the label
                    // (whatever the fence's info string says) is cut to the width
                    let pre = ctx.prefixes(theme);
                    let label_w = width.saturating_sub(spans_width(&pre.0));
                    let label = if lang.is_empty() || label_w <= 2 { "┌".to_string() } else { text::cut(&format!("┌ {lang}"), label_w) };
                    let mut body = vec![vec![Span::styled(label, faint)]];
                    let room = width.saturating_sub(spans_width(&pre.0) + 2);
                    for l in ctx.code_buf.trim_end_matches('\n').split('\n') {
                        for piece in text::chunks(&clean(l), room) {
                            body.push(vec![Span::styled("│ ", faint), Span::styled(piece.to_string(), Style::default().fg(theme.code))]);
                        }
                    }
                    body.push(vec![Span::styled("└", faint)]);
                    emit(&mut lines, pre, body);
                    ctx.code_buf.clear();
                }
                TagEnd::List(_) => {
                    flush(&mut lines, &mut cur, &mut ctx);
                    ctx.list.pop();
                }
                TagEnd::Item => {
                    flush(&mut lines, &mut cur, &mut ctx);
                    if matches!(ctx.nest.last(), Some(Nest::Item { marker: Some(_), .. })) {
                        emit(&mut lines, ctx.prefixes(theme), vec![vec![]]); // an empty item still shows its marker
                    }
                    ctx.close(false);
                }
                TagEnd::Emphasis => ctx.italic = ctx.italic.saturating_sub(1),
                TagEnd::Strong => ctx.bold = ctx.bold.saturating_sub(1),
                TagEnd::Strikethrough => ctx.strike = ctx.strike.saturating_sub(1),
                TagEnd::Link | TagEnd::Image => {
                    if tag == TagEnd::Link {
                        ctx.link = ctx.link.saturating_sub(1);
                    }
                    // the address after the text, where a terminal can show (and often open) it,
                    // unless the text already is the address (or it would only be cut short in a
                    // table cell)
                    if let Some((url, from)) = ctx.targets.pop().filter(|_| !ctx.in_table) {
                        let shown: String = cur.iter().skip(from).map(|s| s.content.as_ref()).collect();
                        let url = clean(&url);
                        if !url.is_empty() && !url.starts_with("data:") && shown.trim() != url.trim_start_matches("mailto:") {
                            cur.push(Span::styled(format!(" ({url})"), Style::default().fg(theme.faint)));
                        }
                    }
                }
                TagEnd::TableCell => {
                    let cell = std::mem::take(&mut ctx.table_cell);
                    ctx.table_row.push(cell.trim().to_string());
                }
                TagEnd::TableHead | TagEnd::TableRow => {
                    let row = std::mem::take(&mut ctx.table_row);
                    ctx.table_rows.push(row);
                }
                TagEnd::Table => {
                    ctx.in_table = false;
                    let rows = std::mem::take(&mut ctx.table_rows);
                    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
                    // columns as wide as their widest cell (up to CELL), then the widest ones
                    // narrowed until a row fits on one line
                    let mut widths = vec![0usize; cols];
                    for r in &rows {
                        for (i, c) in r.iter().enumerate() {
                            widths[i] = widths[i].max(text::width(c).min(CELL));
                        }
                    }
                    let pre = ctx.prefixes(theme);
                    let room = width.saturating_sub(spans_width(&pre.0));
                    while widths.iter().sum::<usize>() + 2 * cols.saturating_sub(1) > room {
                        let Some((i, w)) = widths.iter().copied().enumerate().max_by_key(|(_, w)| *w).filter(|(_, w)| *w > 3) else { break };
                        widths[i] = w - 1;
                    }
                    let mut body = vec![];
                    for (ri, r) in rows.iter().enumerate() {
                        let style = if ri == 0 { Style::default().fg(theme.muted).add_modifier(Modifier::BOLD) } else { Style::default() };
                        let mut spans = Vec::new();
                        for (i, w) in widths.iter().enumerate() {
                            spans.push(Span::styled(text::pad_cut(r.get(i).map(String::as_str).unwrap_or(""), *w, true), style));
                            if i + 1 < widths.len() {
                                spans.push(Span::raw("  "));
                            }
                        }
                        body.push(spans);
                        if ri == 0 {
                            body.push(vec![Span::styled("─".repeat(widths.iter().sum::<usize>() + 2 * cols.saturating_sub(1)), Style::default().fg(theme.line))]);
                        }
                    }
                    emit(&mut lines, pre, body);
                }
                _ => {}
            },
            Event::Text(t) => {
                if ctx.in_code_block {
                    ctx.code_buf.push_str(&t); // cleaned line by line at the end of the block
                } else if ctx.in_table {
                    ctx.table_cell.push_str(&clean(&t));
                } else {
                    cur.push(Span::styled(clean(&t), inline_style(&ctx, theme)));
                }
            }
            Event::Code(t) => {
                if ctx.in_table {
                    ctx.table_cell.push_str(&clean(&t));
                } else {
                    cur.push(Span::styled(format!(" {} ", clean(&t)), Style::default().fg(theme.code).bg(theme.surface3)));
                }
            }
            Event::SoftBreak => cur.push(Span::raw(" ")),
            Event::HardBreak => flush(&mut lines, &mut cur, &mut ctx),
            Event::Rule => {
                flush(&mut lines, &mut cur, &mut ctx);
                let pre = ctx.prefixes(theme);
                let room = width.saturating_sub(spans_width(&pre.0));
                emit(&mut lines, pre, vec![vec![Span::styled("─".repeat(room.clamp(1, 60)), Style::default().fg(theme.line))]]);
            }
            Event::TaskListMarker(done) => {
                cur.push(Span::styled(if done { "☑ " } else { "☐ " }, Style::default().fg(theme.accent)));
            }
            // block HTML keeps its lines; inline HTML stays in the text
            Event::Html(h) => {
                for l in h.lines() {
                    cur.push(Span::styled(clean(l), Style::default().fg(theme.faint)));
                    flush(&mut lines, &mut cur, &mut ctx);
                }
            }
            Event::InlineHtml(h) => cur.push(Span::styled(clean(&h), Style::default().fg(theme.faint))),
            _ => {}
        }
    }
    flush(&mut lines, &mut cur, &mut ctx);
    while lines.first().is_some_and(is_blank) {
        lines.remove(0);
    }
    lines
}

fn inline_style(ctx: &Ctx, theme: &Theme) -> Style {
    let mut s = Style::default();
    if let Some(level) = ctx.heading {
        s = s.add_modifier(Modifier::BOLD);
        s = match level {
            HeadingLevel::H1 | HeadingLevel::H2 => s.fg(theme.text).add_modifier(Modifier::UNDERLINED),
            _ => s.fg(theme.text),
        };
        return s;
    }
    if ctx.bold > 0 {
        s = s.add_modifier(Modifier::BOLD);
    }
    if ctx.italic > 0 {
        s = s.add_modifier(Modifier::ITALIC);
    }
    if ctx.strike > 0 {
        s = s.add_modifier(Modifier::CROSSED_OUT);
    }
    if ctx.link > 0 {
        s = s.fg(theme.accent).add_modifier(Modifier::UNDERLINED);
    }
    if ctx.code {
        s = s.fg(theme.code);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.to_string()).collect::<String>()).collect()
    }

    #[test]
    fn renders_blocks() {
        let t = Theme::dark();
        let out = text(&render("# Title\n\nSome **bold** text.\n\n- a\n- b\n\n```py\nprint(1)\n```\n\n| k | v |\n|---|---|\n| 1 | 2 |", &t, 60));
        assert_eq!(out[0], "Title");
        assert!(out.contains(&"Some bold text.".to_string()));
        assert!(out.iter().any(|l| l == "• a"));
        assert!(out.iter().any(|l| l == "┌ py"));
        assert!(out.iter().any(|l| l == "│ print(1)"));
        assert!(out.iter().any(|l| l.starts_with("k  v")));
    }

    #[test]
    fn nothing_unsafe_reaches_a_span() {
        let t = Theme::dark();
        let src = "red \x1b[31mtext\x1b[0m and &#27;[2J entity, tab\there\r\nnext\rline\n\n```sh\n\tindented\x1b[1m\r\n```\n\n| a\tb | `c\x07` |\n|---|---|\n| 日本 | 👋 |";
        let lines = render(src, &t, 60);
        for l in &lines {
            for s in &l.spans {
                assert!(!s.content.chars().any(char::is_control), "{:?}", s.content);
            }
        }
        let out = text(&lines);
        assert!(out.iter().any(|l| l.starts_with("red text and") && l.contains("entity, tab") && l.ends_with("here next line")), "{out:?}");
        assert!(out.iter().any(|l| l == "│     indented"), "{out:?}");
        assert!(out.iter().any(|l| l.contains("日本")), "{out:?}");
    }

    fn style_of<'a>(lines: &'a [Line], needle: &str) -> &'a Style {
        lines.iter().flat_map(|l| l.spans.iter()).find(|s| s.content.contains(needle)).map(|s| &s.style).unwrap_or_else(|| panic!("{needle:?} not in {:?}", text(lines)))
    }

    #[test]
    fn emphasis_strike_and_headings_are_styled() {
        let t = Theme::dark();
        let lines = render("**b** *i* ~~s~~ `ls` <b>tag</b>", &t, 60);
        assert!(style_of(&lines, "b").add_modifier.contains(Modifier::BOLD));
        assert!(style_of(&lines, "i").add_modifier.contains(Modifier::ITALIC));
        assert!(style_of(&lines, "s").add_modifier.contains(Modifier::CROSSED_OUT), "~~strike~~ is crossed out");
        assert!(!style_of(&lines, "i").add_modifier.contains(Modifier::CROSSED_OUT));
        assert_eq!(style_of(&lines, "ls").bg, Some(t.surface3));
        assert_eq!(style_of(&lines, "<b>").fg, Some(t.faint));
        let lines = render("# A\n## B\n### C", &t, 60);
        assert_eq!(text(&lines), vec!["A", "", "B", "", "C"]);
        for (h, under) in [("A", true), ("B", true), ("C", false)] {
            let s = style_of(&lines, h);
            assert!(s.add_modifier.contains(Modifier::BOLD) && s.add_modifier.contains(Modifier::UNDERLINED) == under, "{h}");
        }
        let lines = render("<details>\nx\n</details>", &t, 60);
        assert_eq!(text(&lines), vec!["<details>", "x", "</details>"], "block HTML keeps its lines");
    }

    #[test]
    fn wrapped_lines_keep_their_prefix() {
        let t = Theme::dark();
        let long = "word ".repeat(30);
        // a code line wraps behind the gutter (it used to lose it)
        let code = format!("```py\n{}\n```", "x".repeat(120));
        let out = text(&render(&code, &t, 40));
        assert_eq!(out[0], "┌ py");
        assert_eq!(out.last().unwrap(), "└");
        let body = &out[1..out.len() - 1];
        assert_eq!(body.len(), 4, "{out:?}");
        assert!(body.iter().all(|l| l.starts_with("│ x") && crate::text::width(l) <= 40), "{out:?}");
        // a quote's bars, also on a code block in it and on the gap between its paragraphs
        let out = text(&render(&format!("> {long}\n>\n> > b\n\n> ```\n> x\n> ```"), &t, 40));
        assert!(out.iter().filter(|l| l.contains("word")).count() > 2);
        assert!(out.iter().filter(|l| !l.is_empty()).all(|l| l.starts_with("▎ ")), "{out:?}");
        assert!(out.contains(&"▎ ▎ b".to_string()) && out.contains(&"▎ │ x".to_string()), "{out:?}");
        // a list item's lines (and a loose item's next paragraph) hang under its text
        let out = text(&render(&format!("- {long}\n- b\n\n  second paragraph of b\n\n  ```\n  code\n  ```\n\n3. c\n4. d\n- [ ] todo\n- [x] done"), &t, 40));
        let first = out.iter().position(|l| l.starts_with("• word")).unwrap();
        assert!(out[first + 1].starts_with("  word"), "{out:?}");
        assert!(out.contains(&"  second paragraph of b".to_string()) && out.contains(&"  │ code".to_string()), "{out:?}");
        assert!(out.contains(&"3. c".to_string()) && out.contains(&"4. d".to_string()));
        assert!(out.contains(&"• ☐ todo".to_string()) && out.contains(&"• ☑ done".to_string()));
        let out = text(&render("- a\n  - b\n    1. c", &t, 40));
        assert_eq!(out, vec!["• a", "  • b", "    1. c"]);
        for l in render(&format!("- {long}\n> {long}"), &t, 40) {
            assert!(l.width() <= 40, "{l:?}");
        }
    }

    #[test]
    fn tables_line_up_in_columns_and_fit() {
        let t = Theme::dark();
        // CJK: the second column starts in the same column on every row
        let lines = render("| 名前 | x |\n|---|---|\n| 中文テキスト | y |\n| a | z |", &t, 60);
        let out = text(&lines);
        let rows: Vec<&String> = out.iter().filter(|l| !l.starts_with('─')).collect();
        let col = |l: &str, c: char| crate::text::width(&l[..l.find(c).unwrap()]);
        assert_eq!([col(rows[0], 'x'), col(rows[1], 'y'), col(rows[2], 'z')], [14, 14, 14], "{out:?}");
        // a cell longer than 40 columns is cut with …
        let out = text(&render(&format!("| h |\n|---|\n| {} |", "c".repeat(50)), &t, 80));
        assert!(out.contains(&format!("{}…", "c".repeat(39))), "{out:?}");
        // a table wider than the pane narrows its widest columns instead of wrapping mid-row
        let cell = "z".repeat(30);
        let row = format!("| {cell} | {cell} | {cell} | {cell} | {cell} |");
        let src = format!("{row}\n|---|---|---|---|---|\n{row}\n{row}");
        let out = text(&render(&src, &t, 60));
        assert_eq!(out.len(), 4, "a header, its rule and two rows: {out:?}");
        assert!(out.iter().all(|l| crate::text::width(l) <= 60), "{out:?}");
    }

    #[test]
    fn rules_breaks_and_links() {
        let t = Theme::dark();
        assert_eq!(text(&render("---", &t, 4)), vec!["────"], "no wider than the pane");
        assert_eq!(text(&render("---", &t, 100)), vec!["─".repeat(60)]);
        assert_eq!(text(&render("a\nb", &t, 40)), vec!["a b"]);
        assert_eq!(text(&render("a  \nb", &t, 40)), vec!["a", "b"]);
        assert_eq!(text(&render("\n\npara", &t, 40)), vec!["para"]);
        // a link says where it goes (a terminal can't follow hidden ones), unless its text is the address
        let lines = render("see [docs](http://x.y) or <http://a.b> ![alt](p.png) ![i](data:image/png;base64,xyz) <me@x.y>", &t, 80);
        assert_eq!(text(&lines), vec!["see docs (http://x.y) or http://a.b alt (p.png) i me@x.y"]);
        assert_eq!(text(&render("| [a](http://x.y) |\n|---|\n| b |", &t, 80))[0], "a", "a table cell keeps just the text");
        let docs = style_of(&lines, "docs");
        assert!(docs.fg == Some(t.accent) && docs.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(style_of(&lines, "(http://x.y)").fg, Some(t.faint));
    }

    #[test]
    fn every_prefix_of_a_streaming_reply_renders_within_its_width() {
        let t = Theme::dark();
        let doc = "# Head\n\nSome **bold *nested* text** and ~~gone~~ and `code` and [a link](http://example.com/path) here.\n\n> quoted\n> > deeper with a long line that wraps\n\n- one\n- two\n  - nested 中文\n1. first\n2. second\n\n```rust\nfn main() { println!(\"a long line of code that will need to wrap\"); }\n```\n\n| a | b 中 |\n|---|---|\n| 1 | 2 |\n\n---\n\n<div>html</div>\n\n- [ ] task";
        for (i, _) in doc.char_indices() {
            for w in [1, 12, 40] {
                for l in render(&doc[..i], &t, w) {
                    assert!(w < 12 || crate::text::width(&l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()) <= w, "prefix {i} at {w}: {l:?}");
                }
            }
        }
    }

    fn out(src: &str, width: usize) -> Vec<String> {
        text(&render(src, &Theme::dark(), width))
    }

    #[test]
    fn every_construct_renders_as_its_text() {
        for (src, want) in [
            // headings, both kinds
            ("Title\n=====", vec!["Title"]),
            ("Sub\n---", vec!["Sub"]),
            ("#### four\n##### five\n###### six", vec!["four", "", "five", "", "six"]),
            // emphasis, escapes, entities
            ("*a* _b_ **c** __d__ ***e*** ~~f~~", vec!["a b c d e f"]),
            ("\\*not em\\* \\# \\`x\\`", vec!["*not em* # `x`"]),
            ("&amp; &lt; &gt; &copy; &#x263A; &nbsp;x", vec!["& < > © ☺ \u{a0}x"]),
            ("**open bold", vec!["**open bold"]),
            // inline code, links, images
            ("``a ` b``", vec![" a ` b "]),
            ("[t](http://u) [http://u](http://u)", vec!["t (http://u) http://u"]),
            ("[![alt](i.png)](http://x)", vec!["alt (i.png) (http://x)"]),
            ("![](p.png)", vec![" (p.png)"]),
            // code blocks: fenced (backticks, tildes, a language, empty), indented, still open
            ("```\nplain\n```", vec!["┌", "│ plain", "└"]),
            ("~~~py\nx\n~~~", vec!["┌ py", "│ x", "└"]),
            ("```\n\n```", vec!["┌", "│ ", "└"]),
            ("    indented code", vec!["┌", "│ indented code", "└"]),
            ("```py\nprint(1)\nmore", vec!["┌ py", "│ print(1)", "│ more", "└"]),
            // lists: start numbers, empty items, a task list, a loose list
            ("0. zero\n1. one", vec!["0. zero", "1. one"]),
            ("7. seven\n1. eight", vec!["7. seven", "8. eight"]),
            ("- \n- b", vec!["• ", "• b"]),
            ("- [ ] open\n- [x] done", vec!["• ☐ open", "• ☑ done"]),
            ("- a\n\n- b", vec!["• a", "• b"]),
            // quotes, rules, breaks, html
            ("> a\n>\n> b", vec!["▎ a", "▎ ", "▎ b"]),
            ("> # H\n> text", vec!["▎ H", "▎ ", "▎ text"]),
            ("***", vec!["──────────"]),
            ("___", vec!["──────────"]),
            ("a\\\nb", vec!["a", "b"]),
            ("<!-- note -->\n\npara", vec!["<!-- note -->", "", "para"]),
            ("x <span>y</span> z", vec!["x <span>y</span> z"]),
            // tables: alignment rows, a short row, inline markup in cells
            ("| a | b |\n|:--|--:|\n| 1 |", vec!["a  b", "────", "1   "]),
            ("| `x` | **y** |\n|---|---|\n| 1 | 2 |", vec!["x  y", "────", "1  2"]),
            // nothing at all
            ("", vec![]),
            ("  \n\n \t\n", vec![]),
        ] {
            assert_eq!(out(src, 10.max(want.iter().map(|l| crate::text::width(l)).max().unwrap_or(0))), want, "{src:?}");
        }
    }

    #[test]
    fn every_heading_level_and_inline_style() {
        let t = Theme::dark();
        let lines = render("# 1\n## 2\n### 3\n#### 4\n##### 5\n###### 6", &t, 40);
        for (h, under) in [("1", true), ("2", true), ("3", false), ("4", false), ("5", false), ("6", false)] {
            let s = style_of(&lines, h);
            assert!(s.add_modifier.contains(Modifier::BOLD), "h{h} is bold");
            assert_eq!(s.add_modifier.contains(Modifier::UNDERLINED), under, "h{h}");
            assert_eq!(s.fg, Some(t.text));
        }
        let lines = render("***both*** **b ~~bs~~** [**lb**](http://u) *i `c`*", &t, 80);
        let both = style_of(&lines, "both");
        assert!(both.add_modifier.contains(Modifier::BOLD | Modifier::ITALIC), "{both:?}");
        assert!(style_of(&lines, "bs").add_modifier.contains(Modifier::BOLD | Modifier::CROSSED_OUT));
        let lb = style_of(&lines, "lb");
        assert!(lb.add_modifier.contains(Modifier::BOLD | Modifier::UNDERLINED) && lb.fg == Some(t.accent), "a bold link: {lb:?}");
        let code = style_of(&lines, "c");
        assert_eq!((code.fg, code.bg), (Some(t.code), Some(t.surface3)), "inline code keeps its own look inside emphasis");
        // a table's header row is bold, its rule spans the columns and their gaps
        let lines = render("| name | v |\n|---|---|\n| x | 1 |", &t, 40);
        assert!(style_of(&lines, "name").add_modifier.contains(Modifier::BOLD));
        assert_eq!(text(&lines)[1], "─".repeat(4 + 2 + 1));
        assert_eq!(style_of(&lines, "──").fg, Some(t.line));
        // a code block's frame is faint, its text the code color
        let lines = render("```sh\nls\n```", &t, 40);
        assert_eq!(style_of(&lines, "┌ sh").fg, Some(t.faint));
        assert_eq!(style_of(&lines, "ls").fg, Some(t.code));
    }

    #[test]
    fn lists_and_quotes_inside_each_other_hang_under_their_text() {
        // a list in a quote: the bars, then the markers
        assert_eq!(out("> - a\n>   - b", 30), vec!["▎ • a", "▎   • b"]);
        // a code block in an item sits under the item's text
        assert_eq!(out("- a\n\n  ```\n  code\n  ```", 30), vec!["• a", "  ┌", "  │ code", "  └"]);
        // a long item wraps under its text, in columns (CJK), at every depth
        for l in out(&format!("- {}\n  - {}", "日本語".repeat(12), "word ".repeat(12)), 20) {
            assert!(crate::text::width(&l) <= 20, "{l:?}");
        }
        let o = out(&format!("12. {}", "word ".repeat(10)), 20);
        assert!(o[0].starts_with("12. word") && o[1..].iter().all(|l| l.starts_with("    word")), "a two-digit marker hangs four columns: {o:?}");
    }

    #[test]
    fn a_quote_inside_a_list_item_hangs_under_the_items_text() {
        // the quote belongs to the item: its bar under the item's text, not at the margin
        // with the item's indent after it
        let o = out("- item\n\n  > quoted text that wraps around", 20);
        assert_eq!(o[0], "• item");
        assert!(o[1..].iter().filter(|l| !l.is_empty()).all(|l| l.starts_with("  ▎ ")), "{o:?}");
    }

    #[test]
    fn a_list_inside_a_numbered_item_starts_under_the_items_text() {
        // CommonMark nests "   - b" in the item "1. a" (its content starts at column 3): drawn
        // under "a", not two columns in whatever the parent's marker is
        assert_eq!(out("1. a\n   - b", 30), vec!["1. a", "   • b"]);
        assert_eq!(out("10. a\n    - b", 30), vec!["10. a", "    • b"]);
    }

    #[test]
    fn a_code_blocks_label_fits_the_width() {
        // the info string after the fence can be anything (```python title="src/app/main.py")
        let src = format!("```python title=\"{}\"\nx = 1\n```", "src/a/very/long/path/".repeat(4));
        for w in [20, 40] {
            for l in out(&src, w) {
                assert!(crate::text::width(&l) <= w, "{l:?} is wider than {w}");
            }
        }
    }

    #[test]
    fn random_markdown_renders_clean_and_in_its_width() {
        let t = Theme::dark();
        let mut rng = crate::text::tests::Rng(41);
        // markdown syntax in pieces (no table rows: a table too wide for the pane is a known
        // exception), mixed with any text at all
        let syntax = ["\n", "\n\n", "# ", "## ", "> ", "- ", "1. ", "  ", "    ", "```", "```py\n", "~~~", "**", "*", "_", "~~", "`", "[", "](http://x.y/", ")", "![", "<", ">", "<b>", "</b>", "&amp;", "&#27;", "\\", "---", "- [ ] ", "- [x] ", "word ", "日本語 ", "👋🏽 ", "\t"];
        for case in 0..1500 {
            let mut src = String::new();
            for _ in 0..rng.below(14) {
                if rng.below(3) == 0 {
                    src.push_str(&crate::text::tests::random_text(&mut rng, 3));
                } else {
                    src.push_str(syntax[rng.below(syntax.len())]);
                }
            }
            for w in [0, 3, 40, 80] {
                let lines = render(&src, &t, w);
                for l in &lines {
                    for s in &l.spans {
                        assert!(!s.content.chars().any(char::is_control), "case {case} at {w}: {src:?} gave {:?}", s.content);
                    }
                    let shown: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                    let lw = crate::text::width(&shown);
                    // (a code block's label line is a_code_blocks_label_fits_the_width's)
                    assert!(w < 40 || lw <= w || shown.trim_start_matches(['▎', ' ', '•']).starts_with("┌ "), "case {case} at {w}: {src:?} gave a line {lw} wide: {l:?}");
                }
                assert!(lines.first().is_none_or(|l| !is_blank(l)), "case {case}: a blank first line for {src:?}");
            }
        }
    }

    #[test]
    fn hostile_text_in_every_construct_stays_out_of_the_spans() {
        let t = Theme::dark();
        let bad = "\x1b[31mred\x1b[0m\ttab\r\x07bel \u{9b}2J\u{85} ✍\u{FE0F} 🇦🇪 👨\u{200D}👩\u{200D}👧";
        let doc = format!("# {bad}\n\n*{bad}* `{bad}` [{bad}](http://x/{bad})\n\n- {bad}\n  1. {bad}\n\n> {bad}\n\n```{bad}\n{bad}\n```\n\n| {bad} | b |\n|---|---|\n| {bad} | 2 |\n\n<div>{bad}</div>\n\n![{bad}](data:x) &#27;[2J &#7; &#0;");
        for w in [1, 7, 20, 60] {
            for l in render(&doc, &t, w) {
                for s in &l.spans {
                    assert!(!s.content.chars().any(char::is_control), "{w}: {:?}", s.content);
                    assert!(!s.content.contains('\u{FE0F}') && !s.content.contains('\u{200D}') && !s.content.chars().any(|c| ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)), "{w}: emoji not in their plain form: {:?}", s.content);
                }
            }
        }
        // a NUL entity is U+FFFD (CommonMark), and drawn as such
        assert_eq!(out("a &#0; b", 20), vec!["a \u{FFFD} b"]);
    }

    #[test]
    fn every_prefix_of_any_reply_renders_without_breaking_its_width() {
        let t = Theme::dark();
        // all the constructs, nested, with wide characters and hostile text, streamed a
        // character at a time (a reply is drawn after every token)
        let doc = "Intro with *em*, **strong**, ~~gone~~, `code`, [link](http://example.com/a/b) and <http://auto.link>.\n\n## 見出し heading\n\n1. first item with enough words to wrap\n   - nested 日本語のテキスト\n     > a quote in a list\n2. second\n   ```js\n   const x = \"code in a list item that is long\";\n   ```\n- [ ] task\n- [x] done\n\n> quote\n> > deeper quote with `code` and a [link](http://x.y)\n>\n> - list in a quote\n\n```\nunlabelled\tcode\x1b[31m\n```\n\n    indented code\n\n| col | 中文 | n |\n|:--|:-:|--:|\n| a | b | 1 |\n| long cell text | x | 22 |\n\nText with a hard  \nbreak and a\\\nbackslash break.\n\n***\n\n<details>\n<summary>html</summary>\n</details>\n\n![image](pic.png) &amp; &#x1F600; done.";
        for (i, _) in doc.char_indices().chain([(doc.len(), ' ')]) {
            for w in [1, 2, 5, 16, 33, 80] {
                let lines = render(&doc[..i], &t, w);
                for l in &lines {
                    assert!(!l.spans.iter().any(|s| s.content.chars().any(char::is_control)), "prefix {i} at {w}: {l:?}");
                    let lw = crate::text::width(&l.spans.iter().map(|s| s.content.as_ref()).collect::<String>());
                    assert!(w < 16 || lw <= w, "prefix {i} at {w} is {lw} wide: {l:?}");
                }
                assert!(lines.first().is_none_or(|l| !is_blank(l)), "prefix {i} at {w} starts with a blank line");
            }
        }
    }
}
