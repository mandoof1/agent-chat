//! A small form widget for the editors (settings, agents, routines, memory, email, calendars):
//! text, multi-line, yes/no, choice and number fields, moved through with Tab/arrows and saved
//! with Ctrl+S.

use crate::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use serde_json::Value;
use tui_textarea::TextArea;

pub enum Field {
    Text { value: String, cursor: usize, secret: bool, placeholder: String },
    Multi { area: TextArea<'static>, rows: u16 },
    Bool { value: bool },
    Select { options: Vec<String>, labels: Vec<String>, index: usize },
    Note,
    Button { label: String },
}

pub struct Item {
    pub key: String,
    pub label: String,
    pub hint: String,
    pub field: Field,
}

pub struct Form {
    pub title: String,
    pub items: Vec<Item>,
    pub focus: usize,
    pub scroll: u16,
    pub actions: Vec<(String, String)>, // (key hint, label) shown in the footer
    pub danger_keys: Vec<String>,       // keys of bool fields that deserve a warning color
}

pub enum FormOut {
    None,
    Save,
    Cancel,
    Button(String),
}

impl Form {
    pub fn new(title: &str) -> Self {
        Form { title: title.into(), items: vec![], focus: 0, scroll: 0, actions: vec![("Ctrl+S".into(), "save".into()), ("Esc".into(), "cancel".into())], danger_keys: vec![] }
    }

    pub fn text(mut self, key: &str, label: &str, value: &str, hint: &str) -> Self {
        self.items.push(Item { key: key.into(), label: label.into(), hint: hint.into(), field: Field::Text { value: value.into(), cursor: value.chars().count(), secret: false, placeholder: String::new() } });
        self
    }
    pub fn placeholder(mut self, p: &str) -> Self {
        if let Some(Item { field: Field::Text { placeholder, .. }, .. }) = self.items.last_mut() {
            *placeholder = p.into();
        }
        self
    }
    pub fn secret(mut self, key: &str, label: &str, hint: &str) -> Self {
        self.items.push(Item { key: key.into(), label: label.into(), hint: hint.into(), field: Field::Text { value: String::new(), cursor: 0, secret: true, placeholder: String::new() } });
        self
    }
    pub fn multi(mut self, key: &str, label: &str, value: &str, rows: u16, hint: &str) -> Self {
        let mut area = TextArea::from(value.lines().map(String::from).collect::<Vec<_>>());
        area.set_cursor_line_style(Style::default());
        self.items.push(Item { key: key.into(), label: label.into(), hint: hint.into(), field: Field::Multi { area, rows } });
        self
    }
    pub fn bool(mut self, key: &str, label: &str, value: bool, hint: &str) -> Self {
        self.items.push(Item { key: key.into(), label: label.into(), hint: hint.into(), field: Field::Bool { value } });
        self
    }
    pub fn select(mut self, key: &str, label: &str, options: &[String], labels: &[String], current: &str, hint: &str) -> Self {
        let index = options.iter().position(|o| o == current).unwrap_or(0);
        self.items.push(Item { key: key.into(), label: label.into(), hint: hint.into(), field: Field::Select { options: options.to_vec(), labels: labels.to_vec(), index } });
        self
    }
    pub fn note(mut self, text: &str) -> Self {
        self.items.push(Item { key: String::new(), label: text.into(), hint: String::new(), field: Field::Note });
        self
    }
    pub fn button(mut self, key: &str, label: &str, hint: &str) -> Self {
        self.items.push(Item { key: key.into(), label: label.into(), hint: hint.into(), field: Field::Button { label: label.into() } });
        self
    }
    pub fn danger(mut self, key: &str) -> Self {
        self.danger_keys.push(key.into());
        self
    }

    pub fn get(&self, key: &str) -> Option<&Field> {
        self.items.iter().find(|i| i.key == key).map(|i| &i.field)
    }

    pub fn set_text(&mut self, key: &str, value: &str) {
        if let Some(Field::Text { value: v, cursor, .. }) = self.items.iter_mut().find(|i| i.key == key).map(|i| &mut i.field) {
            *v = value.into();
            *cursor = value.chars().count();
        }
    }

    pub fn set_bool(&mut self, key: &str, value: bool) {
        if let Some(Field::Bool { value: v }) = self.items.iter_mut().find(|i| i.key == key).map(|i| &mut i.field) {
            *v = value;
        }
    }

    pub fn string(&self, key: &str) -> String {
        match self.get(key) {
            Some(Field::Text { value, .. }) => value.clone(),
            Some(Field::Multi { area, .. }) => area.lines().join("\n"),
            Some(Field::Select { options, index, .. }) => options.get(*index).cloned().unwrap_or_default(),
            Some(Field::Bool { value }) => value.to_string(),
            _ => String::new(),
        }
    }
    pub fn boolean(&self, key: &str) -> bool {
        matches!(self.get(key), Some(Field::Bool { value: true }))
    }
    pub fn number(&self, key: &str) -> Option<f64> {
        self.string(key).trim().parse::<f64>().ok()
    }
    pub fn json_number(&self, key: &str) -> Value {
        match self.number(key) {
            Some(n) if n.fract() == 0.0 => Value::from(n as i64),
            Some(n) => Value::from(n),
            None => Value::Null,
        }
    }

    fn focusable(&self, i: usize) -> bool {
        !matches!(self.items[i].field, Field::Note)
    }

    fn step(&mut self, dir: i32) {
        if self.items.is_empty() {
            return;
        }
        let n = self.items.len() as i32;
        let mut i = self.focus as i32;
        for _ in 0..n {
            i = (i + dir).rem_euclid(n);
            if self.focusable(i as usize) {
                self.focus = i as usize;
                return;
            }
        }
    }

    pub fn handle(&mut self, key: KeyEvent) -> FormOut {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, ctrl) {
            (KeyCode::Esc, _) => return FormOut::Cancel,
            (KeyCode::Char('s'), true) => return FormOut::Save,
            (KeyCode::Tab, _) | (KeyCode::Down, _) if !self.in_multi() || key.code == KeyCode::Tab => {
                self.step(1);
                return FormOut::None;
            }
            (KeyCode::BackTab, _) | (KeyCode::Up, _) if !self.in_multi() || key.code == KeyCode::BackTab => {
                self.step(-1);
                return FormOut::None;
            }
            _ => {}
        }
        let Some(item) = self.items.get_mut(self.focus) else { return FormOut::None };
        let danger = self.danger_keys.contains(&item.key);
        let _ = danger;
        match &mut item.field {
            Field::Text { value, cursor, .. } => match key.code {
                KeyCode::Char(c) if !ctrl => {
                    let mut chars: Vec<char> = value.chars().collect();
                    chars.insert(*cursor, c);
                    *value = chars.into_iter().collect();
                    *cursor += 1;
                }
                KeyCode::Backspace => {
                    if *cursor > 0 {
                        let mut chars: Vec<char> = value.chars().collect();
                        chars.remove(*cursor - 1);
                        *value = chars.into_iter().collect();
                        *cursor -= 1;
                    }
                }
                KeyCode::Delete => {
                    let mut chars: Vec<char> = value.chars().collect();
                    if *cursor < chars.len() {
                        chars.remove(*cursor);
                        *value = chars.into_iter().collect();
                    }
                }
                KeyCode::Left => *cursor = cursor.saturating_sub(1),
                KeyCode::Right => *cursor = (*cursor + 1).min(value.chars().count()),
                KeyCode::Home => *cursor = 0,
                KeyCode::End => *cursor = value.chars().count(),
                KeyCode::Char('u') if ctrl => {
                    value.clear();
                    *cursor = 0;
                }
                KeyCode::Enter => return FormOut::Save,
                _ => {}
            },
            Field::Multi { area, .. } => {
                area.input(key);
            }
            Field::Bool { value } => match key.code {
                KeyCode::Char(' ') | KeyCode::Enter | KeyCode::Left | KeyCode::Right => *value = !*value,
                _ => {}
            },
            Field::Select { options, index, .. } => match key.code {
                KeyCode::Right | KeyCode::Char(' ') | KeyCode::Enter => *index = (*index + 1) % options.len().max(1),
                KeyCode::Left => *index = (*index + options.len().max(1) - 1) % options.len().max(1),
                _ => {}
            },
            Field::Button { .. } => {
                if matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')) {
                    return FormOut::Button(item.key.clone());
                }
            }
            Field::Note => {}
        }
        FormOut::None
    }

    fn in_multi(&self) -> bool {
        matches!(self.items.get(self.focus).map(|i| &i.field), Some(Field::Multi { .. }))
    }

    /// Draw the form as a centered dialog.
    pub fn render(&mut self, f: &mut Frame, area: Rect, th: &Theme) {
        let w = area.width.min(84).max(40);
        let h = area.height.saturating_sub(2).max(10);
        let rect = Rect { x: area.x + (area.width.saturating_sub(w)) / 2, y: area.y + (area.height.saturating_sub(h)) / 2, width: w, height: h };
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(th.line))
            .title(Span::styled(format!(" {} ", self.title), Style::default().fg(th.text).add_modifier(Modifier::BOLD)))
            .style(Style::default().bg(th.surface));
        let inner = block.inner(rect);
        f.render_widget(block, rect);

        // layout rows: each item takes label line + field lines
        let mut rows: Vec<(usize, u16)> = vec![]; // (item index, height)
        for (i, it) in self.items.iter().enumerate() {
            let hgt = match &it.field {
                Field::Multi { rows, .. } => rows + 1,
                Field::Note => (it.label.chars().count() as u16 / inner.width.max(1)) + 1,
                Field::Bool { .. } | Field::Button { .. } => 1,
                _ => 2,
            };
            rows.push((i, hgt));
        }
        let footer_h = 1;
        let body_h = inner.height.saturating_sub(footer_h);
        // keep the focused item visible
        let mut y_of = vec![0u16; self.items.len()];
        let mut acc = 0u16;
        for (i, h) in &rows {
            y_of[*i] = acc;
            acc += h;
        }
        let total = acc;
        let fy = y_of.get(self.focus).copied().unwrap_or(0);
        let fh = rows.get(self.focus).map(|r| r.1).unwrap_or(2);
        if fy < self.scroll {
            self.scroll = fy;
        } else if fy + fh > self.scroll + body_h {
            self.scroll = (fy + fh).saturating_sub(body_h);
        }
        if total <= body_h {
            self.scroll = 0;
        }

        for (i, h) in rows {
            let y = y_of[i] as i32 - self.scroll as i32;
            if y + (h as i32) <= 0 || y >= body_h as i32 {
                continue;
            }
            let focused = i == self.focus;
            let it = &mut self.items[i];
            let top = inner.y + y.max(0) as u16;
            let avail = (body_h as i32 - y).max(0) as u16;
            let label_style = if focused { Style::default().fg(th.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.muted) };
            match &mut it.field {
                Field::Note => {
                    let p = Paragraph::new(Span::styled(it.label.clone(), Style::default().fg(th.muted))).wrap(ratatui::widgets::Wrap { trim: true });
                    f.render_widget(p, Rect { x: inner.x, y: top, width: inner.width, height: h.min(avail) });
                }
                Field::Text { value, cursor, secret, placeholder } => {
                    if y >= 0 {
                        let mut l = vec![Span::styled(it.label.clone(), label_style)];
                        if !it.hint.is_empty() {
                            l.push(Span::styled(format!("  {}", it.hint), Style::default().fg(th.faint)));
                        }
                        f.render_widget(Paragraph::new(Line::from(l)), Rect { x: inner.x, y: top, width: inner.width, height: 1 });
                    }
                    if avail >= 2 {
                        let shown: String = if *secret { "•".repeat(value.chars().count()) } else { value.clone() };
                        let style = Style::default().fg(th.text).bg(if focused { th.surface3 } else { th.surface2 });
                        let text = if shown.is_empty() && !placeholder.is_empty() { Span::styled(placeholder.clone(), style.fg(th.faint)) } else { Span::styled(shown.clone(), style) };
                        let w = inner.width.min(72);
                        f.render_widget(Paragraph::new(Line::from(text)).style(style), Rect { x: inner.x + 1, y: top + 1, width: w, height: 1 });
                        if focused {
                            let cx = (*cursor as u16).min(w.saturating_sub(1));
                            f.set_cursor_position((inner.x + 1 + cx, top + 1));
                        }
                    }
                }
                Field::Multi { area, rows } => {
                    if y >= 0 {
                        let mut l = vec![Span::styled(it.label.clone(), label_style)];
                        if !it.hint.is_empty() {
                            l.push(Span::styled(format!("  {}", it.hint), Style::default().fg(th.faint)));
                        }
                        f.render_widget(Paragraph::new(Line::from(l)), Rect { x: inner.x, y: top, width: inner.width, height: 1 });
                    }
                    let hgt = (*rows).min(avail.saturating_sub(1));
                    if hgt > 0 {
                        area.set_style(Style::default().fg(th.text).bg(if focused { th.surface3 } else { th.surface2 }));
                        area.set_cursor_style(if focused { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() });
                        f.render_widget(&*area, Rect { x: inner.x + 1, y: top + 1, width: inner.width.saturating_sub(2), height: hgt });
                    }
                }
                Field::Bool { value } => {
                    let box_ = if *value { "[x]" } else { "[ ]" };
                    let mut l = vec![Span::styled(format!("{box_} "), Style::default().fg(if *value { th.accent } else { th.muted })), Span::styled(it.label.clone(), label_style.fg(if focused { th.accent } else { th.text }))];
                    if !it.hint.is_empty() {
                        l.push(Span::styled(format!("  {}", it.hint), Style::default().fg(th.faint)));
                    }
                    f.render_widget(Paragraph::new(Line::from(l)).wrap(ratatui::widgets::Wrap { trim: true }), Rect { x: inner.x, y: top, width: inner.width, height: h.min(avail) });
                }
                Field::Select { labels, index, .. } => {
                    if y >= 0 {
                        let mut l = vec![Span::styled(it.label.clone(), label_style)];
                        if !it.hint.is_empty() {
                            l.push(Span::styled(format!("  {}", it.hint), Style::default().fg(th.faint)));
                        }
                        f.render_widget(Paragraph::new(Line::from(l)), Rect { x: inner.x, y: top, width: inner.width, height: 1 });
                    }
                    if avail >= 2 {
                        let cur = labels.get(*index).cloned().unwrap_or_default();
                        let l = Line::from(vec![Span::styled("◂ ", Style::default().fg(th.faint)), Span::styled(cur, Style::default().fg(th.text)), Span::styled(" ▸", Style::default().fg(th.faint))]);
                        f.render_widget(Paragraph::new(l), Rect { x: inner.x + 1, y: top + 1, width: inner.width.saturating_sub(2), height: 1 });
                    }
                }
                Field::Button { label } => {
                    let style = if focused { Style::default().fg(th.bg).bg(th.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.text).bg(th.surface3) };
                    let mut l = vec![Span::styled(format!(" {label} "), style)];
                    if !it.hint.is_empty() {
                        l.push(Span::styled(format!("  {}", it.hint), Style::default().fg(th.faint)));
                    }
                    f.render_widget(Paragraph::new(Line::from(l)), Rect { x: inner.x, y: top, width: inner.width, height: 1 });
                }
            }
        }
        let mut foot = vec![];
        for (k, l) in &self.actions {
            foot.push(Span::styled(format!(" {k} "), Style::default().fg(th.text).bg(th.surface3)));
            foot.push(Span::styled(format!(" {l}  "), Style::default().fg(th.muted)));
        }
        foot.push(Span::styled(" Tab ", Style::default().fg(th.text).bg(th.surface3)));
        foot.push(Span::styled(" next field  ", Style::default().fg(th.muted)));
        f.render_widget(Paragraph::new(Line::from(foot)), Rect { x: inner.x, y: inner.y + inner.height.saturating_sub(1), width: inner.width, height: 1 });
    }
}
