//! Application state, the event loop and every action.

use crate::api::{Agent, Api, ChatSummary, Event, FileEntry, Incoming, Memory, Routine, SearchHit, ServerInfo, ToolInfo, Upload};
use crate::forms::{Form, FormOut};
use crate::theme::{hex, Theme};
use crate::trace::{self, Entry, Trace, Who};
use anyhow::Result;
use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tui_textarea::{CursorMove, TextArea};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Sidebar,
    Messages,
    Composer,
    Files,
}

#[derive(Clone, Debug)]
pub enum SideRow {
    AllAgents,
    Agent(String),
    Group(String),
    Chat(String),
}

#[derive(Clone, Debug)]
pub enum Act {
    DeleteChat(String),
    DeleteIdle(Option<String>),
    DeleteAgent(String),
    DeleteMemory(i64),
    DeleteRoutine(String),
    Bypass(bool),
    Regenerate(usize),
    RenameChat(String),
    AttachPath,
    AddMemory,
    AddCalendar,
    Tidy,
}

pub struct PaletteItem {
    pub group: String,
    pub title: String,
    pub sub: String,
    pub action: PaletteAction,
}

#[derive(Clone, Debug)]
pub enum PaletteAction {
    OpenChat(String),
    NewChat(String),
    Cmd(String),
}

pub enum Overlay {
    Help,
    Palette { q: String, items: Vec<PaletteItem>, sel: usize, hits: Vec<SearchHit> },
    Confirm { title: String, text: String, ok: String, act: Act },
    Prompt { title: String, value: String, act: Act },
    Form { form: Form, kind: FormKind },
    Viewer { title: String, lines: Vec<String>, scroll: usize },
    Memory { data: Vec<Memory>, cats: Vec<String>, total: i64, sel: usize, q: String, cat: usize, typing: bool },
    Routines { items: Vec<Routine>, sel: usize },
    Pick { sel: usize },
    Toasts,
}

#[derive(Clone, Debug)]
pub enum FormKind {
    Settings,
    Email,
    Calendar,
    Agent(Option<String>),
    Routine(Option<String>),
}

#[derive(Serialize, Deserialize)]
pub struct Persisted {
    #[serde(default)]
    pub seen: HashMap<String, f64>,
    #[serde(default)]
    pub verbose: bool,
    #[serde(default = "t")]
    pub sidebar: bool,
    #[serde(default)]
    pub files: bool,
    #[serde(default)]
    pub filter: String,
}
fn t() -> bool {
    true
}
impl Default for Persisted {
    fn default() -> Self {
        Persisted { seen: HashMap::new(), verbose: false, sidebar: true, files: false, filter: String::new() }
    }
}

pub struct Files {
    pub agent_id: String,
    pub path: String,
    pub entries: Vec<FileEntry>,
    pub workspace: String,
    pub sel: usize,
}

pub struct Toast {
    pub text: String,
    pub err: bool,
    pub at: Instant,
}

pub struct App {
    pub api: Api,
    pub theme: Theme,
    pub tx: mpsc::UnboundedSender<Incoming>,
    pub rx: mpsc::UnboundedReceiver<Incoming>,
    pub cfg: Persisted,
    pub cfg_path: PathBuf,

    pub agents: Vec<Agent>,
    pub chats: Vec<ChatSummary>,
    pub settings: Value,
    pub tools: Vec<ToolInfo>,
    pub server: ServerInfo,
    pub version: String,
    pub statuses: HashMap<String, String>,

    pub filter: String,
    pub search: String,
    pub hits: HashMap<String, SearchHit>,
    pub search_focus: bool,

    pub chat_id: Option<String>,
    pub chat: Option<Value>,
    pub trace: Trace,
    pub running: bool,
    pub queue: Vec<Value>,
    pub stream_gen: u64,
    pub ctx_tokens: Option<u64>,
    pub tok_per_s: Option<f64>,

    pub focus: Focus,
    pub side_rows: Vec<SideRow>,
    pub side_sel: usize,
    pub side_scroll: usize,
    pub scroll: usize,
    pub follow: bool,
    pub rendered: Option<trace::Rendered>,
    pub render_key: (usize, bool, u64),
    pub dirty: u64,
    pub selected_turn: Option<usize>,
    pub composer: TextArea<'static>,
    pub attachments: Vec<Upload>,
    pub drafts: HashMap<String, String>,

    pub files: Files,
    pub overlay: Option<Overlay>,
    pub toasts: Vec<Toast>,
    pub quit: bool,
    pub last_size: (u16, u16),
    pub memory_count: i64,
    pub memory_working: bool,
    pub bell: bool,
    pub edit_from: Option<usize>,
    pub side_row_map: HashMap<usize, usize>,
    pub memory_return: bool,
    pub routines_return: bool,
}

pub fn agent_who(agents: &[Agent], id: &str) -> Who {
    Who::from(agents.iter().find(|a| a.id == id), id)
}

impl App {
    pub fn new(api: Api, theme: Theme, cfg_path: PathBuf) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let cfg: Persisted = std::fs::read_to_string(&cfg_path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let mut composer = TextArea::default();
        composer.set_cursor_line_style(ratatui::style::Style::default());
        composer.set_placeholder_text("Message  (Enter sends · Alt+Enter new line · F1 help)");
        let filter = cfg.filter.clone();
        let verbose = cfg.verbose;
        let files_shown = cfg.files;
        Self {
            api,
            theme,
            tx,
            rx,
            cfg,
            cfg_path,
            agents: vec![],
            chats: vec![],
            settings: Value::Null,
            tools: vec![],
            server: ServerInfo::default(),
            version: String::new(),
            statuses: HashMap::new(),
            filter,
            search: String::new(),
            hits: HashMap::new(),
            search_focus: false,
            chat_id: None,
            chat: None,
            trace: Trace::empty(),
            running: false,
            queue: vec![],
            stream_gen: 0,
            ctx_tokens: None,
            tok_per_s: None,
            focus: Focus::Sidebar,
            side_rows: vec![],
            side_sel: 0,
            side_scroll: 0,
            scroll: 0,
            follow: true,
            rendered: None,
            render_key: (0, verbose, 0),
            dirty: 1,
            selected_turn: None,
            composer,
            attachments: vec![],
            drafts: HashMap::new(),
            files: Files { agent_id: String::new(), path: ".".into(), entries: vec![], workspace: String::new(), sel: 0 },
            overlay: None,
            toasts: vec![],
            quit: false,
            last_size: (0, 0),
            memory_count: 0,
            memory_working: false,
            bell: true,
            edit_from: None,
            side_row_map: HashMap::new(),
            memory_return: false,
            routines_return: false,
        }
        .with_files(files_shown)
    }

    fn with_files(mut self, shown: bool) -> Self {
        self.cfg.files = shown;
        self
    }

    pub fn verbose(&self) -> bool {
        self.cfg.verbose
    }

    pub fn save_cfg(&self) {
        if let Some(dir) = self.cfg_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(&self.cfg) {
            let _ = std::fs::write(&self.cfg_path, s);
        }
    }

    pub fn toast(&mut self, text: impl Into<String>, err: bool) {
        self.toasts.push(Toast { text: text.into(), err, at: Instant::now() });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    pub fn agent(&self, id: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.id == id)
    }

    pub fn touch(&mut self) {
        self.dirty += 1;
    }

    // ------------------------------------------------------------------ boot

    pub async fn boot(&mut self) -> Result<()> {
        let st = self.api.state().await?;
        self.agents = st.agents;
        self.chats = st.chats;
        self.settings = st.settings;
        self.tools = st.tools;
        self.version = st.version;
        if self.cfg.seen.is_empty() {
            for c in &self.chats {
                self.cfg.seen.insert(c.id.clone(), c.updated);
            }
            self.save_cfg();
        }
        if !self.filter.is_empty() && self.agent(&self.filter).is_none() {
            self.filter.clear();
        }
        self.rebuild_sidebar();
        let api = self.api.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move { api.follow_global(tx).await });
        self.poll_server().await;
        self.refresh_memory_count().await;
        Ok(())
    }

    pub async fn poll_server(&mut self) {
        self.server = self.api.server().await.unwrap_or_else(|e| ServerInfo { ok: false, error: Some(e.to_string()), ..Default::default() });
    }

    pub async fn refresh_memory_count(&mut self) {
        if let Ok(m) = self.api.memory("", "").await {
            self.memory_count = m.total;
        }
    }

    pub async fn refresh_chats(&mut self) {
        if let Ok(mut chats) = self.api.chats().await {
            for c in chats.iter_mut() {
                if let Some(s) = self.statuses.get(&c.id) {
                    c.status = s.clone();
                }
            }
            self.chats = chats;
            self.rebuild_sidebar();
        }
    }

    pub async fn reload_state(&mut self) {
        if let Ok(st) = self.api.state().await {
            self.agents = st.agents;
            self.chats = st.chats;
            self.settings = st.settings;
            self.tools = st.tools;
            for c in self.chats.iter_mut() {
                if let Some(s) = self.statuses.get(&c.id) {
                    c.status = s.clone();
                }
            }
            self.rebuild_sidebar();
            self.touch();
        }
    }

    // --------------------------------------------------------------- sidebar

    pub fn busy_state(&self, agent_id: Option<&str>) -> Option<&'static str> {
        let st: Vec<&str> = self.chats.iter().filter(|c| agent_id.map(|a| c.agent_id == a).unwrap_or(true)).map(|c| c.status.as_str()).collect();
        for s in ["waiting", "running", "delegated"] {
            if st.contains(&s) {
                return Some(s);
            }
        }
        None
    }

    pub fn state_word(&self, agent_id: &str) -> String {
        match self.busy_state(Some(agent_id)) {
            Some("waiting") => "needs you".into(),
            Some("running") => "working".into(),
            Some("delegated") => {
                let on = self.chats.iter().find(|c| c.parent.as_ref().map(|p| p.caller_id == agent_id).unwrap_or(false) && c.status != "idle");
                match on {
                    Some(c) => format!("→ {}", agent_who(&self.agents, &c.agent_id).name),
                    None => "waiting".into(),
                }
            }
            _ => String::new(),
        }
    }

    pub fn is_unread(&self, c: &ChatSummary) -> bool {
        if c.parent.is_some() || Some(&c.id) == self.chat_id.as_ref() || c.last_role.as_deref() != Some("assistant") || c.status != "idle" {
            return false;
        }
        c.updated > self.cfg.seen.get(&c.id).copied().unwrap_or(0.0) + 1.0
    }

    pub fn visible_chats(&self) -> Vec<&ChatSummary> {
        let q = self.search.trim().to_lowercase();
        self.chats
            .iter()
            .filter(|c| {
                if !q.is_empty() {
                    format!("{} {}", c.title, c.preview).to_lowercase().contains(&q) || self.hits.contains_key(&c.id)
                } else if !self.filter.is_empty() {
                    c.agent_id == self.filter
                } else {
                    c.parent.is_none()
                }
            })
            .collect()
    }

    pub fn rebuild_sidebar(&mut self) {
        let mut rows = vec![SideRow::AllAgents];
        for a in &self.agents {
            rows.push(SideRow::Agent(a.id.clone()));
        }
        let mut group = String::new();
        let chats: Vec<(String, String)> = self
            .visible_chats()
            .iter()
            .map(|c| {
                let g = if !self.search.trim().is_empty() {
                    "Matches".to_string()
                } else if c.pinned && c.parent.is_none() {
                    "Pinned".to_string()
                } else {
                    day_group(c.updated)
                };
                (g, c.id.clone())
            })
            .collect();
        for (g, id) in chats {
            if g != group {
                group = g.clone();
                rows.push(SideRow::Group(g));
            }
            rows.push(SideRow::Chat(id));
        }
        self.side_rows = rows;
        if self.side_sel >= self.side_rows.len() {
            self.side_sel = self.side_rows.len().saturating_sub(1);
        }
    }

    fn side_move(&mut self, dir: i32) {
        if self.side_rows.is_empty() {
            return;
        }
        let n = self.side_rows.len() as i32;
        let mut i = self.side_sel as i32;
        for _ in 0..n {
            i = (i + dir).clamp(0, n - 1);
            if !matches!(self.side_rows[i as usize], SideRow::Group(_)) {
                break;
            }
            if i == 0 || i == n - 1 {
                break;
            }
        }
        self.side_sel = i as usize;
    }

    pub fn select_chat_row(&mut self, id: &str) {
        if let Some(i) = self.side_rows.iter().position(|r| matches!(r, SideRow::Chat(c) if c == id)) {
            self.side_sel = i;
        }
    }

    async fn side_activate(&mut self) {
        match self.side_rows.get(self.side_sel).cloned() {
            Some(SideRow::AllAgents) => {
                self.filter.clear();
                self.cfg.filter.clear();
                self.save_cfg();
                self.rebuild_sidebar();
            }
            Some(SideRow::Agent(id)) => {
                self.filter = id.clone();
                self.cfg.filter = id.clone();
                self.save_cfg();
                self.rebuild_sidebar();
                let busy = self.chats.iter().find(|c| c.agent_id == id && (c.status == "running" || c.status == "waiting")).or_else(|| self.chats.iter().find(|c| c.agent_id == id && c.status == "delegated")).map(|c| c.id.clone());
                if let Some(cid) = busy {
                    if Some(&cid) != self.chat_id.as_ref() {
                        self.open_chat(Some(cid)).await;
                    }
                }
            }
            Some(SideRow::Chat(id)) => {
                self.open_chat(Some(id)).await;
                self.focus = Focus::Composer;
            }
            _ => {}
        }
    }

    pub fn selected_chat(&self) -> Option<ChatSummary> {
        match self.side_rows.get(self.side_sel) {
            Some(SideRow::Chat(id)) => self.chats.iter().find(|c| &c.id == id).cloned(),
            _ => self.chat_id.as_ref().and_then(|id| self.chats.iter().find(|c| &c.id == id).cloned()),
        }
    }

    pub fn selected_agent(&self) -> Option<String> {
        match self.side_rows.get(self.side_sel) {
            Some(SideRow::Agent(id)) => Some(id.clone()),
            Some(SideRow::Chat(id)) => self.chats.iter().find(|c| &c.id == id).map(|c| c.agent_id.clone()),
            _ => self.chat.as_ref().and_then(|c| c.get("agent_id")).and_then(|a| a.as_str()).map(String::from).or_else(|| if self.filter.is_empty() { None } else { Some(self.filter.clone()) }),
        }
    }

    // ------------------------------------------------------------------ chat

    pub async fn open_chat(&mut self, id: Option<String>) {
        if let Some(old) = &self.chat_id {
            let draft = self.composer.lines().join("\n");
            if !draft.trim().is_empty() {
                self.drafts.insert(old.clone(), draft);
            } else {
                self.drafts.remove(old);
            }
        }
        self.stream_gen += 1;
        self.chat_id = id.clone();
        self.chat = None;
        self.trace = Trace::empty();
        self.running = false;
        self.queue.clear();
        self.scroll = 0;
        self.follow = true;
        self.selected_turn = None;
        self.attachments.clear();
        self.ctx_tokens = None;
        self.tok_per_s = None;
        self.composer = TextArea::default();
        self.composer.set_cursor_line_style(ratatui::style::Style::default());
        self.composer.set_placeholder_text("Message  (Enter sends · Alt+Enter new line · F1 help)");
        if let Some(id) = &id {
            if let Some(d) = self.drafts.get(id).cloned() {
                self.composer = TextArea::from(d.lines().map(String::from).collect::<Vec<_>>());
                self.composer.set_cursor_line_style(ratatui::style::Style::default());
                self.composer.move_cursor(CursorMove::Bottom);
                self.composer.move_cursor(CursorMove::End);
            }
            let api = self.api.clone();
            let tx = self.tx.clone();
            let cid = id.clone();
            tokio::spawn(async move { api.follow_chat(cid, tx).await });
            self.select_chat_row(id);
            self.mark_seen(id);
        }
        self.rebuild_sidebar();
        self.touch();
        if self.cfg.files {
            self.refresh_files(true).await;
        }
    }

    fn mark_seen(&mut self, id: &str) {
        let updated = self.chats.iter().find(|c| c.id == id).map(|c| c.updated).unwrap_or_else(trace::now);
        let now = trace::now();
        self.cfg.seen.insert(id.to_string(), updated.max(now));
        self.save_cfg();
    }

    pub async fn new_chat(&mut self, agent_id: &str) {
        match self.api.new_chat(agent_id).await {
            Ok(c) => {
                let id = c.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                self.refresh_chats().await;
                if !self.filter.is_empty() && self.filter != agent_id {
                    self.filter = agent_id.to_string();
                    self.cfg.filter = self.filter.clone();
                    self.save_cfg();
                    self.rebuild_sidebar();
                }
                self.open_chat(Some(id)).await;
                self.focus = Focus::Composer;
            }
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    pub fn is_subchat(&self) -> bool {
        self.chat.as_ref().map(|c| c.get("parent").map(|p| !p.is_null()).unwrap_or(false)).unwrap_or(false)
    }

    pub fn chat_agent_id(&self) -> String {
        self.chat.as_ref().and_then(|c| c.get("agent_id")).and_then(|a| a.as_str()).unwrap_or("").to_string()
    }

    async fn send(&mut self) {
        let Some(cid) = self.chat_id.clone() else { return };
        if self.is_subchat() {
            self.toast("This chat shows an agent working for another agent; reply in the chat that started it.", false);
            return;
        }
        let typed = self.composer.lines().join("\n").trim().to_string();
        let mut content = typed.clone();
        for a in &self.attachments {
            if a.image {
                continue;
            }
            match &a.text {
                Some(t) => {
                    let fence = if t.contains("```") { "````" } else { "```" };
                    content.push_str(&format!("\n\n[Attached file: {}, saved at {}]\n{fence}\n{t}\n{fence}", a.name, a.path));
                }
                None => content.push_str(&format!("\n\n[Attached file: {} ({} bytes), saved in the workspace at {}]", a.name, a.size, a.path)),
            }
        }
        let content = content.trim().to_string();
        if content.is_empty() {
            return;
        }
        let images: Vec<String> = self.attachments.iter().filter(|a| a.image).map(|a| a.path.clone()).collect();
        let body = json!({ "content": content, "images": images });
        match self.api.run(&cid, body).await {
            Ok(res) => {
                self.composer = TextArea::default();
                self.composer.set_cursor_line_style(ratatui::style::Style::default());
                self.composer.set_placeholder_text("Message  (Enter sends · Alt+Enter new line · F1 help)");
                self.attachments.clear();
                self.drafts.remove(&cid);
                if res.get("queued").map(|q| !q.is_null()).unwrap_or(false) {
                    self.toast("Queued: the agent reads it at its next step", false);
                } else {
                    self.running = true;
                    self.follow = true;
                }
            }
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    async fn stop(&mut self) {
        if let Some(cid) = self.chat_id.clone() {
            if let Err(e) = self.api.stop(&cid).await {
                self.toast(e.to_string(), true);
            }
        }
    }

    async fn unqueue_all(&mut self) {
        let Some(cid) = self.chat_id.clone() else { return };
        match self.api.delete::<Value>(&format!("/api/chats/{cid}/queue")).await {
            Ok(v) => {
                let items: Vec<String> = v.get("items").and_then(|i| i.as_array()).map(|a| a.iter().filter_map(|x| x.get("content").and_then(|c| c.as_str()).map(String::from)).collect()).unwrap_or_default();
                if !items.is_empty() {
                    self.put_in_composer(&items.join("\n\n"));
                }
            }
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    fn put_in_composer(&mut self, text: &str) {
        let existing = self.composer.lines().join("\n");
        let all = if existing.trim().is_empty() { text.to_string() } else { format!("{text}\n\n{existing}") };
        self.composer = TextArea::from(all.lines().map(String::from).collect::<Vec<_>>());
        self.composer.set_cursor_line_style(ratatui::style::Style::default());
        self.composer.move_cursor(CursorMove::Bottom);
        self.composer.move_cursor(CursorMove::End);
        self.focus = Focus::Composer;
    }

    async fn run_body(&mut self, body: Value) {
        let Some(cid) = self.chat_id.clone() else { return };
        if self.running {
            self.toast("Wait for the current reply or stop it first (Ctrl+S).", false);
            return;
        }
        match self.api.run(&cid, body).await {
            Ok(_) => {
                self.running = true;
                self.follow = true;
            }
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    /// Regenerate the last reply, or retry after an error.
    async fn regenerate(&mut self) {
        let last_turn_start = self.trace.entries.iter().rev().find_map(|e| if let Entry::Turn(t) = e { Some(t.start) } else { None });
        let Some(start) = last_turn_start else { return self.toast("Nothing to regenerate yet.", false) };
        self.run_body(json!({ "from_index": start })).await;
    }

    async fn edit_last(&mut self) {
        let last_user = self.trace.entries.iter().rev().find_map(|e| if let Entry::User(u) = e { Some(u.clone()) } else { None });
        let Some(u) = last_user else { return };
        if self.running {
            return self.toast("Wait for the current reply or stop it first.", false);
        }
        self.put_in_composer(&u.text);
        self.toast(format!("Editing your last message; Enter resends it from there (the {} message{} after it will be replaced).", self.trace.entries.len().saturating_sub(u.index + 1), if self.trace.entries.len().saturating_sub(u.index + 1) == 1 { "" } else { "s" }), false);
        self.edit_from = Some(u.index);
    }

    async fn branch(&mut self, upto: usize) {
        let Some(cid) = self.chat_id.clone() else { return };
        match self.api.post::<Value>(&format!("/api/chats/{cid}/fork"), json!({ "upto": upto })).await {
            Ok(c) => {
                let id = c.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                self.refresh_chats().await;
                self.toast("Branched. This copy keeps the conversation up to here; the original is unchanged.", false);
                self.open_chat(Some(id)).await;
            }
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    async fn compact(&mut self) {
        let Some(cid) = self.chat_id.clone() else { return };
        if self.running {
            return self.toast("Wait for the current reply or stop it first.", false);
        }
        if let Err(e) = self.api.post::<Value>(&format!("/api/chats/{cid}/compact"), json!({})).await {
            self.toast(e.to_string(), true);
        }
    }

    async fn export(&mut self, kind: &str) {
        let Some(c) = self.selected_chat() else { return };
        let path = format!("/api/chats/{}/export.{kind}", c.id);
        match self.api.get_text(&path).await {
            Ok(text) => {
                let name: String = c.title.chars().map(|ch| if ch.is_alphanumeric() { ch } else { '-' }).take(60).collect::<String>().trim_matches('-').to_string();
                let file = format!("{}.{kind}", if name.is_empty() { "chat".into() } else { name });
                match std::fs::write(&file, text) {
                    Ok(_) => self.toast(format!("Saved {file}"), false),
                    Err(e) => self.toast(e.to_string(), true),
                }
            }
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    async fn pin(&mut self) {
        let Some(c) = self.selected_chat() else { return };
        match self.api.patch::<Value>(&format!("/api/chats/{}", c.id), json!({ "pinned": !c.pinned })).await {
            Ok(_) => {
                self.refresh_chats().await;
                self.toast(if c.pinned { "Unpinned" } else { "Pinned to the top" }, false);
            }
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    async fn approve(&mut self, approve: bool, all: bool) {
        let Some(cid) = self.chat_id.clone() else { return };
        let pending = self.trace.pending_approvals();
        let Some((aid, _, _)) = pending.first() else { return };
        if let Err(e) = self.api.approve(&cid, aid, approve, all).await {
            self.toast(e.to_string(), true);
        } else if all {
            self.toast("Approved for the rest of this run. New runs ask again.", false);
        }
    }

    // ---------------------------------------------------------------- files

    pub async fn refresh_files(&mut self, reset: bool) {
        let agent = if !self.chat_agent_id().is_empty() { self.chat_agent_id() } else if !self.filter.is_empty() { self.filter.clone() } else { self.agents.first().map(|a| a.id.clone()).unwrap_or_default() };
        if agent.is_empty() {
            return;
        }
        if reset || agent != self.files.agent_id {
            self.files.path = ".".into();
            self.files.sel = 0;
        }
        self.files.agent_id = agent.clone();
        match self.api.workspace(&agent, &self.files.path).await {
            Ok(l) => {
                self.files.entries = l.entries;
                self.files.workspace = l.workspace;
                self.files.path = l.path;
                if self.files.sel >= self.files.entries.len() + 1 {
                    self.files.sel = 0;
                }
            }
            Err(e) => {
                if self.files.path != "." {
                    self.files.path = ".".into();
                    self.files.sel = 0;
                } else {
                    self.toast(e.to_string(), true);
                }
            }
        }
    }

    async fn files_activate(&mut self) {
        if self.files.sel == 0 {
            if self.files.path != "." {
                let p = std::path::Path::new(&self.files.path).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                self.files.path = if p.is_empty() { ".".into() } else { p };
                self.files.sel = 0;
                self.refresh_files(false).await;
            }
            return;
        }
        let Some(e) = self.files.entries.get(self.files.sel - 1).cloned() else { return };
        if e.dir {
            self.files.path = e.path;
            self.files.sel = 0;
            self.refresh_files(false).await;
        } else {
            self.view_file(&e).await;
        }
    }

    async fn view_file(&mut self, e: &FileEntry) {
        match self.api.workspace_file(&self.files.agent_id, &e.path).await {
            Ok((bytes, ctype)) => {
                let binary = bytes.iter().take(4096).any(|b| *b == 0);
                let lines = if binary && !ctype.starts_with("text/") {
                    vec![format!("Binary file ({ctype}, {} bytes).", bytes.len()), String::new(), format!("Path: {}/{}", self.files.workspace, e.path)]
                } else {
                    String::from_utf8_lossy(&bytes).lines().map(String::from).collect()
                };
                self.overlay = Some(Overlay::Viewer { title: e.path.clone(), lines, scroll: 0 });
            }
            Err(err) => self.toast(err.to_string(), true),
        }
    }

    // -------------------------------------------------------------- overlays

    pub fn open_help(&mut self) {
        self.overlay = Some(Overlay::Help);
    }

    pub fn open_palette(&mut self) {
        let mut o = Overlay::Palette { q: String::new(), items: vec![], sel: 0, hits: vec![] };
        self.fill_palette(&mut o);
        self.overlay = Some(o);
    }

    fn fill_palette(&self, o: &mut Overlay) {
        let Overlay::Palette { q, items, sel, hits } = o else { return };
        let ql = q.trim().to_lowercase();
        let m = |s: &str| ql.is_empty() || s.to_lowercase().contains(&ql);
        items.clear();
        for a in &self.agents {
            let title = format!("New {} chat", a.name);
            if m(&title) || m(&a.purpose) {
                items.push(PaletteItem { group: "Agents".into(), title, sub: a.purpose.clone(), action: PaletteAction::NewChat(a.id.clone()) });
            }
        }
        let hitmap: HashMap<&str, &SearchHit> = hits.iter().map(|h| (h.chat_id.as_str(), h)).collect();
        let chats: Vec<&ChatSummary> = self.chats.iter().filter(|c| if ql.is_empty() { c.parent.is_none() } else { m(&c.title) || hitmap.contains_key(c.id.as_str()) }).collect();
        for c in chats.iter().take(if ql.is_empty() { 8 } else { 30 }) {
            let a = agent_who(&self.agents, &c.agent_id);
            let sub = match hitmap.get(c.id.as_str()) {
                Some(h) => format!("{}: {}", a.name, h.snippet),
                None => format!("{} · {}", a.name, time_ago(c.updated)),
            };
            items.push(PaletteItem { group: if ql.is_empty() { "Recent chats".into() } else { "Chats".into() }, title: c.title.clone(), sub, action: PaletteAction::OpenChat(c.id.clone()) });
        }
        let in_chat = self.chat.is_some() && !self.is_subchat();
        let cmds: Vec<(&str, &str, &str)> = vec![
            ("New chat", "Pick an agent", "new"),
            ("Settings", "Model server, agents, email, calendars", "settings"),
            ("Memory", "What your agents know about you", "memory"),
            ("Routines", "Agents that run on a schedule", "routines"),
            ("New agent", "From a blank sheet", "new-agent"),
            (if self.cfg.files { "Hide workspace files" } else { "Show workspace files" }, "What the agent's file tools can see", "files"),
            (if self.cfg.verbose { "Collapse thinking and tool details" } else { "Show all thinking and tool details" }, "", "verbose"),
            (if self.cfg.sidebar { "Hide the sidebar" } else { "Show the sidebar" }, "", "sidebar"),
            (if self.settings.get("bypass_approvals").and_then(|b| b.as_bool()).unwrap_or(false) { "Ask before acting again" } else { "Bypass permissions" }, "Let agents run commands and send email without asking", "bypass"),
            ("Keyboard shortcuts", "", "help"),
            ("Email account", "IMAP/SMTP for the Mail agent", "email"),
            ("Calendars", "iCal links for the Planner", "calendars"),
            ("Quit", "", "quit"),
        ];
        for (title, sub, key) in cmds {
            if m(title) || m(sub) {
                items.push(PaletteItem { group: "Commands".into(), title: title.into(), sub: sub.into(), action: PaletteAction::Cmd(key.into()) });
            }
        }
        if in_chat {
            let chat_cmds: Vec<(&str, &str, &str)> = vec![
                ("Export as Markdown", "to the current folder", "export-md"),
                ("Export as JSON", "with agent-to-agent work", "export-json"),
                ("Branch from the end", "A new chat that continues from here", "branch"),
                ("Compact older messages", "Summarize to free up context", "compact"),
                ("Rename chat", "", "rename"),
                ("Pin or unpin chat", "", "pin"),
                ("Edit this agent", "", "edit-agent"),
                ("Delete chat", "", "delete"),
            ];
            for (title, sub, key) in chat_cmds {
                if m(title) || m(sub) {
                    items.push(PaletteItem { group: "This chat".into(), title: title.into(), sub: sub.into(), action: PaletteAction::Cmd(key.into()) });
                }
            }
        }
        if *sel >= items.len() {
            *sel = items.len().saturating_sub(1);
        }
    }

    async fn run_cmd(&mut self, key: &str) {
        match key {
            "new" => self.overlay = Some(Overlay::Pick { sel: 0 }),
            "settings" => self.open_settings().await,
            "email" => self.open_email().await,
            "calendars" => self.open_calendars().await,
            "memory" => self.open_memory().await,
            "routines" => self.open_routines().await,
            "new-agent" => self.open_agent_editor(None),
            "edit-agent" => {
                let id = self.chat_agent_id();
                if !id.is_empty() {
                    self.open_agent_editor(Some(id));
                }
            }
            "files" => self.toggle_files().await,
            "verbose" => self.toggle_verbose(),
            "sidebar" => {
                self.cfg.sidebar = !self.cfg.sidebar;
                self.save_cfg();
            }
            "bypass" => {
                let on = !self.settings.get("bypass_approvals").and_then(|b| b.as_bool()).unwrap_or(false);
                if on {
                    self.overlay = Some(Overlay::Confirm { title: "Bypass permissions?".into(), text: "Agents will run shell commands and send email without asking you first. Anything they read (web pages, emails) could try to trick them. You can switch this off again at any time.".into(), ok: "Bypass".into(), act: Act::Bypass(true) });
                } else {
                    self.set_bypass(false).await;
                }
            }
            "help" => self.open_help(),
            "quit" => self.quit = true,
            "export-md" => self.export("md").await,
            "export-json" => self.export("json").await,
            "branch" => {
                let n = self.chat.as_ref().and_then(|c| c.get("messages")).and_then(|m| m.as_array()).map(|m| m.len()).unwrap_or(0);
                if n > 0 {
                    self.branch(n).await;
                }
            }
            "compact" => self.compact().await,
            "rename" => {
                if let Some(c) = self.selected_chat() {
                    self.overlay = Some(Overlay::Prompt { title: "Rename chat".into(), value: c.title.clone(), act: Act::RenameChat(c.id.clone()) });
                }
            }
            "pin" => self.pin().await,
            "delete" => {
                if let Some(c) = self.selected_chat() {
                    self.overlay = Some(Overlay::Confirm { title: format!("Delete “{}”?", trace::one_line(&c.title, 40)), text: "Its messages, and any work other agents did for it, will be removed.".into(), ok: "Delete".into(), act: Act::DeleteChat(c.id.clone()) });
                }
            }
            _ => {}
        }
    }

    async fn set_bypass(&mut self, on: bool) {
        match self.api.settings(json!({ "bypass_approvals": on })).await {
            Ok(s) => {
                self.settings = s;
                self.toast(if on { "Permissions bypassed: agents act without asking" } else { "Agents will ask before acting again" }, false);
            }
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    pub fn toggle_verbose(&mut self) {
        self.cfg.verbose = !self.cfg.verbose;
        self.save_cfg();
        self.touch();
    }

    pub async fn toggle_files(&mut self) {
        self.cfg.files = !self.cfg.files;
        self.save_cfg();
        if self.cfg.files {
            self.refresh_files(true).await;
            self.focus = Focus::Files;
        } else if self.focus == Focus::Files {
            self.focus = Focus::Composer;
        }
    }

    async fn open_settings(&mut self) {
        let s = &self.settings;
        let g = |k: &str| s.get(k).map(|v| match v { Value::String(x) => x.clone(), other => other.to_string() }).unwrap_or_default();
        let b = |k: &str| s.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
        let mut models = self.server.models.clone();
        if !g("model").is_empty() && !models.contains(&g("model")) {
            models.push(g("model"));
        }
        let mut opts = vec![String::new()];
        opts.extend(models.clone());
        let mut labels = vec!["Server default / first model".to_string()];
        labels.extend(models);
        let form = Form::new("Settings")
            .text("base_url", "Model server URL", &g("base_url"), "any OpenAI-compatible API")
            .text("api_key", "API key", &g("api_key"), "anything works for llama-server")
            .select("model", "Default model", &opts, &labels, &g("model"), "")
            .text("context_size", "Context size", &g("context_size"), "tokens; 0 asks the server")
            .bool("auto_compact", "Compact long chats automatically", b("auto_compact"), "")
            .text("compact_at", "Compact at % of the context", &g("compact_at"), "30–95")
            .text("max_steps", "Max steps per turn", &g("max_steps"), "0 = no limit")
            .text("shell_timeout", "Shell command timeout (s)", &g("shell_timeout"), "0 = no limit")
            .bool("auto_title", "Let the model name each chat after its first reply", b("auto_title"), "")
            .bool("auto_memory", "Let the model save new things it learned about you", b("auto_memory"), "")
            .bool("bypass_approvals", "Bypass permissions (run commands and send email without asking)", b("bypass_approvals"), "")
            .danger("bypass_approvals")
            .text("search_url", "Search engine URL", &g("search_url"), "a SearXNG instance; empty scrapes DuckDuckGo")
            .text("browser_executable", "Browser executable", &g("browser_executable"), "Brave or another Chromium")
            .bool("browser_headless", "Run the browser headless", b("browser_headless"), "")
            .note(&format!("Server: {}  ·  Agent Chat {}", if self.server.ok { format!("connected ({}), {} model(s){}", if self.server.kind == "llama" { "llama-server" } else { "OpenAI-compatible" }, self.server.models.len(), self.server.n_ctx.map(|n| format!(", context {n}")).unwrap_or_default()) } else { "not reachable".into() }, self.version));
        self.overlay = Some(Overlay::Form { form, kind: FormKind::Settings });
    }

    async fn open_email(&mut self) {
        let cfg = self.api.email().await.unwrap_or(Value::Null);
        let g = |k: &str| cfg.get(k).map(|v| match v { Value::String(x) => x.clone(), Value::Null => String::new(), other => other.to_string() }).unwrap_or_default();
        let sec = vec!["ssl".to_string(), "starttls".into(), "none".into()];
        let presets = vec!["".to_string(), "gmail".into(), "icloud".into(), "yahoo".into(), "fastmail".into()];
        let plabels = vec!["keep servers below".to_string(), "Gmail".into(), "iCloud".into(), "Yahoo".into(), "Fastmail".into()];
        let form = Form::new("Email account")
            .note("Lets the Mail agent read (IMAP) and send (SMTP) your email. Gmail, iCloud, Yahoo and Fastmail need an app password. The password stays on this computer.")
            .select("preset", "Provider preset", &presets, &plabels, "", "fills in the servers on save")
            .text("username", "Email address", &g("username"), "")
            .secret("password", "App password", if cfg.get("has_password").and_then(|h| h.as_bool()).unwrap_or(false) { "saved; leave empty to keep" } else { "" })
            .text("from_name", "Your name", &g("from_name"), "shown as the sender")
            .text("imap_host", "Incoming server (IMAP)", &g("imap_host"), "")
            .text("imap_port", "IMAP port", &g("imap_port"), "")
            .select("imap_security", "IMAP security", &sec, &sec, &g("imap_security"), "")
            .text("smtp_host", "Outgoing server (SMTP)", &g("smtp_host"), "")
            .text("smtp_port", "SMTP port", &g("smtp_port"), "")
            .select("smtp_security", "SMTP security", &sec, &sec, &g("smtp_security"), "")
            .button("test", "Save and test", "checks both servers");
        self.overlay = Some(Overlay::Form { form, kind: FormKind::Email });
    }

    async fn open_calendars(&mut self) {
        let cals = self.api.calendars().await.unwrap_or_default();
        let mut form = Form::new("Calendars").note("Read-only iCal links for the Planner (and any agent with “Read your calendars”). Google: calendar settings → Secret address in iCal format. iCloud: public calendar webcal:// link. Outlook: Publish → ICS link. A local .ics path works too.");
        for c in &cals {
            form = form.button(&format!("rm:{}", c.id), &format!("Disconnect {}", c.name), &c.link);
        }
        form = form.text("name", "Name", "", "e.g. Personal").text("url", "iCal link or .ics path", "", "").button("add", "Add calendar", "").button("test", "Test calendars", "");
        form.actions = vec![("Esc".into(), "close".into())];
        self.overlay = Some(Overlay::Form { form, kind: FormKind::Calendar });
    }

    pub fn open_agent_editor(&mut self, id: Option<String>) {
        let a = id.as_ref().and_then(|i| self.agent(i).cloned()).unwrap_or(Agent { emoji: "🤖".into(), color: "#7c6cff".into(), tools: vec!["ask_agent".into()], delegate_all: true, confirm_shell: true, memory: true, ..Default::default() });
        let mut models = self.server.models.clone();
        if !a.model.is_empty() && !models.contains(&a.model) {
            models.push(a.model.clone());
        }
        let mut opts = vec![String::new()];
        opts.extend(models.clone());
        let mut labels = vec!["Default (from Settings)".to_string()];
        labels.extend(models);
        let mut form = Form::new(&if id.is_some() { format!("Edit {}", a.name) } else { "New agent".into() })
            .text("name", "Name", &a.name, "")
            .text("emoji", "Icon", &a.emoji, "an emoji")
            .text("color", "Color", &a.color, "#rrggbb")
            .text("purpose", "Purpose", &a.purpose, "one line; shown to other agents")
            .multi("system_prompt", "Instructions", &a.system_prompt, 8, "")
            .note("Tools:");
        for t in &self.tools {
            form = form.bool(&format!("tool:{}", t.name), &format!("{}{}", t.label, if t.danger { "  (risky)" } else { "" }), a.tools.contains(&t.name), "");
        }
        form = form
            .bool("confirm_shell", "Ask me before every shell command", a.confirm_shell, "")
            .bool("memory", "Uses memory about you, and can remember new things", a.memory, "")
            .bool("delegate_all", "Can talk to every other agent, including ones added later", a.delegate_all, "");
        for o in self.agents.iter().filter(|o| Some(&o.id) != id.as_ref()) {
            form = form.bool(&format!("del:{}", o.id), &format!("  may talk to {} {}", o.emoji, o.name), a.delegate_all || a.delegates.contains(&o.id), "(when not every agent)");
        }
        form = form
            .select("model", "Model", &opts, &labels, &a.model, "")
            .text("temperature", "Temperature", &a.temperature.map(|t| t.to_string()).unwrap_or_default(), "empty = default")
            .text("workspace", "Workspace folder", &a.workspace, "empty = the shared default");
        if id.is_some() {
            form = form.button("duplicate", "Duplicate this agent", "").button("delete", "Delete this agent", "");
        }
        self.overlay = Some(Overlay::Form { form, kind: FormKind::Agent(id) });
    }

    fn agent_from_form(&self, form: &Form) -> Value {
        let tools: Vec<String> = self.tools.iter().filter(|t| form.boolean(&format!("tool:{}", t.name))).map(|t| t.name.clone()).collect();
        let delegate_all = form.boolean("delegate_all");
        let delegates: Vec<String> = if delegate_all { vec![] } else { self.agents.iter().filter(|o| form.boolean(&format!("del:{}", o.id))).map(|o| o.id.clone()).collect() };
        let temp = form.number("temperature");
        json!({
            "name": form.string("name").trim(),
            "emoji": if form.string("emoji").trim().is_empty() { "🤖".to_string() } else { form.string("emoji").trim().to_string() },
            "color": if hex(&form.string("color")).is_some() { form.string("color").trim().to_string() } else { "#7c6cff".to_string() },
            "purpose": form.string("purpose").trim(),
            "system_prompt": form.string("system_prompt"),
            "tools": tools,
            "delegate_all": delegate_all,
            "delegates": delegates,
            "model": form.string("model"),
            "temperature": temp,
            "workspace": form.string("workspace").trim(),
            "confirm_shell": form.boolean("confirm_shell"),
            "memory": form.boolean("memory"),
        })
    }

    async fn open_memory(&mut self) {
        match self.api.memory("", "").await {
            Ok(m) => {
                self.memory_count = m.total;
                self.overlay = Some(Overlay::Memory { data: m.memories, cats: m.categories, total: m.total, sel: 0, q: String::new(), cat: 0, typing: false });
            }
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    async fn reload_memory(&mut self) {
        let (q, cat) = match &self.overlay {
            Some(Overlay::Memory { q, cat, cats, .. }) => (q.clone(), if *cat == 0 { String::new() } else { cats.get(*cat - 1).cloned().unwrap_or_default() }),
            _ => return,
        };
        if let Ok(m) = self.api.memory(&q, &cat).await {
            self.memory_count = m.total;
            if let Some(Overlay::Memory { data, cats, total, sel, .. }) = &mut self.overlay {
                *data = m.memories;
                *cats = m.categories;
                *total = m.total;
                if *sel >= data.len() {
                    *sel = data.len().saturating_sub(1);
                }
            }
        }
    }

    async fn open_routines(&mut self) {
        match self.api.routines().await {
            Ok(items) => self.overlay = Some(Overlay::Routines { items, sel: 0 }),
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    async fn reload_routines(&mut self) {
        if let Ok(items) = self.api.routines().await {
            if let Some(Overlay::Routines { items: it, sel }) = &mut self.overlay {
                *it = items;
                if *sel >= it.len() {
                    *sel = it.len().saturating_sub(1);
                }
            }
        }
    }

    fn open_routine_editor(&mut self, r: Option<&Routine>) {
        let ids: Vec<String> = self.agents.iter().map(|a| a.id.clone()).collect();
        let labels: Vec<String> = self.agents.iter().map(|a| format!("{} {}", a.emoji, a.name)).collect();
        let templates = routine_templates();
        let (name, agent, prompt, sched) = match r {
            Some(r) => (r.name.clone(), r.agent_id.clone(), r.prompt.clone(), r.schedule.clone()),
            None => (String::new(), ids.first().cloned().unwrap_or_default(), String::new(), json!({"type": "daily", "time": "08:00", "days": [0,1,2,3,4,5,6]})),
        };
        let days: Vec<u64> = sched.get("days").and_then(|d| d.as_array()).map(|a| a.iter().filter_map(|x| x.as_u64()).collect()).unwrap_or_else(|| (0..7).collect());
        let mut form = Form::new(&if r.is_some() { format!("Edit routine “{name}”") } else { "New routine".into() });
        if r.is_none() {
            let mut topts = vec![String::new()];
            let mut tlabels = vec!["Blank".to_string()];
            for (i, t) in templates.iter().enumerate() {
                topts.push(i.to_string());
                tlabels.push(t.0.to_string());
            }
            form = form.select("template", "Start from", &topts, &tlabels, "", "picks name, agent, prompt and schedule on save when the name is empty");
        }
        form = form
            .text("name", "Name", &name, "")
            .select("agent_id", "Agent", &ids, &labels, &agent, "")
            .multi("prompt", "What should it do?", &prompt, 5, "")
            .select("type", "Repeat", &["daily".into(), "interval".into()], &["At a set time".into(), "Every N minutes".into()], sched.get("type").and_then(|t| t.as_str()).unwrap_or("daily"), "")
            .text("time", "Time", sched.get("time").and_then(|t| t.as_str()).unwrap_or("08:00"), "HH:MM, for set times")
            .text("minutes", "Every (minutes)", &sched.get("minutes").and_then(|m| m.as_u64()).unwrap_or(60).to_string(), "for intervals, at least 5");
        let names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        for (i, n) in names.iter().enumerate() {
            form = form.bool(&format!("day:{i}"), n, days.contains(&(i as u64)), "");
        }
        form = form.bool("enabled", "Enabled", r.map(|r| r.enabled).unwrap_or(true), "");
        self.overlay = Some(Overlay::Form { form, kind: FormKind::Routine(r.map(|r| r.id.clone())) });
    }

    async fn handle_form(&mut self, out: FormOut) {
        let Some(Overlay::Form { form, kind }) = &self.overlay else { return };
        let kind = kind.clone();
        match out {
            FormOut::Cancel => self.overlay = None,
            FormOut::None => {}
            FormOut::Button(key) => {
                match (&kind, key.as_str()) {
                    (FormKind::Email, "test") => {
                        self.save_email().await;
                        match self.api.post::<Value>("/api/email/test", json!({})).await {
                            Ok(r) => {
                                let ok = r.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);
                                let msg = r.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
                                self.toast(msg, !ok);
                            }
                            Err(e) => self.toast(e.to_string(), true),
                        }
                    }
                    (FormKind::Calendar, "add") => self.add_calendar().await,
                    (FormKind::Calendar, "test") => match self.api.post::<Value>("/api/calendars/test", json!({})).await {
                        Ok(r) => self.toast(r.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string(), false),
                        Err(e) => self.toast(e.to_string(), true),
                    },
                    (FormKind::Calendar, k) if k.starts_with("rm:") => {
                        let id = k[3..].to_string();
                        match self.api.delete::<Value>(&format!("/api/calendars/{id}")).await {
                            Ok(_) => {
                                self.toast("Calendar disconnected", false);
                                self.open_calendars().await;
                            }
                            Err(e) => self.toast(e.to_string(), true),
                        }
                    }
                    (FormKind::Agent(Some(id)), "delete") => {
                        let name = self.agent(id).map(|a| a.name.clone()).unwrap_or_default();
                        self.overlay = Some(Overlay::Confirm { title: format!("Delete {name}?"), text: "Existing chats with it stay readable but can't continue.".into(), ok: "Delete agent".into(), act: Act::DeleteAgent(id.clone()) });
                    }
                    (FormKind::Agent(Some(_)), "duplicate") => {
                        let mut body = self.agent_from_form(form);
                        let name = format!("{} copy", body.get("name").and_then(|n| n.as_str()).unwrap_or("Agent"));
                        body["name"] = Value::from(name.clone());
                        match self.api.post::<Value>("/api/agents", body).await {
                            Ok(a) => {
                                self.reload_state().await;
                                self.toast(format!("Created {name}"), false);
                                let id = a.get("id").and_then(|i| i.as_str()).map(String::from);
                                self.open_agent_editor(id);
                            }
                            Err(e) => self.toast(e.to_string(), true),
                        }
                    }
                    _ => {}
                }
            }
            FormOut::Save => match kind {
                FormKind::Settings => {
                    let body = json!({
                        "base_url": form.string("base_url").trim(),
                        "api_key": form.string("api_key"),
                        "model": form.string("model"),
                        "context_size": form.json_number("context_size"),
                        "auto_compact": form.boolean("auto_compact"),
                        "compact_at": form.json_number("compact_at"),
                        "max_steps": form.json_number("max_steps"),
                        "shell_timeout": form.json_number("shell_timeout"),
                        "auto_title": form.boolean("auto_title"),
                        "auto_memory": form.boolean("auto_memory"),
                        "bypass_approvals": form.boolean("bypass_approvals"),
                        "search_url": form.string("search_url").trim(),
                        "browser_executable": form.string("browser_executable").trim(),
                        "browser_headless": form.boolean("browser_headless"),
                    });
                    let body: serde_json::Map<String, Value> = body.as_object().unwrap().iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.clone(), v.clone())).collect();
                    match self.api.settings(Value::Object(body)).await {
                        Ok(s) => {
                            self.settings = s;
                            self.overlay = None;
                            self.poll_server().await;
                            self.toast("Settings saved", false);
                        }
                        Err(e) => self.toast(e.to_string(), true),
                    }
                }
                FormKind::Email => {
                    self.save_email().await;
                    self.overlay = None;
                }
                FormKind::Calendar => self.overlay = None,
                FormKind::Agent(id) => {
                    let body = self.agent_from_form(form);
                    if body.get("name").and_then(|n| n.as_str()).unwrap_or("").is_empty() {
                        return self.toast("The agent needs a name.", true);
                    }
                    let res = match &id {
                        Some(i) => self.api.put::<Value>(&format!("/api/agents/{i}"), body).await,
                        None => self.api.post::<Value>("/api/agents", body).await,
                    };
                    match res {
                        Ok(a) => {
                            self.overlay = None;
                            self.reload_state().await;
                            self.toast(format!("Saved {}", a.get("name").and_then(|n| n.as_str()).unwrap_or("agent")), false);
                        }
                        Err(e) => self.toast(e.to_string(), true),
                    }
                }
                FormKind::Routine(id) => {
                    let templates = routine_templates();
                    let tsel = form.string("template");
                    let mut name = form.string("name").trim().to_string();
                    let mut agent = form.string("agent_id");
                    let mut prompt = form.string("prompt").trim().to_string();
                    let mut schedule = if form.string("type") == "interval" {
                        json!({ "type": "interval", "minutes": form.json_number("minutes") })
                    } else {
                        let days: Vec<u64> = (0..7).filter(|i| form.boolean(&format!("day:{i}"))).collect();
                        json!({ "type": "daily", "time": form.string("time").trim(), "days": days })
                    };
                    if name.is_empty() {
                        if let Some(t) = tsel.parse::<usize>().ok().and_then(|i| templates.get(i)) {
                            name = t.0.to_string();
                            agent = t.1.to_string();
                            prompt = t.3.to_string();
                            schedule = t.2.clone();
                        }
                    }
                    let body = json!({ "name": name, "agent_id": agent, "prompt": prompt, "schedule": schedule, "enabled": form.boolean("enabled") });
                    let res = match &id {
                        Some(i) => self.api.put::<Value>(&format!("/api/routines/{i}"), body).await,
                        None => self.api.post::<Value>("/api/routines", body).await,
                    };
                    match res {
                        Ok(_) => {
                            self.toast("Routine saved", false);
                            self.open_routines().await;
                        }
                        Err(e) => self.toast(e.to_string(), true),
                    }
                }
            },
        }
    }

    async fn save_email(&mut self) {
        let Some(Overlay::Form { form, .. }) = &self.overlay else { return };
        let presets: HashMap<&str, (&str, u64, &str, &str, u64, &str)> = HashMap::from([
            ("gmail", ("imap.gmail.com", 993, "ssl", "smtp.gmail.com", 465, "ssl")),
            ("icloud", ("imap.mail.me.com", 993, "ssl", "smtp.mail.me.com", 587, "starttls")),
            ("yahoo", ("imap.mail.yahoo.com", 993, "ssl", "smtp.mail.yahoo.com", 465, "ssl")),
            ("fastmail", ("imap.fastmail.com", 993, "ssl", "smtp.fastmail.com", 465, "ssl")),
        ]);
        let mut body = json!({
            "username": form.string("username").trim(),
            "password": form.string("password"),
            "from_name": form.string("from_name").trim(),
            "from_address": form.string("username").trim(),
            "imap_host": form.string("imap_host").trim(),
            "imap_port": form.json_number("imap_port"),
            "imap_security": form.string("imap_security"),
            "smtp_host": form.string("smtp_host").trim(),
            "smtp_port": form.json_number("smtp_port"),
            "smtp_security": form.string("smtp_security"),
        });
        if let Some(p) = presets.get(form.string("preset").as_str()) {
            body["imap_host"] = Value::from(p.0);
            body["imap_port"] = Value::from(p.1);
            body["imap_security"] = Value::from(p.2);
            body["smtp_host"] = Value::from(p.3);
            body["smtp_port"] = Value::from(p.4);
            body["smtp_security"] = Value::from(p.5);
        }
        let body: serde_json::Map<String, Value> = body.as_object().unwrap().iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.clone(), v.clone())).collect();
        match self.api.put::<Value>("/api/email", Value::Object(body)).await {
            Ok(_) => self.toast("Email settings saved", false),
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    async fn add_calendar(&mut self) {
        let Some(Overlay::Form { form, .. }) = &self.overlay else { return };
        let body = json!({ "name": form.string("name").trim(), "url": form.string("url").trim() });
        match self.api.post::<Value>("/api/calendars", body).await {
            Ok(_) => {
                self.toast("Calendar connected", false);
                self.open_calendars().await;
            }
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    async fn do_act(&mut self, act: Act, value: Option<String>) {
        match act {
            Act::DeleteChat(id) => match self.api.delete::<Value>(&format!("/api/chats/{id}")).await {
                Ok(_) => {
                    if self.chat_id.as_deref() == Some(&id) {
                        self.open_chat(None).await;
                    }
                    self.refresh_chats().await;
                    self.toast("Chat deleted", false);
                }
                Err(e) => self.toast(e.to_string(), true),
            },
            Act::DeleteIdle(agent) => {
                let ids: Vec<String> = self.chats.iter().filter(|c| agent.as_ref().map(|a| &c.agent_id == a).unwrap_or(c.parent.is_none()) && c.status == "idle" && !c.pinned && c.routine_id.is_none()).map(|c| c.id.clone()).collect();
                match self.api.post::<Value>("/api/chats/delete", json!({ "ids": ids })).await {
                    Ok(_) => {
                        if self.chat_id.as_ref().map(|c| ids.contains(c)).unwrap_or(false) {
                            self.open_chat(None).await;
                        }
                        self.refresh_chats().await;
                        self.toast(format!("Deleted {} chats", ids.len()), false);
                    }
                    Err(e) => self.toast(e.to_string(), true),
                }
            }
            Act::DeleteAgent(id) => match self.api.delete::<Value>(&format!("/api/agents/{id}")).await {
                Ok(_) => {
                    self.reload_state().await;
                    self.toast("Agent deleted", false);
                }
                Err(e) => self.toast(e.to_string(), true),
            },
            Act::DeleteMemory(id) => match self.api.delete::<Value>(&format!("/api/memory/{id}")).await {
                Ok(_) => self.reload_memory().await,
                Err(e) => self.toast(e.to_string(), true),
            },
            Act::DeleteRoutine(id) => match self.api.delete::<Value>(&format!("/api/routines/{id}")).await {
                Ok(_) => self.reload_routines().await,
                Err(e) => self.toast(e.to_string(), true),
            },
            Act::Bypass(on) => self.set_bypass(on).await,
            Act::Regenerate(start) => self.run_body(json!({ "from_index": start })).await,
            Act::RenameChat(id) => {
                let title = value.unwrap_or_default().trim().to_string();
                if title.is_empty() {
                    return;
                }
                match self.api.patch::<Value>(&format!("/api/chats/{id}"), json!({ "title": title })).await {
                    Ok(_) => {
                        if let Some(c) = &mut self.chat {
                            if c.get("id").and_then(|i| i.as_str()) == Some(&id) {
                                c["title"] = Value::from(title.clone());
                            }
                        }
                        self.refresh_chats().await;
                    }
                    Err(e) => self.toast(e.to_string(), true),
                }
            }
            Act::AttachPath => {
                let path = value.unwrap_or_default().trim().to_string();
                if path.is_empty() {
                    return;
                }
                let Some(cid) = self.chat_id.clone() else { return };
                let p = PathBuf::from(shellexpand(&path));
                match std::fs::read(&p) {
                    Ok(bytes) => {
                        if bytes.len() > 20 * 1024 * 1024 {
                            return self.toast("That file is larger than 20 MB", true);
                        }
                        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or("file".into());
                        match self.api.upload(&cid, &name, bytes).await {
                            Ok(u) => {
                                self.toast(format!("Attached {} ({}){}", u.name, u.size, if u.image { ", as an image" } else { "" }), false);
                                self.attachments.push(u);
                            }
                            Err(e) => self.toast(e.to_string(), true),
                        }
                    }
                    Err(e) => self.toast(format!("Couldn't read {}: {e}", p.display()), true),
                }
            }
            Act::AddMemory => {
                let fact = value.unwrap_or_default().trim().to_string();
                if fact.is_empty() {
                    return;
                }
                match self.api.post::<Value>("/api/memory", json!({ "fact": fact, "importance": 7 })).await {
                    Ok(m) => {
                        if m.get("action").and_then(|a| a.as_str()) == Some("merged") {
                            self.toast("Updated a similar memory instead of adding a duplicate", false);
                        }
                        self.reload_memory().await;
                    }
                    Err(e) => self.toast(e.to_string(), true),
                }
            }
            Act::AddCalendar => {}
            Act::Tidy => match self.api.post::<Value>("/api/memory/tidy", json!({})).await {
                Ok(r) => {
                    self.toast(r.get("message").and_then(|m| m.as_str()).unwrap_or("Tidied").to_string(), false);
                    self.reload_memory().await;
                }
                Err(e) => self.toast(e.to_string(), true),
            },
        }
    }

    // ---------------------------------------------------------------- events

    pub async fn on_incoming(&mut self, inc: Incoming) {
        match inc {
            Incoming::Global(ev) => self.on_global(ev).await,
            Incoming::Chat { chat_id, event } => {
                if Some(&chat_id) == self.chat_id.as_ref() {
                    self.on_chat_event(event).await;
                }
            }
            Incoming::StreamEnded { chat_id, error } => {
                if Some(&chat_id) == self.chat_id.as_ref() {
                    if let Some(e) = error {
                        self.toast(format!("Lost the chat stream: {e}"), true);
                    }
                    // reconnect after a moment
                    let api = self.api.clone();
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        api.follow_chat(chat_id, tx).await;
                    });
                }
            }
            Incoming::GlobalEnded(_) => {
                let api = self.api.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    api.follow_global(tx).await;
                });
            }
        }
    }

    async fn on_global(&mut self, ev: Event) {
        let d = &ev.data;
        match ev.kind.as_str() {
            "chat_status" => {
                let id = d.get("chat_id").and_then(|c| c.as_str()).unwrap_or("").to_string();
                let status = d.get("status").and_then(|s| s.as_str()).unwrap_or("idle").to_string();
                let before = self.statuses.insert(id.clone(), status.clone());
                if let Some(c) = self.chats.iter_mut().find(|c| c.id == id) {
                    c.status = status.clone();
                    if status == "waiting" && before.as_deref() != Some("waiting") && self.bell {
                        print!("\x07");
                    }
                }
                self.rebuild_sidebar();
            }
            "chats_changed" | "hello" => {
                self.refresh_chats().await;
                if ev.kind == "hello" {
                    self.refresh_memory_count().await;
                }
            }
            "chat_title" => {
                let id = d.get("chat_id").and_then(|c| c.as_str()).unwrap_or("");
                let title = d.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
                if let Some(c) = self.chats.iter_mut().find(|c| c.id == id) {
                    c.title = title.clone();
                }
                if let Some(c) = &mut self.chat {
                    if c.get("id").and_then(|i| i.as_str()) == Some(id) {
                        c["title"] = Value::from(title);
                    }
                }
            }
            "queue_returned" => {
                let id = d.get("chat_id").and_then(|c| c.as_str()).unwrap_or("");
                let text = d.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                if Some(id) == self.chat_id.as_deref() {
                    self.put_in_composer(&text);
                    self.toast("The run ended before the agent read your queued message; it's back in the message box.", false);
                } else {
                    let prev = self.drafts.get(id).cloned().unwrap_or_default();
                    self.drafts.insert(id.to_string(), if prev.is_empty() { text } else { format!("{text}\n\n{prev}") });
                }
            }
            "routines_changed" => {
                if matches!(self.overlay, Some(Overlay::Routines { .. })) {
                    self.reload_routines().await;
                }
            }
            "memory_status" => self.memory_working = d.get("state").and_then(|s| s.as_str()) == Some("working"),
            "memory_changed" => {
                self.refresh_memory_count().await;
                if matches!(self.overlay, Some(Overlay::Memory { .. })) {
                    self.reload_memory().await;
                }
                let by = d.get("by").and_then(|b| b.as_str()).unwrap_or("");
                let learned: Vec<String> = ["added", "updated"].iter().flat_map(|k| d.get(*k).and_then(|a| a.as_array()).cloned().unwrap_or_default()).filter_map(|v| v.as_str().map(String::from)).collect();
                if by == "auto" && !learned.is_empty() {
                    self.toast(format!("Remembered: {}{}", learned[0], if learned.len() > 1 { format!(" (+{} more)", learned.len() - 1) } else { String::new() }), false);
                } else if !by.is_empty() && by != "you" && by != "auto" && by != "tidy" {
                    if let Some(s) = d.get("summary").and_then(|s| s.as_str()) {
                        self.toast(format!("{by}: {s}"), false);
                    }
                }
            }
            _ => {}
        }
    }

    async fn on_chat_event(&mut self, ev: Event) {
        let d = ev.data.clone();
        let path: Vec<String> = d.get("path").and_then(|p| p.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        let agents = self.agents.clone();
        match ev.kind.as_str() {
            "snapshot" => {
                let chat = d.get("chat").cloned().unwrap_or(Value::Null);
                let running = d.get("running").and_then(|r| r.as_bool()).unwrap_or(false);
                let base = d.get("run_base").and_then(|r| r.as_u64()).map(|r| r as usize);
                self.trace = Trace::from_chat(&chat, &agents, if running { base } else { None });
                if running {
                    let aid = chat.get("agent_id").and_then(|a| a.as_str()).unwrap_or("").to_string();
                    self.trace.start_turn(agent_who(&agents, &aid), base.unwrap_or(0));
                }
                self.read_stats(&chat);
                self.chat = Some(chat);
                self.running = running;
                self.queue = d.get("queue").and_then(|q| q.as_array()).cloned().unwrap_or_default();
                self.follow = true;
                self.touch();
            }
            "run_start" => {
                let chat = d.get("chat").cloned().unwrap_or(Value::Null);
                let base = d.get("run_base").and_then(|r| r.as_u64()).unwrap_or(0) as usize;
                let aid = d.get("agent_id").and_then(|a| a.as_str()).unwrap_or("").to_string();
                self.trace = Trace::from_chat(&chat, &agents, Some(base));
                self.trace.start_turn(agent_who(&agents, &aid), base);
                self.chat = Some(chat);
                self.running = true;
                self.follow = true;
                self.touch();
            }
            "assistant_start" => {
                self.trace.assistant_start(&path, &agents);
                self.touch();
            }
            "delta" => {
                let kind = d.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                let text = d.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if kind == "compact" {
                    self.trace.compact_delta(&path, text.len(), &agents);
                } else {
                    self.trace.delta(&path, kind, text, d.get("index").and_then(|i| i.as_u64()), d.get("name").and_then(|n| n.as_str()), &agents);
                }
                self.touch();
            }
            "assistant_end" => {
                let m = d.get("message").cloned().unwrap_or(Value::Null);
                self.trace.assistant_end(&path, &m, &agents);
                if path.is_empty() {
                    if let Some(st) = m.get("_stats") {
                        if let Some(p) = st.get("prompt_tokens").and_then(|x| x.as_u64()) {
                            self.ctx_tokens = Some(p + st.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0));
                        }
                        self.tok_per_s = st.get("tok_per_s").and_then(|x| x.as_f64()).or(self.tok_per_s);
                    }
                }
                self.touch();
            }
            "tool_start" => {
                self.trace.tool_start(&path, d.get("call_id").and_then(|c| c.as_str()).unwrap_or(""), d.get("name").and_then(|n| n.as_str()).unwrap_or(""), d.get("args").cloned().unwrap_or(Value::Null), &agents);
                self.touch();
            }
            "tool_progress" => {
                self.trace.tool_progress(&path, d.get("call_id").and_then(|c| c.as_str()).unwrap_or(""), d.get("content").and_then(|c| c.as_str()).unwrap_or(""), &agents);
                self.touch();
            }
            "tool_result" => {
                self.trace.tool_result(&path, d.get("call_id").and_then(|c| c.as_str()).unwrap_or(""), d.get("content").and_then(|c| c.as_str()).unwrap_or(""), &agents);
                if self.cfg.files {
                    self.refresh_files(false).await;
                }
                self.touch();
            }
            "user_message" => {
                let m = d.get("message").cloned().unwrap_or(Value::Null);
                let index = d.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                self.trace.entries.push(Entry::User(trace::user_from_event(&m, index)));
                let aid = self.chat_agent_id();
                self.trace.start_turn(agent_who(&agents, &aid), index + 1);
                self.touch();
            }
            "queue" => {
                self.queue = d.get("items").and_then(|q| q.as_array()).cloned().unwrap_or_default();
            }
            "ctx" => {
                if path.is_empty() {
                    self.ctx_tokens = d.get("tokens").and_then(|t| t.as_u64());
                }
            }
            "subagent_start" => {
                self.trace.subagent_start(&path, d.get("agent_id").and_then(|a| a.as_str()).unwrap_or(""), d.get("chat_id").and_then(|c| c.as_str()).unwrap_or(""), d.get("continued").and_then(|c| c.as_bool()).unwrap_or(false), &agents);
                self.touch();
            }
            "compact_start" => {
                self.trace.compact_start(&path, &agents);
                self.touch();
            }
            "compact_end" => {
                self.trace.compact_end(&path, d.get("compaction").unwrap_or(&Value::Null), &agents);
                self.touch();
            }
            "approval" => {
                self.trace.approval(&path, d.get("call_id").and_then(|c| c.as_str()).unwrap_or(""), d.get("approval_id").and_then(|a| a.as_str()), None, &agents);
                if self.bell {
                    print!("\x07");
                }
                self.touch();
            }
            "approval_done" => {
                self.trace.approval(&path, d.get("call_id").and_then(|c| c.as_str()).unwrap_or(""), None, d.get("approved").and_then(|a| a.as_bool()), &agents);
                self.touch();
            }
            "approval_bypassed" => {
                self.trace.bypassed(&path, d.get("call_id").and_then(|c| c.as_str()).unwrap_or(""), d.get("reason").and_then(|r| r.as_str()).unwrap_or("settings"), &agents);
                self.touch();
            }
            "notice" => {
                self.trace.notice(&path, d.get("text").and_then(|t| t.as_str()).unwrap_or(""), &agents);
                self.touch();
            }
            "error" => {
                let n = self.chat.as_ref().and_then(|c| c.get("messages")).and_then(|m| m.as_array()).map(|m| m.len()).unwrap_or(0);
                self.trace.error(d.get("message").and_then(|m| m.as_str()).unwrap_or(""), n);
                self.touch();
            }
            "done" => {
                let chat = d.get("chat").cloned().unwrap_or(Value::Null);
                self.trace = Trace::from_chat(&chat, &agents, None);
                self.read_stats(&chat);
                self.chat = Some(chat);
                self.running = false;
                self.queue.clear();
                if let Some(id) = self.chat_id.clone() {
                    self.mark_seen(&id);
                }
                if self.cfg.files {
                    self.refresh_files(false).await;
                }
                self.touch();
            }
            _ => {}
        }
    }

    fn read_stats(&mut self, chat: &Value) {
        if let Some(st) = chat.get("stats") {
            self.ctx_tokens = st.get("context_tokens").and_then(|x| x.as_u64());
            self.tok_per_s = st.get("tok_per_s").and_then(|x| x.as_f64());
        }
    }

    // ------------------------------------------------------------------ keys

    pub async fn on_term(&mut self, ev: TermEvent) {
        match ev {
            TermEvent::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k).await,
            TermEvent::Mouse(m) => self.on_mouse(m).await,
            TermEvent::Resize(_, _) => self.touch(),
            TermEvent::Paste(text) => {
                if self.overlay.is_none() && self.focus == Focus::Composer {
                    self.composer.insert_str(text);
                }
            }
            _ => {}
        }
    }

    async fn on_mouse(&mut self, m: MouseEvent) {
        if self.overlay.is_some() {
            return;
        }
        let (sx, _) = self.layout_cols();
        match m.kind {
            MouseEventKind::ScrollUp => {
                if (m.column as usize) < sx && self.cfg.sidebar {
                    self.side_scroll = self.side_scroll.saturating_sub(3);
                } else {
                    self.scroll = self.scroll.saturating_sub(3);
                    self.follow = false;
                }
            }
            MouseEventKind::ScrollDown => {
                if (m.column as usize) < sx && self.cfg.sidebar {
                    self.side_scroll += 3;
                } else {
                    self.scroll += 3;
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if (m.column as usize) < sx && self.cfg.sidebar {
                    // rows start under the brand line (2) and the "Agents" label (1)
                    let row = m.row as usize;
                    if let Some(i) = self.side_row_at(row) {
                        self.side_sel = i;
                        self.focus = Focus::Sidebar;
                        self.side_activate().await;
                    }
                } else {
                    self.focus = Focus::Composer;
                }
            }
            _ => {}
        }
    }

    pub fn layout_cols(&self) -> (usize, usize) {
        let side = if self.cfg.sidebar { 30 } else { 0 };
        let files = if self.cfg.files { 32 } else { 0 };
        (side, files)
    }

    /// Which sidebar row is drawn at terminal row `y` (the drawing code records this each frame).
    pub fn side_row_at(&self, y: usize) -> Option<usize> {
        self.side_row_map.get(&y).copied()
    }

    async fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        // global
        match (k.code, ctrl, alt) {
            (KeyCode::Char('q'), true, _) | (KeyCode::Char('c'), true, _) => {
                self.quit = true;
                return;
            }
            (KeyCode::Char('k'), true, _) => {
                if matches!(self.overlay, Some(Overlay::Palette { .. })) {
                    self.overlay = None;
                } else {
                    self.open_palette();
                }
                return;
            }
            (KeyCode::F(1), _, _) => {
                self.overlay = if matches!(self.overlay, Some(Overlay::Help)) { None } else { Some(Overlay::Help) };
                return;
            }
            _ => {}
        }
        if self.overlay.is_some() {
            self.on_overlay_key(k).await;
            return;
        }
        match (k.code, ctrl, alt) {
            (KeyCode::Char('o'), _, true) => return self.toggle_verbose(),
            (KeyCode::Char('b'), _, true) | (KeyCode::Char('b'), true, _) => {
                self.cfg.sidebar = !self.cfg.sidebar;
                self.save_cfg();
                if !self.cfg.sidebar && self.focus == Focus::Sidebar {
                    self.focus = Focus::Composer;
                }
                return;
            }
            (KeyCode::Char('f'), _, true) => return self.toggle_files().await,
            (KeyCode::Char('n'), _, true) | (KeyCode::Char('n'), true, _) => {
                if !self.filter.is_empty() {
                    let f = self.filter.clone();
                    self.new_chat(&f).await;
                } else {
                    self.overlay = Some(Overlay::Pick { sel: 0 });
                }
                return;
            }
            (KeyCode::Char('m'), _, true) => return self.open_memory().await,
            (KeyCode::Char('r'), _, true) => return self.open_routines().await,
            (KeyCode::Char('s'), _, true) => return self.open_settings().await,
            (KeyCode::Char('a'), _, true) => {
                let id = self.selected_agent();
                return self.open_agent_editor(id);
            }
            (KeyCode::Char('p'), _, true) => return self.pin().await,
            (KeyCode::Char('x'), _, true) => return self.export("md").await,
            (KeyCode::Char('d'), _, true) => return self.run_cmd("delete").await,
            (KeyCode::Char('2'), _, true) | (KeyCode::F(2), _, _) => return self.run_cmd("rename").await,
            (KeyCode::Char('u'), true, _) | (KeyCode::Char('u'), _, true) => {
                if self.chat_id.is_some() && !self.is_subchat() {
                    self.overlay = Some(Overlay::Prompt { title: "Attach a file (path)".into(), value: String::new(), act: Act::AttachPath });
                }
                return;
            }
            (KeyCode::Char('s'), true, _) => {
                if self.running {
                    self.stop().await;
                }
                return;
            }
            (KeyCode::Char('r'), true, _) => return self.regenerate().await,
            (KeyCode::Char('e'), true, _) => return self.edit_last().await,
            (KeyCode::Char('l'), true, _) => return self.compact().await,
            (KeyCode::Char('y'), true, _) => return self.run_cmd("bypass").await,
            (KeyCode::Char('g'), true, _) => {
                let n = self.chat.as_ref().and_then(|c| c.get("messages")).and_then(|m| m.as_array()).map(|m| m.len()).unwrap_or(0);
                if n > 0 && !self.is_subchat() {
                    self.branch(n).await;
                }
                return;
            }
            (KeyCode::Tab, false, false) => {
                self.focus = match self.focus {
                    Focus::Sidebar => if self.chat_id.is_some() { Focus::Messages } else if self.cfg.files { Focus::Files } else { Focus::Sidebar },
                    Focus::Messages => if self.is_subchat() { if self.cfg.files { Focus::Files } else { Focus::Sidebar } } else { Focus::Composer },
                    Focus::Composer => if self.cfg.files { Focus::Files } else if self.cfg.sidebar { Focus::Sidebar } else { Focus::Messages },
                    Focus::Files => if self.cfg.sidebar { Focus::Sidebar } else { Focus::Messages },
                };
                return;
            }
            (KeyCode::BackTab, _, _) => {
                self.focus = match self.focus {
                    Focus::Sidebar => if self.cfg.files { Focus::Files } else if self.chat_id.is_some() { Focus::Composer } else { Focus::Sidebar },
                    Focus::Messages => if self.cfg.sidebar { Focus::Sidebar } else { Focus::Composer },
                    Focus::Composer => Focus::Messages,
                    Focus::Files => if self.chat_id.is_some() { Focus::Composer } else { Focus::Sidebar },
                };
                return;
            }
            _ => {}
        }
        // approvals: y / a / n work anywhere outside the composer text (and in it with Alt)
        if !self.trace.pending_approvals().is_empty() && (self.focus != Focus::Composer || alt || self.composer.lines().join("").is_empty()) {
            match k.code {
                KeyCode::Char('y') => return self.approve(true, false).await,
                KeyCode::Char('a') => return self.approve(true, true).await,
                KeyCode::Char('n') => return self.approve(false, false).await,
                _ => {}
            }
        }
        match self.focus {
            Focus::Sidebar => self.on_sidebar_key(k).await,
            Focus::Messages => self.on_messages_key(k).await,
            Focus::Composer => self.on_composer_key(k).await,
            Focus::Files => self.on_files_key(k).await,
        }
    }

    async fn on_sidebar_key(&mut self, k: KeyEvent) {
        if self.search_focus {
            match k.code {
                KeyCode::Esc => {
                    self.search_focus = false;
                    self.search.clear();
                    self.hits.clear();
                    self.rebuild_sidebar();
                }
                KeyCode::Enter | KeyCode::Down => {
                    self.search_focus = false;
                    if let Some(i) = self.side_rows.iter().position(|r| matches!(r, SideRow::Chat(_))) {
                        self.side_sel = i;
                    }
                }
                KeyCode::Backspace => {
                    self.search.pop();
                    self.update_search().await;
                }
                KeyCode::Char(c) => {
                    self.search.push(c);
                    self.update_search().await;
                }
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => self.side_move(-1),
            KeyCode::Down | KeyCode::Char('j') => self.side_move(1),
            KeyCode::PageUp => for _ in 0..8 { self.side_move(-1) },
            KeyCode::PageDown => for _ in 0..8 { self.side_move(1) },
            KeyCode::Home => self.side_sel = 0,
            KeyCode::End => self.side_sel = self.side_rows.len().saturating_sub(1),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.side_activate().await,
            KeyCode::Char('/') => self.search_focus = true,
            KeyCode::Char('n') => {
                if let Some(SideRow::Agent(id)) = self.side_rows.get(self.side_sel).cloned() {
                    self.new_chat(&id).await;
                } else if !self.filter.is_empty() {
                    let f = self.filter.clone();
                    self.new_chat(&f).await;
                } else {
                    self.overlay = Some(Overlay::Pick { sel: 0 });
                }
            }
            KeyCode::Char('e') => {
                let id = self.selected_agent();
                self.open_agent_editor(id);
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                match self.side_rows.get(self.side_sel).cloned() {
                    Some(SideRow::Chat(_)) => self.run_cmd("delete").await,
                    Some(SideRow::Agent(id)) => {
                        let name = self.agent(&id).map(|a| a.name.clone()).unwrap_or_default();
                        self.overlay = Some(Overlay::Confirm { title: format!("Delete {name}'s idle chats?"), text: "Every idle, unpinned chat with it will be removed. Pinned chats and routine chats stay.".into(), ok: "Delete all".into(), act: Act::DeleteIdle(Some(id)) });
                    }
                    Some(SideRow::AllAgents) => self.overlay = Some(Overlay::Confirm { title: "Delete all idle chats?".into(), text: "Every idle, unpinned chat you started will be removed.".into(), ok: "Delete all".into(), act: Act::DeleteIdle(None) }),
                    _ => {}
                }
            }
            KeyCode::Char('r') => self.run_cmd("rename").await,
            KeyCode::Char('p') => self.pin().await,
            KeyCode::Char('x') => self.export("md").await,
            KeyCode::Char('J') => self.export("json").await,
            KeyCode::Char('m') => {
                for c in &self.chats {
                    self.cfg.seen.insert(c.id.clone(), c.updated);
                }
                self.save_cfg();
                self.toast("Marked all as read", false);
            }
            KeyCode::Esc => {
                if !self.search.is_empty() {
                    self.search.clear();
                    self.hits.clear();
                    self.rebuild_sidebar();
                }
            }
            _ => {}
        }
    }

    async fn update_search(&mut self) {
        let q = self.search.trim().to_string();
        if q.chars().count() >= 2 {
            if let Ok(hits) = self.api.search(&q).await {
                self.hits = hits.into_iter().map(|h| (h.chat_id.clone(), h)).collect();
            }
        } else {
            self.hits.clear();
        }
        self.rebuild_sidebar();
    }

    async fn on_messages_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll = self.scroll.saturating_sub(1);
                self.follow = false;
            }
            KeyCode::Down | KeyCode::Char('j') => self.scroll += 1,
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(20);
                self.follow = false;
            }
            KeyCode::PageDown => self.scroll += 20,
            KeyCode::Home | KeyCode::Char('g') => {
                self.scroll = 0;
                self.follow = false;
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.follow = true;
                self.scroll = usize::MAX / 2;
            }
            KeyCode::Char('[') => self.select_turn(-1),
            KeyCode::Char(']') => self.select_turn(1),
            KeyCode::Char('c') => {
                if let Some(text) = self.selected_turn_text() {
                    self.copy(&text);
                    self.toast("Copied the reply", false);
                }
            }
            KeyCode::Char('b') => {
                let end = self.selected_turn.and_then(|i| match self.trace.entries.get(i) { Some(Entry::Turn(t)) => Some(t.end), _ => None });
                if let Some(end) = end {
                    self.branch(end).await;
                }
            }
            KeyCode::Char('r') => {
                let start = self.selected_turn.and_then(|i| match self.trace.entries.get(i) { Some(Entry::Turn(t)) => Some(t.start), _ => None });
                if let Some(start) = start {
                    let last = self.trace.entries.iter().rev().find_map(|e| if let Entry::Turn(t) = e { Some(t.start) } else { None });
                    if last == Some(start) {
                        self.run_body(json!({ "from_index": start })).await;
                    } else {
                        self.overlay = Some(Overlay::Confirm { title: "Regenerate this reply?".into(), text: "Every message after it will be deleted.".into(), ok: "Regenerate".into(), act: Act::Regenerate(start) });
                    }
                }
            }
            KeyCode::Char('o') => {
                // open the handoff chat of the selected turn's first handoff
                if let Some(id) = self.selected_turn.and_then(|i| match self.trace.entries.get(i) {
                    Some(Entry::Turn(t)) => t.steps.iter().find_map(|s| match s { trace::Step::Tool(x) => x.sub.as_ref().and_then(|s| s.chat_id.clone()), _ => None }),
                    _ => None,
                }) {
                    self.open_chat(Some(id)).await;
                }
            }
            KeyCode::Enter | KeyCode::Char('i') => self.focus = Focus::Composer,
            KeyCode::Esc => self.focus = if self.cfg.sidebar { Focus::Sidebar } else { Focus::Composer },
            _ => {}
        }
    }

    fn select_turn(&mut self, dir: i32) {
        let turns: Vec<usize> = self.trace.entries.iter().enumerate().filter_map(|(i, e)| if matches!(e, Entry::Turn(_)) { Some(i) } else { None }).collect();
        if turns.is_empty() {
            return;
        }
        let cur = self.selected_turn.and_then(|s| turns.iter().position(|t| *t == s));
        let next = match (cur, dir) {
            (None, _) => turns.len() - 1,
            (Some(i), d) => (i as i32 + d).clamp(0, turns.len() as i32 - 1) as usize,
        };
        self.selected_turn = Some(turns[next]);
        self.follow = false;
        self.touch();
        // scroll so the turn is visible
        if let Some(r) = &self.rendered {
            if let Some(line) = r.owners.iter().position(|o| *o == Some(turns[next])) {
                self.scroll = line;
            }
        }
    }

    fn selected_turn_text(&self) -> Option<String> {
        let i = self.selected_turn.or_else(|| self.trace.entries.iter().rposition(|e| matches!(e, Entry::Turn(_))))?;
        match self.trace.entries.get(i) {
            Some(Entry::Turn(t)) => {
                let texts: Vec<String> = t.steps.iter().filter_map(|s| if let trace::Step::Text { text, .. } = s { Some(text.clone()) } else { None }).collect();
                if texts.is_empty() { None } else { Some(texts.join("\n\n")) }
            }
            _ => None,
        }
    }

    fn copy(&self, text: &str) {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        print!("\x1b]52;c;{b64}\x07");
    }

    async fn on_composer_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::Enter if alt || ctrl || k.modifiers.contains(KeyModifiers::SHIFT) => {
                self.composer.insert_newline();
            }
            KeyCode::Char('j') if ctrl => self.composer.insert_newline(),
            KeyCode::Enter => {
                if let Some(idx) = self.edit_from.take() {
                    let content = self.composer.lines().join("\n").trim().to_string();
                    if !content.is_empty() {
                        self.run_body(json!({ "content": content, "from_index": idx })).await;
                        self.composer = TextArea::default();
                        self.composer.set_cursor_line_style(ratatui::style::Style::default());
                    }
                } else {
                    self.send().await;
                }
            }
            KeyCode::Esc => {
                if self.edit_from.take().is_some() {
                    self.composer = TextArea::default();
                    self.composer.set_cursor_line_style(ratatui::style::Style::default());
                    self.toast("Edit cancelled", false);
                } else if self.running {
                    self.stop().await;
                } else {
                    self.focus = Focus::Messages;
                }
            }
            KeyCode::Up if self.composer.lines().join("").is_empty() && !self.queue.is_empty() => self.unqueue_all().await,
            KeyCode::Up if self.composer.cursor().0 == 0 && self.composer.lines().len() <= 1 => {
                self.focus = Focus::Messages;
                self.follow = false;
                self.scroll = self.scroll.saturating_sub(1);
            }
            KeyCode::PageUp => {
                self.focus = Focus::Messages;
                self.scroll = self.scroll.saturating_sub(20);
                self.follow = false;
            }
            KeyCode::PageDown => self.scroll += 20,
            KeyCode::Backspace if self.composer.lines().join("").is_empty() && !self.attachments.is_empty() => {
                self.attachments.pop();
            }
            _ => {
                self.composer.input(k);
            }
        }
    }

    async fn on_files_key(&mut self, k: KeyEvent) {
        let n = self.files.entries.len() + 1;
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => self.files.sel = self.files.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.files.sel = (self.files.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.files_activate().await,
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => {
                self.files.sel = 0;
                self.files_activate().await;
            }
            KeyCode::Char('r') => self.refresh_files(false).await,
            KeyCode::Esc => self.focus = Focus::Composer,
            _ => {}
        }
    }

    async fn on_overlay_key(&mut self, k: KeyEvent) {
        let Some(overlay) = self.overlay.take() else { return };
        match overlay {
            Overlay::Help | Overlay::Toasts => {
                if !matches!(k.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::F(1) | KeyCode::Char('?')) {
                    self.overlay = Some(overlay);
                }
            }
            Overlay::Confirm { title, text, ok, act } => match k.code {
                KeyCode::Enter | KeyCode::Char('y') => self.do_act(act, None).await,
                KeyCode::Esc | KeyCode::Char('n') => {}
                _ => self.overlay = Some(Overlay::Confirm { title, text, ok, act }),
            },
            Overlay::Prompt { title, mut value, act } => match k.code {
                KeyCode::Enter => self.do_act(act, Some(value)).await,
                KeyCode::Esc => {}
                KeyCode::Backspace => {
                    value.pop();
                    self.overlay = Some(Overlay::Prompt { title, value, act });
                }
                KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    value.clear();
                    self.overlay = Some(Overlay::Prompt { title, value, act });
                }
                KeyCode::Char(c) => {
                    value.push(c);
                    self.overlay = Some(Overlay::Prompt { title, value, act });
                }
                _ => self.overlay = Some(Overlay::Prompt { title, value, act }),
            },
            Overlay::Pick { mut sel } => {
                let n = self.agents.len();
                match k.code {
                    KeyCode::Esc => {}
                    KeyCode::Up | KeyCode::Char('k') => {
                        sel = sel.saturating_sub(1);
                        self.overlay = Some(Overlay::Pick { sel });
                    }
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                        sel = (sel + 1).min(n.saturating_sub(1));
                        self.overlay = Some(Overlay::Pick { sel });
                    }
                    KeyCode::Enter => {
                        if let Some(a) = self.agents.get(sel).cloned() {
                            self.new_chat(&a.id).await;
                        }
                    }
                    KeyCode::Char(c) if c.is_ascii_digit() => {
                        let i = c.to_digit(10).unwrap() as usize;
                        if i >= 1 && i <= n {
                            let id = self.agents[i - 1].id.clone();
                            self.new_chat(&id).await;
                        } else {
                            self.overlay = Some(Overlay::Pick { sel });
                        }
                    }
                    _ => self.overlay = Some(Overlay::Pick { sel }),
                }
            }
            Overlay::Palette { mut q, items, mut sel, mut hits } => {
                match k.code {
                    KeyCode::Esc => return,
                    KeyCode::Up => sel = sel.saturating_sub(1),
                    KeyCode::Down | KeyCode::Tab => sel = (sel + 1).min(items.len().saturating_sub(1)),
                    KeyCode::Enter => {
                        if let Some(it) = items.get(sel) {
                            let action = it.action.clone();
                            match action {
                                PaletteAction::OpenChat(id) => {
                                    self.open_chat(Some(id)).await;
                                    self.focus = Focus::Composer;
                                }
                                PaletteAction::NewChat(id) => self.new_chat(&id).await,
                                PaletteAction::Cmd(c) => self.run_cmd(&c).await,
                            }
                        }
                        return;
                    }
                    KeyCode::Backspace => {
                        q.pop();
                        sel = 0;
                    }
                    KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                        q.push(c);
                        sel = 0;
                    }
                    _ => {}
                }
                if q.trim().chars().count() >= 2 {
                    hits = self.api.search(q.trim()).await.unwrap_or_default();
                } else {
                    hits.clear();
                }
                let mut o = Overlay::Palette { q, items, sel, hits };
                self.fill_palette(&mut o);
                self.overlay = Some(o);
            }
            Overlay::Form { mut form, kind } => {
                let out = form.handle(k);
                self.overlay = Some(Overlay::Form { form, kind });
                self.handle_form(out).await;
            }
            Overlay::Viewer { title, lines, mut scroll } => match k.code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Up | KeyCode::Char('k') => {
                    scroll = scroll.saturating_sub(1);
                    self.overlay = Some(Overlay::Viewer { title, lines, scroll });
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    scroll = (scroll + 1).min(lines.len().saturating_sub(1));
                    self.overlay = Some(Overlay::Viewer { title, lines, scroll });
                }
                KeyCode::PageUp => {
                    scroll = scroll.saturating_sub(20);
                    self.overlay = Some(Overlay::Viewer { title, lines, scroll });
                }
                KeyCode::PageDown => {
                    scroll = (scroll + 20).min(lines.len().saturating_sub(1));
                    self.overlay = Some(Overlay::Viewer { title, lines, scroll });
                }
                KeyCode::Home => self.overlay = Some(Overlay::Viewer { title, lines, scroll: 0 }),
                KeyCode::End => {
                    scroll = lines.len().saturating_sub(1);
                    self.overlay = Some(Overlay::Viewer { title, lines, scroll });
                }
                KeyCode::Char('c') => {
                    self.copy(&lines.join("\n"));
                    self.toast("Copied the file", false);
                    self.overlay = Some(Overlay::Viewer { title, lines, scroll });
                }
                _ => self.overlay = Some(Overlay::Viewer { title, lines, scroll }),
            },
            Overlay::Memory { data, cats, total, mut sel, mut q, mut cat, mut typing } => {
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                if typing {
                    match k.code {
                        KeyCode::Esc | KeyCode::Enter => typing = false,
                        KeyCode::Backspace => q.pop().map(|_| ()).unwrap_or(()),
                        KeyCode::Char(c) if !ctrl => q.push(c),
                        _ => {}
                    }
                    self.overlay = Some(Overlay::Memory { data, cats, total, sel, q, cat, typing });
                    self.reload_memory().await;
                    return;
                }
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') => return,
                    KeyCode::Up | KeyCode::Char('k') => sel = sel.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => sel = (sel + 1).min(data.len().saturating_sub(1)),
                    KeyCode::Char('/') => typing = true,
                    KeyCode::Tab | KeyCode::Right => cat = (cat + 1) % (cats.len() + 1),
                    KeyCode::Left => cat = (cat + cats.len()) % (cats.len() + 1),
                    KeyCode::Char('a') => {
                        self.overlay = Some(Overlay::Memory { data, cats, total, sel, q, cat, typing });
                        self.overlay = Some(Overlay::Prompt { title: "Remember something (e.g. “I prefer short answers”)".into(), value: String::new(), act: Act::AddMemory });
                        // the prompt replaces the panel; AddMemory reopens it
                        self.memory_return = true;
                        return;
                    }
                    KeyCode::Char('d') | KeyCode::Delete => {
                        if let Some(m) = data.get(sel) {
                            let id = m.id;
                            self.overlay = Some(Overlay::Memory { data: data.clone(), cats: cats.clone(), total, sel, q: q.clone(), cat, typing });
                            self.do_act(Act::DeleteMemory(id), None).await;
                            return;
                        }
                    }
                    KeyCode::Char('p') => {
                        if let Some(m) = data.get(sel) {
                            let pinned = m.pinned != 0;
                            let id = m.id;
                            self.overlay = Some(Overlay::Memory { data: data.clone(), cats: cats.clone(), total, sel, q: q.clone(), cat, typing });
                            if let Err(e) = self.api.put::<Value>(&format!("/api/memory/{id}"), json!({ "pinned": !pinned })).await {
                                self.toast(e.to_string(), true);
                            }
                            self.reload_memory().await;
                            return;
                        }
                    }
                    KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Char('-') => {
                        if let Some(m) = data.get(sel) {
                            let imp = (m.importance + if k.code == KeyCode::Char('-') { -1 } else { 1 }).clamp(1, 10);
                            let id = m.id;
                            self.overlay = Some(Overlay::Memory { data: data.clone(), cats: cats.clone(), total, sel, q: q.clone(), cat, typing });
                            if let Err(e) = self.api.put::<Value>(&format!("/api/memory/{id}"), json!({ "importance": imp })).await {
                                self.toast(e.to_string(), true);
                            }
                            self.reload_memory().await;
                            return;
                        }
                    }
                    KeyCode::Char('t') => {
                        self.overlay = Some(Overlay::Memory { data, cats, total, sel, q, cat, typing });
                        self.overlay = Some(Overlay::Confirm { title: "Tidy up memory?".into(), text: "The model merges duplicates, resolves contradictions (newest wins) and drops trivia. Facts you typed in yourself are never changed.".into(), ok: "Tidy up".into(), act: Act::Tidy });
                        self.memory_return = true;
                        return;
                    }
                    _ => {}
                }
                let cat_changed = matches!(k.code, KeyCode::Tab | KeyCode::Right | KeyCode::Left);
                self.overlay = Some(Overlay::Memory { data, cats, total, sel, q, cat, typing });
                if cat_changed {
                    self.reload_memory().await;
                }
            }
            Overlay::Routines { items, mut sel } => match k.code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Up | KeyCode::Char('k') => {
                    sel = sel.saturating_sub(1);
                    self.overlay = Some(Overlay::Routines { items, sel });
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    sel = (sel + 1).min(items.len().saturating_sub(1));
                    self.overlay = Some(Overlay::Routines { items, sel });
                }
                KeyCode::Char('n') => self.open_routine_editor(None),
                KeyCode::Enter | KeyCode::Char('e') => match items.get(sel).cloned() {
                    Some(r) => self.open_routine_editor(Some(&r)),
                    None => self.open_routine_editor(None),
                },
                KeyCode::Char(' ') => {
                    if let Some(r) = items.get(sel) {
                        let id = r.id.clone();
                        let on = !r.enabled;
                        self.overlay = Some(Overlay::Routines { items: items.clone(), sel });
                        if let Err(e) = self.api.put::<Value>(&format!("/api/routines/{id}"), json!({ "enabled": on })).await {
                            self.toast(e.to_string(), true);
                        }
                        self.reload_routines().await;
                    }
                }
                KeyCode::Char('r') => {
                    if let Some(r) = items.get(sel) {
                        let id = r.id.clone();
                        match self.api.post::<Value>(&format!("/api/routines/{id}/run"), json!({})).await {
                            Ok(res) => {
                                let status = res.get("status").and_then(|s| s.as_str()).unwrap_or("").to_string();
                                if status == "started" {
                                    let cid = res.get("chat_id").and_then(|c| c.as_str()).map(String::from);
                                    self.refresh_chats().await;
                                    self.open_chat(cid).await;
                                } else {
                                    self.toast(format!("Not started: {status}"), false);
                                    self.overlay = Some(Overlay::Routines { items, sel });
                                }
                            }
                            Err(e) => {
                                self.toast(e.to_string(), true);
                                self.overlay = Some(Overlay::Routines { items, sel });
                            }
                        }
                    }
                }
                KeyCode::Char('o') => {
                    if let Some(cid) = items.get(sel).and_then(|r| r.chat_id.clone()) {
                        self.open_chat(Some(cid)).await;
                    } else {
                        self.overlay = Some(Overlay::Routines { items, sel });
                    }
                }
                KeyCode::Char('d') | KeyCode::Delete => {
                    if let Some(r) = items.get(sel) {
                        let (id, name) = (r.id.clone(), r.name.clone());
                        self.overlay = Some(Overlay::Confirm { title: format!("Delete the routine “{name}”?"), text: "Its chat is kept.".into(), ok: "Delete".into(), act: Act::DeleteRoutine(id) });
                        self.routines_return = true;
                    }
                }
                _ => self.overlay = Some(Overlay::Routines { items, sel }),
            },
        }
        if self.overlay.is_none() {
            if self.memory_return {
                self.memory_return = false;
                self.open_memory().await;
            } else if self.routines_return {
                self.routines_return = false;
                self.open_routines().await;
            }
        }
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        self.toasts.retain(|t| now.duration_since(t.at) < Duration::from_millis(if t.err { 7000 } else { 3500 }));
    }
}

/// `~/x` to a home path.
fn shellexpand(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    p.to_string()
}

pub fn day_group(ts: f64) -> String {
    let d = chrono::DateTime::from_timestamp(ts as i64, 0).map(|d| d.with_timezone(&chrono::Local).date_naive());
    let today = chrono::Local::now().date_naive();
    match d {
        Some(d) => {
            let days = (today - d).num_days();
            if days <= 0 { "Today" } else if days == 1 { "Yesterday" } else if days < 7 { "This week" } else if days < 30 { "This month" } else { "Older" }.to_string()
        }
        None => "Older".into(),
    }
}

pub fn time_ago(ts: f64) -> String {
    let s = trace::now() - ts;
    if s < 60.0 {
        "just now".into()
    } else if s < 3600.0 {
        format!("{} min ago", (s / 60.0) as u64)
    } else if s < 86400.0 {
        format!("{} h ago", (s / 3600.0) as u64)
    } else if s < 7.0 * 86400.0 {
        format!("{} d ago", (s / 86400.0) as u64)
    } else {
        chrono::DateTime::from_timestamp(ts as i64, 0).map(|d| d.with_timezone(&chrono::Local).format("%b %-d").to_string()).unwrap_or_default()
    }
}

pub fn routine_templates() -> Vec<(&'static str, &'static str, Value, &'static str)> {
    vec![
        ("Morning briefing", "planner", json!({"type": "daily", "time": "07:30", "days": [0,1,2,3,4]}), "Give me a short briefing for today: my calendar (times, conflicts, anything to prepare), and ask the Mail agent whether anything urgent came in. End with the 3 things I should focus on today."),
        ("Morning email briefing", "mail", json!({"type": "daily", "time": "08:00", "days": [0,1,2,3,4]}), "Check my unread email. Group it into: needs a reply, worth knowing, and low priority. For each one that needs a reply, suggest a short reply in my voice. Don't send anything."),
        ("Plan tomorrow", "assistant", json!({"type": "daily", "time": "20:00", "days": [0,1,2,3,4,5,6]}), "Help me plan tomorrow. Remind me of open tasks, deadlines and commitments you know about from memory, suggest a realistic order for them, and ask what I want to focus on."),
        ("News digest", "researcher", json!({"type": "daily", "time": "09:00", "days": [0,1,2,3,4,5,6]}), "Find 5 notable news items from the last 24 hours about topics I care about (use what you remember about my interests and projects). One-line summary and a link for each."),
        ("Weekly memory check", "assistant", json!({"type": "daily", "time": "18:00", "days": [6]}), "Look through what you remember about me (use recall_memory with a few broad searches). Point out anything that seems outdated or contradictory and ask me about it, and forget anything I confirm is wrong."),
    ]
}

/// The agent's terminal color.
pub fn agent_color(a: &Agent) -> Color {
    hex(&a.color).unwrap_or(Color::Gray)
}
