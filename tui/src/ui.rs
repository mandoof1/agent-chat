//! Drawing: sidebar | header + trace + composer | files drawer, and the overlays.

use crate::app::{agent_color, agent_who, time_ago, App, Focus, Overlay, SideRow};
use crate::trace::{self, fmt_k, RenderOpts};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &mut App) {
    let th = app.theme.clone();
    let area = f.area();
    app.last_size = (area.width, area.height);
    f.render_widget(Block::default().style(Style::default().bg(th.bg).fg(th.text)), area);

    let (side_w, files_w) = app.layout_cols();
    let small = area.width < 90;
    let side_w = if small && app.focus != Focus::Sidebar { 0 } else { side_w as u16 };
    let files_w = if small { 0 } else { files_w as u16 };
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(side_w), Constraint::Min(30), Constraint::Length(files_w)])
        .split(area);

    if side_w > 0 {
        draw_sidebar(f, app, cols[0]);
    }
    draw_main(f, app, cols[1]);
    if files_w > 0 {
        draw_files(f, app, cols[2]);
    }
    draw_toasts(f, app, area);
    draw_overlay(f, app, area);
}

fn sep(th: &crate::theme::Theme) -> Span<'static> {
    Span::styled("│", Style::default().fg(th.line))
}

// ---------------------------------------------------------------- sidebar

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.theme.clone();
    f.render_widget(Block::default().style(Style::default().bg(th.surface)), area);
    let inner = Rect { x: area.x, y: area.y, width: area.width.saturating_sub(1), height: area.height };
    // right border
    for y in area.y..area.y + area.height {
        f.render_widget(Paragraph::new(sep(&th)), Rect { x: area.x + area.width - 1, y, width: 1, height: 1 });
    }
    let focused = app.focus == Focus::Sidebar;
    let mut y = inner.y;
    let w = inner.width as usize;
    let put = |f: &mut Frame, y: u16, line: Line| {
        f.render_widget(Paragraph::new(line), Rect { x: inner.x, y, width: inner.width, height: 1 });
    };
    put(f, y, Line::from(vec![Span::styled(" ⟟ ", Style::default().fg(th.accent)), Span::styled("Agent Chat", Style::default().fg(th.text).add_modifier(Modifier::BOLD)), Span::styled(format!("{:>w$}", "Ctrl+K ", w = w.saturating_sub(13)), Style::default().fg(th.faint))]));
    y += 1;
    put(f, y, Line::from(Span::styled(" Agents", Style::default().fg(th.muted).add_modifier(Modifier::BOLD))));
    y += 1;

    app.side_row_map.clear();
    let agent_rows: Vec<(usize, SideRow)> = app.side_rows.iter().cloned().enumerate().filter(|(_, r)| matches!(r, SideRow::AllAgents | SideRow::Agent(_))).collect();
    let chat_rows: Vec<(usize, SideRow)> = app.side_rows.iter().cloned().enumerate().filter(|(_, r)| matches!(r, SideRow::Group(_) | SideRow::Chat(_))).collect();
    let agents_h = (agent_rows.len() as u16).min(inner.height.saturating_sub(10).max(3));
    for (i, r) in agent_rows.iter().take(agents_h as usize) {
        let selected = *i == app.side_sel;
        let (mark, name, color) = match r {
            SideRow::AllAgents => ("▦".to_string(), "All chats".to_string(), th.muted),
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
        let name_w = w.saturating_sub(4 + state.chars().count() + 2);
        let name_t: String = name.chars().take(name_w).collect();
        let pad = name_w.saturating_sub(name_t.chars().count());
        let line = Line::from(vec![
            Span::styled(if active { "▎" } else { " " }, Style::default().fg(th.accent).bg(bg)),
            Span::styled(format!("{mark} "), Style::default().bg(bg).fg(color)),
            Span::styled(name_t, Style::default().bg(bg).fg(th.text).add_modifier(if active || selected { Modifier::BOLD } else { Modifier::empty() })),
            Span::styled(" ".repeat(pad), Style::default().bg(bg)),
            Span::styled(format!(" {state} "), Style::default().bg(bg).fg(state_color)),
        ]);
        put(f, y, line);
        app.side_row_map.insert(y as usize, *i);
        y += 1;
    }
    y += 1;
    // chats header
    let head = if app.filter.is_empty() { "Chats".to_string() } else { format!("{} chats", agent_who(&app.agents, &app.filter).name) };
    let filter_w = w.saturating_sub(head.chars().count() + 4);
    let search = if app.search_focus { format!("/{}▏", app.search) } else if !app.search.is_empty() { format!("/{}", app.search) } else { "/ filter".to_string() };
    let search_t: String = search.chars().rev().take(filter_w).collect::<Vec<_>>().into_iter().rev().collect();
    put(f, y, Line::from(vec![
        Span::styled(format!(" {head}"), Style::default().fg(th.muted).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:>w$}", search_t, w = filter_w + 1), Style::default().fg(if app.search_focus { th.accent } else { th.faint })),
    ]));
    y += 1;
    put(f, y, Line::from(vec![Span::styled(" n ", Style::default().fg(th.bg).bg(th.accent).add_modifier(Modifier::BOLD)), Span::styled(" new chat", Style::default().fg(th.text))]));
    y += 2;

    // chat list with scrolling to keep the selection visible
    let list_h = (inner.y + inner.height).saturating_sub(y + 1) as usize;
    let sel_pos = chat_rows.iter().position(|(i, _)| *i == app.side_sel);
    if let Some(p) = sel_pos {
        if p < app.side_scroll {
            app.side_scroll = p;
        } else if p >= app.side_scroll + list_h.max(1) {
            app.side_scroll = p + 1 - list_h.max(1);
        }
    }
    if app.side_scroll > chat_rows.len().saturating_sub(1) {
        app.side_scroll = chat_rows.len().saturating_sub(1);
    }
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
                let title_w = w.saturating_sub(6 + pin.chars().count());
                let title: String = format!("{sub}{}", c.title).chars().take(title_w).collect();
                let pad = title_w.saturating_sub(title.chars().count());
                put(f, y, Line::from(vec![
                    Span::styled(if open { "▎" } else { " " }, Style::default().fg(th.accent).bg(bg)),
                    Span::styled(format!("{} ", who.emoji), Style::default().bg(bg)),
                    Span::styled(pin.to_string(), Style::default().fg(th.amber).bg(bg)),
                    Span::styled(title, Style::default().bg(bg).fg(th.text).add_modifier(if open { Modifier::BOLD } else { Modifier::empty() })),
                    Span::styled(" ".repeat(pad), Style::default().bg(bg)),
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
    let sy = inner.y + inner.height - 1;
    let model = app.settings.get("model").and_then(|m| m.as_str()).filter(|m| !m.is_empty()).map(String::from).or_else(|| app.server.models.first().cloned()).unwrap_or_else(|| "online".into());
    let model: String = model.rsplit('/').next().unwrap_or(&model).trim_end_matches(".gguf").to_string();
    let (dot, text) = if app.server.ok { ("●", model) } else { ("●", "model offline".into()) };
    let mem = if app.memory_count > 0 { format!(" ◉{}", app.memory_count) } else { String::new() };
    let text: String = text.chars().take(w.saturating_sub(6 + mem.len())).collect();
    put(f, sy, Line::from(vec![
        Span::styled(format!(" {dot} "), Style::default().fg(if app.server.ok { th.ok } else { th.danger })),
        Span::styled(text, Style::default().fg(th.muted)),
        Span::styled(mem, Style::default().fg(if app.memory_working { th.accent } else { th.faint })),
    ]));
}

// ------------------------------------------------------------------- main

fn draw_main(f: &mut Frame, app: &mut App, area: Rect) {
    let _th = app.theme.clone();
    if app.chat_id.is_none() {
        draw_welcome(f, app, area);
        return;
    }
    let composer_h: u16 = if app.is_subchat() { 2 } else { (app.composer.lines().len() as u16).clamp(1, 8) + 3 + if app.attachments.is_empty() { 0 } else { 1 } + if app.queue.is_empty() { 0 } else { app.queue.len().min(3) as u16 + 1 } };
    let rows = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(2), Constraint::Min(3), Constraint::Length(composer_h)]).split(area);
    draw_header(f, app, rows[0]);
    draw_messages(f, app, rows[1]);
    draw_composer(f, app, rows[2]);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let chat = app.chat.as_ref();
    let title = chat.and_then(|c| c.get("title")).and_then(|t| t.as_str()).unwrap_or("…").to_string();
    let aid = app.chat_agent_id();
    let who = agent_who(&app.agents, &aid);
    let agent = app.agent(&aid);
    let sub = if app.is_subchat() {
        let caller = chat.and_then(|c| c.get("parent")).and_then(|p| p.get("caller_id")).and_then(|c| c.as_str()).unwrap_or("");
        format!("{} · working for {}", who.name, agent_who(&app.agents, caller).name)
    } else {
        format!("{}{}", who.name, agent.map(|a| if a.purpose.is_empty() { String::new() } else { format!(" · {}", a.purpose) }).unwrap_or_default())
    };
    // right side: context bar + speed
    let mut right = vec![];
    if let Some(t) = app.ctx_tokens {
        let n = app.server.n_ctx;
        let pct = n.map(|n| (t as f64 / n as f64 * 100.0).min(100.0)).unwrap_or(0.0);
        let bar_w = 12usize;
        let fill = ((pct / 100.0) * bar_w as f64).round() as usize;
        let color = if pct >= 90.0 { th.danger } else if pct >= 70.0 { th.amber } else { th.ok };
        if n.is_some() {
            right.push(Span::styled("▮".repeat(fill.max(if t > 0 { 1 } else { 0 })), Style::default().fg(color)));
            right.push(Span::styled("▯".repeat(bar_w - fill.max(if t > 0 { 1 } else { 0 }).min(bar_w)), Style::default().fg(th.line)));
        }
        right.push(Span::styled(format!(" ctx {}{}", fmt_k(t), n.map(|n| format!("/{}", fmt_k(n))).unwrap_or_default()), Style::default().fg(th.muted)));
    }
    if let Some(s) = app.tok_per_s {
        right.push(Span::styled(format!("  {s:.1} tok/s"), Style::default().fg(th.muted)));
    }
    let right_w: usize = right.iter().map(|s| s.content.chars().count()).sum();
    let left_w = (area.width as usize).saturating_sub(right_w + 3);
    let t: String = format!(" {} {}", who.emoji, title).chars().take(left_w).collect();
    let mut l1 = vec![Span::styled(t.clone(), Style::default().fg(th.text).add_modifier(Modifier::BOLD))];
    l1.push(Span::styled(" ".repeat(left_w.saturating_sub(t.chars().count()) + 1), Style::default()));
    l1.extend(right);
    f.render_widget(Paragraph::new(Line::from(l1)), Rect { x: area.x, y: area.y, width: area.width, height: 1 });
    let s: String = format!("   {sub}").chars().take(area.width as usize).collect();
    let mut l2 = vec![Span::styled(s, Style::default().fg(th.muted))];
    if app.running {
        l2.push(Span::styled("  ● running", Style::default().fg(th.accent)));
    }
    f.render_widget(Paragraph::new(Line::from(l2)), Rect { x: area.x, y: area.y + 1, width: area.width, height: 1 });
}

fn draw_messages(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.theme.clone();
    let inner = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: area.height.saturating_sub(1) };
    let width = inner.width as usize;
    let key = (width, app.verbose(), app.dirty);
    if app.rendered.is_none() || app.render_key != key {
        let opts = RenderOpts { theme: &th, width, verbose: app.verbose(), selected_turn: app.selected_turn };
        app.rendered = Some(trace::render(&app.trace, &opts));
        app.render_key = key;
    }
    let rendered = app.rendered.as_ref().unwrap();
    let total = rendered.lines.len();
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
    let lines: Vec<Line> = rendered.lines.iter().skip(app.scroll).take(h).cloned().collect();
    f.render_widget(Paragraph::new(lines), inner);
    if app.trace.entries.is_empty() && !app.running {
        let aid = app.chat_agent_id();
        let a = app.agent(&aid);
        let msg = match a {
            Some(a) if !a.purpose.is_empty() => format!("{} {} is ready. Its job: {}.", a.emoji, a.name, a.purpose),
            Some(a) => format!("{} {} is ready.", a.emoji, a.name),
            None => "Ready.".into(),
        };
        f.render_widget(Paragraph::new(Span::styled(msg, Style::default().fg(th.muted))).wrap(Wrap { trim: true }), inner);
    }
    // scroll hint
    if !app.follow && total > h {
        let below = max_scroll.saturating_sub(app.scroll);
        let tag = format!(" ↓ {} more line{} · End for newest ", below, if below == 1 { "" } else { "s" });
        let x = inner.x + inner.width.saturating_sub(tag.chars().count() as u16 + 1);
        f.render_widget(Paragraph::new(Span::styled(tag, Style::default().fg(th.bg).bg(th.accent))), Rect { x, y: inner.y + inner.height - 1, width: inner.width.saturating_sub(x - inner.x), height: 1 });
    }
    if app.focus == Focus::Messages {
        f.render_widget(Paragraph::new(Span::styled("▏", Style::default().fg(th.accent))), Rect { x: area.x, y: area.y + 1, width: 1, height: area.height.saturating_sub(1) });
    }
}

fn draw_composer(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.theme.clone();
    if app.is_subchat() {
        let caller = app.chat.as_ref().and_then(|c| c.get("parent")).and_then(|p| p.get("root_chat_id")).and_then(|c| c.as_str()).unwrap_or("");
        let root = app.chats.iter().find(|c| c.id == caller).map(|c| c.title.clone()).unwrap_or_else(|| "the original chat".into());
        let text = format!(" This agent is working for another agent. You can watch here; to reply, open “{}”.", trace::one_line(&root, 40));
        f.render_widget(Paragraph::new(Line::from(Span::styled(text, Style::default().fg(th.muted)))).wrap(Wrap { trim: true }), area);
        return;
    }
    let focused = app.focus == Focus::Composer;
    let mut y = area.y;
    if !app.queue.is_empty() {
        f.render_widget(Paragraph::new(Line::from(Span::styled(format!(" queued ({}) · the agent reads these at its next step · ↑ in an empty box takes them back", app.queue.len()), Style::default().fg(th.accent)))), Rect { x: area.x, y, width: area.width, height: 1 });
        y += 1;
        for q in app.queue.iter().take(3) {
            let text = q.get("content").and_then(|c| c.as_str()).unwrap_or("");
            f.render_widget(Paragraph::new(Line::from(vec![Span::styled("   ◦ ", Style::default().fg(th.accent)), Span::styled(trace::one_line(text, area.width as usize - 6), Style::default().fg(th.text))])), Rect { x: area.x, y, width: area.width, height: 1 });
            y += 1;
        }
    }
    if !app.attachments.is_empty() {
        let mut spans = vec![Span::styled(" ", Style::default())];
        for a in &app.attachments {
            spans.push(Span::styled(format!(" {}{} ", if a.image { "🖼 " } else { "📎 " }, a.name), Style::default().fg(th.text).bg(th.surface3)));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled("Backspace in an empty box removes the last", Style::default().fg(th.faint)));
        f.render_widget(Paragraph::new(Line::from(spans)), Rect { x: area.x, y, width: area.width, height: 1 });
        y += 1;
    }
    let box_h = area.height.saturating_sub(y - area.y);
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(if focused { th.accent } else { th.line })).style(Style::default().bg(th.surface));
    let rect = Rect { x: area.x, y, width: area.width, height: box_h };
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let text_h = inner.height.saturating_sub(1);
    app.composer.set_style(Style::default().fg(th.text).bg(th.surface));
    app.composer.set_placeholder_style(Style::default().fg(th.faint));
    app.composer.set_cursor_style(if focused { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() });
    f.render_widget(&app.composer, Rect { x: inner.x + 1, y: inner.y, width: inner.width.saturating_sub(2), height: text_h.max(1) });
    // bottom bar: mode chip, hints, send/stop
    let bypass = app.settings.get("bypass_approvals").and_then(|b| b.as_bool()).unwrap_or(false);
    let mut bar = vec![Span::styled(if bypass { " ⚠ bypassing permissions " } else { " ⛨ asks before acting " }, Style::default().fg(if bypass { th.amber } else { th.muted }))];
    let hint = if app.edit_from.is_some() { "editing your last message · Enter resends · Esc cancels" } else if app.running { "Enter queues · Ctrl+S stops" } else { "Enter sends · Alt+Enter new line · Ctrl+U attach · F1 help" };
    bar.push(Span::styled(format!(" {hint}"), Style::default().fg(th.faint)));
    let action = if app.running { " Stop ■ " } else if app.edit_from.is_some() { " Resend ↑ " } else { " Send ↑ " };
    let used: usize = bar.iter().map(|s| s.content.chars().count()).sum();
    let pad = (inner.width as usize).saturating_sub(used + action.chars().count() + 1);
    bar.push(Span::raw(" ".repeat(pad)));
    bar.push(Span::styled(action, Style::default().fg(if app.running { th.text } else { th.bg }).bg(if app.running { th.danger } else { th.accent }).add_modifier(Modifier::BOLD)));
    f.render_widget(Paragraph::new(Line::from(bar)), Rect { x: inner.x, y: inner.y + text_h, width: inner.width, height: 1 });
}

fn draw_welcome(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.theme.clone();
    let inner = Rect { x: area.x + 3, y: area.y + 1, width: area.width.saturating_sub(6), height: area.height.saturating_sub(2) };
    let mut lines: Vec<Line> = vec![];
    let model = app.server.models.first().cloned().unwrap_or_default();
    lines.push(Line::from(Span::styled(if app.server.ok { format!("Local · {} · {} agents{}", model.rsplit('/').next().unwrap_or(&model), app.agents.len(), app.server.n_ctx.map(|n| format!(" · {} context", fmt_k(n))).unwrap_or_default()) } else { format!("Model server offline · {} agents", app.agents.len()) }, Style::default().fg(th.faint))));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Pick an agent.", Style::default().fg(th.text).add_modifier(Modifier::BOLD))));
    lines.push(Line::from(Span::styled("Watch every step.", Style::default().fg(th.muted).add_modifier(Modifier::BOLD))));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Each agent has its own job, tools and instructions. They share what they know about you, and hand work to each other when a job needs another skill. Everything they do shows up as a trace you can read.", Style::default().fg(th.muted))));
    lines.push(Line::from(""));
    if !app.server.ok {
        lines.push(Line::from(vec![Span::styled("✖ ", Style::default().fg(th.danger)), Span::styled(format!("The model server isn't reachable at {}. Start llama-server, or point Settings (Alt+S) at the server you use.", if app.server.base_url.is_empty() { app.settings.get("base_url").and_then(|b| b.as_str()).unwrap_or("").to_string() } else { app.server.base_url.clone() }), Style::default().fg(th.danger))]));
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
                    let l = if info.name == "run_shell" { "Shell".to_string() } else { info.group.clone() };
                    if !g.contains(&l) {
                        g.push(l);
                    }
                }
            }
            if g.is_empty() { vec!["Chat only".into()] } else { g }
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{:>2} ", i + 1), Style::default().fg(th.faint)),
            Span::styled(format!("{} ", a.emoji), Style::default()),
            Span::styled(a.name.clone(), Style::default().fg(agent_color(a)).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {}", a.purpose), Style::default().fg(th.muted)),
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
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

// ------------------------------------------------------------------ files

fn draw_files(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.theme.clone();
    f.render_widget(Block::default().style(Style::default().bg(th.surface)), area);
    for y in area.y..area.y + area.height {
        f.render_widget(Paragraph::new(sep(&th)), Rect { x: area.x, y, width: 1, height: 1 });
    }
    let inner = Rect { x: area.x + 1, y: area.y, width: area.width - 1, height: area.height };
    let focused = app.focus == Focus::Files;
    let w = inner.width as usize;
    let put = |f: &mut Frame, y: u16, line: Line| f.render_widget(Paragraph::new(line), Rect { x: inner.x, y, width: inner.width, height: 1 });
    let who = agent_who(&app.agents, &app.files.agent_id);
    put(f, inner.y, Line::from(vec![Span::styled(" Workspace ", Style::default().fg(th.text).add_modifier(Modifier::BOLD)), Span::styled(format!("{} {}", who.emoji, who.name), Style::default().fg(th.muted))]));
    put(f, inner.y + 1, Line::from(Span::styled(format!(" /{}", if app.files.path == "." { String::new() } else { app.files.path.clone() }), Style::default().fg(th.faint))));
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
            let meta = if e.dir { String::new() } else { format!("{} ", human_size(e.size.unwrap_or(0))) };
            let name_w = w.saturating_sub(3 + meta.chars().count());
            let name: String = e.name.chars().take(name_w).collect();
            let pad = name_w.saturating_sub(name.chars().count());
            rows.push(Line::from(vec![
                Span::styled(format!(" {icon} "), Style::default().fg(if e.dir { th.accent } else { th.faint }).bg(bg(i + 1))),
                Span::styled(name, Style::default().fg(if e.name.starts_with('.') { th.faint } else { th.text }).bg(bg(i + 1))),
                Span::styled(" ".repeat(pad), Style::default().bg(bg(i + 1))),
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
    put(f, inner.y + inner.height - 1, Line::from(Span::styled(format!(" {}", trace::one_line(&app.files.workspace, w.saturating_sub(2))), Style::default().fg(th.faint))));
}

pub fn human_size(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1048576.0)
    }
}

// ----------------------------------------------------------------- toasts

fn draw_toasts(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let mut y = area.y + 1;
    for t in &app.toasts {
        let w = (t.text.chars().count() as u16 + 4).min(area.width.saturating_sub(4));
        let x = area.x + area.width.saturating_sub(w + 2);
        let lines = ((t.text.chars().count() as u16) / w.saturating_sub(4).max(1)) + 1;
        let rect = Rect { x, y, width: w, height: lines + 2 };
        f.render_widget(Clear, rect);
        let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(if t.err { th.danger } else { th.accent })).style(Style::default().bg(th.surface2));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        f.render_widget(Paragraph::new(Span::styled(t.text.clone(), Style::default().fg(th.text))).wrap(Wrap { trim: true }), inner);
        y += lines + 2;
    }
}

// ---------------------------------------------------------------- overlays

fn dialog(f: &mut Frame, area: Rect, w: u16, h: u16, title: &str, th: &crate::theme::Theme) -> Rect {
    let w = w.min(area.width.saturating_sub(2)).max(20);
    let h = h.min(area.height.saturating_sub(2)).max(5);
    let rect = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    f.render_widget(Clear, rect);
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(th.line)).title(Span::styled(format!(" {title} "), Style::default().fg(th.text).add_modifier(Modifier::BOLD))).style(Style::default().bg(th.surface));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    inner
}

fn draw_overlay(f: &mut Frame, app: &mut App, area: Rect) {
    let th = app.theme.clone();
    let Some(overlay) = &mut app.overlay else { return };
    match overlay {
        Overlay::Help => {
            let rows: Vec<(&str, &str)> = vec![
                ("Ctrl+K", "search chats and commands"), ("n / Ctrl+N", "new chat (number picks the agent)"), ("Tab / Shift+Tab", "move between sidebar, trace, composer, files"),
                ("Enter", "send (queue while the agent works)"), ("Alt+Enter / Ctrl+J", "new line"), ("Ctrl+S / Esc", "stop the agent"),
                ("↑ in an empty box", "take queued messages back"), ("Ctrl+U", "attach a file"), ("Ctrl+E", "edit and resend your last message"),
                ("Ctrl+R", "regenerate the last reply / retry after an error"), ("Ctrl+G", "branch from the end"), ("Ctrl+L", "compact older messages"),
                ("y / a / n", "approve · approve all for this run · deny"), ("Alt+O", "show thinking, results and tool details"), ("Alt+F", "workspace files"),
                ("Alt+B", "hide or show the sidebar"), ("Alt+M", "memory"), ("Alt+R", "routines"), ("Alt+S", "settings"), ("Alt+A", "edit the agent"),
                ("Alt+P", "pin or unpin the chat"), ("F2", "rename the chat"), ("Alt+D", "delete the chat"), ("Alt+X", "export as Markdown"),
                ("Ctrl+Y", "bypass permissions on/off"), ("In the trace: [ ]", "select a reply · c copy · r regenerate · b branch · o open the handoff chat"),
                ("In the sidebar", "j/k move · Enter open · / filter · d delete · r rename · p pin · e edit agent · m mark all read"),
                ("Ctrl+Q", "quit"),
            ];
            let inner = dialog(f, area, 86, rows.len() as u16 + 3, "Keyboard shortcuts", &th);
            let lines: Vec<Line> = rows.iter().map(|(k, v)| Line::from(vec![Span::styled(format!("{k:>20}  "), Style::default().fg(th.accent)), Span::styled(v.to_string(), Style::default().fg(th.text))])).collect();
            f.render_widget(Paragraph::new(lines), inner);
        }
        Overlay::Confirm { title, text, ok, .. } => {
            let inner = dialog(f, area, 60, 7, title, &th);
            f.render_widget(Paragraph::new(Span::styled(text.clone(), Style::default().fg(th.muted))).wrap(Wrap { trim: true }), Rect { x: inner.x, y: inner.y, width: inner.width, height: inner.height.saturating_sub(2) });
            let l = Line::from(vec![Span::styled(" Enter ", Style::default().fg(th.bg).bg(th.accent)), Span::styled(format!(" {ok}   "), Style::default().fg(th.text)), Span::styled(" Esc ", Style::default().fg(th.text).bg(th.surface3)), Span::styled(" cancel", Style::default().fg(th.muted))]);
            f.render_widget(Paragraph::new(l), Rect { x: inner.x, y: inner.y + inner.height - 1, width: inner.width, height: 1 });
        }
        Overlay::Prompt { title, value, .. } => {
            let inner = dialog(f, area, 70, 5, title, &th);
            f.render_widget(Paragraph::new(Line::from(Span::styled(format!(" {value}"), Style::default().fg(th.text).bg(th.surface3)))), Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 });
            f.set_cursor_position((inner.x + 1 + value.chars().count() as u16, inner.y));
            f.render_widget(Paragraph::new(Line::from(vec![Span::styled(" Enter ", Style::default().fg(th.bg).bg(th.accent)), Span::styled(" ok   ", Style::default().fg(th.text)), Span::styled(" Esc ", Style::default().fg(th.text).bg(th.surface3)), Span::styled(" cancel", Style::default().fg(th.muted))])), Rect { x: inner.x, y: inner.y + 2, width: inner.width, height: 1 });
        }
        Overlay::Pick { sel } => {
            let inner = dialog(f, area, 70, app.agents.len() as u16 + 4, "New chat with", &th);
            let mut lines = vec![];
            for (i, a) in app.agents.iter().enumerate() {
                let bg = if i == *sel { th.surface3 } else { th.surface };
                lines.push(Line::from(vec![
                    Span::styled(format!(" {} ", i + 1), Style::default().fg(th.faint).bg(bg)),
                    Span::styled(format!("{} ", a.emoji), Style::default().bg(bg)),
                    Span::styled(format!("{:<14}", a.name), Style::default().fg(agent_color(a)).bg(bg).add_modifier(Modifier::BOLD)),
                    Span::styled(trace::one_line(&a.purpose, inner.width as usize - 22), Style::default().fg(th.muted).bg(bg)),
                ]));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(" Enter or a number starts the chat · Esc closes", Style::default().fg(th.faint))));
            f.render_widget(Paragraph::new(lines), inner);
        }
        Overlay::Palette { q, items, sel, .. } => {
            let h = (items.len() as u16 + 5).min(area.height.saturating_sub(4)).max(8);
            let inner = dialog(f, area, 76, h, "Search and commands", &th);
            f.render_widget(Paragraph::new(Line::from(vec![Span::styled(" ⌕ ", Style::default().fg(th.muted)), Span::styled(q.clone(), Style::default().fg(th.text))])), Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 });
            f.set_cursor_position((inner.x + 3 + q.chars().count() as u16, inner.y));
            let list_h = inner.height.saturating_sub(3) as usize;
            let start = sel.saturating_sub(list_h.saturating_sub(1));
            let mut lines = vec![];
            let mut group = String::new();
            let mut shown = 0usize;
            for (i, it) in items.iter().enumerate().skip(start) {
                if it.group != group {
                    group = it.group.clone();
                    if shown > 0 || i == start {
                        lines.push(Line::from(Span::styled(format!(" {}", it.group), Style::default().fg(th.faint))));
                    }
                }
                let bg = if i == *sel { th.surface3 } else { th.surface };
                let title: String = it.title.chars().take(40).collect();
                lines.push(Line::from(vec![
                    Span::styled(if i == *sel { " ▸ " } else { "   " }, Style::default().fg(th.accent).bg(bg)),
                    Span::styled(format!("{title:<40}"), Style::default().fg(th.text).bg(bg)),
                    Span::styled(trace::one_line(&it.sub, inner.width as usize - 46), Style::default().fg(th.muted).bg(bg)),
                ]));
                shown += 1;
                if lines.len() >= list_h {
                    break;
                }
            }
            if items.is_empty() {
                lines.push(Line::from(Span::styled("  Nothing matches.", Style::default().fg(th.muted))));
            }
            f.render_widget(Paragraph::new(lines), Rect { x: inner.x, y: inner.y + 1, width: inner.width, height: inner.height.saturating_sub(2) });
            f.render_widget(Paragraph::new(Line::from(Span::styled(" ↑↓ move · Enter open · Esc close", Style::default().fg(th.faint)))), Rect { x: inner.x, y: inner.y + inner.height - 1, width: inner.width, height: 1 });
        }
        Overlay::Form { form, .. } => form.render(f, area, &th),
        Overlay::Viewer { title, lines, scroll } => {
            let inner = dialog(f, area, area.width.saturating_sub(6), area.height.saturating_sub(2), &format!("{title}  ({} lines)", lines.len()), &th);
            let h = inner.height.saturating_sub(1) as usize;
            let body: Vec<Line> = lines.iter().skip(*scroll).take(h).enumerate().map(|(i, l)| Line::from(vec![Span::styled(format!("{:>4} ", *scroll + i + 1), Style::default().fg(th.faint)), Span::styled(l.clone(), Style::default().fg(th.code))])).collect();
            f.render_widget(Paragraph::new(body), Rect { x: inner.x, y: inner.y, width: inner.width, height: inner.height.saturating_sub(1) });
            f.render_widget(Paragraph::new(Line::from(Span::styled(" j/k PgUp/PgDn scroll · c copy · Esc close", Style::default().fg(th.faint)))), Rect { x: inner.x, y: inner.y + inner.height - 1, width: inner.width, height: 1 });
        }
        Overlay::Memory { data, cats, total, sel, q, cat, typing } => {
            let inner = dialog(f, area, 90, area.height.saturating_sub(4), &format!("Memory · {total} remembered"), &th);
            let catname = if *cat == 0 { "all kinds".to_string() } else { cats.get(*cat - 1).cloned().unwrap_or_default() };
            f.render_widget(Paragraph::new(Line::from(vec![
                Span::styled(" ⌕ ", Style::default().fg(th.muted)),
                Span::styled(if q.is_empty() && !*typing { "/ to search".to_string() } else { q.clone() }, Style::default().fg(if *typing { th.text } else { th.faint })),
                Span::styled(format!("   ◂ {catname} ▸  (Tab)"), Style::default().fg(th.muted)),
            ])), Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 });
            let list_h = inner.height.saturating_sub(3) as usize / 2;
            let start = sel.saturating_sub(list_h.saturating_sub(1));
            let mut lines = vec![];
            for (i, m) in data.iter().enumerate().skip(start).take(list_h) {
                let bg = if i == *sel { th.surface3 } else { th.surface };
                lines.push(Line::from(vec![
                    Span::styled(if m.pinned != 0 { " ⚲ " } else { "   " }, Style::default().fg(th.amber).bg(bg)),
                    Span::styled(trace::one_line(&m.fact, inner.width as usize - 4), Style::default().fg(th.text).bg(bg)),
                ]));
                let src = if m.source == "user" { "added by you".to_string() } else if m.source.starts_with("auto") { "learned automatically".into() } else if m.source.starts_with("tidy") { "merged by tidy-up".into() } else { format!("saved by {}", agent_who(&app.agents, m.source.trim_start_matches("agent:")).name) };
                lines.push(Line::from(Span::styled(format!("     {} · importance {} · {src} · used {}× · {}", m.category, m.importance, m.uses, time_ago(m.updated)), Style::default().fg(th.faint).bg(bg))));
            }
            if data.is_empty() {
                lines.push(Line::from(Span::styled(if *total == 0 { "  Nothing yet. Chat with your agents, or press a to add something." } else { "  Nothing matches." }, Style::default().fg(th.muted))));
            }
            f.render_widget(Paragraph::new(lines), Rect { x: inner.x, y: inner.y + 1, width: inner.width, height: inner.height.saturating_sub(2) });
            f.render_widget(Paragraph::new(Line::from(Span::styled(" a add · d delete · p pin · +/- importance · t tidy up · Esc close", Style::default().fg(th.faint)))), Rect { x: inner.x, y: inner.y + inner.height - 1, width: inner.width, height: 1 });
        }
        Overlay::Routines { items, sel } => {
            let inner = dialog(f, area, 90, (items.len() as u16 * 2 + 5).min(area.height.saturating_sub(4)).max(8), "Routines", &th);
            let mut lines = vec![];
            for (i, r) in items.iter().enumerate() {
                let bg = if i == *sel { th.surface3 } else { th.surface };
                let who = agent_who(&app.agents, &r.agent_id);
                lines.push(Line::from(vec![
                    Span::styled(if r.enabled { " [x] " } else { " [ ] " }, Style::default().fg(if r.enabled { th.accent } else { th.muted }).bg(bg)),
                    Span::styled(format!("{} ", who.emoji), Style::default().bg(bg)),
                    Span::styled(r.name.clone(), Style::default().fg(th.text).add_modifier(Modifier::BOLD).bg(bg)),
                ]));
                let next = if r.enabled { r.next_run.map(|t| format!("next {}", trace::clock(Some(t)))).unwrap_or("—".into()) } else { "off".into() };
                let last = r.last_run.map(|t| format!("last run {}{}", time_ago(t), r.last_status.as_ref().filter(|s| *s != "started").map(|s| format!(" ({s})")).unwrap_or_default())).unwrap_or("never run".into());
                lines.push(Line::from(Span::styled(format!("      {} · {} · {next} · {last}", schedule_text(&r.schedule), who.name), Style::default().fg(th.faint).bg(bg))));
            }
            if items.is_empty() {
                lines.push(Line::from(Span::styled("  No routines yet. Press n to create one; the templates are a good start.", Style::default().fg(th.muted))));
            }
            f.render_widget(Paragraph::new(lines), Rect { x: inner.x, y: inner.y, width: inner.width, height: inner.height.saturating_sub(1) });
            f.render_widget(Paragraph::new(Line::from(Span::styled(" n new · Enter edit · Space on/off · r run now · o open its chat · d delete · Esc close", Style::default().fg(th.faint)))), Rect { x: inner.x, y: inner.y + inner.height - 1, width: inner.width, height: 1 });
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
