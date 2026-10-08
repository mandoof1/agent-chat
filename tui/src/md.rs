//! Markdown to terminal lines: headings, emphasis, inline and fenced code, lists, quotes,
//! tables and rules, in the app's theme. Plenty for model replies; no images.

use crate::theme::Theme;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

#[derive(Default)]
struct Ctx {
    bold: usize,
    italic: usize,
    code: bool,
    link: usize,
    heading: Option<HeadingLevel>,
    quote: usize,
    list: Vec<Option<u64>>, // None = bullet, Some(n) = next number
    in_code_block: bool,
    code_lang: String,
    code_buf: String,
    table_row: Vec<String>,
    table_cell: String,
    in_table: bool,
    table_rows: Vec<Vec<String>>,
}

/// Render `text` as styled lines. `width` only matters for horizontal rules.
pub fn render(text: &str, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut ctx = Ctx::default();
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(text, opts);

    let flush = |lines: &mut Vec<Line<'static>>, cur: &mut Vec<Span<'static>>, ctx: &Ctx| {
        if cur.is_empty() {
            return;
        }
        let mut spans = Vec::new();
        if ctx.quote > 0 {
            spans.push(Span::styled("▎ ".repeat(ctx.quote), Style::default().fg(theme.faint)));
        }
        spans.append(cur);
        lines.push(Line::from(spans));
    };
    let blank = |lines: &mut Vec<Line<'static>>| {
        if lines.last().map(|l| !l.spans.is_empty()).unwrap_or(false) {
            lines.push(Line::from(""));
        }
    };

    for ev in parser {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    if ctx.list.is_empty() {
                        blank(&mut lines);
                    }
                }
                Tag::Heading { level, .. } => {
                    blank(&mut lines);
                    ctx.heading = Some(level);
                }
                Tag::BlockQuote(_) => {
                    blank(&mut lines);
                    ctx.quote += 1;
                }
                Tag::CodeBlock(kind) => {
                    flush(&mut lines, &mut cur, &ctx);
                    blank(&mut lines);
                    ctx.in_code_block = true;
                    ctx.code_lang = match kind {
                        CodeBlockKind::Fenced(l) => l.to_string(),
                        _ => String::new(),
                    };
                    ctx.code_buf.clear();
                }
                Tag::List(start) => {
                    if ctx.list.is_empty() {
                        blank(&mut lines);
                    }
                    ctx.list.push(start);
                }
                Tag::Item => {
                    flush(&mut lines, &mut cur, &ctx);
                    let depth = ctx.list.len().saturating_sub(1);
                    let marker = match ctx.list.last_mut() {
                        Some(Some(n)) => {
                            let m = format!("{n}. ");
                            *n += 1;
                            m
                        }
                        _ => "• ".to_string(),
                    };
                    cur.push(Span::raw("  ".repeat(depth)));
                    cur.push(Span::styled(marker, Style::default().fg(theme.muted)));
                }
                Tag::Emphasis => ctx.italic += 1,
                Tag::Strong => ctx.bold += 1,
                Tag::Strikethrough => {}
                Tag::Link { .. } => ctx.link += 1,
                Tag::Table(_) => {
                    flush(&mut lines, &mut cur, &ctx);
                    blank(&mut lines);
                    ctx.in_table = true;
                    ctx.table_rows.clear();
                }
                Tag::TableHead | Tag::TableRow => ctx.table_row.clear(),
                Tag::TableCell => ctx.table_cell.clear(),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => flush(&mut lines, &mut cur, &ctx),
                TagEnd::Heading(_) => {
                    flush(&mut lines, &mut cur, &ctx);
                    ctx.heading = None;
                }
                TagEnd::BlockQuote(_) => {
                    flush(&mut lines, &mut cur, &ctx);
                    ctx.quote = ctx.quote.saturating_sub(1);
                }
                TagEnd::CodeBlock => {
                    ctx.in_code_block = false;
                    let lang = ctx.code_lang.clone();
                    let head = if lang.is_empty() { "┌".to_string() } else { format!("┌ {lang}") };
                    lines.push(Line::from(Span::styled(head, Style::default().fg(theme.faint))));
                    for l in ctx.code_buf.trim_end_matches('\n').split('\n') {
                        lines.push(Line::from(vec![
                            Span::styled("│ ", Style::default().fg(theme.faint)),
                            Span::styled(l.to_string(), Style::default().fg(theme.code)),
                        ]));
                    }
                    lines.push(Line::from(Span::styled("└", Style::default().fg(theme.faint))));
                    ctx.code_buf.clear();
                }
                TagEnd::List(_) => {
                    flush(&mut lines, &mut cur, &ctx);
                    ctx.list.pop();
                }
                TagEnd::Item => flush(&mut lines, &mut cur, &ctx),
                TagEnd::Emphasis => ctx.italic = ctx.italic.saturating_sub(1),
                TagEnd::Strong => ctx.bold = ctx.bold.saturating_sub(1),
                TagEnd::Link => ctx.link = ctx.link.saturating_sub(1),
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
                    let mut widths = vec![0usize; cols];
                    for r in &rows {
                        for (i, c) in r.iter().enumerate() {
                            widths[i] = widths[i].max(c.chars().count().min(40));
                        }
                    }
                    for (ri, r) in rows.iter().enumerate() {
                        let mut spans = Vec::new();
                        for (i, w) in widths.iter().enumerate() {
                            let c = r.get(i).cloned().unwrap_or_default();
                            let c: String = c.chars().take(40).collect();
                            let pad = w.saturating_sub(c.chars().count());
                            let style = if ri == 0 { Style::default().fg(theme.muted).add_modifier(Modifier::BOLD) } else { Style::default() };
                            spans.push(Span::styled(format!("{c}{}", " ".repeat(pad)), style));
                            if i + 1 < widths.len() {
                                spans.push(Span::styled("  ", Style::default()));
                            }
                        }
                        lines.push(Line::from(spans));
                        if ri == 0 {
                            lines.push(Line::from(Span::styled("─".repeat(widths.iter().sum::<usize>() + 2 * widths.len().saturating_sub(1)), Style::default().fg(theme.line))));
                        }
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if ctx.in_code_block {
                    ctx.code_buf.push_str(&t);
                } else if ctx.in_table {
                    ctx.table_cell.push_str(&t);
                } else {
                    cur.push(Span::styled(t.to_string(), inline_style(&ctx, theme)));
                }
            }
            Event::Code(t) => {
                if ctx.in_table {
                    ctx.table_cell.push_str(&t);
                } else {
                    cur.push(Span::styled(format!(" {t} "), Style::default().fg(theme.code).bg(theme.surface3)));
                }
            }
            Event::SoftBreak => cur.push(Span::raw(" ")),
            Event::HardBreak => flush(&mut lines, &mut cur, &ctx),
            Event::Rule => {
                flush(&mut lines, &mut cur, &ctx);
                lines.push(Line::from(Span::styled("─".repeat(width.clamp(8, 60)), Style::default().fg(theme.line))));
            }
            Event::TaskListMarker(done) => {
                cur.push(Span::styled(if done { "☑ " } else { "☐ " }, Style::default().fg(theme.accent)));
            }
            Event::Html(h) | Event::InlineHtml(h) => cur.push(Span::styled(h.to_string(), Style::default().fg(theme.faint))),
            _ => {}
        }
    }
    flush(&mut lines, &mut cur, &ctx);
    while lines.first().map(|l| l.spans.is_empty()).unwrap_or(false) {
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
}
