//! Drawing: sidebar | header + trace + composer | files drawer, and the overlays.

use crate::app::{agent_color, agent_who, time_ago, App, Focus, Overlay, SideRow};
use crate::commands::{self, Cmd, Parsed};
use crate::text::{self, line as clean};
use crate::trace::{self, fmt_k, RenderOpts};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Widget, Wrap};
use ratatui::Frame;

/// Render clipped to the frame. On a small terminal the layout hands out rows and columns that
/// aren't there, and ratatui panics on a cell outside its buffer.
pub fn render<W: Widget>(f: &mut Frame, w: W, r: Rect) {
    let r = r.intersection(f.area());
    if !r.is_empty() {
        f.render_widget(w, r);
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let th = app.theme.clone();
    let area = f.area();
    app.last_size = (area.width, area.height);
    render(f, Block::default().style(Style::default().bg(th.bg).fg(th.text)), area);

    // (pane_cols leaves the trace its room, so these add up to the width)
    let (side_w, files_w) = app.pane_cols(area.width);
    let main_w = area.width.saturating_sub(side_w + files_w);
    let cols = [Rect { width: side_w, ..area }, Rect { x: area.x + side_w, width: main_w, ..area }, Rect { x: area.x + side_w + main_w, width: files_w, ..area }];

    app.drawn_cols = (cols[0].width, cols[2].width);
    app.side_row_map.clear(); // no sidebar drawn, no rows to click
    if cols[0].width > 0 {
        draw_sidebar(f, app, cols[0]);
    }
    let toast_area = draw_main(f, app, cols[1]);
    if cols[2].width > 0 {
        draw_files(f, app, cols[2]);
    }
    // over the dialogs: what an action taken in one says (a form's error, "Copied the file")
    draw_overlay(f, app, area);
    draw_toasts(f, app, toast_area);
}

fn sep(th: &crate::theme::Theme) -> Span<'static> {
    Span::styled("│", Style::default().fg(th.line))
}

// ---------------------------------------------------------------- sidebar

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.theme.clone();
    render(f, Block::default().style(Style::default().bg(th.surface)), area);
    let inner = Rect { x: area.x, y: area.y, width: area.width.saturating_sub(1), height: area.height };
    // right border
    for y in area.y..area.y + area.height {
        render(f, Paragraph::new(sep(&th)), Rect { x: inner.right(), y, width: 1, height: 1 });
    }
    let focused = app.focus == Focus::Sidebar;
    let mut y = inner.y;
    let w = inner.width as usize;
    let put = |f: &mut Frame, y: u16, line: Line| {
        render(f, Paragraph::new(line), Rect { x: inner.x, y, width: inner.width, height: 1 });
    };
    put(f, y, Line::from(vec![Span::styled(" ⟟ ", Style::default().fg(th.accent)), Span::styled("Agent Chat", Style::default().fg(th.text).add_modifier(Modifier::BOLD)), Span::styled(format!("{:>w$}", "Ctrl+K ", w = w.saturating_sub(13)), Style::default().fg(th.faint))]));
    y += 1;
    put(f, y, Line::from(Span::styled(" Agents", Style::default().fg(th.muted).add_modifier(Modifier::BOLD))));
    y += 1;

    let agent_rows: Vec<(usize, SideRow)> = app.side_rows.iter().cloned().enumerate().filter(|(_, r)| matches!(r, SideRow::AllAgents | SideRow::Agent(_))).collect();
    let chat_rows: Vec<(usize, SideRow)> = app.side_rows.iter().cloned().enumerate().filter(|(_, r)| matches!(r, SideRow::Group(_) | SideRow::Chat(_))).collect();
    let agents_h = (agent_rows.len() as u16).min(inner.height.saturating_sub(10).max(3));
    for (i, r) in agent_rows.iter().take(agents_h as usize) {
        let selected = *i == app.side_sel;
        let (mark, name, color) = match r {
            SideRow::AllAgents => (text::icon("▦"), "All chats".to_string(), th.muted),
            SideRow::Agent(id) => {
                let w = agent_who(&app.agents, id);
                (w.emoji.clone(), w.name.clone(), w.color)
            }
            _ => unreachable!(),
        };
        let active = match r {
            SideRow::AllAgents => app.filter.is_empty(),
            SideRow::Agent(id) => &app.filter == id,
            _ => false,
        };
        let state = match r {
            SideRow::Agent(id) => app.state_word(id),
            SideRow::AllAgents => match app.busy_state(None) { Some("waiting") => "needs you".into(), Some("running") => "working".into(), _ => String::new() },
            _ => String::new(),
        };
        let state_color = if state == "needs you" { th.amber } else if state == "working" { th.accent } else { th.faint };
        let bg = if selected && focused { th.surface3 } else if active { th.surface2 } else { th.surface };
        // columns, not characters: the state words line up whatever the names and icons are
        let name_w = w.saturating_sub(4 + text::width(&state) + 2);
        let line = Line::from(vec![
            Span::styled(if active { "▎" } else { " " }, Style::default().fg(th.accent).bg(bg)),
            Span::styled(format!("{mark} "), Style::default().bg(bg).fg(color)),
            Span::styled(text::pad_cut(&name, name_w, true), Style::default().bg(bg).fg(th.text).add_modifier(if active || selected { Modifier::BOLD } else { Modifier::empty() })),
            Span::styled(format!(" {state} "), Style::default().bg(bg).fg(state_color)),
        ]);
        put(f, y, line);
        app.side_row_map.insert(y as usize, *i);
        y += 1;
    }
    y += 1;
    // chats header
    let head = if app.filter.is_empty() { "Chats".to_string() } else { text::cut(&format!("{} chats", agent_who(&app.agents, &app.filter).name), w.saturating_sub(10)) };
    let filter_w = w.saturating_sub(text::width(&head) + 4);
    let search = if app.search_focus { format!("/{}▏", clean(&app.search)) } else if !app.search.is_empty() { format!("/{}", clean(&app.search)) } else { "/ filter".to_string() };
    let search_t = text::fit_end(&search, filter_w);
    put(f, y, Line::from(vec![
        Span::styled(format!(" {head}"), Style::default().fg(th.muted).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{}{search_t}", " ".repeat(filter_w + 1 - text::width(search_t))), Style::default().fg(if app.search_focus { th.accent } else { th.faint })),
    ]));
    y += 1;
    put(f, y, Line::from(vec![Span::styled(" n ", Style::default().fg(th.bg).bg(th.accent).add_modifier(Modifier::BOLD)), Span::styled(" new chat", Style::default().fg(th.text))]));
    y += 2;

    // chat list, scrolled to show the selection when it moves (the wheel scrolls it otherwise)
    let list_h = (inner.y + inner.height).saturating_sub(y + 1) as usize;
    let sel_pos = chat_rows.iter().position(|(i, _)| *i == app.side_sel);
    let snap = (app.side_sel, list_h, chat_rows.len());
    if let Some(p) = sel_pos.filter(|_| app.side_snap != Some(snap)) {
        // (a group's first chat brings its header into view too)
        let top = if p > 0 && list_h > 1 && matches!(chat_rows[p - 1].1, SideRow::Group(_)) { p - 1 } else { p };
        if top < app.side_scroll {
            app.side_scroll = top;
        } else if p >= app.side_scroll + list_h.max(1) {
            app.side_scroll = p + 1 - list_h.max(1);
        }
    }
    app.side_snap = Some(snap);
    app.side_scroll = app.side_scroll.min(chat_rows.len().saturating_sub(list_h.max(1)));
    if chat_rows.is_empty() {
        let msg = if !app.search.is_empty() { "No chats match." } else { "No chats yet. Press n." };
        put(f, y, Line::from(Span::styled(format!(" {msg}"), Style::default().fg(th.muted))));
    }
    for (i, r) in chat_rows.iter().skip(app.side_scroll).take(list_h) {
        match r {
            SideRow::Group(g) => put(f, y, Line::from(Span::styled(format!(" {g}"), Style::default().fg(th.faint)))),
            SideRow::Chat(id) => {
                let Some(c) = app.chats.iter().find(|c| &c.id == id) else { continue };
                let selected = *i == app.side_sel;
                let open = app.chat_id.as_deref() == Some(id);
                let bg = if selected && focused { th.surface3 } else if open { th.surface2 } else { th.surface };
                let who = agent_who(&app.agents, &c.agent_id);
                let dot = match c.status.as_str() {
                    "running" => Span::styled("●", Style::default().fg(th.accent).bg(bg)),
                    "waiting" => Span::styled("●", Style::default().fg(th.amber).bg(bg)),
                    "delegated" => Span::styled("◌", Style::default().fg(th.accent).bg(bg)),
                    _ if app.is_unread(c) => Span::styled("•", Style::default().fg(th.accent).bg(bg)),
                    _ => Span::styled(" ", Style::default().bg(bg)),
                };
                let pin = if c.pinned { "⚲" } else { "" };
                let sub = c.parent.as_ref().map(|_| "↳ ").unwrap_or("");
                // the status dot keeps its column, whatever the title is written in
                let title_w = w.saturating_sub(6 + text::width(pin));
                put(f, y, Line::from(vec![
                    Span::styled(if open { "▎" } else { " " }, Style::default().fg(th.accent).bg(bg)),
                    Span::styled(format!("{} ", who.emoji), Style::default().bg(bg)),
                    Span::styled(pin.to_string(), Style::default().fg(th.amber).bg(bg)),
                    Span::styled(text::pad_cut(&format!("{sub}{}", clean(&c.title)), title_w, true), Style::default().bg(bg).fg(th.text).add_modifier(if open { Modifier::BOLD } else { Modifier::empty() })),
                    dot,
                    Span::styled(" ", Style::default().bg(bg)),
                ]));
                app.side_row_map.insert(y as usize, *i);
            }
            _ => {}
        }
        y += 1;
        if y >= inner.y + inner.height.saturating_sub(1) {
            break;
        }
    }
    // status line
    let sy = (inner.y + inner.height).saturating_sub(1);
    let model = app.settings.get("model").and_then(|m| m.as_str()).filter(|m| !m.is_empty()).map(String::from).or_else(|| app.server.models.first().cloned()).unwrap_or_else(|| "online".into());
    let model: String = clean(model.rsplit('/').next().unwrap_or(&model).trim_end_matches(".gguf"));
    let (dot, text) = if app.server.ok { ("●", model) } else { ("●", "model offline".into()) };
    let mem = if app.memory_count > 0 { format!(" ◉{}", app.memory_count) } else { String::new() };
    let text = text::cut(&text, w.saturating_sub(4 + text::width(&mem)));
    put(f, sy, Line::from(vec![
        Span::styled(format!(" {dot} "), Style::default().fg(if app.server.ok { th.ok } else { th.danger })),
        Span::styled(text, Style::default().fg(th.muted)),
        Span::styled(mem, Style::default().fg(if app.memory_working { th.accent } else { th.faint })),
    ]));
}

// ------------------------------------------------------------------- main

/// Draws the chat (or the welcome page) and returns the area toasts may cover.
fn draw_main(f: &mut Frame, app: &mut App, area: Rect) -> Rect {
    if app.chat_id.is_none() {
        draw_welcome(f, app, area);
        return area;
    }
    let composer_h: u16 = if app.is_subchat() { 2 } else { (app.composer.lines().len() as u16).clamp(1, 8) + 3 + if app.attachments.is_empty() { 0 } else { 1 } + if app.queue.is_empty() { 0 } else { app.queue.len().min(3) as u16 + 1 } };
    let rows = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(2), Constraint::Min(3), Constraint::Length(composer_h)]).split(area);
    draw_header(f, app, rows[0]);
    draw_messages(f, app, rows[1]);
    draw_composer(f, app, rows[2]);
    let menu_h = draw_menu(f, app, rows[1]);
    Rect { height: rows[1].height.saturating_sub(menu_h), ..rows[1] }
}

/// At most this many commands show in the menu at once; it scrolls with the selection.
pub const MENU_ROWS: usize = 8;

/// The slash-command menu, along the bottom of the trace right above the message box (the box
/// keeps the focus and the cursor). Returns the rows it covers.
fn draw_menu(f: &mut Frame, app: &mut App, area: Rect) -> u16 {
    let Some(items) = app.menu() else { return 0 };
    let th = app.theme.clone();
    let h = (items.len().min(MENU_ROWS) as u16 + 2).min(area.height);
    if h < 3 || area.width < 12 {
        return 0;
    }
    let rect = Rect { x: area.x, y: area.bottom() - h, width: area.width, height: h };
    let fit = (h - 2) as usize;
    let sel = app.menu_sel;
    let start = sel.saturating_sub(fit - 1);
    let faint = Style::default().fg(th.faint);
    let mut block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(th.line)).style(Style::default().bg(th.surface2)).title(Span::styled(" commands ", Style::default().fg(th.muted)));
    if items.len() > fit {
        block = block.title(Line::from(Span::styled(format!(" {}/{} ", sel + 1, items.len()), faint)).right_aligned());
    }
    let keys = if rect.width >= 56 { " ↑↓ select · Tab complete · Enter run · Esc close " } else { " ↑↓ · Tab complete · Enter run · Esc " };
    let block = block.title_bottom(Span::styled(keys, faint));
    render(f, Clear, rect);
    let inner = block.inner(rect);
    render(f, block, rect);
    // "/name /alias [args]": the alias only when it is what matched
    let q = app.menu_for.clone();
    let call = |c: &Cmd| {
        let alias = if c.name.contains(q.as_str()) { None } else { c.aliases.iter().find(|a| a.starts_with(q.as_str())).or_else(|| c.aliases.iter().find(|a| a.contains(q.as_str()))) };
        (format!("/{}", c.name), alias.map(|a| format!(" /{a}")).unwrap_or_default(), if c.args.is_empty() { String::new() } else { format!(" {}", c.args) })
    };
    let w = inner.width as usize;
    let col = items.iter().map(|c| { let (n, a, g) = call(c); text::width(&n) + text::width(&a) + text::width(&g) }).max().unwrap_or(0) + 5;
    let col = col.min(w / 2).max(w.min(12));
    let mut lines = vec![];
    for (i, c) in items.iter().enumerate().skip(start).take(fit) {
        let on = i == sel;
        let bg = if on { th.surface3 } else { th.surface2 };
        let (name, alias, args) = call(c);
        let mut spans = vec![
            Span::styled(if on { " ▸ " } else { "   " }, Style::default().fg(th.accent).bg(bg)),
            Span::styled(name, Style::default().fg(if on { th.accent } else { th.text }).bg(bg).add_modifier(Modifier::BOLD)),
            Span::styled(alias, Style::default().fg(th.faint).bg(bg)),
            Span::styled(args, Style::default().fg(th.muted).bg(bg)),
        ];
        let used: usize = spans.iter().map(|s| s.width()).sum();
        spans.push(Span::styled(" ".repeat(col.saturating_sub(used).max(1)), Style::default().bg(bg)));
        let left = w.saturating_sub(used.max(col) + 1);
        if left >= 8 {
            spans.push(Span::styled(trace::one_line(c.desc, left), Style::default().fg(th.muted).bg(bg)));
        }
        let used: usize = spans.iter().map(|s| s.width()).sum();
        spans.push(Span::styled(" ".repeat(w.saturating_sub(used)), Style::default().bg(bg)));
        lines.push(Line::from(spans));
    }
    render(f, Paragraph::new(lines), inner);
    h
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let chat = app.chat.as_ref();
    let title = clean(chat.and_then(|c| c.get("title")).and_then(|t| t.as_str()).unwrap_or("…"));
    let aid = app.chat_agent_id();
    let who = agent_who(&app.agents, &aid);
    let agent = app.agent(&aid);
    let sub = if app.is_subchat() {
        let caller = chat.and_then(|c| c.get("parent")).and_then(|p| p.get("caller_id")).and_then(|c| c.as_str()).unwrap_or("");
        format!("{} · working for {}", who.name, agent_who(&app.agents, caller).name)
    } else {
        format!("{}{}", who.name, agent.map(|a| if a.purpose.is_empty() { String::new() } else { format!(" · {}", clean(&a.purpose)) }).unwrap_or_default())
    };
    // right side: context bar + speed
    let (mut bar, mut ctx, mut speed) = (vec![], None, None);
    if let Some(t) = app.ctx_tokens {
        let n = app.server.n_ctx;
        let pct = n.map(|n| (t as f64 / n as f64 * 100.0).min(100.0)).unwrap_or(0.0);
        let bar_w = 12usize;
        let fill = ((pct / 100.0) * bar_w as f64).round() as usize;
        let color = if pct >= 90.0 { th.danger } else if pct >= 70.0 { th.amber } else { th.ok };
        if n.is_some() {
            bar.push(Span::styled("▮".repeat(fill.max(if t > 0 { 1 } else { 0 })), Style::default().fg(color)));
            bar.push(Span::styled("▯".repeat(bar_w.saturating_sub(fill.max(if t > 0 { 1 } else { 0 }))), Style::default().fg(th.line)));
        }
        ctx = Some(Span::styled(format!(" ctx {}{}", fmt_k(t), n.map(|n| format!("/{}", fmt_k(n))).unwrap_or_default()), Style::default().fg(th.muted)));
    }
    if let Some(s) = app.tok_per_s {
        speed = Some(Span::styled(format!("  {s:.1} tok/s"), Style::default().fg(th.muted)));
    }
    // in a narrow header the bar, then the speed, give way before the title does
    let w = area.width as usize;
    let cols = |b: &[Span], c: &Option<Span>, s: &Option<Span>| b.iter().chain(c).chain(s).map(|x| text::width(&x.content)).sum::<usize>();
    if cols(&bar, &ctx, &speed) + 3 + 16 > w {
        bar.clear();
    }
    if cols(&bar, &ctx, &speed) + 3 + 16 > w {
        speed = None;
    }
    let right_w = cols(&bar, &ctx, &speed);
    let left_w = w.saturating_sub(right_w + 3);
    // columns, not characters: a CJK or emoji title can't push the stats off the edge
    let t = text::cut(&format!(" {} {}", who.emoji, title), left_w);
    let pad = " ".repeat(left_w.saturating_sub(text::width(&t)) + 1);
    let mut l1 = vec![Span::styled(t, Style::default().fg(th.text).add_modifier(Modifier::BOLD)), Span::raw(pad)];
    l1.extend(bar.into_iter().chain(ctx).chain(speed));
    render(f, Paragraph::new(Line::from(l1)), Rect { x: area.x, y: area.y, width: area.width, height: 1 });
    // a long name or purpose is cut to leave "● running" its room
    let running = if app.running { "  ● running" } else { "" };
    let mut l2 = vec![Span::styled(text::cut(&format!("   {sub}"), w.saturating_sub(text::width(running))), Style::default().fg(th.muted))];
    if app.running {
        l2.push(Span::styled(running, Style::default().fg(th.accent)));
    }
    render(f, Paragraph::new(Line::from(l2)), Rect { x: area.x, y: area.y + 1, width: area.width, height: 1 });
}

/// Columns the trace needs to be drawn at all.
pub const MIN_TRACE: usize = 6;

fn draw_messages(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.theme.clone();
    let inner = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: area.height.saturating_sub(1) };
    let width = inner.width as usize;
    // too narrow to read: nothing is drawn, rather than the whole chat wrapped a column or two
    // wide (a long one takes seconds, and the screen waits for it)
    if width < MIN_TRACE || inner.height == 0 {
        return;
    }
    let key = (width, app.verbose(), app.dirty);
    if app.rendered.is_none() || app.render_key != key {
        // (reusing the settled part of the last render: a token redoes the reply, not the chat)
        let opts = RenderOpts { theme: &th, width, verbose: app.verbose(), selected_turn: app.selected_turn };
        app.rendered = Some(trace::render_from(&app.trace, &opts, app.rendered.take()));
        app.render_key = key;
    }
    let rendered = app.rendered.as_ref().unwrap();
    // how often the chat was compacted and how long it has been going, after the last turn (and
    // under a running one). It counts up, so it is drawn fresh, not kept with the rendered lines
    let foot = app.chat.as_ref().and_then(|c| trace::footer(c, trace::now())).map(|s| [Line::from(""), Line::from(Span::styled(text::cut(&s, width), Style::default().fg(th.faint)))]);
    let foot = foot.as_ref().map_or(&[][..], |f| &f[..]);
    let total = rendered.lines.len() + foot.len();
    let h = inner.height as usize;
    let max_scroll = total.saturating_sub(h);
    if app.follow {
        app.scroll = max_scroll;
    }
    if app.scroll > max_scroll {
        app.scroll = max_scroll;
        app.follow = true;
    }
    if app.scroll == max_scroll {
        app.follow = true;
    }
    let lines: Vec<Line> = rendered.lines.iter().chain(foot).skip(app.scroll).take(h).cloned().collect();
    render(f, Paragraph::new(lines), inner);
    // (once the chat has loaded: before that it is only the list's summary of it)
    if app.trace.entries.is_empty() && !app.running && app.chat.as_ref().is_some_and(|c| c.get("messages").is_some()) {
        let aid = app.chat_agent_id();
        let a = app.agent(&aid);
        let msg = clean(&match a {
            Some(a) if !a.purpose.is_empty() => format!("{} {} is ready. Its job: {}.", a.emoji, a.name, a.purpose),
            Some(a) => format!("{} {} is ready.", a.emoji, a.name),
            None => "Ready.".into(),
        });
        render(f, Paragraph::new(Span::styled(msg, Style::default().fg(th.muted))).wrap(Wrap { trim: true }), inner);
    }
    // scroll hint
    if !app.follow && total > h && h > 0 {
        let below = max_scroll.saturating_sub(app.scroll);
        let tag = format!(" ↓ {} more line{} · End for newest ", below, if below == 1 { "" } else { "s" });
        let x = inner.x + inner.width.saturating_sub(tag.chars().count() as u16 + 1);
        render(f, Paragraph::new(Span::styled(tag, Style::default().fg(th.bg).bg(th.accent))), Rect { x, y: inner.bottom() - 1, width: inner.width.saturating_sub(x - inner.x), height: 1 });
    }
    if app.focus == Focus::Messages {
        render(f, Paragraph::new(Span::styled("▏", Style::default().fg(th.accent))), Rect { x: area.x, y: area.y + 1, width: 1, height: area.height.saturating_sub(1) });
    }
}

fn draw_composer(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.theme.clone();
    if app.is_subchat() {
        let caller = app.chat.as_ref().and_then(|c| c.get("parent")).and_then(|p| p.get("root_chat_id")).and_then(|c| c.as_str()).unwrap_or("");
        let root = app.chats.iter().find(|c| c.id == caller).map(|c| c.title.clone()).unwrap_or_else(|| "the original chat".into());
        let text = format!(" This agent is working for another agent. You can watch here; to reply, open “{}”.", trace::one_line(&root, 40));
        render(f, Paragraph::new(Line::from(Span::styled(text, Style::default().fg(th.muted)))).wrap(Wrap { trim: true }), area);
        return;
    }
    let focused = app.focus == Focus::Composer;
    let mut y = area.y;
    if !app.queue.is_empty() {
        render(f, Paragraph::new(Line::from(Span::styled(format!(" queued ({}) · the agent reads these at its next step · ↑ in an empty box takes them back", app.queue.len()), Style::default().fg(th.accent)))), Rect { x: area.x, y, width: area.width, height: 1 });
        y += 1;
        for q in app.queue.iter().take(3) {
            let text = q.get("content").and_then(|c| c.as_str()).unwrap_or("");
            render(f, Paragraph::new(Line::from(vec![Span::styled("   ◦ ", Style::default().fg(th.accent)), Span::styled(trace::one_line(text, (area.width as usize).saturating_sub(6)), Style::default().fg(th.text))])), Rect { x: area.x, y, width: area.width, height: 1 });
            y += 1;
        }
    }
    if !app.attachments.is_empty() {
        let mut spans = vec![Span::styled(" ", Style::default())];
        for a in &app.attachments {
            spans.push(Span::styled(format!(" {}{} ", if a.image { "🖼 " } else { "📎 " }, clean(&a.name)), Style::default().fg(th.text).bg(th.surface3)));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled("Backspace in an empty box removes the last", Style::default().fg(th.faint)));
        render(f, Paragraph::new(Line::from(spans)), Rect { x: area.x, y, width: area.width, height: 1 });
        y += 1;
    }
    let box_h = area.bottom().saturating_sub(y);
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(if focused { th.accent } else { th.line })).style(Style::default().bg(th.surface));
    let rect = Rect { x: area.x, y, width: area.width, height: box_h };
    let inner = block.inner(rect);
    render(f, block, rect);
    let text_h = inner.height.saturating_sub(1);
    app.composer.set_style(Style::default().fg(th.text).bg(th.surface));
    app.composer.set_placeholder_style(Style::default().fg(th.faint));
    app.composer.set_cursor_style(if focused { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() });
    render(f, &app.composer, Rect { x: inner.x + 1, y: inner.y, width: inner.width.saturating_sub(2), height: text_h.max(1) });
    // bottom bar: mode chip, hints, send/stop
    let bypass = app.settings.get("bypass_approvals").and_then(|b| b.as_bool()).unwrap_or(false);
    let chip = if bypass { " ⚠ bypassing permissions " } else { " ⛨ asks before acting " };
    // what Enter will do: run the menu's pick while it is open, else a complete command; refuse
    // one that waits for the agent; say a /word is no command; or send (queue) the text
    let text = app.composer_text();
    let (command, unknown) = match app.menu() {
        Some(items) => (Some(items[app.menu_sel]), None),
        None => match commands::parse(&text) {
            Parsed::Run(name, _) => (commands::find(name), None),
            Parsed::Unknown(word) => (None, Some(word.to_string())),
            Parsed::Message(_) => (None, None),
        },
    };
    let refused = command.filter(|c| commands::check(c, app.in_chat(), app.running).is_err());
    let command = command.filter(|_| refused.is_none()).map(|c| c.name);
    let hint = match (command, refused, &unknown) {
        (Some(name), _, _) => format!("Enter runs /{name}{}", if app.running { " now (not queued)" } else { "" }),
        (_, Some(c), _) => format!("/{} waits for the agent · Ctrl+S stops it", c.name),
        (_, _, Some(word)) => format!("Unknown command /{} · // sends it as a message", clean(word)),
        _ if app.edit_from.is_some() => "editing your last message · Enter resends · Esc cancels".into(),
        _ if app.running => "Enter queues · Ctrl+S stops".into(),
        _ => "Enter sends · Alt+Enter new line · Ctrl+U attach · F1 help".into(),
    };
    let action = if command.is_some() {
        " Run ▸ "
    } else if app.running {
        " Stop ■ "
    } else if unknown.is_some() {
        "" // (Enter neither sends nor runs it)
    } else if app.edit_from.is_some() {
        " Resend ↑ "
    } else {
        " Send ↑ "
    };
    // the button keeps the right edge: the hint, then the mode chip, give way in a narrow box
    let iw = inner.width as usize;
    let room = iw.saturating_sub(text::width(action) + 1);
    let chip = text::cut(chip, room);
    let hint_room = room - text::width(&chip);
    let hint = if hint_room > 1 { format!(" {}", fit_items(&hint, hint_room - 1)) } else { String::new() };
    let pad = room - text::width(&chip) - text::width(&hint);
    let stop = app.running && command.is_none();
    let bar = vec![
        Span::styled(chip, Style::default().fg(if bypass { th.amber } else { th.muted })),
        Span::styled(hint, Style::default().fg(th.faint)),
        Span::raw(" ".repeat(pad)),
        Span::styled(text::fit(action, iw), Style::default().fg(if stop { th.text } else { th.bg }).bg(if stop { th.danger } else { th.accent }).add_modifier(Modifier::BOLD)),
    ];
    render(f, Paragraph::new(Line::from(bar)), Rect { x: inner.x, y: inner.y + text_h, width: inner.width, height: 1 });
}

/// The " · "-separated items of `s` that fit in `cols`, whole (the first one cut if it alone
/// doesn't fit), so a hint loses its last items rather than a word's end.
fn fit_items(s: &str, cols: usize) -> String {
    let mut out = String::new();
    for item in s.split(" · ") {
        let next = if out.is_empty() { item.to_string() } else { format!("{out} · {item}") };
        if text::width(&next) > cols {
            break;
        }
        out = next;
    }
    if out.is_empty() { text::cut(s, cols) } else { out }
}

fn draw_welcome(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.theme.clone();
    let inner = Rect { x: area.x + 3, y: area.y + 1, width: area.width.saturating_sub(6), height: area.height.saturating_sub(2) };
    let mut lines: Vec<Line> = vec![];
    let model = clean(&app.server.models.first().cloned().unwrap_or_default());
    lines.push(Line::from(Span::styled(if app.server.ok { format!("Local · {} · {} agents{}", model.rsplit('/').next().unwrap_or(&model), app.agents.len(), app.server.n_ctx.map(|n| format!(" · {} context", fmt_k(n))).unwrap_or_default()) } else { format!("Model server offline · {} agents", app.agents.len()) }, Style::default().fg(th.faint))));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Pick an agent.", Style::default().fg(th.text).add_modifier(Modifier::BOLD))));
    lines.push(Line::from(Span::styled("Watch every step.", Style::default().fg(th.muted).add_modifier(Modifier::BOLD))));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Each agent has its own job, tools and instructions. They share what they know about you, and hand work to each other when a job needs another skill. Everything they do shows up as a trace you can read.", Style::default().fg(th.muted))));
    lines.push(Line::from(""));
    if !app.server.ok {
        lines.push(Line::from(vec![Span::styled("✖ ", Style::default().fg(th.danger)), Span::styled(format!("The model server isn't reachable at {}. Start llama-server, or point Settings (Alt+S) at the server you use.", clean(if app.server.base_url.is_empty() { app.settings.get("base_url").and_then(|b| b.as_str()).unwrap_or("") } else { &app.server.base_url })), Style::default().fg(th.danger))]));
        lines.push(Line::from(""));
    }
    if app.memory_count == 0 {
        lines.push(Line::from(vec![Span::styled("◉ ", Style::default().fg(th.accent)), Span::styled("They don't know you yet. Tell them your name, what you work on and how you like answers; every agent will remember it (Alt+M adds facts by hand).", Style::default().fg(th.muted))]));
        lines.push(Line::from(""));
    }
    for (i, a) in app.agents.iter().enumerate() {
        let tags: Vec<String> = {
            let mut g: Vec<String> = vec![];
            for t in &a.tools {
                if let Some(info) = app.tools.iter().find(|x| &x.name == t) {
                    let l = if info.name == "run_shell" { "Shell".to_string() } else { clean(&info.group) };
                    if !g.contains(&l) {
                        g.push(l);
                    }
                }
            }
            if g.is_empty() { vec!["Chat only".into()] } else { g }
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{:>2} ", i + 1), Style::default().fg(th.faint)),
            Span::styled(format!("{} ", text::icon(&a.emoji)), Style::default()),
            Span::styled(clean(&a.name), Style::default().fg(agent_color(a)).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {}", clean(&a.purpose)), Style::default().fg(th.muted)),
            Span::styled(format!("  {}", tags.join(" · ")), Style::default().fg(th.faint)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(" n ", Style::default().fg(th.bg).bg(th.accent)), Span::styled(" or a number starts a chat · ", Style::default().fg(th.muted)), Span::styled(" Ctrl+K ", Style::default().fg(th.text).bg(th.surface3)), Span::styled(" finds anything · ", Style::default().fg(th.muted)), Span::styled(" F1 ", Style::default().fg(th.text).bg(th.surface3)), Span::styled(" every shortcut", Style::default().fg(th.muted))]));
    let recent: Vec<_> = app.chats.iter().filter(|c| c.parent.is_none()).take(5).cloned().collect();
    if !recent.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Pick up where you left off", Style::default().fg(th.muted).add_modifier(Modifier::BOLD))));
        for c in recent {
            let w = agent_who(&app.agents, &c.agent_id);
            lines.push(Line::from(vec![Span::styled(format!("   {} ", w.emoji), Style::default()), Span::styled(trace::one_line(&c.title, 60), Style::default().fg(th.text)), Span::styled(format!("  {} · {}", w.name, time_ago(c.updated)), Style::default().fg(th.faint))]));
        }
    }
    render(f, Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

// ------------------------------------------------------------------ files

fn draw_files(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.theme.clone();
    render(f, Block::default().style(Style::default().bg(th.surface)), area);
    for y in area.y..area.y + area.height {
        render(f, Paragraph::new(sep(&th)), Rect { x: area.x, y, width: 1, height: 1 });
    }
    let inner = Rect { x: area.x + 1, y: area.y, width: area.width.saturating_sub(1), height: area.height };
    let focused = app.focus == Focus::Files;
    let w = inner.width as usize;
    let put = |f: &mut Frame, y: u16, line: Line| render(f, Paragraph::new(line), Rect { x: inner.x, y, width: inner.width, height: 1 });
    let who = agent_who(&app.agents, &app.files.agent_id);
    put(f, inner.y, Line::from(vec![Span::styled(" Workspace ", Style::default().fg(th.text).add_modifier(Modifier::BOLD)), Span::styled(format!("{} {}", who.emoji, who.name), Style::default().fg(th.muted))]));
    put(f, inner.y + 1, Line::from(Span::styled(format!(" /{}", if app.files.path == "." { String::new() } else { clean(&app.files.path) }), Style::default().fg(th.faint))));
    let mut y = inner.y + 2;
    let list_h = inner.height.saturating_sub(4) as usize;
    let sel = app.files.sel;
    let start = sel.saturating_sub(list_h.saturating_sub(1));
    let rows: Vec<Line> = {
        let mut rows = vec![];
        let bg = |i: usize| if i == sel && focused { th.surface3 } else { th.surface };
        rows.push(Line::from(Span::styled(format!("{:<w$}", if app.files.path == "." { " ↻ refresh" } else { " ↰ .." }, w = w), Style::default().fg(th.muted).bg(bg(0)))));
        for (i, e) in app.files.entries.iter().enumerate() {
            let icon = if e.dir { "▸" } else { "·" };
            let meta = if e.dir { String::new() } else { format!(" {} ", human_size(e.size.unwrap_or(0))) };
            let name_w = w.saturating_sub(3 + meta.len());
            rows.push(Line::from(vec![
                Span::styled(format!(" {icon} "), Style::default().fg(if e.dir { th.accent } else { th.faint }).bg(bg(i + 1))),
                Span::styled(text::pad_cut(&clean(&e.name), name_w, true), Style::default().fg(if e.name.starts_with('.') { th.faint } else { th.text }).bg(bg(i + 1))),
                Span::styled(meta, Style::default().fg(th.faint).bg(bg(i + 1))),
            ]));
        }
        rows
    };
    for l in rows.into_iter().skip(start).take(list_h) {
        put(f, y, l);
        y += 1;
    }
    if app.files.entries.is_empty() {
        put(f, y, Line::from(Span::styled(" Empty folder.", Style::default().fg(th.muted))));
    }
    put(f, (inner.y + inner.height).saturating_sub(1), Line::from(Span::styled(format!(" {}", trace::one_line(&app.files.workspace, w.saturating_sub(2))), Style::default().fg(th.faint))));
}

pub fn human_size(n: u64) -> String {
    let kb = n as f64 / 1024.0;
    if n < 1024 {
        format!("{n} B")
    } else if (kb * 10.0).round() < 10240.0 {
        // (what would read "1024.0 KB" is "1.0 MB")
        format!("{kb:.1} KB")
    } else {
        format!("{:.1} MB", n as f64 / 1048576.0)
    }
}

// ----------------------------------------------------------------- toasts

/// Toasts stack down the top right of `area` (the trace), as many as fit, each at most a few
/// lines; a repeated one shows its count.
fn draw_toasts(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let mut y = area.y;
    for t in &app.toasts {
        let msg = if t.count > 1 { format!("{} ×{}", t.text, t.count) } else { t.text.clone() };
        let w = (text::width(&msg) as u16 + 4).min(area.width.saturating_sub(2));
        if w < 8 {
            break;
        }
        let p = Paragraph::new(Span::styled(msg, Style::default().fg(th.text))).wrap(Wrap { trim: true });
        let h = (p.line_count(w - 4) as u16).clamp(1, 4) + 2;
        if y + h > area.bottom() {
            break;
        }
        let rect = Rect { x: area.right() - w - 1, y, width: w, height: h };
        render(f, Clear, rect);
        let block = Block::default().borders(Borders::ALL).padding(Padding::horizontal(1)).border_style(Style::default().fg(if t.err { th.danger } else { th.accent })).style(Style::default().bg(th.surface2));
        let inner = block.inner(rect);
        render(f, block, rect);
        render(f, p, inner);
        y += h;
    }
}

// ---------------------------------------------------------------- overlays

fn dialog(f: &mut Frame, area: Rect, w: u16, h: u16, title: &str, th: &crate::theme::Theme) -> Rect {
    let w = w.min(area.width.saturating_sub(2)).max(20).min(area.width);
    let h = h.min(area.height.saturating_sub(2)).max(5).min(area.height);
    let rect = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    render(f, Clear, rect);
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(th.line)).title(Span::styled(format!(" {} ", clean(title)), Style::default().fg(th.text).add_modifier(Modifier::BOLD))).style(Style::default().bg(th.surface));
    let inner = block.inner(rect);
    render(f, block, rect);
    inner
}

/// The bottom row of `r` (for a dialog's key hints).
fn last_row(r: Rect) -> Rect {
    Rect { x: r.x, y: r.bottom().saturating_sub(1), width: r.width, height: r.height.min(1) }
}

fn draw_overlay(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.theme.clone();
    let Some(overlay) = &mut app.overlay else { return };
    match overlay {
        Overlay::Help { scroll } => {
            let rows: Vec<(&str, &str)> = vec![
                ("Ctrl+K", "search chats and commands (type / for the slash commands, chat or not)"), ("/ in the message box", "slash commands (/help lists them; // sends a message starting with /)"), ("n / Ctrl+N", "new chat (number picks the agent)"), ("Tab / Shift+Tab", "move between sidebar, trace, composer, files"),
                ("Enter", "send (queue while the agent works)"), ("Alt+Enter / Ctrl+J", "new line"), ("Ctrl+S / Esc", "stop the agent"),
                ("↑ in an empty box", "take queued messages back"), ("Ctrl+U", "attach a file"), ("Ctrl+E", "edit and resend your last message"),
                ("Ctrl+R", "regenerate the last reply / retry after an error"), ("Ctrl+G", "branch from the end"), ("Ctrl+L", "compact older messages"),
                ("Alt+Y / A / N", "approve · approve all for this run · deny (in the trace: y / a / n)"), ("Alt+O", "show thinking, results and tool details"), ("Alt+F", "workspace files"),
                ("Alt+B", "hide or show the sidebar"), ("Alt+M", "memory"), ("Alt+R", "routines"), ("Alt+S", "settings"), ("Alt+A", "edit the agent"),
                ("Alt+P", "pin or unpin the chat"), ("F2", "rename the chat"), ("Alt+D", "delete the chat"), ("Alt+X", "export as Markdown"),
                ("Ctrl+Y", "bypass permissions on/off"), ("In the trace: [ ]", "select a reply · c copy · r regenerate · b branch · o open the handoff chat"),
                ("In the sidebar", "j/k move · Enter open · / filter · d delete · r rename · p pin · e edit agent · m mark all read"),
                ("Ctrl+C", "close a dialog, clear the box or the filter; quit when there's nothing to clear"), ("Ctrl+Q", "quit"),
            ];
            // wide enough for the longest row where there's room; else each description wraps
            // under itself, and the list scrolls when it is taller than the screen
            let w = area.width.saturating_sub(2).min(114);
            let iw = w.saturating_sub(2) as usize;
            let kw = if iw >= 50 { 20 } else { iw / 3 };
            let mut lines: Vec<Line> = vec![];
            for (k, v) in &rows {
                let key = text::cut(k, kw);
                let key = Span::styled(format!("{}{key}  ", " ".repeat(kw - text::width(&key))), Style::default().fg(th.accent));
                // whole "k does this" items to a line where they fit, words where one doesn't
                let room = iw.saturating_sub(kw + 2);
                let mut parts: Vec<String> = vec![];
                for item in v.split(" · ") {
                    match parts.last_mut() {
                        Some(p) if text::width(p) + 3 + text::width(item) <= room => *p = format!("{p} · {item}"),
                        Some(p) => {
                            p.push_str(" ·");
                            parts.push(item.to_string());
                        }
                        None => parts.push(item.to_string()),
                    }
                }
                let pieces = parts.iter().flat_map(|p| trace::wrap_spans(&[Span::styled(p.clone(), Style::default().fg(th.text))], room));
                for (n, piece) in pieces.enumerate() {
                    let mut l = vec![if n == 0 { key.clone() } else { Span::raw(" ".repeat(kw + 2)) }];
                    l.extend(piece);
                    lines.push(Line::from(l));
                }
            }
            let inner = dialog(f, area, w, lines.len() as u16 + 3, "Keyboard shortcuts", &th);
            let h = inner.height.saturating_sub(1) as usize;
            *scroll = (*scroll).min(lines.len().saturating_sub(h));
            let total = lines.len();
            render(f, Paragraph::new(lines.into_iter().skip(*scroll).take(h).collect::<Vec<_>>()), Rect { height: h as u16, ..inner });
            let more = if total > h { format!(" ↑↓ PgUp PgDn scroll ({}–{} of {}) ·", *scroll + 1, (*scroll + h).min(total), total) } else { String::new() };
            render(f, Paragraph::new(Line::from(Span::styled(format!("{more} Esc close"), Style::default().fg(th.faint)))), last_row(inner));
        }
        Overlay::Confirm { title, text, ok, .. } => {
            let inner = dialog(f, area, 60, 7, title, &th);
            render(f, Paragraph::new(Span::styled(clean(text), Style::default().fg(th.muted))).wrap(Wrap { trim: true }), Rect { x: inner.x, y: inner.y, width: inner.width, height: inner.height.saturating_sub(2) });
            let l = Line::from(vec![Span::styled(" Enter ", Style::default().fg(th.bg).bg(th.accent)), Span::styled(format!(" {ok}   "), Style::default().fg(th.text)), Span::styled(" Esc ", Style::default().fg(th.text).bg(th.surface3)), Span::styled(" cancel", Style::default().fg(th.muted))]);
            render(f, Paragraph::new(l), last_row(inner));
        }
        Overlay::Prompt { title, value, cursor, .. } => {
            let inner = dialog(f, area, 70, 5, title, &th);
            let (shown, at) = crate::forms::line_window(value, *cursor, inner.width.saturating_sub(1) as usize, false);
            render(f, Paragraph::new(Line::from(Span::styled(format!(" {shown}"), Style::default().fg(th.text).bg(th.surface3)))).style(Style::default().bg(th.surface3)), Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 });
            f.set_cursor_position((inner.x + 1 + at, inner.y));
            render(f, Paragraph::new(Line::from(vec![Span::styled(" Enter ", Style::default().fg(th.bg).bg(th.accent)), Span::styled(" ok   ", Style::default().fg(th.text)), Span::styled(" Esc ", Style::default().fg(th.text).bg(th.surface3)), Span::styled(" cancel", Style::default().fg(th.muted))])), Rect { x: inner.x, y: inner.y + 2, width: inner.width, height: 1 });
        }
        Overlay::Pick { sel } => {
            let inner = dialog(f, area, 70, app.agents.len() as u16 + 4, "New chat with", &th);
            let mut lines = vec![];
            for (i, a) in app.agents.iter().enumerate() {
                let bg = if i == *sel { th.surface3 } else { th.surface };
                lines.push(Line::from(vec![
                    Span::styled(format!(" {} ", i + 1), Style::default().fg(th.faint).bg(bg)),
                    Span::styled(format!("{} ", text::icon(&a.emoji)), Style::default().bg(bg)),
                    Span::styled(format!("{} ", text::pad_cut(&clean(&a.name), 14, true)), Style::default().fg(agent_color(a)).bg(bg).add_modifier(Modifier::BOLD)),
                    Span::styled(trace::one_line(&a.purpose, (inner.width as usize).saturating_sub(22)), Style::default().fg(th.muted).bg(bg)),
                ]));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(" Enter or a number starts the chat · Esc closes", Style::default().fg(th.faint))));
            render(f, Paragraph::new(lines), inner);
        }
        Overlay::Palette { q, items, sel, .. } => {
            let h = (items.len() as u16 + 5).min(area.height.saturating_sub(4)).max(8);
            let inner = dialog(f, area, 76, h, "Search and commands", &th);
            let shown = clean(q);
            f.set_cursor_position((inner.x + 3 + text::width(&shown) as u16, inner.y));
            render(f, Paragraph::new(Line::from(vec![Span::styled(" ⌕ ", Style::default().fg(th.muted)), Span::styled(shown, Style::default().fg(th.text))])), Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 });
            let list_h = (inner.height.saturating_sub(3) as usize).max(1);
            // the rows: each group's header, then its items. Scrolled by rows (headers count too),
            // so the selection stays in view, with its header when it is its group's first item
            let mut rows: Vec<Option<usize>> = vec![]; // None: the header of the item after it
            for (i, it) in items.iter().enumerate() {
                if i == 0 || items[i - 1].group != it.group {
                    rows.push(None);
                }
                rows.push(Some(i));
            }
            let at = rows.iter().position(|r| *r == Some(*sel)).unwrap_or(0);
            let top = if at > 0 && rows[at - 1].is_none() && list_h > 1 { at - 1 } else { at };
            let start = (at + 1).saturating_sub(list_h).min(top);
            let header = |i: usize| Line::from(Span::styled(format!(" {}", items[i].group), Style::default().fg(th.faint)));
            let mut lines = vec![];
            for (k, r) in rows.iter().enumerate().skip(start) {
                if lines.len() >= list_h {
                    break;
                }
                let Some(i) = *r else {
                    lines.push(header(rows[k + 1].unwrap_or(0)));
                    continue;
                };
                // a list scrolled into a group starts with that group's name (in place of an item
                // above the selection)
                if k == start && k > 0 && i != *sel && rows.get(k + 1).is_some_and(|r| r.is_some()) {
                    lines.push(header(i));
                    continue;
                }
                let it = &items[i];
                let bg = if i == *sel { th.surface3 } else { th.surface };
                // titles in a column of up to 40 (display columns), what's left for the detail
                let iw = inner.width as usize;
                let tw = iw.saturating_sub(3).min(40);
                let sub = trace::one_line(&it.sub, iw.saturating_sub(3 + tw + 2));
                lines.push(Line::from(vec![
                    Span::styled(if i == *sel { " ▸ " } else { "   " }, Style::default().fg(th.accent).bg(bg)),
                    Span::styled(text::pad_cut(&clean(&it.title), tw, true), Style::default().fg(th.text).bg(bg)),
                    Span::styled(if sub.is_empty() { sub } else { format!("  {sub}") }, Style::default().fg(th.muted).bg(bg)),
                ]));
            }
            if items.is_empty() {
                lines.push(Line::from(Span::styled("  Nothing matches.", Style::default().fg(th.muted))));
            }
            render(f, Paragraph::new(lines), Rect { x: inner.x, y: inner.y + 1, width: inner.width, height: inner.height.saturating_sub(2) });
            render(f, Paragraph::new(Line::from(Span::styled(" ↑↓ move · Enter open · Esc close", Style::default().fg(th.faint)))), last_row(inner));
        }
        Overlay::Form { form, .. } => form.render(f, area, &th),
        Overlay::Viewer { title, lines, scroll } => {
            let inner = dialog(f, area, area.width.saturating_sub(6), area.height.saturating_sub(2), &format!("{title}  ({} lines)", lines.len()), &th);
            let h = inner.height.saturating_sub(1) as usize;
            // long lines wrap under their number instead of running off the edge
            let mut body: Vec<Line> = vec![];
            for (i, l) in lines.iter().enumerate().skip(*scroll) {
                for (k, piece) in text::chunks(&clean(l), (inner.width as usize).saturating_sub(5)).into_iter().enumerate() {
                    let num = if k == 0 { format!("{:>4} ", i + 1) } else { " ".repeat(5) };
                    body.push(Line::from(vec![Span::styled(num, Style::default().fg(th.faint)), Span::styled(piece.to_string(), Style::default().fg(th.code))]));
                }
                if body.len() >= h {
                    break;
                }
            }
            body.truncate(h);
            render(f, Paragraph::new(body), Rect { x: inner.x, y: inner.y, width: inner.width, height: inner.height.saturating_sub(1) });
            render(f, Paragraph::new(Line::from(Span::styled(" j/k PgUp/PgDn scroll · c copy · Esc close", Style::default().fg(th.faint)))), last_row(inner));
        }
        Overlay::Info { title, lines, scroll } => {
            let want = lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16 + 4;
            let p = Paragraph::new(lines.clone()).wrap(Wrap { trim: false });
            let inner = dialog(f, area, want.clamp(48, 110), lines.len() as u16 + 3, title, &th);
            let h = inner.height.saturating_sub(1);
            let total = p.line_count(inner.width) as u16;
            *scroll = (*scroll).min(total.saturating_sub(h) as usize);
            render(f, p.scroll((*scroll as u16, 0)), Rect { height: h, ..inner });
            let more = if total > h { format!(" ↑↓ scroll ({}–{} of {}) ·", *scroll + 1, (*scroll as u16 + h).min(total), total) } else { String::new() };
            render(f, Paragraph::new(Line::from(Span::styled(format!("{more} Esc close"), Style::default().fg(th.faint)))), last_row(inner));
        }
        Overlay::Models { agent, items, sel } => {
            let a = app.agents.iter().find(|a| a.id == *agent);
            let who = agent_who(&app.agents, agent);
            let current = a.map(|a| a.model.clone()).unwrap_or_default();
            let fallback = app.settings.get("model").and_then(|m| m.as_str()).filter(|m| !m.is_empty()).map(String::from).or_else(|| app.server.models.first().cloned()).unwrap_or_else(|| "the server's first model".into());
            let inner = dialog(f, area, 76, items.len() as u16 + 6, &format!("Model for {} {}", who.emoji, who.name), &th);
            let list_h = inner.height.saturating_sub(3) as usize;
            let start = sel.saturating_sub(list_h.saturating_sub(1));
            let w = inner.width as usize;
            let mut lines = vec![];
            for (i, m) in items.iter().enumerate().skip(start).take(list_h) {
                let bg = if i == *sel { th.surface3 } else { th.surface };
                let label = if m.is_empty() { format!("default · {}", clean(&fallback)) } else { clean(m) };
                lines.push(Line::from(vec![
                    Span::styled(if i == *sel { " ▸ " } else { "   " }, Style::default().fg(th.accent).bg(bg)),
                    Span::styled(if *m == current { "● " } else { "  " }, Style::default().fg(th.ok).bg(bg)),
                    Span::styled(trace::one_line(&label, w.saturating_sub(6)), Style::default().fg(th.text).bg(bg)),
                ]));
            }
            lines.push(Line::from(""));
            let note = if !app.server.ok { "The model server isn't reachable, so no models are listed." } else { "It changes the agent, so its other chats use it too." };
            lines.push(Line::from(Span::styled(format!(" {note}"), Style::default().fg(th.faint))));
            render(f, Paragraph::new(lines), Rect { height: inner.height.saturating_sub(1), ..inner });
            render(f, Paragraph::new(Line::from(Span::styled(" ↑↓ move · Enter use · Esc close  (or /model <name>)", Style::default().fg(th.faint)))), last_row(inner));
        }
        Overlay::Memory { data, cats, total, sel, q, cat, typing } => {
            let inner = dialog(f, area, 90, area.height.saturating_sub(4), &format!("Memory · {total} remembered"), &th);
            let catname = if *cat == 0 { "all kinds".to_string() } else { clean(&cats.get(*cat - 1).cloned().unwrap_or_default()) };
            render(f, Paragraph::new(Line::from(vec![
                Span::styled(" ⌕ ", Style::default().fg(th.muted)),
                Span::styled(if q.is_empty() && !*typing { "/ to search".to_string() } else { clean(q) }, Style::default().fg(if *typing { th.text } else { th.faint })),
                Span::styled(format!("   ◂ {catname} ▸  (Tab)"), Style::default().fg(th.muted)),
            ])), Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 });
            let list_h = inner.height.saturating_sub(3) as usize / 2;
            let start = sel.saturating_sub(list_h.saturating_sub(1));
            let mut lines = vec![];
            for (i, m) in data.iter().enumerate().skip(start).take(list_h) {
                let bg = if i == *sel { th.surface3 } else { th.surface };
                lines.push(Line::from(vec![
                    Span::styled(if m.pinned != 0 { " ⚲ " } else { "   " }, Style::default().fg(th.amber).bg(bg)),
                    Span::styled(trace::one_line(&m.fact, (inner.width as usize).saturating_sub(4)), Style::default().fg(th.text).bg(bg)),
                ]));
                let src = if m.source == "user" { "added by you".to_string() } else if m.source.starts_with("auto") { "learned automatically".into() } else if m.source.starts_with("tidy") { "merged by tidy-up".into() } else { format!("saved by {}", agent_who(&app.agents, m.source.trim_start_matches("agent:")).name) };
                lines.push(Line::from(Span::styled(clean(&format!("     {} · importance {} · {src} · used {}× · {}", m.category, m.importance, m.uses, time_ago(m.updated))), Style::default().fg(th.faint).bg(bg))));
            }
            if data.is_empty() {
                lines.push(Line::from(Span::styled(if *total == 0 { "  Nothing yet. Chat with your agents, or press a to add something." } else { "  Nothing matches." }, Style::default().fg(th.muted))));
            }
            render(f, Paragraph::new(lines), Rect { x: inner.x, y: inner.y + 1, width: inner.width, height: inner.height.saturating_sub(2) });
            render(f, Paragraph::new(Line::from(Span::styled(" a add · d delete · p pin · +/- importance · t tidy up · Esc close", Style::default().fg(th.faint)))), last_row(inner));
        }
        Overlay::Routines { items, sel } => {
            let inner = dialog(f, area, 90, (items.len() as u16 * 2 + 5).min(area.height.saturating_sub(4)).max(8), "Routines", &th);
            // two rows each, scrolled to keep the selected one in view
            let fit = (inner.height.saturating_sub(1) as usize / 2).max(1);
            let mut lines = vec![];
            for (i, r) in items.iter().enumerate().skip(sel.saturating_sub(fit - 1)).take(fit) {
                let bg = if i == *sel { th.surface3 } else { th.surface };
                let who = agent_who(&app.agents, &r.agent_id);
                lines.push(Line::from(vec![
                    Span::styled(if r.enabled { " [x] " } else { " [ ] " }, Style::default().fg(if r.enabled { th.accent } else { th.muted }).bg(bg)),
                    Span::styled(format!("{} ", who.emoji), Style::default().bg(bg)),
                    Span::styled(clean(&r.name), Style::default().fg(th.text).add_modifier(Modifier::BOLD).bg(bg)),
                ]));
                let next = if r.enabled { r.next_run.map(|t| format!("next {}", trace::clock(Some(t)))).unwrap_or("—".into()) } else { "off".into() };
                let last = r.last_run.map(|t| format!("last run {}{}", time_ago(t), r.last_status.as_ref().filter(|s| *s != "started").map(|s| format!(" ({s})")).unwrap_or_default())).unwrap_or("never run".into());
                lines.push(Line::from(Span::styled(clean(&format!("      {} · {} · {next} · {last}", schedule_text(&r.schedule), who.name)), Style::default().fg(th.faint).bg(bg))));
            }
            if items.is_empty() {
                lines.push(Line::from(Span::styled("  No routines yet. Press n to create one; the templates are a good start.", Style::default().fg(th.muted))));
            }
            render(f, Paragraph::new(lines), Rect { x: inner.x, y: inner.y, width: inner.width, height: inner.height.saturating_sub(1) });
            render(f, Paragraph::new(Line::from(Span::styled(" n new · Enter edit · Space on/off · r run now · o open its chat · d delete · Esc close", Style::default().fg(th.faint)))), last_row(inner));
        }
        Overlay::Toasts => {}
    }
    // approvals: a reminder line at the bottom when one is pending and no other overlay is up
    let _ = th;
}

pub fn schedule_text(s: &serde_json::Value) -> String {
    if s.get("type").and_then(|t| t.as_str()) == Some("interval") {
        let m = s.get("minutes").and_then(|m| m.as_u64()).unwrap_or(0);
        return if m % 60 == 0 { format!("every {} h", m / 60) } else { format!("every {m} min") };
    }
    let days: Vec<u64> = s.get("days").and_then(|d| d.as_array()).map(|a| a.iter().filter_map(|x| x.as_u64()).collect()).unwrap_or_default();
    let names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let d = match days.as_slice() {
        [0, 1, 2, 3, 4, 5, 6] => "every day".to_string(),
        [0, 1, 2, 3, 4] => "weekdays".to_string(),
        [5, 6] => "weekends".to_string(),
        _ => days.iter().filter_map(|d| names.get(*d as usize)).cloned().collect::<Vec<_>>().join(", "),
    };
    format!("{d} at {}", s.get("time").and_then(|t| t.as_str()).unwrap_or("?"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Agent, Api, ChatSummary, FileEntry, Memory, Parent, Routine, ServerInfo, ToolInfo, Upload};
    use crate::app::Act;
    use crate::theme::Theme;
    use crate::trace::{Trace, Who};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use serde_json::json;

    // every kind of hostile text: tabs, CR, CRLF, ANSI CSI and OSC, C1, BEL, plus emoji and CJK
    const NASTY: &str = "tab\there\r\nCR\rover \x1b[31mred\x1b[0m \x1b]0;title\x07 c1\u{9b}2J bel\x07 日本語 👋🏽 🧑‍💻";

    fn n(s: &str) -> String {
        format!("{s} {NASTY}")
    }

    fn app() -> App {
        let dir = std::env::temp_dir().join(format!("agent-chat-tui-ui-test-{}", std::process::id()));
        let mut app = App::new(Api::new("http://127.0.0.1:9"), Theme::dark(), dir.join("state.json"));
        app.agents = vec![
            Agent { id: "a1".into(), name: n("Agent"), emoji: "🤖\t".into(), color: "#3fb27f".into(), purpose: n("purpose"), system_prompt: n("prompt"), tools: vec!["run_shell".into()], ..Default::default() },
            Agent { id: "a2".into(), name: "日本語エージェント".into(), emoji: "🧭".into(), color: "#f2a541".into(), purpose: "short".into(), ..Default::default() },
        ];
        app.tools = vec![ToolInfo { name: "run_shell".into(), label: n("Run"), group: n("Shell"), danger: true }];
        app.settings = json!({ "model": n("model"), "base_url": n("http://x"), "bypass_approvals": true });
        app.server = ServerInfo { ok: false, models: vec![n("m1")], n_ctx: Some(8192), base_url: n("http://y"), ..Default::default() };
        app.chats = vec![
            ChatSummary { id: "c1".into(), title: n("Title"), agent_id: "a1".into(), updated: trace::now(), pinned: true, last_role: Some("assistant".into()), status: "running".into(), ..Default::default() },
            ChatSummary { id: "c2".into(), title: n("Sub"), agent_id: "a2".into(), updated: 1.0, parent: Some(Parent { root_chat_id: "c1".into(), caller_id: "a1".into() }), status: "idle".into(), ..Default::default() },
        ];
        let chat = json!({
            "id": "c1", "title": n("Title"), "agent_id": "a1",
            "compactions": [{ "upto": 0, "summary": n("summary"), "reason": "auto", "before_tokens": 9000, "after_tokens": 900 }],
            "messages": [
                { "role": "user", "content": n("hello"), "_ts": 1.0, "_images": [n("img.png")], "_recall": "a\nb" },
                { "role": "assistant", "content": "", "reasoning_content": n("thinking"), "tool_calls": [
                    { "id": "t1", "function": { "name": "run_shell", "arguments": json!({ "command": n("echo") }).to_string() } },
                    { "id": "t2", "function": { "name": "write_file", "arguments": json!({ "path": n("p"), "content": n("body") }).to_string() } },
                    { "id": "t3", "function": { "name": "ask_agent", "arguments": json!({ "agent": n("Coder"), "message": n("do") }).to_string() } },
                ] },
                { "role": "tool", "tool_call_id": "t1", "content": format!("exit code 0\n{NASTY}\n\tindented\rprogress 100%") },
                { "role": "tool", "tool_call_id": "t2", "content": n("Wrote") },
                { "role": "tool", "tool_call_id": "t3", "content": n("Error"), "_sub": { "agent_id": "a2", "chat_id": "c2", "messages": [{ "role": "assistant", "content": n("sub reply") }] } },
                { "role": "assistant", "content": format!("# Head {NASTY}\n\nPara &#27;[31m `code\t\x07`\n\n```{NASTY}\n\tcode {NASTY}\n```\n\n| a\tb | c |\n|---|---|\n| 日本 | {NASTY} |\n\n- item {NASTY}"), "_stats": { "tok_per_s": 30.0, "prompt_tokens": 1200, "completion_tokens": 50 } },
                { "role": "assistant", "content": "", "_error": n("boom") },
            ],
        });
        app.trace = Trace::from_chat(&chat, &app.agents, None);
        let ag = app.agents.clone();
        app.trace.start_turn(Who::from(ag.first(), "a1"), 7);
        app.trace.assistant_start(&[], &ag);
        app.trace.delta(&[], "reasoning", &n("live thinking"), None, None, &ag);
        app.trace.delta(&[], "content", &n("live text"), None, None, &ag);
        app.trace.delta(&[], "tool_args", &n("{\"command"), Some(0), Some(&n("tool")), &ag);
        app.trace.tool_start(&[], "t4", &n("run_shell"), json!({ "command": n("cmd") }), &ag);
        app.trace.approval(&[], "t4", Some("ap1"), None, &ag);
        app.trace.tool_progress(&[], "t4", &format!("{NASTY}\n{NASTY}\r50%"), &ag);
        app.trace.notice(&[], &n("notice"), &ag);
        app.trace.compact_start(&[], &ag);
        app.chat = Some(chat);
        app.chat_id = Some("c1".into());
        app.running = true;
        app.ctx_tokens = Some(4000);
        app.tok_per_s = Some(12.5);
        app.queue = vec![json!({ "content": n("queued") }), json!({ "content": "two" })];
        app.attachments = vec![Upload { name: n("file.txt"), path: "p".into(), size: 3, text: None, image: false }];
        app.files.entries = vec![FileEntry { name: n("weird.txt"), dir: false, size: Some(10), mtime: 0.0, path: "x".into() }, FileEntry { name: n("dir"), dir: true, size: None, mtime: 0.0, path: "d".into() }];
        app.files.workspace = n("/ws");
        app.files.path = n("sub");
        app.files.agent_id = "a1".into();
        app.search = n("q");
        app.memory_count = 3;
        app.composer.insert_str("typed\tline\nsecond 日本");
        app.toast(n("toast"), true);
        app.toast(n("toast"), true);
        app.toast(n("another toast that is quite a bit longer so it has to wrap over a few lines in a narrow trace"), false);
        app.cfg.verbose = true;
        app.rebuild_sidebar();
        app
    }

    fn overlays(app: &mut App) -> Vec<Option<Overlay>> {
        let mem = Memory { id: 1, fact: n("fact"), category: n("cat"), importance: 5, pinned: 1, source: "agent:a1".into(), uses: 2, updated: 1.0 };
        let routine = Routine { id: "r1".into(), name: n("routine"), agent_id: "a1".into(), prompt: n("p"), schedule: json!({ "type": "daily", "time": n("08:00"), "days": [0, 1] }), enabled: true, last_run: Some(1.0), last_status: Some(n("error")), next_run: Some(2.0), chat_id: None };
        app.open_palette();
        let palette = app.overlay.take();
        app.open_agent_editor(Some("a1".into()));
        let form = app.overlay.take();
        app.ctx_tokens = Some(4000);
        vec![
            Some(app.help_info()),
            Some(app.context_info()),
            Some(app.usage_info()),
            Some(app.status_info()),
            app.models_overlay(),
            None,
            Some(Overlay::Help { scroll: 0 }),
            palette,
            form,
            Some(Overlay::Confirm { title: n("Delete?"), text: n("text"), ok: "Delete".into(), act: Act::Tidy }),
            Some(Overlay::prompt(&n("Rename"), &n("value"), Act::Tidy)),
            Some(Overlay::Pick { sel: 1 }),
            Some(Overlay::Viewer { title: n("file"), lines: NASTY.split('\n').map(String::from).chain([n("x")]).collect(), scroll: 0 }),
            Some(Overlay::Memory { data: vec![mem], cats: vec![n("cat")], total: 1, sel: 0, q: n("q"), cat: 1, typing: true }),
            Some(Overlay::Routines { items: vec![routine], sel: 0 }),
        ]
    }

    #[test]
    fn draws_hostile_text_at_any_size_without_control_characters() {
        let sizes = [(1, 1), (2, 2), (5, 3), (20, 6), (30, 10), (40, 12), (60, 20), (89, 30), (90, 30), (120, 40), (200, 60), (300, 5), (6, 120)];
        let mut app = app();
        let mut overlay_list = overlays(&mut app);
        let mut saw_cjk = false;
        for state in 0..7 {
            match state {
                0 => app.focus = Focus::Composer,
                1 => app.focus = Focus::Sidebar,
                2 => {
                    app.focus = Focus::Messages;
                    app.follow = false;
                    app.scroll = 3;
                    app.selected_turn = Some(2);
                }
                3 => {
                    app.focus = Focus::Files;
                    app.cfg.files = true;
                }
                4 => {
                    app.chat_id = None; // the welcome page
                    app.server.ok = true;
                }
                5 => {
                    app.chat_id = Some("c2".into()); // watching a sub-agent's chat
                    if let Some(c) = &mut app.chat {
                        c["parent"] = json!({ "root_chat_id": "c1", "caller_id": "a1" });
                    }
                }
                _ => {
                    // the slash-command menu, scrolled down
                    app.chat_id = Some("c1".into());
                    if let Some(c) = app.chat.as_mut().and_then(|c| c.as_object_mut()) {
                        c.remove("parent");
                    }
                    app.focus = Focus::Composer;
                    app.set_composer("/");
                    app.menu_sel = 20;
                }
            }
            for o in overlay_list.iter_mut() {
                app.overlay = o.take();
                for (w, h) in sizes {
                    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
                    term.draw(|f| draw(f, &mut app)).unwrap();
                    let buf = term.backend().buffer();
                    for (i, cell) in buf.content.iter().enumerate() {
                        let sym = cell.symbol();
                        let row = || (0..w).map(|x| buf[(x, i as u16 / w)].symbol()).collect::<String>();
                        assert!(!sym.chars().any(char::is_control), "control character {sym:?} at cell {i} ({w}x{h}, state {state}, overlay {}): {:?}", app.overlay.is_some(), row());
                        saw_cjk |= sym == "日";
                    }
                }
                *o = app.overlay.take();
            }
        }
        assert!(saw_cjk, "wide characters should come through");
    }

    #[tokio::test]
    async fn the_wheel_scrolls_the_sidebar_and_narrow_panes_show_when_focused() {
        use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
        let mut app = app();
        app.overlay = None;
        app.search.clear();
        app.chats = (0..40).map(|i| ChatSummary { id: format!("x{i}"), title: format!("chat number {i}"), agent_id: "a2".into(), updated: trace::now(), status: "idle".into(), ..Default::default() }).collect();
        app.rebuild_sidebar();
        app.select_chat_row("x0");
        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let first_chat = |term: &mut Terminal<TestBackend>, app: &mut App| {
            term.draw(|f| draw(f, app)).unwrap();
            screen(term).iter().find_map(|r| r.find("chat number ").map(|i| r[i..].split_whitespace().nth(2).unwrap_or("").to_string())).unwrap_or_default()
        };
        assert_eq!(first_chat(&mut term, &mut app), "0");
        let wheel = |kind| Event::Mouse(MouseEvent { kind, column: 5, row: 20, modifiers: KeyModifiers::NONE });
        for _ in 0..3 {
            app.on_input(std::time::Instant::now(), wheel(MouseEventKind::ScrollDown), None).await;
        }
        // (the list's first row is the "Today" heading)
        assert_eq!(first_chat(&mut term, &mut app), "8", "three notches, three rows each; the selection stays where it is");
        app.on_input(std::time::Instant::now(), wheel(MouseEventKind::ScrollUp), None).await;
        assert_eq!(first_chat(&mut term, &mut app), "5");
        // moving the selection brings it back into view
        app.select_chat_row("x1");
        assert_eq!(first_chat(&mut term, &mut app), "1");
        // below 90 columns the drawer shows while it has the focus, like the sidebar
        app.cfg.files = true;
        app.focus = Focus::Files;
        let mut term = Terminal::new(TestBackend::new(80, 30)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        assert!(screen(&term).iter().any(|r| r.contains("Workspace")), "{}", screen(&term).join("\n"));
        assert!(screen(&term).iter().any(|r| r.contains("weird.txt") && r.ends_with(" 10 B ")), "a cut name keeps a space before the size:\n{}", screen(&term).join("\n"));
        assert_eq!(app.drawn_cols, (0, 32));
        app.focus = Focus::Composer;
        term.draw(|f| draw(f, &mut app)).unwrap();
        assert!(!screen(&term).iter().any(|r| r.contains("Workspace")));
        assert_eq!(app.drawn_cols, (0, 0));
    }

    #[test]
    fn the_approval_question_names_its_keys_and_fits() {
        let mut app = app();
        app.overlay = None;
        app.toasts.clear();
        app.cfg.verbose = false;
        for w in [140, 90] {
            let mut term = Terminal::new(TestBackend::new(w, 60)).unwrap();
            term.draw(|f| draw(f, &mut app)).unwrap();
            let all = screen(&term).join("\n");
            for needle in ["Let the agent do this?", "Alt+Y approve", "Alt+N deny"] {
                assert!(all.contains(needle), "{needle:?} at {w} columns:\n{all}");
            }
        }
    }

    #[test]
    fn a_long_routine_list_scrolls_to_the_selection() {
        let mut app = app();
        let items: Vec<Routine> = (0..15).map(|i| Routine { id: format!("r{i}"), name: format!("Routine number {i}"), agent_id: "a1".into(), prompt: "p".into(), schedule: json!({ "type": "interval", "minutes": 30 }), enabled: true, ..Default::default() }).collect();
        for (sel, shown) in [(0, "Routine number 0"), (14, "Routine number 14"), (7, "Routine number 7")] {
            app.overlay = Some(Overlay::Routines { items: items.clone(), sel });
            let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
            term.draw(|f| draw(f, &mut app)).unwrap();
            let rows = screen(&term);
            assert!(rows.iter().any(|r| r.contains(shown)), "{shown} should be in view:\n{}", rows.join("\n"));
        }
    }

    fn screen(term: &Terminal<TestBackend>) -> Vec<String> {
        let buf = term.backend().buffer();
        let (w, h) = (buf.area.width, buf.area.height);
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>()).collect()
    }

    #[test]
    fn the_command_menu_sits_right_above_the_message_box() {
        let mut app = app();
        app.queue.clear();
        app.attachments.clear();
        app.toasts.clear();
        app.running = false;
        app.focus = Focus::Composer;
        app.set_composer("/re");
        for (w, h, wide) in [(120, 40, true), (44, 20, false)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw(f, &mut app)).unwrap();
            let rows = screen(&term);
            let at = |needle: &str| rows.iter().position(|r| r.contains(needle)).unwrap_or_else(|| panic!("{needle:?} not on the {w}x{h} screen:\n{}", rows.join("\n")));
            let top = at(" commands ");
            let bottom = at("Tab complete");
            assert_eq!(bottom - top, 5, "four commands match /re");
            for (i, name) in ["/resume", "/rename", "/retry", "/remember"].iter().enumerate() {
                assert!(rows[top + 1 + i].contains(name), "{name} on row {}: {:?}", top + 1 + i, rows[top + 1 + i]);
            }
            assert!(rows[top + 1].contains(" ▸ /resume"), "the first is selected");
            assert!(rows[top + 2].contains("[title]"), "the args hint");
            // the message box's top border is the very next row, and the box still has the text
            assert!(rows[bottom + 1].contains('┌'), "{:?}", rows[bottom + 1]);
            assert!(rows[bottom + 2].contains("/re"), "{:?}", rows[bottom + 2]);
            assert!(!wide || rows.iter().any(|r| r.contains("Enter runs /resume")), "Enter runs the menu's pick");
            if std::env::var("SHOW_MENU").is_ok() {
                eprintln!("{}", rows.join("\n"));
            }
            let desc = "Save a fact to the memory every agent shares";
            assert_eq!(rows[top + 4].contains(desc), wide, "the whole description where it fits: {:?}", rows[top + 4]);
            assert!(wide || rows[top + 4].contains("Save a fact to the …"), "cut short where it doesn't: {:?}", rows[top + 4]);
            assert!(rows[bottom].contains("Esc"), "the key hints fit: {:?}", rows[bottom]);
        }
        // an alias shows next to the name when it is what matched
        app.set_composer("/st");
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let rows = screen(&term);
        assert!(rows.iter().any(|r| r.contains(" ▸ /stop ")), "{}", rows.join("\n"));
        assert!(rows.iter().any(|r| r.contains("   /usage /stats ")));
        assert!(rows.iter().any(|r| r.contains("   /status ")));
        // every command: eight rows and a position counter; scrolled to keep the selection in view
        app.set_composer("/");
        assert!(app.menu().is_some()); // a new word puts the selection back on top; then move it
        app.menu_sel = 12;
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let rows = screen(&term);
        let top = rows.iter().position(|r| r.contains(" commands ")).unwrap();
        assert!(rows[top].contains(&format!(" 13/{} ", commands::COMMANDS.len())), "{:?}", rows[top]);
        assert!(rows[top + MENU_ROWS + 1].contains("Tab complete"));
        assert!(rows[top + MENU_ROWS].contains(" ▸ /delete"), "the selection is the bottom row: {:?}", rows[top + MENU_ROWS]);
        // a complete command says Enter runs it
        app.set_composer("/compact keep the notes");
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let rows = screen(&term);
        assert!(!rows.iter().any(|r| r.contains(" commands ")), "past the first word the menu is closed");
        assert!(rows.iter().any(|r| r.contains("Enter runs /compact") && r.contains("Run ▸")));
    }

    /// An app with ordinary text in it: four agents (one with ✍️, whose width terminals disagree
    /// on), a chat each, the first one open with a reply.
    fn plain_app() -> App {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("agent-chat-tui-ui-plain-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        let mut app = App::new(Api::new("http://127.0.0.1:9"), Theme::dark(), dir.join("state.json"));
        app.agents = vec![
            Agent { id: "w".into(), name: "Writer".into(), emoji: "✍\u{FE0F}".into(), color: "#e05d8c".into(), purpose: "Drafts text".into(), ..Default::default() },
            Agent { id: "j".into(), name: "日本語エージェント".into(), emoji: "🧭".into(), color: "#f2a541".into(), purpose: "日本語で答える".into(), ..Default::default() },
            Agent { id: "c".into(), name: "Coder".into(), emoji: "💻".into(), color: "#3fb27f".into(), purpose: "Writes code".into(), ..Default::default() },
            Agent { id: "k".into(), name: "Kin".into(), emoji: "👨\u{200D}👩\u{200D}👧".into(), color: "#7c6cff".into(), purpose: "Family".into(), ..Default::default() },
        ];
        app.chats = ["w", "j", "c", "k"].iter().enumerate().map(|(i, a)| ChatSummary { id: format!("c{i}"), title: ["✍\u{FE0F} notes", "中文のタイトルがとても長いチャット", "plain title", "🇦🇪 1\u{FE0F}\u{20E3} 👋\u{1F3FD} trip"][i].into(), agent_id: a.to_string(), updated: trace::now(), status: "running".into(), ..Default::default() }).collect();
        let chat = json!({ "id": "c0", "title": "✍\u{FE0F} notes", "agent_id": "w", "messages": [
            { "role": "user", "content": "hello ✍\u{FE0F} 👨\u{200D}👩\u{200D}👧 🇦🇪" },
            { "role": "assistant", "content": "A reply with **bold** and ✍\u{FE0F}.", "_stats": { "tok_per_s": 33.3, "prompt_tokens": 1200, "completion_tokens": 50 } },
        ] });
        app.trace = Trace::from_chat(&chat, &app.agents, None);
        app.chat = Some(chat);
        app.chat_id = Some("c0".into());
        app.server = ServerInfo { ok: true, models: vec!["m".into()], n_ctx: Some(85000), ..Default::default() };
        app.ctx_tokens = Some(30000);
        app.tok_per_s = Some(33.3);
        app.cfg.sidebar = true;
        app.cfg.files = false;
        app.focus = Focus::Composer;
        app.rebuild_sidebar();
        app
    }

    fn shot(app: &mut App, w: u16, h: u16) -> (Terminal<TestBackend>, Vec<String>) {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let rows = screen(&term);
        (term, rows)
    }

    /// The column (cell) where `needle` starts on row `y`.
    fn col_of(term: &Terminal<TestBackend>, y: u16, needle: &str) -> Option<u16> {
        let buf = term.backend().buffer();
        let first = needle.chars().next()?.to_string();
        (0..buf.area.width).find(|&x| {
            let mut cx = x;
            needle.chars().all(|c| {
                let ok = cx < buf.area.width && buf[(cx, y)].symbol() == c.to_string();
                cx += text::width(&c.to_string()).max(1) as u16;
                ok
            })
        }).filter(|&x| buf[(x, y)].symbol() == first)
    }

    #[test]
    fn the_trace_keeps_its_room_beside_both_panes() {
        let mut app = plain_app();
        app.cfg.files = true;
        app.files.agent_id = "c".into();
        app.files.entries = (0..30).map(|i| FileEntry { name: format!("file number {i} with a long name.txt"), dir: false, size: Some(1000 * i), mtime: 0.0, path: "x".into() }).collect();
        for w in 85..=100u16 {
            for h in (10..=40).step_by(6) {
                let (term, _) = shot(&mut app, w, h);
                let (side, files) = app.drawn_cols;
                let main = w - side - files;
                if w >= 90 {
                    assert!(side == 30 && files >= 30 && main >= 30, "{w}x{h}: {side} | {main} | {files}");
                    assert_eq!(files, 32.min(w - 60));
                } else {
                    assert_eq!((side, files), (0, 0), "below 90 only a focused side pane shows");
                }
                // every pane stays on its side of its separator
                let buf = term.backend().buffer();
                for y in 0..h {
                    if side > 0 {
                        assert_eq!(buf[(side - 1, y)].symbol(), "│", "{w}x{h} row {y}");
                    }
                    if files > 0 {
                        assert_eq!(buf[(w - files, y)].symbol(), "│", "{w}x{h} row {y}");
                    }
                }
            }
        }
        let (term, rows) = shot(&mut app, 120, 40);
        assert_eq!(app.drawn_cols, (30, 32));
        let buf = term.backend().buffer();
        assert!((0..40).all(|y| buf[(29, y)].symbol() == "│" && buf[(88, y)].symbol() == "│"), "{}", rows.join("\n"));
    }

    #[tokio::test]
    async fn the_help_wraps_its_rows_and_scrolls_on_a_short_screen() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let mut app = plain_app();
        app.overlay = Some(Overlay::Help { scroll: 0 });
        let has = |rows: &[String], n: &str| rows.iter().any(|r| r.contains(n));
        let (_, rows) = shot(&mut app, 120, 40);
        for needle in ["Keyboard shortcuts", "o open the handoff chat", "m mark all read", "Ctrl+Q", "Esc close"] {
            assert!(has(&rows, needle), "{needle:?} at 120x40:\n{}", rows.join("\n"));
        }
        assert!(!has(&rows, "scroll ("), "everything fits");
        // 24 rows: the list scrolls, and says so
        let (_, rows) = shot(&mut app, 100, 24);
        assert!(has(&rows, "search chats and commands") && !has(&rows, "Ctrl+Q") && has(&rows, "scroll (1–"), "{}", rows.join("\n"));
        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        app.on_input(std::time::Instant::now(), key(KeyCode::End), None).await;
        let (_, rows) = shot(&mut app, 100, 24);
        assert!(has(&rows, "Ctrl+Q") && has(&rows, "e edit agent · m mark all read") && !has(&rows, "search chats and commands"), "{}", rows.join("\n"));
        app.on_input(std::time::Instant::now(), key(KeyCode::Home), None).await;
        app.on_input(std::time::Instant::now(), key(KeyCode::Down), None).await;
        let (_, rows) = shot(&mut app, 100, 24);
        assert!(!has(&rows, "search chats and commands") && has(&rows, "scroll (2–"), "one row down:\n{}", rows.join("\n"));
        // narrow: long rows wrap under their description instead of being cut off
        let (_, rows) = shot(&mut app, 50, 60);
        assert!(has(&rows, "Keyboard shortcuts") && has(&rows, "m mark all read"), "{}", rows.join("\n"));
        app.on_input(std::time::Instant::now(), key(KeyCode::Char('x')), None).await;
        assert!(matches!(app.overlay, Some(Overlay::Help { .. })), "other keys keep it open");
        app.on_input(std::time::Instant::now(), key(KeyCode::Esc), None).await;
        assert!(app.overlay.is_none());
    }

    #[test]
    fn a_wide_title_leaves_the_header_stats_in_view() {
        let mut app = plain_app();
        app.cfg.sidebar = false;
        app.chat.as_mut().unwrap()["title"] = json!(format!("{}✍\u{FE0F}", "测试".repeat(40)));
        for (w, bar) in [(140, true), (90, true), (60, true), (40, false)] {
            let (term, rows) = shot(&mut app, w, 20);
            assert!(rows[0].contains("ctx 30k/85k"), "{w}: {:?}", rows[0]);
            assert_eq!(rows[0].contains("tok/s"), w >= 60, "{w}: {:?}", rows[0]);
            assert_eq!(rows[0].contains('▮'), bar, "the bar gives way first: {:?}", rows[0]);
            assert!(rows[0].contains('测') && rows[0].contains('…'), "{w}: {:?}", rows[0]);
            // nothing pushed past the edge: the stats end the row
            let end = col_of(&term, 0, if w >= 60 { "tok/s" } else { "85k" }).unwrap();
            assert!(end as usize + if w >= 60 { 5 } else { 3 } <= w as usize - 2, "{w}: {:?}", rows[0]);
        }
    }

    #[test]
    fn names_titles_and_icons_line_up_by_columns() {
        let mut app = plain_app();
        app.focus = Focus::Sidebar;
        let (term, rows) = shot(&mut app, 120, 40);
        // the agents: every name starts in one column whatever the icon, and so does "working"
        let names: Vec<u16> = ["Writer", "日本語", "Coder", "Kin"].iter().map(|n| (0..40).find_map(|y| col_of(&term, y, n).filter(|&x| x < 30)).unwrap_or_else(|| panic!("{n}:\n{}", rows.join("\n")))).collect();
        assert_eq!(names, vec![4; 4], "{}", rows.join("\n"));
        let working: Vec<u16> = (0..40).filter_map(|y| col_of(&term, y, "working").filter(|&x| x < 30)).collect();
        assert_eq!(working, vec![working[0]; 5], "All chats and the four agents:\n{}", rows.join("\n"));
        // the chats: the status dot keeps its column, whatever the title is written in
        let dots: Vec<u16> = (0..40).filter_map(|y| col_of(&term, y, "●").filter(|&x| x < 30 && x > 20)).collect();
        assert_eq!(dots.len(), 4, "{}", rows.join("\n"));
        assert!(dots.iter().all(|&d| d == 27), "{dots:?}\n{}", rows.join("\n"));
        // the palette's second column, and the new-chat picker's purposes
        app.open_palette();
        let (term, rows) = shot(&mut app, 120, 40);
        let subs: Vec<u16> = ["Writer ·", "日本語エージェント ·", "Coder ·", "Kin ·"].iter().map(|n| (2..40).find_map(|y| col_of(&term, y, n)).unwrap_or_else(|| panic!("{n}:\n{}", rows.join("\n")))).collect();
        assert!(subs.iter().all(|&c| c == subs[0]), "{subs:?}\n{}", rows.join("\n"));
        app.overlay = Some(Overlay::Pick { sel: 0 });
        let (term, rows) = shot(&mut app, 120, 40);
        let purposes: Vec<u16> = ["Drafts text", "日本語で答える", "Writes code", "Family"].iter().map(|n| (2..40).find_map(|y| col_of(&term, y, n)).unwrap_or_else(|| panic!("{n}:\n{}", rows.join("\n")))).collect();
        assert!(purposes.iter().all(|&c| c == purposes[0]), "{purposes:?}\n{}", rows.join("\n"));
    }

    #[test]
    fn every_cell_measures_the_same_to_ratatui_and_the_terminal() {
        // the emoji ratatui counts two and a terminal one (or six), wherever they are drawn:
        // icons, names, titles, messages, the message box (which draws what it holds)
        let mut app = plain_app();
        app.set_composer("typed ✍\u{FE0F} 👨\u{200D}👩\u{200D}👧 🇦🇪 1\u{FE0F}\u{20E3}");
        for (focus, overlay) in [(Focus::Composer, None), (Focus::Sidebar, None), (Focus::Composer, Some(Overlay::Pick { sel: 1 }))] {
            app.focus = focus;
            app.overlay = overlay;
            for w in [60, 89, 120] {
                let mut term = Terminal::new(TestBackend::new(w, 30)).unwrap();
                term.draw(|f| {
                    draw(f, &mut app);
                    text::scrub(f.buffer_mut());
                }).unwrap();
                for cell in &term.backend().buffer().content {
                    let sym = cell.symbol();
                    assert_eq!(Span::raw(sym).width(), text::width(sym), "{sym:?} ({}) measures differently per character", sym.escape_unicode());
                }
            }
        }
    }

    #[test]
    fn the_send_button_stays_on_screen() {
        let mut app = plain_app();
        app.cfg.sidebar = false;
        for w in [14, 20, 30, 44, 60, 120] {
            for running in [false, true] {
                app.running = running;
                let (_, rows) = shot(&mut app, w, 16);
                let button = if running { " Stop ■ " } else { " Send ↑ " };
                let bar = rows.iter().find(|r| r.contains(button.trim())).unwrap_or_else(|| panic!("{button:?} at {w}:\n{}", rows.join("\n")));
                assert!(bar.trim_end_matches('│').trim_end().ends_with(button.trim()), "the button at the right edge, whole: {bar:?}");
                assert!(!bar.contains("h…") && !bar.contains("F1 h "), "the hint drops whole items: {bar:?}");
            }
        }
    }

    #[test]
    fn no_size_or_state_panics() {
        // every overlay, a queue of 0, 3 and 5, attachments on and off, long toasts, the panes
        // on and off, at sizes from one cell up, and the small ones that once crashed
        let mut app = app();
        let mut overlay_list = overlays(&mut app);
        let mut sizes: Vec<(u16, u16)> = (1..=130).step_by(9).flat_map(|w| (1..=50).step_by(7).map(move |h| (w, h))).collect();
        sizes.extend([(18, 12), (30, 4), (10, 3), (36, 20), (80, 8), (60, 10), (40, 20), (22, 12), (8, 10), (5, 30), (90, 30), (91, 30), (95, 30)]);
        sizes.extend((1..=9).map(|h| (100, h)));
        for state in 0..3 {
            match state {
                0 => {
                    app.queue.clear();
                    app.attachments.clear();
                    for i in 0..4 {
                        app.toast(format!("{i} {}", "a long error message that has to wrap ".repeat(5)), true);
                    }
                }
                1 => {
                    app.queue = (0..3).map(|i| json!({ "content": format!("queued {i}") })).collect();
                    app.cfg.files = true;
                    app.cfg.sidebar = false;
                }
                _ => {
                    app.queue = (0..5).map(|i| json!({ "content": format!("queued {i}") })).collect();
                    app.attachments = vec![Upload { name: "a.png".into(), path: "p".into(), size: 3, text: None, image: true }];
                    app.cfg.sidebar = true;
                    app.focus = Focus::Files;
                }
            }
            for o in overlay_list.iter_mut() {
                app.overlay = o.take();
                for &(w, h) in &sizes {
                    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
                    term.draw(|f| draw(f, &mut app)).unwrap();
                }
                *o = app.overlay.take();
            }
        }
    }

    #[test]
    fn the_viewer_wraps_long_lines_under_their_number() {
        let mut app = plain_app();
        let long = format!("{}END", "ab".repeat(100));
        app.overlay = Some(Overlay::Viewer { title: "f.txt".into(), lines: vec!["short".into(), long, "日本語".repeat(30), "last".into()], scroll: 0 });
        let (_, rows) = shot(&mut app, 60, 30);
        let all = rows.join("\n");
        let wrapped: String = rows.iter().filter(|r| r.contains("   2 ab") || r.contains("│     ")).map(|r| r.as_str()).collect();
        assert_eq!(wrapped.matches('b').count(), 100, "every piece of the long line is on screen:\n{all}");
        assert!(all.contains("END") && all.contains("   4 last"), "{all}");
        let first = rows.iter().position(|r| r.contains("   2 abab")).unwrap();
        assert!(rows[first + 1].contains("│     bab"), "a continuation has no number: {:?}", rows[first + 1]);
        // scrolled to the last line, it is the first one shown
        app.overlay = Some(Overlay::Viewer { title: "f.txt".into(), lines: vec!["a".into(), "b".into()], scroll: 1 });
        let (_, rows) = shot(&mut app, 60, 30);
        assert!(rows.iter().any(|r| r.contains("   2 b")) && !rows.iter().any(|r| r.contains("   1 a")));
    }

    #[test]
    fn the_header_meters_context_and_names_who_a_sub_agent_works_for() {
        let mut app = plain_app();
        app.cfg.sidebar = false;
        let th = app.theme.clone();
        for (tokens, filled, color) in [(1, 1, th.ok), (59000, 8, th.ok), (59500, 8, th.amber), (76500, 11, th.danger), (85000, 12, th.danger)] {
            app.ctx_tokens = Some(tokens);
            let (term, rows) = shot(&mut app, 120, 20);
            let buf = term.backend().buffer();
            let cells: Vec<u16> = (0..120).filter(|&x| buf[(x, 0)].symbol() == "▮").collect();
            assert_eq!(cells.len(), filled, "{tokens}: {:?}", rows[0]);
            assert!(cells.iter().all(|&x| buf[(x, 0)].fg == color), "{tokens}");
            assert_eq!(rows[0].matches('▯').count(), 12 - filled);
        }
        app.server.n_ctx = None;
        let (_, rows) = shot(&mut app, 120, 20);
        assert!(rows[0].contains(" ctx 85k") && !rows[0].contains('▮') && !rows[0].contains('▯'), "no bar without a window size: {:?}", rows[0]);
        assert!(rows[1].contains("Writer · Drafts text"));
        app.chat.as_mut().unwrap()["parent"] = json!({ "root_chat_id": "c1", "caller_id": "c" });
        let (_, rows) = shot(&mut app, 120, 20);
        assert!(rows[1].contains("Writer · working for Coder"), "{:?}", rows[1]);
    }

    #[test]
    fn the_trace_ends_with_how_often_the_chat_was_compacted_and_how_long_it_has_gone() {
        let mut app = plain_app();
        app.cfg.sidebar = false;
        let now = trace::now();
        let c = app.chat.as_mut().unwrap();
        c["created"] = json!(now - 11_530.0);
        c["compactions"] = json!([{ "upto": 0 }, { "upto": 1 }]);
        let foot = "compacted 2× · going for 3h 12m";
        let at = |rows: &[String]| rows.iter().position(|r| r.contains(foot));
        // one quiet line after the last turn, a blank one between, nothing under it
        let (term, rows) = shot(&mut app, 100, 30);
        let y = at(&rows).unwrap_or_else(|| panic!("no footer:\n{}", rows.join("\n")));
        assert!(rows[y - 1].trim().is_empty() && rows[y - 2].contains("A reply with"), "{}", rows.join("\n"));
        assert!(rows[y + 1..].iter().take_while(|r| !r.contains('╭') && !r.contains('┌')).all(|r| r.trim().is_empty()), "{}", rows.join("\n"));
        let x = col_of(&term, y as u16, "compacted").unwrap();
        assert_eq!(x, 1, "in the trace's first column");
        assert_eq!(term.backend().buffer()[(x, y as u16)].fg, app.theme.faint, "the trace's meta style");
        // under a running turn
        app.running = true;
        let ag = app.agents.clone();
        app.trace.start_turn(Who::from(ag.first(), "w"), 2);
        app.trace.assistant_start(&[], &ag);
        app.trace.delta(&[], "content", "streaming now", None, None, &ag);
        app.touch();
        let (_, rows) = shot(&mut app, 100, 30);
        let y = at(&rows).unwrap_or_else(|| panic!("no footer under the running turn:\n{}", rows.join("\n")));
        let live = rows.iter().position(|r| r.contains("streaming now")).unwrap();
        assert!(live < y && rows[y - 2].contains('▍'), "{}", rows.join("\n"));
        // a long chat: following, the footer is the trace's last row; scrolled up, it is below
        for i in 0..30 {
            app.trace.entries.push(trace::Entry::User(trace::user_from_event(&json!({ "role": "user", "content": format!("message {i}") }), 3 + i)));
        }
        app.touch();
        let (_, rows) = shot(&mut app, 100, 30);
        let y = at(&rows).expect("the footer in view while following");
        assert!(rows[y + 1].contains('╭') || rows[y + 1].contains('┌'), "right above the message box: {}", rows.join("\n"));
        app.follow = false;
        app.scroll = 0;
        let (_, rows) = shot(&mut app, 100, 30);
        assert!(at(&rows).is_none() && rows.iter().any(|r| r.contains("more lines · End for newest")));
        // cut to a narrow trace, never wrapped
        app.follow = true;
        let (_, rows) = shot(&mut app, 24, 30);
        assert!(rows.iter().any(|r| r.contains("compacted 2× · going") && r.trim_end().ends_with('…') && text::width(r.trim_end()) <= 24), "{}", rows.join("\n"));
        // nothing before the chat has a message (or has loaded)
        for chat in [json!({ "id": "c0", "title": "t", "agent_id": "w", "created": now, "messages": [] }), json!({ "id": "c0", "title": "t", "agent_id": "w" })] {
            app.chat = Some(chat);
            app.trace = Trace::empty();
            app.running = false;
            app.touch();
            let (_, rows) = shot(&mut app, 100, 30);
            assert!(!rows.iter().any(|r| r.contains("going for") || r.contains("compacted")), "{}", rows.join("\n"));
        }
    }

    #[test]
    fn an_empty_chat_says_who_is_ready() {
        let mut app = plain_app();
        app.trace = Trace::empty();
        for (agent, purpose, want) in [("w", "Drafts text", "✍ Writer is ready. Its job: Drafts text."), ("w", "", "✍ Writer is ready."), ("gone", "", "Ready.")] {
            if let Some(a) = app.agents.iter_mut().find(|a| a.id == "w") {
                a.purpose = purpose.into();
            }
            app.chat = Some(json!({ "id": "c0", "title": "t", "agent_id": agent, "messages": [] }));
            let (_, rows) = shot(&mut app, 120, 20);
            assert!(rows.iter().any(|r| r.contains(want)), "{want:?}:\n{}", rows.join("\n"));
        }
    }

    use serde_json::Value;

    fn has(rows: &[String], needle: &str) -> bool {
        rows.iter().any(|r| r.contains(needle))
    }

    #[track_caller]
    fn shows(rows: &[String], needles: &[&str]) {
        for n in needles {
            assert!(has(rows, n), "{n:?} not on screen:\n{}", rows.join("\n"));
        }
    }

    #[test]
    fn the_welcome_page_numbers_the_agents_and_offers_recent_chats() {
        let mut app = plain_app();
        app.chat_id = None;
        app.chat = None;
        app.memory_count = 0;
        let (_, rows) = shot(&mut app, 130, 40);
        shows(&rows, &[
            "Local · m · 4 agents · 85k context", "Pick an agent.", "Watch every step.", "They don't know you yet.",
            " 1 ✍  Writer  Drafts text  Chat only", "Coder  Writes code  Chat only", " n  or a number starts a chat ·", "Ctrl+K  finds anything",
            "Pick up where you left off", "plain title  Coder · just now",
        ]);
        assert!(!has(&rows, "isn't reachable"));
        // a server that is down says where it was looked for; known facts drop the hint
        app.server.ok = false;
        app.settings = json!({ "base_url": "http://127.0.0.1:8080/v1" });
        app.memory_count = 3;
        let (_, rows) = shot(&mut app, 130, 40);
        shows(&rows, &["Model server offline · 4 agents", "✖ The model server isn't reachable at http://127.0.0.1:8080/v1."]);
        assert!(!has(&rows, "They don't know you yet"));
        // an agent's tools show as their groups, shell first by name
        app.tools = vec![ToolInfo { name: "run_shell".into(), label: "Run".into(), group: "System".into(), danger: true }, ToolInfo { name: "read_file".into(), label: "Read".into(), group: "Files".into(), danger: false }];
        app.agents[2].tools = vec!["run_shell".into(), "read_file".into(), "ask_agent".into()];
        let (_, rows) = shot(&mut app, 130, 40);
        shows(&rows, &["Coder  Writes code  Shell · Files"]);
    }

    #[test]
    fn dialogs_draw_their_title_text_and_keys() {
        let mut app = plain_app();
        app.cfg.sidebar = false;
        app.overlay = Some(Overlay::Confirm { title: "Delete “plain title”?".into(), text: "Its messages will be removed.".into(), ok: "Delete".into(), act: Act::Tidy });
        let (_, rows) = shot(&mut app, 100, 30);
        shows(&rows, &[" Delete “plain title”? ", "Its messages will be removed.", " Enter  Delete", "Esc  cancel"]);
        // a prompt: its value, the cursor after it
        app.overlay = Some(Overlay::prompt("Rename chat", "new name", Act::Tidy));
        let (mut term, rows) = shot(&mut app, 100, 30);
        shows(&rows, &[" Rename chat ", " new name", "Enter  ok", "Esc  cancel"]);
        let y = rows.iter().position(|r| r.contains(" new name")).unwrap() as u16;
        let x = col_of(&term, y, "new name").unwrap();
        term.backend_mut().assert_cursor_position((x + 8, y));
        // the agent picker: numbered, the selected row marked
        app.overlay = Some(Overlay::Pick { sel: 2 });
        let (term, rows) = shot(&mut app, 100, 30);
        shows(&rows, &[" New chat with ", " 1 ✍  Writer", "Drafts text", " 3 ", "Coder", "Writes code", "Enter or a number starts the chat · Esc closes"]);
        let y = rows.iter().position(|r| r.contains("Writes code")).unwrap() as u16;
        assert_eq!(term.backend().buffer()[(col_of(&term, y, "Coder").unwrap(), y)].bg, app.theme.surface3);
        // the model list: the default (named), the current one marked
        app.chat_id = Some("c0".into());
        app.server.models = vec!["m".into(), "other".into()];
        app.overlay = app.models_overlay();
        let (_, rows) = shot(&mut app, 100, 30);
        shows(&rows, &["Model for ✍  Writer", " ▸ ● default · m", "     other", "It changes the agent, so its other chats use it too.", "Enter use · Esc close  (or /model <name>)"]);
    }

    #[test]
    fn the_memory_and_routines_panels_list_what_they_hold() {
        let mut app = plain_app();
        let mem = |id: i64, fact: &str, source: &str, pinned: i64| Memory { id, fact: fact.into(), category: "preference".into(), importance: 7, pinned, source: source.into(), uses: 2, updated: trace::now() };
        app.overlay = Some(Overlay::Memory { data: vec![mem(1, "likes tea", "user", 1), mem(2, "uses vim", "agent:c", 0), mem(3, "lives by the sea", "auto:extract", 0)], cats: vec!["preference".into()], total: 3, sel: 1, q: String::new(), cat: 0, typing: false });
        let (_, rows) = shot(&mut app, 120, 40);
        shows(&rows, &[
            " Memory · 3 remembered ", " ⌕ / to search   ◂ all kinds ▸  (Tab)", " ⚲ likes tea", "preference · importance 7 · added by you · used 2× · just now",
            "saved by Coder", "learned automatically", "a add · d delete · p pin · +/- importance · t tidy up · Esc close",
        ]);
        app.overlay = Some(Overlay::Memory { data: vec![], cats: vec!["preference".into()], total: 3, sel: 0, q: "zz".into(), cat: 1, typing: true });
        let (_, rows) = shot(&mut app, 120, 40);
        shows(&rows, &[" ⌕ zz", "◂ preference ▸", "Nothing matches."]);
        app.overlay = Some(Overlay::Memory { data: vec![], cats: vec![], total: 0, sel: 0, q: String::new(), cat: 0, typing: false });
        let (_, rows) = shot(&mut app, 120, 40);
        shows(&rows, &["Nothing yet. Chat with your agents, or press a to add something."]);
        // routines: on or off, when they run, how the last run went
        let at = trace::now() + 600.0;
        let r = |name: &str, enabled: bool, sched: Value, last: Option<(f64, &str)>| Routine { id: name.into(), name: name.into(), agent_id: "w".into(), prompt: "p".into(), schedule: sched, enabled, last_run: last.map(|l| l.0), last_status: last.map(|l| l.1.to_string()), next_run: Some(at), chat_id: None };
        app.overlay = Some(Overlay::Routines { items: vec![
            r("Brief", true, json!({ "type": "daily", "time": "08:00", "days": [0, 1, 2, 3, 4] }), None),
            r("Poll", false, json!({ "type": "interval", "minutes": 120 }), Some((trace::now() - 30.0, "error: boom"))),
            r("Weekend", true, json!({ "type": "daily", "time": "10:30", "days": [5, 6] }), Some((trace::now() - 30.0, "started"))),
        ], sel: 0 });
        let (_, rows) = shot(&mut app, 120, 40);
        let next = trace::clock(Some(at));
        shows(&rows, &[
            " Routines ", " [x] ✍  Brief", &format!("weekdays at 08:00 · Writer · next {next} · never run"), " [ ] ✍  Poll", "every 2 h · Writer · off · last run just now (error: boom)",
            &format!("weekends at 10:30 · Writer · next {next} · last run just now"), "n new · Enter edit · Space on/off · r run now",
        ]);
        app.overlay = Some(Overlay::Routines { items: vec![], sel: 0 });
        let (_, rows) = shot(&mut app, 120, 40);
        shows(&rows, &["No routines yet. Press n to create one"]);
    }

    #[test]
    fn toasts_stack_top_right_in_the_trace_three_at_most() {
        let mut app = plain_app();
        for i in 0..5 {
            app.toast(format!("note {i}"), i == 4);
        }
        app.toast("note 4", true);
        let (term, rows) = shot(&mut app, 120, 30);
        assert!(!has(&rows, "note 0") && !has(&rows, "note 1"), "{}", rows.join("\n"));
        shows(&rows, &["note 2", "note 3", "note 4 ×2"]);
        let buf = term.backend().buffer();
        let ys: Vec<u16> = ["note 2", "note 3", "note 4"].iter().map(|n| rows.iter().position(|r| r.contains(n)).unwrap() as u16).collect();
        assert!(ys[0] < ys[1] && ys[1] < ys[2], "newest at the bottom of the stack: {ys:?}");
        let composer_top = rows.iter().position(|r| r.contains("Send ↑")).unwrap() as u16;
        for (n, y) in ["note 2", "note 3", "note 4"].iter().zip(&ys) {
            let x = col_of(&term, *y, n).unwrap();
            assert!(x > 30, "{n} beside the sidebar, over the trace");
            assert!(*y > 1 && *y < composer_top - 2, "{n} under the header and above the message box");
            // its box closes one column in from the right edge
            assert_eq!(buf[(118, *y)].symbol(), "│", "{n}: {:?}", rows[*y as usize]);
        }
        assert_eq!(buf[(118, ys[2])].fg, app.theme.danger, "an error's box is red");
        assert_eq!(buf[(118, ys[0])].fg, app.theme.accent);
        // long ones wrap, up to four lines each
        app.toasts.clear();
        app.toast("word ".repeat(80), false);
        let (_, rows) = shot(&mut app, 120, 30);
        assert_eq!(rows.iter().filter(|r| r.contains("word word")).count(), 4, "{}", rows.join("\n"));
    }

    #[test]
    fn the_sidebar_marks_pins_unread_and_what_each_chat_is_doing() {
        let mut app = plain_app();
        app.focus = Focus::Sidebar;
        app.chats[0].status = "waiting".into();
        app.chats[1].status = "delegated".into();
        app.chats[2].status = "idle".into();
        app.chats[2].last_role = Some("assistant".into());
        app.cfg.seen.insert("c2".into(), 0.0);
        app.chats[3].status = "idle".into();
        app.chats[3].pinned = true;
        app.memory_count = 3;
        app.rebuild_sidebar();
        app.select_chat_row("c2");
        let (term, rows) = shot(&mut app, 120, 30);
        shows(&rows, &[" Agent Chat", "Ctrl+K", " Agents", " All chats", " Chats", "/ filter", " n  new chat", " Pinned", " Today", "⚲", " ● m ◉3"]);
        let buf = term.backend().buffer();
        // (in the sidebar's columns: the header right of it names the open chat too)
        let row_of = |needle: &str| (0..30u16).find(|&y| (0..30u16).map(|x| buf[(x, y)].symbol()).collect::<String>().contains(needle)).unwrap_or_else(|| panic!("{needle}:\n{}", rows.join("\n")));
        let dot = |y: u16| (0..30).rev().map(|x| &buf[(x, y)]).find(|c| c.symbol() != " " && c.symbol() != "│").map(|c| (c.symbol().to_string(), c.fg)).unwrap();
        assert_eq!(dot(row_of("notes")), ("●".into(), app.theme.amber), "waiting on you");
        assert_eq!(dot(row_of("中")), ("◌".into(), app.theme.accent), "handed to another agent");
        assert_eq!(dot(row_of("plain title")), ("•".into(), app.theme.accent), "unread");
        assert_eq!(buf[(1, row_of("plain title"))].bg, app.theme.surface3, "the selected row, the sidebar focused");
        assert_eq!(buf[(0, row_of("notes"))].symbol(), "▎", "the open chat");
        // agents: what each one is doing, aligned
        shows(&rows, &["needs you"]);
        // the filter being typed, and a search with no hits
        app.search_focus = true;
        app.search = "zz".into();
        app.rebuild_sidebar();
        let (_, rows) = shot(&mut app, 120, 30);
        shows(&rows, &["/zz▏", "No chats match."]);
        // offline, no memory: the status line says so
        app.search_focus = false;
        app.search.clear();
        app.server.ok = false;
        app.memory_count = 0;
        app.rebuild_sidebar();
        let (_, rows) = shot(&mut app, 120, 30);
        assert!(rows.last().unwrap().contains(" ● model offline") && !rows.last().unwrap().contains('◉'), "{:?}", rows.last());
    }

    #[test]
    fn the_message_box_shows_the_queue_attachments_and_the_mode() {
        let mut app = plain_app();
        app.cfg.sidebar = false;
        app.queue = vec![json!({ "content": "first queued" }), json!({ "content": "second\nqueued" })];
        app.attachments = vec![Upload { name: "a.txt".into(), path: "uploads/a.txt".into(), size: 3, text: Some("abc".into()), image: false }, Upload { name: "b.png".into(), path: "uploads/b.png".into(), size: 9, text: None, image: true }];
        app.settings = json!({ "bypass_approvals": true });
        app.running = true;
        let (term, rows) = shot(&mut app, 120, 30);
        shows(&rows, &[" queued (2) · the agent reads these at its next step · ↑ in an empty box takes them back", "   ◦ first queued", "   ◦ second queued", "a.txt", "b.png", "Backspace in an empty box removes the last", "⚠ bypassing permissions", "Enter queues · Ctrl+S stops", " Stop ■ ", "● running"]);
        let y = rows.iter().position(|r| r.contains("bypassing")).unwrap() as u16;
        assert_eq!(term.backend().buffer()[(col_of(&term, y, "bypassing").unwrap(), y)].fg, app.theme.amber);
        app.settings = json!({});
        app.running = false;
        app.queue.clear();
        app.edit_from = Some(0);
        let (_, rows) = shot(&mut app, 120, 30);
        shows(&rows, &["asks before acting", "editing your last message · Enter resends · Esc cancels", " Resend ↑ "]);
        assert!(!has(&rows, "queued") && !has(&rows, "● running"));
        // a sub-agent's chat: no box, a note naming the chat to reply in
        app.chat.as_mut().unwrap()["parent"] = json!({ "root_chat_id": "c2", "caller_id": "c" });
        let (_, rows) = shot(&mut app, 120, 30);
        shows(&rows, &["This agent is working for another agent. You can watch here; to reply, open “plain title”."]);
        assert!(!has(&rows, "Send ↑") && !has(&rows, "Resend"));
    }

    #[test]
    fn the_files_drawer_lists_the_workspace() {
        let mut app = plain_app();
        app.cfg.files = true;
        app.focus = Focus::Files;
        app.files.agent_id = "c".into();
        app.files.workspace = "/home/me/agent-ws".into();
        app.files.entries = vec![
            FileEntry { name: "src".into(), dir: true, size: None, mtime: 0.0, path: "src".into() },
            FileEntry { name: "notes.txt".into(), dir: false, size: Some(1536), mtime: 0.0, path: "notes.txt".into() },
            FileEntry { name: ".env".into(), dir: false, size: Some(12), mtime: 0.0, path: ".env".into() },
        ];
        app.files.sel = 1;
        let (term, rows) = shot(&mut app, 140, 30);
        shows(&rows, &[" Workspace 💻", "Coder", " /", " ↻ refresh", " ▸ src", " · notes.txt", "1.5 KB", " · .env", "12 B", " /home/me/agent-ws"]);
        let buf = term.backend().buffer();
        let y = rows.iter().position(|r| r.contains("▸ src")).unwrap() as u16;
        assert_eq!(buf[(col_of(&term, y, "src").unwrap(), y)].bg, app.theme.surface3, "the selected row, the drawer focused");
        let y = rows.iter().position(|r| r.contains(".env")).unwrap() as u16;
        assert_eq!(buf[(col_of(&term, y, ".env").unwrap(), y)].fg, app.theme.faint, "a dot file is faint");
        // in a folder the first row goes up; an empty one says so
        app.files.path = "src/deep".into();
        app.files.entries.clear();
        app.files.sel = 0;
        let (_, rows) = shot(&mut app, 140, 30);
        shows(&rows, &[" /src/deep", " ↰ ..", " Empty folder."]);
    }

    #[test]
    fn sizes_read_in_the_unit_that_fits() {
        for (n, want) in [(0, "0 B"), (1023, "1023 B"), (1024, "1.0 KB"), (1536, "1.5 KB"), (1_048_576, "1.0 MB"), (5 * 1_048_576 + 104_858, "5.1 MB"), (3 * 1024 * 1_048_576, "3072.0 MB")] {
            assert_eq!(human_size(n), want, "{n}");
        }
        // just under a unit rounds up into it, not to "1024.0 KB"
        assert_eq!(human_size(1_048_575), "1.0 MB");
        assert_eq!(human_size(1_048_525), "1.0 MB"); // (1023.95 KB)
        assert_eq!(human_size(1_048_524), "1023.9 KB");
    }

    #[test]
    fn each_theme_paints_the_screen() {
        for (theme, bg) in [(Theme::dark(), Theme::dark().bg), (Theme::light(), Theme::light().bg), (Theme::plain(), ratatui::style::Color::Reset)] {
            let mut app = plain_app();
            app.cfg.sidebar = false;
            app.set_theme(theme.clone());
            let (term, rows) = shot(&mut app, 100, 30);
            let buf = term.backend().buffer();
            // a blank cell of the trace, the main background
            let y = rows.iter().position(|r| r.contains("A reply with")).unwrap() as u16 + 1;
            assert_eq!(buf[(90, y)].bg, bg, "{}", theme.name);
            let y = rows.iter().position(|r| r.contains("A reply with")).unwrap() as u16;
            assert_eq!(buf[(col_of(&term, y, "reply").unwrap(), y)].fg, theme.text, "{}: the reply in the text color", theme.name);
        }
    }

    #[test]
    fn the_palette_lists_its_groups_and_marks_the_pick() {
        let mut app = plain_app();
        app.open_palette();
        let (term, rows) = shot(&mut app, 120, 50);
        shows(&rows, &[" Search and commands ", " ⌕ ", " Agents", " ▸ New Writer chat", "Drafts text", " Recent chats", "plain title", " Commands", "Keyboard shortcuts", " This chat", "Export as Markdown", "↑↓ move · Enter open · Esc close"]);
        let y = rows.iter().position(|r| r.contains("▸ New Writer chat")).unwrap() as u16;
        assert_eq!(term.backend().buffer()[(col_of(&term, y, "New Writer").unwrap(), y)].bg, app.theme.surface3);
        app.overlay = Some(Overlay::Palette { q: "zzz".into(), items: vec![], sel: 0, hits: vec![] });
        let (mut term, rows) = shot(&mut app, 120, 50);
        shows(&rows, &[" ⌕ zzz", "Nothing matches."]);
        let y = rows.iter().position(|r| r.contains(" ⌕ zzz")).unwrap() as u16;
        let x = col_of(&term, y, "zzz").unwrap();
        term.backend_mut().assert_cursor_position((x + 3, y));
    }

    #[tokio::test]
    async fn random_keys_pastes_and_clicks_never_break_a_frame() {
        use crate::text::tests::{random_text, Rng};
        use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut app = plain_app();
        app.bell = false;
        app.server.models = vec!["m".into(), "n".into()];
        app.queue = vec![json!({ "content": "queued" })];
        let mut rng = Rng(2024);
        let plain = "abcdeghijklmnoprstuvwyz[]/ 0123456789G?+-=";
        // every shortcut but the ones that quit or write a file into the working folder
        let alt = "obfmrsapdn2uyN";
        let ctrl = "kneruglsjpyb";
        let special = [KeyCode::Enter, KeyCode::Esc, KeyCode::Tab, KeyCode::BackTab, KeyCode::Up, KeyCode::Down, KeyCode::Left, KeyCode::Right, KeyCode::Home, KeyCode::End, KeyCode::PageUp, KeyCode::PageDown, KeyCode::Backspace, KeyCode::Delete, KeyCode::F(1), KeyCode::F(2)];
        let mut at = std::time::Instant::now();
        let sizes = [(120u16, 40u16), (89, 30), (60, 20), (40, 12), (24, 8)];
        for step in 0..1500 {
            at += std::time::Duration::from_millis(if rng.below(6) == 0 { 2 } else { 120 });
            let ev = match rng.below(20) {
                0 => TermEvent::Paste(random_text(&mut rng, 6)),
                1 => {
                    let (w, h) = app.last_size;
                    let kind = [MouseEventKind::Down(MouseButton::Left), MouseEventKind::ScrollUp, MouseEventKind::ScrollDown][rng.below(3)];
                    TermEvent::Mouse(MouseEvent { kind, column: rng.below(w.max(1) as usize) as u16, row: rng.below(h.max(1) as usize) as u16, modifiers: KeyModifiers::NONE })
                }
                2 => TermEvent::Key(KeyEvent::new(KeyCode::Char(alt.chars().nth(rng.below(alt.len())).unwrap()), KeyModifiers::ALT)),
                3 => TermEvent::Key(KeyEvent::new(KeyCode::Char(ctrl.chars().nth(rng.below(ctrl.len())).unwrap()), KeyModifiers::CONTROL)),
                4..=8 => TermEvent::Key(KeyEvent::new(special[rng.below(special.len())], KeyModifiers::NONE)),
                _ => TermEvent::Key(KeyEvent::new(KeyCode::Char(plain.chars().nth(rng.below(plain.len())).unwrap()), KeyModifiers::NONE)),
            };
            let shown = format!("{ev:?}");
            app.on_input(at, ev, None).await;
            if app.burst_deadline().is_some() && rng.below(3) == 0 {
                app.flush_burst().await;
            }
            app.quit = false; // /quit and the palette's Quit are fair game; keep going
            if step % 7 == 0 {
                app.tick();
            }
            let (w, h) = sizes[rng.below(sizes.len())];
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| {
                draw(f, &mut app);
                text::scrub(f.buffer_mut());
            }).unwrap_or_else(|e| panic!("step {step} after {shown}: {e}"));
            for cell in &term.backend().buffer().content {
                let sym = cell.symbol();
                assert!(!sym.chars().any(char::is_control), "step {step} after {shown}: {sym:?}");
                assert_eq!(Span::raw(sym).width(), text::width(sym), "step {step} after {shown}: {sym:?}");
            }
        }
    }

    // ------------------------------------------------------------ found in review

    #[test]
    fn the_palettes_selection_stays_in_view_with_its_groups_name() {
        let mut app = plain_app();
        app.open_palette();
        let n = match &app.overlay { Some(Overlay::Palette { items, .. }) => items.len(), _ => unreachable!() };
        for h in [12, 16, 20, 30] {
            for sel in 0..n {
                if let Some(Overlay::Palette { sel: s, .. }) = &mut app.overlay {
                    *s = sel;
                }
                let (_, rows) = shot(&mut app, 120, h);
                let title = match &app.overlay { Some(Overlay::Palette { items, .. }) => items[sel].title.clone(), _ => unreachable!() };
                let at = rows.iter().position(|r| r.contains(" ▸ "));
                assert!(at.is_some(), "{h} rows, item {sel} ({title}) out of view:\n{}", rows.join("\n"));
                // a group's first item comes with its group's name above it
                let group = match &app.overlay { Some(Overlay::Palette { items, .. }) => (sel == 0 || items[sel - 1].group != items[sel].group).then(|| items[sel].group.clone()), _ => unreachable!() };
                if let (Some(g), Some(y)) = (group, at) {
                    assert!(rows[y - 1].contains(&format!(" {g}")), "{h} rows: {g} above {title}:\n{}", rows.join("\n"));
                }
            }
        }
    }

    #[test]
    fn a_form_too_short_for_its_field_holds_still() {
        let mut app = plain_app();
        app.open_agent_editor(Some("c".into()));
        // focus the multi-line instructions (taller than the room), then the URL-like text fields
        for tabs in 0..6 {
            for (w, h) in [(100, 12), (100, 10), (100, 8), (30, 4), (40, 6)] {
                let first = shot(&mut app, w, h).1;
                for _ in 0..3 {
                    assert_eq!(shot(&mut app, w, h).1, first, "{w}x{h}, {tabs} tabs: the form moves by itself");
                }
            }
            if let Some(Overlay::Form { form, .. }) = &mut app.overlay {
                form.handle(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Tab, crossterm::event::KeyModifiers::NONE));
            }
        }
    }

    #[test]
    fn running_keeps_its_place_beside_a_long_purpose() {
        let mut app = plain_app();
        app.cfg.sidebar = false;
        app.agents[0].purpose = "writes long things ".repeat(20);
        app.running = true;
        for w in [40, 80, 120] {
            let (_, rows) = shot(&mut app, w, 20);
            assert!(rows[1].trim_end().ends_with("● running") && rows[1].contains("…"), "{w}: {:?}", rows[1]);
        }
        app.running = false;
        let (_, rows) = shot(&mut app, 120, 20);
        assert!(!rows[1].contains("running") && rows[1].contains("Writer · writes long"));
    }

    #[test]
    fn a_trace_with_no_room_is_not_wrapped_at_all() {
        let mut app = plain_app();
        let long = format!("{{\"role\": \"assistant\", \"content\": \"{}\"}}", "word ".repeat(4000));
        let chat = json!({ "id": "c0", "agent_id": "w", "messages": [{ "role": "user", "content": "hi" }, serde_json::from_str::<serde_json::Value>(&long).unwrap()] });
        app.trace = Trace::from_chat(&chat, &app.agents, None);
        app.chat = Some(chat);
        app.focus = Focus::Sidebar;
        for (w, h) in [(1, 1), (2, 80), (20, 6), (31, 10)] {
            app.rendered = None;
            let t0 = std::time::Instant::now();
            shot(&mut app, w, h);
            assert!(app.rendered.is_none(), "{w}x{h}: the trace was wrapped");
            assert!(t0.elapsed() < std::time::Duration::from_millis(500), "{w}x{h} took {:?}", t0.elapsed());
        }
        app.focus = Focus::Messages;
        shot(&mut app, 120, 20);
        assert!(app.rendered.is_some());
    }

    #[test]
    fn a_groups_first_chat_brings_its_header_into_view() {
        let mut app = plain_app();
        app.chats = (0..24).map(|i| ChatSummary { id: format!("n{i:02}"), title: format!("chat n{i:02}"), agent_id: "c".into(), updated: trace::now() - i as f64, pinned: i == 5, status: "idle".into(), ..Default::default() }).collect();
        app.rebuild_sidebar();
        app.focus = Focus::Sidebar;
        // the end of the list, then up to the pinned chat (the Pinned group's only one)
        app.side_sel = app.side_rows.len() - 1;
        shot(&mut app, 120, 30);
        let pinned = app.side_rows.iter().position(|r| matches!(r, SideRow::Chat(id) if id == "n05")).unwrap();
        let today = app.side_rows.iter().position(|r| matches!(r, SideRow::Group(g) if g == "Today")).unwrap();
        for sel in (pinned..app.side_rows.len()).rev() {
            if matches!(app.side_rows[sel], SideRow::Group(_)) {
                continue;
            }
            app.side_sel = sel;
            let (_, rows) = shot(&mut app, 120, 30);
            if sel == pinned {
                assert!(rows.iter().any(|r| r.starts_with(" Pinned")), "{}", rows.join("\n"));
            }
            if sel == today + 1 {
                assert!(rows.iter().any(|r| r.starts_with(" Today")), "{}", rows.join("\n"));
            }
        }
    }

    #[test]
    fn toasts_show_over_dialogs() {
        let mut app = plain_app();
        app.overlay = Some(Overlay::Viewer { title: "notes.txt".into(), lines: vec!["x".repeat(200); 40], scroll: 0 });
        app.toast("Copied the file", false);
        let (_, rows) = shot(&mut app, 120, 30);
        assert!(has(&rows, "Copied the file"), "{}", rows.join("\n"));
        app.overlay = None;
        app.open_agent_editor(None);
        app.toast("The agent needs a name.", true);
        let (_, rows) = shot(&mut app, 120, 30);
        assert!(has(&rows, "The agent needs a name."));
    }
}
