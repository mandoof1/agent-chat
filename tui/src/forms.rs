//! A small form widget for the editors (settings, agents, routines, memory, email, calendars):
//! text, multi-line, yes/no, choice and number fields, moved through with Tab/arrows and saved
//! with Ctrl+S.

use crate::text::{self, line as clean};
use crate::theme::Theme;
use crate::ui::render;
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
        // the text area draws what it holds as is, so no escapes or stray CRs in it
        let mut area = TextArea::from(text::normalize(value).lines().map(String::from).collect::<Vec<_>>());
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
        self.settle();
        let Some(item) = self.items.get_mut(self.focus) else { return FormOut::None };
        match &mut item.field {
            Field::Text { value, cursor, .. } => {
                if key.code == KeyCode::Enter {
                    return FormOut::Save;
                }
                edit_line(value, cursor, key);
            }
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

    /// Pasted text into the focused field: one line for a text field, as is for a multi-line one.
    pub fn paste(&mut self, pasted: &str) {
        self.settle();
        match self.items.get_mut(self.focus).map(|i| &mut i.field) {
            Some(Field::Text { value, cursor, .. }) => insert_line(value, cursor, pasted),
            Some(Field::Multi { area, .. }) => {
                area.insert_str(pasted);
            }
            _ => {}
        }
    }

    fn in_multi(&self) -> bool {
        matches!(self.items.get(self.focus).map(|i| &i.field), Some(Field::Multi { .. }))
    }

    /// Off a note onto the first field there is: a form that opens with a note (Email, Calendars)
    /// would otherwise take no typing until Tab.
    fn settle(&mut self) {
        if self.items.get(self.focus).is_some_and(|i| matches!(i.field, Field::Note)) {
            self.step(1);
        }
    }

    /// Focus a field by its key.
    pub fn focus_on(&mut self, key: &str) {
        if let Some(i) = self.items.iter().position(|i| i.key == key) {
            self.focus = i;
        }
    }

    /// Draw the form as a centered dialog.
    pub fn render(&mut self, f: &mut Frame, area: Rect, th: &Theme) {
        self.settle();
        let w = area.width.min(84).max(40).min(area.width);
        let h = area.height.saturating_sub(2).max(10).min(area.height);
        let rect = Rect { x: area.x + (area.width.saturating_sub(w)) / 2, y: area.y + (area.height.saturating_sub(h)) / 2, width: w, height: h };
        render(f, Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(th.line))
            .title(Span::styled(format!(" {} ", clean(&self.title)), Style::default().fg(th.text).add_modifier(Modifier::BOLD)))
            .style(Style::default().bg(th.surface));
        let inner = block.inner(rect);
        render(f, block, rect);

        // layout rows: each item takes label line + field lines
        let mut rows: Vec<(usize, u16)> = vec![]; // (item index, height)
        for (i, it) in self.items.iter().enumerate() {
            let hgt = match &it.field {
                Field::Multi { rows, .. } => rows + 1,
                Field::Note => (text::width(&it.label) as u16 / inner.width.max(1)) + 1,
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
        // (a field taller than the room shows from its top: moving to its bottom would put the
        // top above the room, and the next frame would move back, flickering every frame)
        if fy < self.scroll || fh > body_h {
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
            let danger = self.danger_keys.contains(&self.items[i].key);
            let it = &mut self.items[i];
            let top = inner.y + y.max(0) as u16;
            let avail = (body_h as i32 - y).max(0) as u16;
            let label_style = if focused { Style::default().fg(th.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.muted) };
            match &mut it.field {
                Field::Note => {
                    let p = Paragraph::new(Span::styled(clean(&it.label), Style::default().fg(th.muted))).wrap(ratatui::widgets::Wrap { trim: true });
                    render(f, p, Rect { x: inner.x, y: top, width: inner.width, height: h.min(avail) });
                }
                Field::Text { value, cursor, secret, placeholder } => {
                    if y >= 0 {
                        let mut l = vec![Span::styled(clean(&it.label), label_style)];
                        if !it.hint.is_empty() {
                            l.push(Span::styled(format!("  {}", clean(&it.hint)), Style::default().fg(th.faint)));
                        }
                        render(f, Paragraph::new(Line::from(l)), Rect { x: inner.x, y: top, width: inner.width, height: 1 });
                    }
                    if avail >= 2 {
                        let w = inner.width.saturating_sub(2).min(72); // (one column in from each edge)
                        let (shown, cx) = line_window(value, *cursor, w as usize, *secret);
                        let style = Style::default().fg(th.text).bg(if focused { th.surface3 } else { th.surface2 });
                        let text = if value.is_empty() && !placeholder.is_empty() { Span::styled(placeholder.clone(), style.fg(th.faint)) } else { Span::styled(shown, style) };
                        render(f, Paragraph::new(Line::from(text)).style(style), Rect { x: inner.x + 1, y: top + 1, width: w, height: 1 });
                        if focused {
                            f.set_cursor_position((inner.x + 1 + cx, top + 1));
                        }
                    }
                }
                Field::Multi { area, rows } => {
                    if y >= 0 {
                        let mut l = vec![Span::styled(clean(&it.label), label_style)];
                        if !it.hint.is_empty() {
                            l.push(Span::styled(format!("  {}", clean(&it.hint)), Style::default().fg(th.faint)));
                        }
                        render(f, Paragraph::new(Line::from(l)), Rect { x: inner.x, y: top, width: inner.width, height: 1 });
                    }
                    let hgt = (*rows).min(avail.saturating_sub(1));
                    if hgt > 0 {
                        area.set_style(Style::default().fg(th.text).bg(if focused { th.surface3 } else { th.surface2 }));
                        area.set_cursor_style(if focused { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() });
                        render(f, &*area, Rect { x: inner.x + 1, y: top + 1, width: inner.width.saturating_sub(2), height: hgt });
                    }
                }
                Field::Bool { value } => {
                    let box_ = if *value { "[x]" } else { "[ ]" };
                    // a risky switch that is on (bypass permissions) shows in amber
                    let on = if danger { th.amber } else { th.accent };
                    let mut l = vec![Span::styled(format!("{box_} "), Style::default().fg(if *value { on } else { th.muted })), Span::styled(clean(&it.label), label_style.fg(if danger && *value { th.amber } else if focused { th.accent } else { th.text }))];
                    if !it.hint.is_empty() {
                        l.push(Span::styled(format!("  {}", clean(&it.hint)), Style::default().fg(th.faint)));
                    }
                    render(f, Paragraph::new(Line::from(l)).wrap(ratatui::widgets::Wrap { trim: true }), Rect { x: inner.x, y: top, width: inner.width, height: h.min(avail) });
                }
                Field::Select { labels, index, .. } => {
                    if y >= 0 {
                        let mut l = vec![Span::styled(clean(&it.label), label_style)];
                        if !it.hint.is_empty() {
                            l.push(Span::styled(format!("  {}", clean(&it.hint)), Style::default().fg(th.faint)));
                        }
                        render(f, Paragraph::new(Line::from(l)), Rect { x: inner.x, y: top, width: inner.width, height: 1 });
                    }
                    if avail >= 2 {
                        let cur = clean(&labels.get(*index).cloned().unwrap_or_default());
                        let l = Line::from(vec![Span::styled("◂ ", Style::default().fg(th.faint)), Span::styled(cur, Style::default().fg(th.text)), Span::styled(" ▸", Style::default().fg(th.faint))]);
                        render(f, Paragraph::new(l), Rect { x: inner.x + 1, y: top + 1, width: inner.width.saturating_sub(2), height: 1 });
                    }
                }
                Field::Button { label } => {
                    let style = if focused { Style::default().fg(th.bg).bg(th.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.text).bg(th.surface3) };
                    let mut l = vec![Span::styled(format!(" {} ", clean(label)), style)];
                    if !it.hint.is_empty() {
                        l.push(Span::styled(format!("  {}", clean(&it.hint)), Style::default().fg(th.faint)));
                    }
                    render(f, Paragraph::new(Line::from(l)), Rect { x: inner.x, y: top, width: inner.width, height: 1 });
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
        render(f, Paragraph::new(Line::from(foot)), Rect { x: inner.x, y: inner.y + inner.height.saturating_sub(1), width: inner.width, height: 1 });
    }
}

/// One-line editing at a cursor (counted in characters), shared by form text fields and prompts:
/// typing inserts, ←→ Home End move, Backspace and Delete remove, Ctrl+U clears. False for a key
/// it doesn't use.
pub fn edit_line(value: &mut String, cursor: &mut usize, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let mut chars: Vec<char> = value.chars().collect();
    *cursor = (*cursor).min(chars.len());
    match key.code {
        KeyCode::Char('u') if ctrl => {
            chars.clear();
            *cursor = 0;
        }
        KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
            chars.insert(*cursor, c);
            *cursor += 1;
        }
        KeyCode::Backspace if *cursor > 0 => {
            chars.remove(*cursor - 1);
            *cursor -= 1;
        }
        KeyCode::Delete if *cursor < chars.len() => {
            chars.remove(*cursor);
        }
        KeyCode::Left => *cursor = cursor.saturating_sub(1),
        KeyCode::Right => *cursor = (*cursor + 1).min(chars.len()),
        KeyCode::Home => *cursor = 0,
        KeyCode::End => *cursor = chars.len(),
        KeyCode::Backspace | KeyCode::Delete => {}
        _ => return false,
    }
    *value = chars.into_iter().collect();
    true
}

/// Pasted text into a one-line value at the cursor, its line breaks and tabs as spaces.
pub fn insert_line(value: &mut String, cursor: &mut usize, pasted: &str) {
    let row: Vec<char> = pasted.trim_end_matches('\n').chars().map(|c| if c == '\n' || c == '\t' { ' ' } else { c }).collect();
    let mut chars: Vec<char> = value.chars().collect();
    let at = (*cursor).min(chars.len());
    chars.splice(at..at, row.iter().copied());
    *value = chars.into_iter().collect();
    *cursor = at + row.len();
}

/// What a one-line value shows in `width` columns with the cursor in view, and the cursor's
/// column: the start while the cursor fits, else scrolled so the cursor sits at the right edge.
/// Cleaned (or dotted, for a secret) on either side of the cursor, so it lands where it shows.
pub fn line_window(value: &str, cursor: usize, width: usize, secret: bool) -> (String, u16) {
    let show = |s: String| if secret { "•".repeat(s.chars().count()) } else { clean(&s) };
    let before = show(value.chars().take(cursor).collect());
    let after = show(value.chars().skip(cursor).collect());
    let cols = |c: char| text::width(c.encode_utf8(&mut [0; 4]));
    let mut used = 0;
    let mut shown = String::new();
    // the end of the text before the cursor, as much as fits with a column left for the cursor
    let mut tail: Vec<char> = vec![];
    for c in before.chars().rev() {
        if used + cols(c) >= width.max(1) {
            break;
        }
        used += cols(c);
        tail.push(c);
    }
    shown.extend(tail.into_iter().rev());
    let at = used;
    for c in after.chars() {
        if used + cols(c) > width {
            break;
        }
        used += cols(c);
        shown.push(c);
    }
    (shown, at as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn forms_start_on_a_field_not_a_note() {
        let mut form = Form::new("Calendars").note("about calendars").text("name", "Name", "", "").text("url", "URL", "", "");
        form.handle(key(KeyCode::Char('T')));
        assert_eq!(form.string("name"), "T", "typing reaches the first field");
        let mut form = Form::new("x").note("n").text("a", "A", "", "");
        form.paste("pasted");
        assert_eq!(form.string("a"), "pasted");
        // Tab still skips notes and wraps around
        let mut form = Form::new("t").text("a", "A", "xy", "").note("n").bool("b", "B", false, "");
        form.handle(key(KeyCode::Tab));
        assert_eq!(form.items[form.focus].key, "b");
        form.handle(key(KeyCode::Tab));
        assert_eq!(form.items[form.focus].key, "a");
    }

    #[test]
    fn a_risky_switch_that_is_on_shows_in_amber() {
        use ratatui::backend::TestBackend;
        let th = Theme::dark();
        for on in [true, false] {
            let mut form = Form::new("Settings").text("url", "URL", "", "").bool("bypass", "Bypass permissions", on, "").danger("bypass").bool("other", "Other switch", true, "");
            let mut term = ratatui::Terminal::new(TestBackend::new(90, 20)).unwrap();
            term.draw(|f| form.render(f, f.area(), &th)).unwrap();
            let buf = term.backend().buffer();
            let fg_of = |label: &str| {
                let (x, y) = (0..20u16).flat_map(|y| (0..90u16).map(move |x| (x, y))).find(|&(x, y)| (x..x + label.len() as u16).enumerate().all(|(i, cx)| cx < 90 && buf[(cx, y)].symbol() == &label[i..i + 1])).expect(label);
                buf[(x, y)].fg
            };
            assert_eq!(fg_of("Bypass permissions") == th.amber, on, "the risky switch, on: {on}");
            assert_ne!(fg_of("Other switch"), th.amber, "an ordinary one that is on");
        }
    }

    #[test]
    fn one_line_editing_moves_the_cursor() {
        let (mut v, mut c) = ("abc".to_string(), 3);
        for k in [KeyCode::Left, KeyCode::Left, KeyCode::Char('X')] {
            edit_line(&mut v, &mut c, key(k));
        }
        assert_eq!((v.as_str(), c), ("aXbc", 2));
        edit_line(&mut v, &mut c, key(KeyCode::Home));
        edit_line(&mut v, &mut c, key(KeyCode::Char('é')));
        edit_line(&mut v, &mut c, key(KeyCode::Delete));
        assert_eq!((v.as_str(), c), ("éXbc", 1));
        edit_line(&mut v, &mut c, key(KeyCode::End));
        edit_line(&mut v, &mut c, key(KeyCode::Backspace));
        assert_eq!((v.as_str(), c), ("éXb", 3));
        insert_line(&mut v, &mut c, "1\n2\t3\n");
        assert_eq!(v, "éXb1 2 3");
        assert!(!edit_line(&mut v, &mut c, key(KeyCode::Enter)));
        edit_line(&mut v, &mut c, KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!((v.as_str(), c), ("", 0));
    }

    #[test]
    fn long_values_scroll_to_keep_the_cursor_in_view() {
        let long = "x".repeat(100) + "END";
        let (shown, at) = line_window(&long, 103, 72, false);
        assert!(shown.ends_with("END") && shown.chars().count() == 71, "{shown}");
        assert_eq!(at, 71, "the cursor right after the last character");
        let (shown, at) = line_window(&long, 0, 72, false);
        assert_eq!((shown.chars().count(), at), (72, 0));
        // the cursor lands where the cleaned text puts it (a tab is spaces on screen)
        assert_eq!(line_window("a\tb", 2, 72, false), ("a   b".to_string(), 4));
        assert_eq!(line_window("pw", 2, 72, true), ("••".to_string(), 2));
        // wide characters count their columns
        let (shown, at) = line_window(&"日".repeat(50), 50, 20, false);
        assert_eq!((shown.chars().count(), at), (9, 18));
    }

    #[test]
    fn text_fields_stay_inside_the_dialog() {
        use ratatui::backend::TestBackend;
        let th = Theme::dark();
        for w in [24u16, 40, 60, 74, 76, 100] {
            let mut form = Form::new("Settings").text("url", "Model server URL", &"http://example.com/".repeat(8), "");
            let mut term = ratatui::Terminal::new(TestBackend::new(w, 12)).unwrap();
            term.draw(|f| form.render(f, f.area(), &th)).unwrap();
            let buf = term.backend().buffer();
            let top = (0..12).find(|&y| (0..w).any(|x| buf[(x, y)].symbol() == "┌")).expect("the dialog");
            let right = (0..w).rev().find(|&x| buf[(x, top)].symbol() == "┐").expect("the dialog's corner");
            let left = (0..w).find(|&x| buf[(x, top)].symbol() == "┌").unwrap();
            for y in top + 1..11 {
                assert!(buf[(right, y)].symbol() == "│" || buf[(right, y)].symbol() == "┘", "{w}: the field ran over the border on row {y}");
            }
            // the value shows from one column in to one column short of the border (or 72 wide)
            let field = (top + 1..11).find(|&y| buf[(left + 2, y)].bg == th.surface3).expect("the value's row");
            let filled = (left..=right).filter(|&x| buf[(x, field)].bg == th.surface3).count();
            assert_eq!(filled as u16, (right - left - 3).min(72), "{w}");
        }
    }

    use serde_json::json;

    fn with(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    fn out_name(o: FormOut) -> String {
        match o {
            FormOut::None => "none".into(),
            FormOut::Save => "save".into(),
            FormOut::Cancel => "cancel".into(),
            FormOut::Button(k) => format!("button:{k}"),
        }
    }

    /// One of every field kind, the way the editors build them.
    fn every_kind() -> Form {
        let opts: Vec<String> = ["", "a-model", "b-model"].iter().map(|s| s.to_string()).collect();
        let labels: Vec<String> = ["Default", "A", "B"].iter().map(|s| s.to_string()).collect();
        Form::new("All")
            .note("A note first, as Email and Calendars have")
            .text("name", "Name", "Bob", "")
            .placeholder("e.g. Ann")
            .secret("pw", "Password", "saved; leave empty to keep")
            .multi("prompt", "Prompt", "line one\r\nline\ttwo \x1b[31mred\x1b[0m", 4, "")
            .bool("on", "Switch", false, "")
            .select("model", "Model", &opts, &labels, "b-model", "")
            .button("go", "Go", "")
            .text("num", "Number", "", "")
    }

    #[test]
    fn every_field_kind_takes_its_keys_and_gives_its_value() {
        let mut f = every_kind();
        let none = KeyModifiers::NONE;
        // starts off the note, on the first field; values as given (the multi-line one cleaned)
        assert_eq!(out_name(f.handle(with(KeyCode::End, none))), "none");
        assert_eq!(f.items[f.focus].key, "name");
        assert_eq!((f.string("name"), f.string("pw"), f.string("prompt"), f.string("on"), f.string("model")), ("Bob".into(), "".into(), "line one\nline\ttwo red".into(), "false".into(), "b-model".into()));
        // text: typing at the cursor, the editing keys, Enter saves
        for k in [KeyCode::Home, KeyCode::Right, KeyCode::Char('é'), KeyCode::Char('日'), KeyCode::End, KeyCode::Backspace, KeyCode::Left, KeyCode::Delete] {
            f.handle(with(k, none));
        }
        assert_eq!(f.string("name"), "Bé日");
        f.handle(with(KeyCode::Char('x'), KeyModifiers::ALT));
        f.handle(with(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(f.string("name"), "Bé日", "Alt and Ctrl letters are not text");
        f.handle(with(KeyCode::Char('U'), KeyModifiers::SHIFT));
        assert_eq!(f.string("name"), "Bé日U", "Shift is");
        assert_eq!(out_name(f.handle(with(KeyCode::Enter, none))), "save");
        f.handle(with(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(f.string("name"), "");
        // the secret: typed like text, kept as typed
        f.handle(with(KeyCode::Tab, none));
        for c in "p@ss 日".chars() {
            f.handle(with(KeyCode::Char(c), none));
        }
        f.paste("\tword\n");
        assert_eq!(f.string("pw"), "p@ss 日 word", "a paste into one line: tabs and breaks as spaces, the last break dropped");
        // multi-line: Enter is a line break, ↑↓ move inside it, Tab leaves, Esc and Ctrl+S still work
        f.handle(with(KeyCode::Down, none));
        assert_eq!(f.items[f.focus].key, "prompt");
        f.handle(with(KeyCode::End, none));
        f.handle(with(KeyCode::Enter, none));
        f.handle(with(KeyCode::Char('3'), none));
        f.handle(with(KeyCode::Up, none));
        f.handle(with(KeyCode::Down, none));
        assert_eq!(f.items[f.focus].key, "prompt", "↑↓ stay in the text");
        f.paste("four\nfive");
        assert_eq!(f.string("prompt"), "line one\n3four\nfive\nline\ttwo red");
        assert_eq!(out_name(f.handle(with(KeyCode::Char('s'), KeyModifiers::CONTROL))), "save");
        assert_eq!(out_name(f.handle(with(KeyCode::Esc, none))), "cancel");
        // yes/no: Space, Enter, ← and → flip it, other keys don't
        f.handle(with(KeyCode::Tab, none));
        assert_eq!(f.items[f.focus].key, "on");
        for (k, want) in [(KeyCode::Char(' '), true), (KeyCode::Enter, false), (KeyCode::Left, true), (KeyCode::Right, false), (KeyCode::Char('y'), false), (KeyCode::Backspace, false)] {
            assert_eq!(out_name(f.handle(with(k, none))), "none");
            assert_eq!(f.boolean("on"), want, "{k:?}");
        }
        f.set_bool("on", true);
        assert_eq!(f.string("on"), "true");
        // choice: → Space Enter go on (around the end), ← back; the value is the option, not its label
        f.handle(with(KeyCode::Tab, none));
        for (k, want) in [(KeyCode::Right, ""), (KeyCode::Char(' '), "a-model"), (KeyCode::Enter, "b-model"), (KeyCode::Left, "a-model"), (KeyCode::Left, ""), (KeyCode::Left, "b-model"), (KeyCode::Char('x'), "b-model")] {
            assert_eq!(out_name(f.handle(with(k, none))), "none");
            assert_eq!(f.string("model"), want, "{k:?}");
        }
        // a button: Enter or Space press it, nothing else does
        f.handle(with(KeyCode::Tab, none));
        assert_eq!(out_name(f.handle(with(KeyCode::Char('x'), none))), "none");
        assert_eq!(out_name(f.handle(with(KeyCode::Enter, none))), "button:go");
        assert_eq!(out_name(f.handle(with(KeyCode::Char(' '), none))), "button:go");
        // around the end and back, never onto the note
        f.handle(with(KeyCode::Tab, none));
        f.handle(with(KeyCode::Tab, none));
        assert_eq!(f.items[f.focus].key, "name");
        f.handle(with(KeyCode::BackTab, none));
        f.handle(with(KeyCode::Up, none));
        assert_eq!(f.items[f.focus].key, "go");
        // set_text puts the cursor at the end; a key no field has changes nothing
        f.set_text("num", "12");
        f.focus_on("num");
        f.handle(with(KeyCode::Char('3'), none));
        assert_eq!(f.string("num"), "123");
        f.focus_on("nope");
        assert_eq!(f.items[f.focus].key, "num");
        assert_eq!(f.string("nope"), "");
        assert!(f.get("nope").is_none() && !f.boolean("nope") && f.number("nope").is_none());
    }

    #[test]
    fn numbers_go_to_the_server_as_json_numbers() {
        let mut f = Form::new("n").text("v", "V", "", "");
        for (typed, want) in [("42", json!(42)), (" 7 ", json!(7)), ("4.5", json!(4.5)), ("-3", json!(-3)), ("1e3", json!(1000)), ("0", json!(0)), ("131072", json!(131072)), ("", Value::Null), ("abc", Value::Null), ("1,000", Value::Null), ("NaN", Value::Null), ("inf", Value::Null)] {
            f.set_text("v", typed);
            assert_eq!(f.json_number("v"), want, "{typed:?}");
        }
        // a multi-line or yes/no field has no number
        let f = Form::new("n").multi("m", "M", "12", 2, "").bool("b", "B", true, "");
        assert_eq!((f.number("m"), f.number("b")), (Some(12.0), None));
    }

    #[test]
    fn odd_forms_take_keys_without_trouble() {
        let none = KeyModifiers::NONE;
        // nothing in it, only notes, a choice with no options, an unknown current choice
        let mut empty = Form::new("e");
        for k in [KeyCode::Tab, KeyCode::BackTab, KeyCode::Enter, KeyCode::Char('x')] {
            assert_eq!(out_name(empty.handle(with(k, none))), "none");
        }
        empty.paste("x");
        let mut notes = Form::new("n").note("one").note("two");
        notes.handle(with(KeyCode::Tab, none));
        notes.handle(with(KeyCode::Char('x'), none));
        assert_eq!(notes.focus, 0);
        let mut f = Form::new("s").select("s", "S", &[], &[], "", "");
        for k in [KeyCode::Left, KeyCode::Right, KeyCode::Enter] {
            f.handle(with(k, none));
        }
        assert_eq!(f.string("s"), "");
        let opts = vec!["x".to_string(), "y".to_string()];
        assert_eq!(Form::new("s").select("s", "S", &opts, &opts, "zzz", "").string("s"), "x", "an unknown value starts on the first");
        // a multi-line field from nothing has no lines to lose
        assert_eq!(Form::new("m").multi("m", "M", "", 3, "").string("m"), "");
        // the draw survives every size, every kind focused
        let th = Theme::dark();
        let mut f = every_kind();
        for focus in 0..f.items.len() {
            f.focus = focus;
            for (w, h) in [(1, 1), (10, 3), (39, 9), (40, 10), (84, 30), (200, 5)] {
                let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
                term.draw(|fr| f.render(fr, fr.area(), &th)).unwrap();
            }
        }
    }

    fn screen(term: &ratatui::Terminal<ratatui::backend::TestBackend>) -> Vec<String> {
        let buf = term.backend().buffer();
        (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()).collect()
    }

    #[test]
    fn each_field_kind_draws_its_value_and_the_cursor_sits_in_the_focused_one() {
        use ratatui::backend::TestBackend;
        let th = Theme::dark();
        let mut f = every_kind();
        f.set_bool("on", true);
        f.paste("x"); // settles onto "name": "Bobx"
        f.focus_on("pw");
        f.paste("hunter2");
        let mut term = ratatui::Terminal::new(TestBackend::new(84, 34)).unwrap();
        term.draw(|fr| f.render(fr, fr.area(), &th)).unwrap();
        let rows = screen(&term);
        let all = rows.join("\n");
        for want in [" All ", "A note first", "Name", " Bobx", "Password  saved; leave empty to keep", " •••••••", "Prompt", "line one", "[x] Switch", "◂ B ▸", " Go ", "Ctrl+S  save", "Esc  cancel", "Tab  next field"] {
            assert!(all.contains(want), "{want:?}:\n{all}");
        }
        assert!(!all.contains("hunter2"), "a secret never shows");
        // the cursor: after the dots of the focused secret
        let y = rows.iter().position(|r| r.contains("•••••••")).unwrap() as u16;
        let x = rows[y as usize].chars().position(|c| c == '•').unwrap() as u16;
        term.backend_mut().assert_cursor_position((x + 7, y));
        // an empty text field shows its placeholder
        f.set_text("name", "");
        term.draw(|fr| f.render(fr, fr.area(), &th)).unwrap();
        assert!(screen(&term).join("\n").contains("e.g. Ann"));
        // a form taller than the screen scrolls to keep the focused field in view
        let mut long = Form::new("Long");
        for i in 0..40 {
            long = long.text(&format!("k{i}"), &format!("Field {i}"), &format!("v{i}"), "");
        }
        long.focus_on("k39");
        let mut term = ratatui::Terminal::new(TestBackend::new(60, 14)).unwrap();
        term.draw(|fr| long.render(fr, fr.area(), &th)).unwrap();
        let all = screen(&term).join("\n");
        assert!(all.contains("Field 39") && all.contains("v39") && !all.contains("Field 0 "), "{all}");
        long.focus_on("k0");
        term.draw(|fr| long.render(fr, fr.area(), &th)).unwrap();
        assert!(screen(&term).join("\n").contains("Field 0"), "and back up");
    }

    #[test]
    fn a_one_line_window_keeps_the_cursor_in_view_at_any_width() {
        let mut rng = crate::text::tests::Rng(99);
        for case in 0..2000 {
            let v = crate::text::tests::random_text(&mut rng, 12);
            let n = v.chars().count();
            let cursor = rng.below(n + 2);
            let width = rng.below(20);
            for secret in [false, true] {
                let (shown, at) = line_window(&v, cursor, width, secret);
                let w = crate::text::width(&shown);
                assert!(w <= width.max(1) || shown.chars().count() <= 1, "case {case}: {shown:?} is {w} wide in {width}");
                assert!((at as usize) <= width.max(1), "case {case}: the cursor at {at} of {width}");
                assert!(!shown.chars().any(char::is_control), "case {case}: {shown:?}");
                if secret {
                    assert!(shown.chars().all(|c| c == '•'), "case {case}");
                }
            }
        }
    }
}
