//! Application state, the event loop and every action.

use crate::api::{Agent, Api, ChatSummary, Event, FileEntry, Incoming, Memory, Routine, SearchHit, ServerInfo, ToolInfo, Upload};
use crate::commands::{self, Cmd, Parsed};
use crate::forms::{self, Form, FormOut};
use crate::text::{self, Pastes};
use crate::theme::{hex, Theme};
use crate::trace::{self, Entry, Trace, Who};
use anyhow::{anyhow, Result};
use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::style::Color;
use ratatui::text::Line;
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
    /// A slash command, with whatever follows its name in the search.
    Slash(&'static str),
}

pub enum Overlay {
    /// The keyboard shortcuts; `scroll` is the first row shown (drawing clamps it).
    Help { scroll: usize },
    Palette { q: String, items: Vec<PaletteItem>, sel: usize, hits: Vec<SearchHit> },
    Confirm { title: String, text: String, ok: String, act: Act },
    /// A one-line input; `cursor` counts characters.
    Prompt { title: String, value: String, cursor: usize, act: Act },
    Form { form: Form, kind: FormKind },
    Viewer { title: String, lines: Vec<String>, scroll: usize },
    /// What /help, /context, /usage and /status say: styled lines, already cleaned.
    Info { title: String, lines: Vec<Line<'static>>, scroll: usize },
    /// /model's pick list for an agent: "" (the default) and the server's models.
    Models { agent: String, items: Vec<String>, sel: usize },
    Memory { data: Vec<Memory>, cats: Vec<String>, total: i64, sel: usize, q: String, cat: usize, typing: bool },
    Routines { items: Vec<Routine>, sel: usize },
    Pick { sel: usize },
    Toasts,
}

impl Overlay {
    pub fn prompt(title: &str, value: &str, act: Act) -> Overlay {
        Overlay::Prompt { title: title.into(), value: value.into(), cursor: value.chars().count(), act }
    }
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
    /// Set by /theme; the --light and --plain flags win over it.
    #[serde(default)]
    pub theme: String,
}
fn t() -> bool {
    true
}
impl Default for Persisted {
    fn default() -> Self {
        Persisted { seen: HashMap::new(), verbose: false, sidebar: true, files: false, filter: String::new(), theme: String::new() }
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
    pub count: usize,
}

/// At most this many toasts at once.
pub const TOASTS_SHOWN: usize = 3;

/// Add a toast. The same message again counts up on the last one ("… ×5") and restarts its
/// timer; only the newest few are kept, so they can't pile up over the trace.
pub fn push_toast(list: &mut Vec<Toast>, text: String, err: bool, at: Instant) {
    if let Some(last) = list.last_mut().filter(|t| t.text == text && t.err == err) {
        last.count += 1;
        last.at = at;
        return;
    }
    list.push(Toast { text, err, at, count: 1 });
    if list.len() > TOASTS_SHOWN {
        list.drain(..list.len() - TOASTS_SHOWN);
    }
}

const COMPOSER_HINT: &str = "Message  (Enter sends · Alt+Enter new line · / commands · F1 help)";

fn composer_with(text: &str) -> TextArea<'static> {
    let mut c = TextArea::from(text.split('\n').map(String::from).collect::<Vec<_>>());
    c.set_cursor_line_style(ratatui::style::Style::default());
    c.set_placeholder_text(COMPOSER_HINT);
    c.move_cursor(CursorMove::Bottom);
    c.move_cursor(CursorMove::End);
    c
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
    /// Bumped for every chat opened; the open chat's stream task (aborted on the next open).
    pub stream_gen: u64,
    pub chat_stream: Option<tokio::task::AbortHandle>,
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
    /// Large pastes behind the composer's placeholder tokens (and those of saved drafts).
    pub pastes: Pastes,
    pub draft_pastes: HashMap<String, Pastes>,
    /// A keystroke paste being collected, and when the last key arrived.
    pub burst: String,
    pub last_key: Option<Instant>,
    /// The slash-command menu: its selection, the word it was made for, and the message text Esc
    /// closed it on (it stays closed until that text changes).
    pub menu_sel: usize,
    pub menu_for: String,
    pub menu_closed: Option<String>,
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
    /// Approvals the bell was for (rung or, with --quiet, not).
    pub bells: usize,
    pub edit_from: Option<usize>,
    pub side_row_map: HashMap<usize, usize>,
    /// Columns the sidebar and the files drawer took in the last frame (0 when not drawn): what a
    /// mouse click lands on.
    pub drawn_cols: (u16, u16),
    /// The sidebar selection (with the list's height and length) the list was last scrolled to
    /// show; it scrolls to it again only when one of those changes, so the wheel can scroll it.
    pub side_snap: Option<(usize, usize, usize)>,
    /// The Memory panel, kept while a prompt or confirmation from it is up; it comes back after.
    pub memory_back: Option<Box<Overlay>>,
    pub routines_return: bool,
    /// While a bracketed paste (not a keystroke one) is put where it goes.
    pub pasting: bool,
    /// The open chat's run's notices so far (most aren't saved with the chat).
    pub run_notices: Vec<String>,
    /// The exports this client saved, and whose: only those are ever written over.
    pub exported: HashMap<PathBuf, String>,
    /// The model server check running in the background.
    pub server_check: Option<tokio::task::AbortHandle>,
}

/// The model server's state as the app sees it (not reachable, with why, when the app can't say).
async fn server_info(api: &Api) -> ServerInfo {
    api.server().await.unwrap_or_else(|e| ServerInfo { ok: false, error: Some(e.to_string()), ..Default::default() })
}

pub fn agent_who(agents: &[Agent], id: &str) -> Who {
    Who::from(agents.iter().find(|a| a.id == id), id)
}

impl App {
    pub fn new(api: Api, theme: Theme, cfg_path: PathBuf) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let cfg: Persisted = std::fs::read_to_string(&cfg_path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
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
            chat_stream: None,
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
            composer: composer_with(""),
            pastes: Pastes::default(),
            draft_pastes: HashMap::new(),
            burst: String::new(),
            last_key: None,
            menu_sel: 0,
            menu_for: String::new(),
            menu_closed: None,
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
            bells: 0,
            edit_from: None,
            side_row_map: HashMap::new(),
            drawn_cols: (0, 0),
            side_snap: None,
            memory_back: None,
            routines_return: false,
            pasting: false,
            run_notices: vec![],
            exported: HashMap::new(),
            server_check: None,
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
        push_toast(&mut self.toasts, text::line(&text.into()), err, Instant::now());
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
        self.drop_missing_filter();
        self.rebuild_sidebar();
        let api = self.api.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move { api.follow_global(tx).await });
        self.poll_server().await;
        self.refresh_memory_count().await;
        if self.cfg.files {
            self.refresh_files(true).await; // a drawer left open last time lists its folder at once
        }
        Ok(())
    }

    /// The sidebar's agent filter goes when its agent does (deleted here or elsewhere).
    fn drop_missing_filter(&mut self) {
        if !self.filter.is_empty() && self.agent(&self.filter).is_none() {
            self.filter.clear();
            if !self.cfg.filter.is_empty() {
                self.cfg.filter.clear();
                self.save_cfg();
            }
        }
    }

    pub async fn poll_server(&mut self) {
        self.server = server_info(&self.api).await;
    }

    /// The regular check of the model server, in the background: an app that stalls mid-check
    /// mustn't stop the keys. Its answer comes back as an `Incoming::Server` (one check at a time).
    pub fn poll_server_soon(&mut self) {
        if self.server_check.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let (api, tx) = (self.api.clone(), self.tx.clone());
        self.server_check = Some(tokio::spawn(async move { let _ = tx.send(Incoming::Server(server_info(&api).await)); }).abort_handle());
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
            // the open chat's header follows a rename made elsewhere (the server says only
            // "chats_changed" for it), the way its sidebar row does
            if let Some(c) = &mut self.chat
                && let Some(t) = c.get("id").and_then(|i| i.as_str()).and_then(|id| self.chats.iter().find(|s| s.id == id)).map(|s| s.title.clone())
            {
                c["title"] = Value::from(t);
            }
            self.rebuild_sidebar();
            self.leave_if_deleted().await;
        }
    }

    /// The open chat is gone from the server (deleted in another window): back to the welcome
    /// page, saying so once.
    async fn leave_if_deleted(&mut self) {
        if self.chat_id.as_ref().is_some_and(|id| !self.chats.iter().any(|c| &c.id == id)) {
            self.toast("That chat no longer exists.", true);
            self.open_chat(None).await;
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
            self.drop_missing_filter();
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

    /// The sidebar row a shortcut acts on: the highlighted one while the sidebar has the focus;
    /// anywhere else "the chat" in a shortcut is the open one, whatever the sidebar highlights.
    fn side_target(&self) -> Option<&SideRow> {
        self.side_rows.get(self.side_sel).filter(|_| self.focus == Focus::Sidebar)
    }

    /// The chat Alt+P, Alt+D, Alt+X, F2 (and the sidebar's p, d, x, J, r) act on.
    pub fn target_chat(&self) -> Option<ChatSummary> {
        match self.side_target() {
            Some(SideRow::Chat(id)) => self.chats.iter().find(|c| &c.id == id).cloned(),
            _ => self.this_chat(),
        }
    }

    /// The agent Alt+A (and the sidebar's e) edits: the highlighted agent or chat's agent in the
    /// sidebar, else the open chat's, else the filter's.
    pub fn target_agent(&self) -> Option<String> {
        match self.side_target() {
            Some(SideRow::Agent(id)) => Some(id.clone()),
            Some(SideRow::Chat(id)) => self.chats.iter().find(|c| &c.id == id).map(|c| c.agent_id.clone()),
            _ => self.chat.as_ref().and_then(|c| c.get("agent_id")).and_then(|a| a.as_str()).map(String::from).or_else(|| if self.filter.is_empty() { None } else { Some(self.filter.clone()) }),
        }
    }

    // ------------------------------------------------------------------ chat

    /// The composer's text (placeholder tokens as shown).
    pub fn composer_text(&self) -> String {
        self.composer.lines().join("\n")
    }

    /// Replace the composer's text, cursor at the end. Emptying it drops the held pastes too.
    pub fn set_composer(&mut self, text: &str) {
        self.composer = composer_with(text);
        if text.is_empty() {
            self.pastes.clear();
        }
    }

    pub async fn open_chat(&mut self, id: Option<String>) {
        if let Some(old) = self.chat_id.clone() {
            let draft = self.composer_text();
            if !draft.trim().is_empty() {
                self.drafts.insert(old.clone(), draft);
                self.draft_pastes.insert(old, std::mem::take(&mut self.pastes));
            } else {
                self.drafts.remove(&old);
                self.draft_pastes.remove(&old);
            }
        }
        self.stream_gen += 1;
        if let Some(h) = self.chat_stream.take() {
            h.abort(); // one stream at a time: an old one would apply its events twice
        }
        self.chat_id = id.clone();
        // what the chat list knows, until the snapshot brings the rest: the header names the
        // chat and its agent at once (not "Deleted agent"), and a sub-chat reads as one
        self.chat = id.as_ref().and_then(|id| self.chats.iter().find(|c| &c.id == id)).map(|c| {
            json!({ "id": c.id, "title": c.title, "agent_id": c.agent_id, "parent": c.parent.as_ref().map(|p| json!({ "root_chat_id": p.root_chat_id, "caller_id": p.caller_id })) })
        });
        self.trace = Trace::empty();
        self.run_notices.clear();
        self.running = false;
        self.queue.clear();
        self.scroll = 0;
        self.follow = true;
        self.selected_turn = None;
        self.attachments.clear();
        self.ctx_tokens = None;
        self.tok_per_s = None;
        self.edit_from = None;
        self.set_composer("");
        if let Some(id) = &id {
            if let Some(d) = self.drafts.get(id).cloned() {
                self.set_composer(&d);
                self.pastes = self.draft_pastes.get(id).cloned().unwrap_or_default();
            }
            self.follow_open_chat(Duration::ZERO);
            self.select_chat_row(id);
            self.mark_seen(id);
        }
        self.rebuild_sidebar();
        self.touch();
        if self.cfg.files {
            self.refresh_files(true).await;
        }
    }

    /// Follow the open chat's event stream (after `delay`, for a reconnect), replacing any stream
    /// still running.
    fn follow_open_chat(&mut self, delay: Duration) {
        let Some(cid) = self.chat_id.clone() else { return };
        if let Some(h) = self.chat_stream.take() {
            h.abort();
        }
        let (api, tx, visit) = (self.api.clone(), self.tx.clone(), self.stream_gen);
        self.chat_stream = Some(
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                api.follow_chat(cid, visit, tx).await
            })
            .abort_handle(),
        );
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
        let mut content = self.pastes.expand(commands::message(&self.composer_text())).trim().to_string();
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
                self.set_composer("");
                self.attachments.clear();
                self.drafts.remove(&cid);
                self.draft_pastes.remove(&cid);
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
                    self.put_in_composer(&commands::escape(&items.join("\n\n")));
                }
            }
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    fn put_in_composer(&mut self, text: &str) {
        let existing = self.composer_text();
        let all = if existing.trim().is_empty() { text.to_string() } else { format!("{text}\n\n{existing}") };
        self.set_composer(&all);
        self.focus = Focus::Composer;
    }

    /// Start a run (a regenerate, an edit resent); false when it was refused or failed.
    async fn run_body(&mut self, body: Value) -> bool {
        let Some(cid) = self.chat_id.clone() else { return false };
        if self.running {
            self.toast("Wait for the current reply or stop it first (Ctrl+S).", false);
            return false;
        }
        match self.api.run(&cid, body).await {
            Ok(_) => {
                self.running = true;
                self.follow = true;
                true
            }
            Err(e) => {
                self.toast(e.to_string(), true);
                false
            }
        }
    }

    /// Regenerate the last reply, or retry after an error.
    async fn regenerate(&mut self) {
        let last_turn_start = self.trace.entries.iter().rev().find_map(|e| if let Entry::Turn(t) = e { Some(t.start) } else { None });
        let Some(start) = last_turn_start else { return self.toast("Nothing to regenerate yet.", false) };
        self.run_body(json!({ "from_index": start })).await;
    }

    async fn edit_last(&mut self) {
        if self.is_subchat() {
            return self.toast("This chat shows an agent working for another agent; reply in the chat that started it.", false);
        }
        let last_user = self.trace.entries.iter().rev().find_map(|e| if let Entry::User(u) = e { Some(u.clone()) } else { None });
        let Some(u) = last_user else { return self.toast("No message of yours to edit yet.", false) };
        if self.running {
            return self.toast("Wait for the current reply or stop it first.", false);
        }
        self.put_in_composer(&commands::escape(&u.text));
        // messages, not trace entries: a reply with tool calls is several messages
        let n = self.chat.as_ref().and_then(|c| c.get("messages")).and_then(|m| m.as_array()).map(|m| m.len()).unwrap_or(0);
        let after = n.saturating_sub(u.index + 1);
        self.toast(format!("Editing your last message; Enter resends it from there (the {after} message{} after it will be replaced).", if after == 1 { "" } else { "s" }), false);
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
        self.compact_with("").await
    }

    /// Summarize older messages now; `instructions` (from /compact) say what the summary keeps.
    async fn compact_with(&mut self, instructions: &str) {
        let Some(cid) = self.chat_id.clone() else { return };
        if self.running {
            return self.toast("Wait for the current reply or stop it first.", false);
        }
        let body = if instructions.is_empty() { json!({}) } else { json!({ "instructions": instructions }) };
        if let Err(e) = self.api.post::<Value>(&format!("/api/chats/{cid}/compact"), body).await {
            self.toast(e.to_string(), true);
        }
    }

    /// The open chat's summary: slash commands act on it, not on whatever the sidebar selects.
    fn this_chat(&self) -> Option<ChatSummary> {
        let id = self.chat_id.as_ref()?;
        self.chats.iter().find(|c| &c.id == id).cloned()
    }

    /// Whether a chat is working (or waiting on an approval, or on another agent): the server
    /// refuses to rename or pin it then ("409 chat is busy").
    fn busy(&self, c: &ChatSummary) -> bool {
        c.status != "idle" || (Some(&c.id) == self.chat_id.as_ref() && self.running)
    }

    /// The refusal /pin and /rename give mid-run, for the same change made with a key.
    fn refuse_busy(&mut self, what: &str) {
        self.toast(format!("The agent is working. Wait for it, or /stop it first, then {what}."), false);
    }

    /// export-md, export-json, pin, rename or delete, on chat `c`.
    async fn chat_cmd(&mut self, key: &str, c: ChatSummary) {
        match key {
            "pin" if self.busy(&c) => self.refuse_busy(if c.pinned { "unpin it" } else { "pin it" }),
            "rename" if self.busy(&c) => self.refuse_busy("rename it"),
            "export-md" => self.export_chat(c, "md").await,
            "export-json" => self.export_chat(c, "json").await,
            "pin" => self.pin_chat(c).await,
            "rename" => self.overlay = Some(Overlay::prompt("Rename chat", &c.title, Act::RenameChat(c.id.clone()))),
            "delete" => self.confirm_delete(&c),
            _ => {}
        }
    }

    /// A chat command from a shortcut: on the sidebar's highlighted chat or the open one.
    async fn target_cmd(&mut self, key: &str) {
        if let Some(c) = self.target_chat() {
            self.chat_cmd(key, c).await;
        }
    }

    /// Save a chat's export in the current folder, named after its title. A file that is there
    /// already is never replaced, unless it is this chat's export saved earlier in this session:
    /// the name gets a number instead ("README-2.md" beside a project's README.md).
    async fn export_chat(&mut self, c: ChatSummary, kind: &str) {
        let path = format!("/api/chats/{}/export.{kind}", c.id);
        match self.api.get_text(&path).await {
            Ok(text) => {
                let name: String = c.title.chars().map(|ch| if ch.is_alphanumeric() { ch } else { '-' }).take(60).collect::<String>().trim_matches('-').to_string();
                let name = if name.is_empty() { "chat".to_string() } else { name };
                let dir = std::env::current_dir().unwrap_or_default();
                match save_export(&dir, &name, kind, &text, &c.id, &mut self.exported) {
                    Ok(full) => self.toast(format!("Saved {}", full.display()), false),
                    Err(e) => self.toast(format!("Couldn't save {name}.{kind} in {}: {e}", dir.display()), true),
                }
            }
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    async fn pin_chat(&mut self, c: ChatSummary) {
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
        for _ in 0..2 {
            if self.list_files(reset).await {
                break;
            }
        }
    }

    /// One listing of the drawer's folder. False when that folder is gone and the drawer went back
    /// to the root, to be listed again.
    async fn list_files(&mut self, reset: bool) -> bool {
        let agent = if !self.chat_agent_id().is_empty() { self.chat_agent_id() } else if !self.filter.is_empty() { self.filter.clone() } else { self.agents.first().map(|a| a.id.clone()).unwrap_or_default() };
        if agent.is_empty() {
            return true;
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
                    return false;
                }
                self.toast(e.to_string(), true);
            }
        }
        true
    }

    async fn files_activate(&mut self) {
        if self.files.sel == 0 {
            // the first row: "↰ .." in a folder, "↻ refresh" at the root
            if self.files.path != "." {
                let p = std::path::Path::new(&self.files.path).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                self.files.path = if p.is_empty() { ".".into() } else { p };
                self.files.sel = 0;
            }
            self.refresh_files(false).await;
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
        self.overlay = Some(Overlay::Help { scroll: 0 });
    }

    pub fn open_palette(&mut self) {
        let mut o = Overlay::Palette { q: String::new(), items: vec![], sel: 0, hits: vec![] };
        self.fill_palette(&mut o);
        self.overlay = Some(o);
    }

    fn fill_palette(&self, o: &mut Overlay) {
        let Overlay::Palette { q, items, sel, hits } = o else { return };
        // "/…": the slash commands, as the message box's menu lists them; the way to run one with
        // no message box (the welcome page, a sub-agent's chat)
        if let Some(rest) = q.trim_start().strip_prefix('/') {
            let word = rest.split_whitespace().next().unwrap_or("");
            items.clear();
            for c in commands::matching(word, self.in_chat()) {
                let title = format!("/{}{}{}", c.name, if c.args.is_empty() { "" } else { " " }, c.args);
                items.push(PaletteItem { group: "Slash commands".into(), title, sub: c.desc.into(), action: PaletteAction::Slash(c.name) });
            }
            *sel = (*sel).min(items.len().saturating_sub(1));
            return;
        }
        let ql = q.trim().to_lowercase();
        let m = |s: &str| ql.is_empty() || s.to_lowercase().contains(&ql);
        items.clear();
        // within a group, a title that matches goes before a description that does: "Calendars"
        // then Enter opens Calendars, not Settings ("… email, calendars")
        let add = |items: &mut Vec<PaletteItem>, mut group: Vec<PaletteItem>| {
            group.sort_by_key(|it| !m(&it.title));
            items.extend(group);
        };
        let agents = self.agents.iter().map(|a| PaletteItem { group: "Agents".into(), title: format!("New {} chat", a.name), sub: a.purpose.clone(), action: PaletteAction::NewChat(a.id.clone()) });
        add(items, agents.filter(|it| m(&it.title) || m(&it.sub)).collect());
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
        add(items, cmds.into_iter().filter(|(title, sub, _)| m(title) || m(sub)).map(|(title, sub, key)| PaletteItem { group: "Commands".into(), title: title.into(), sub: sub.into(), action: PaletteAction::Cmd(key.into()) }).collect());
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
            add(items, chat_cmds.into_iter().filter(|(title, sub, _)| m(title) || m(sub)).map(|(title, sub, key)| PaletteItem { group: "This chat".into(), title: title.into(), sub: sub.into(), action: PaletteAction::Cmd(key.into()) }).collect());
        }
        if *sel >= items.len() {
            *sel = items.len().saturating_sub(1);
        }
    }

    /// Search message text for the palette's (new) query, two characters or more, refill it and
    /// show it, the selection on the best match: a title starting with the query, else one
    /// containing it, else the top.
    async fn palette_search(&mut self, mut o: Overlay) {
        if let Overlay::Palette { q, hits, .. } = &mut o {
            *hits = if q.trim().chars().count() >= 2 && !q.trim_start().starts_with('/') { self.api.search(q.trim()).await.unwrap_or_default() } else { vec![] };
        }
        self.fill_palette(&mut o);
        if let Overlay::Palette { q, items, sel, .. } = &mut o {
            let ql = q.trim().to_lowercase();
            let title = |it: &PaletteItem| it.title.to_lowercase();
            *sel = if ql.is_empty() { 0 } else { items.iter().position(|it| title(it).starts_with(&ql)).or_else(|| items.iter().position(|it| title(it).contains(&ql))).unwrap_or(0) };
        }
        self.overlay = Some(o);
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
            // the palette's "This chat" group: the open chat
            "export-md" | "export-json" | "pin" | "rename" | "delete" => {
                if let Some(c) = self.this_chat() {
                    self.chat_cmd(key, c).await;
                }
            }
            "branch" => {
                let n = self.chat.as_ref().and_then(|c| c.get("messages")).and_then(|m| m.as_array()).map(|m| m.len()).unwrap_or(0);
                if n > 0 {
                    self.branch(n).await;
                }
            }
            "compact" => self.compact().await,
            _ => {}
        }
    }

    fn confirm_delete(&mut self, c: &ChatSummary) {
        self.overlay = Some(Overlay::Confirm { title: format!("Delete “{}”?", trace::one_line(&c.title, 40)), text: "Its messages, and any work other agents did for it, will be removed.".into(), ok: "Delete".into(), act: Act::DeleteChat(c.id.clone()) });
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
        form.focus_on("name"); // typing adds one, rather than landing on (and Enter pressing) "Disconnect …"
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
        self.routines_return = false; // (here already)
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
                FormKind::Calendar => {
                    // Enter (or Ctrl+S) adds the calendar typed in, as Enter in the web's link
                    // field does; from a filled-in name it goes on to the link; with nothing
                    // typed it closes
                    let (name, url) = (form.string("name").trim().to_string(), form.string("url").trim().to_string());
                    if !url.is_empty() {
                        self.add_calendar().await;
                    } else if !name.is_empty() {
                        if let Some(Overlay::Form { form, .. }) = &mut self.overlay {
                            form.focus_on("url");
                        }
                    } else {
                        self.overlay = None;
                    }
                }
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

    /// Rename a chat; the title the server kept (trimmed, capped), or None when it refused.
    async fn rename_chat(&mut self, id: &str, title: &str) -> Option<String> {
        if title.is_empty() {
            return None;
        }
        match self.api.patch::<Value>(&format!("/api/chats/{id}"), json!({ "title": title })).await {
            Ok(v) => {
                let title = v.get("title").and_then(|t| t.as_str()).unwrap_or(title).to_string();
                if let Some(c) = &mut self.chat && c.get("id").and_then(|i| i.as_str()) == Some(id) {
                    c["title"] = Value::from(title.clone());
                }
                self.refresh_chats().await;
                Some(title)
            }
            Err(e) => {
                self.toast(e.to_string(), true);
                None
            }
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
            Act::Regenerate(start) => {
                self.run_body(json!({ "from_index": start })).await;
            }
            Act::RenameChat(id) => {
                let title = value.unwrap_or_default();
                if !title.trim().is_empty() && self.chats.iter().find(|c| c.id == id).is_some_and(|c| self.busy(c)) {
                    return self.refuse_busy("rename it"); // (a run started while the prompt was open)
                }
                self.rename_chat(&id, title.trim()).await;
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

    // --------------------------------------------------------- slash commands

    /// A chat you write in is open (not a sub-agent's).
    pub fn in_chat(&self) -> bool {
        self.chat_id.is_some() && !self.is_subchat()
    }

    /// The slash-command menu over the message box while it is open: the text starts with `/`, the
    /// cursor is still in that first word, something matches, and Esc hasn't closed it for this
    /// text. Keeps the selection in range, back at the top when the word changes.
    pub fn menu(&mut self) -> Option<Vec<&'static Cmd>> {
        if self.overlay.is_some() || self.focus != Focus::Composer || !self.in_chat() {
            return None;
        }
        let text = self.composer_text();
        if self.menu_closed.as_ref().is_some_and(|t| *t != text) {
            self.menu_closed = None;
        }
        let q = commands::typed_word(&text, self.composer.cursor()).filter(|_| self.menu_closed.is_none())?.to_lowercase();
        let items = commands::matching(&q, true);
        if items.is_empty() {
            return None;
        }
        if self.menu_for != q {
            self.menu_for = q;
            self.menu_sel = 0;
        }
        self.menu_sel = self.menu_sel.min(items.len() - 1);
        Some(items)
    }

    /// Keys the open menu takes: ↑↓ (Ctrl+P/N) move, Tab completes, Enter runs, Esc closes it.
    async fn on_menu_key(&mut self, k: KeyEvent, items: &[&'static Cmd]) -> bool {
        let n = items.len();
        match (k.code, k.modifiers) {
            (KeyCode::Up, KeyModifiers::NONE) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => self.menu_sel = (self.menu_sel + n - 1) % n,
            (KeyCode::Down, KeyModifiers::NONE) | (KeyCode::Char('n'), KeyModifiers::CONTROL) => self.menu_sel = (self.menu_sel + 1) % n,
            (KeyCode::Tab, KeyModifiers::NONE) => {
                let (text, col) = commands::complete(&self.composer_text(), items[self.menu_sel]);
                self.set_composer(&text);
                self.composer.move_cursor(CursorMove::Jump(0, col as u16));
            }
            (KeyCode::Enter, KeyModifiers::NONE) => {
                let args = commands::args_of(&self.composer_text()).to_string();
                self.run_slash(items[self.menu_sel], &args).await;
            }
            (KeyCode::Esc, _) => self.menu_closed = Some(self.composer_text()),
            _ => return false,
        }
        true
    }

    /// Run a slash command typed or picked in the message box. The box is cleared (unless the
    /// command is refused), and nothing reaches the agent: a command runs even while it works,
    /// instead of being queued.
    pub async fn run_slash(&mut self, cmd: &'static Cmd, args: &str) {
        // refused (the agent is working, say): the text stays, to run once it's done
        if let Err(why) = commands::check(cmd, self.in_chat(), self.running) {
            return self.toast(why, false);
        }
        let a = self.pastes.expand(args).trim().to_string();
        // arguments it can't use: say why and keep the text to fix
        if let Some(why) = self.bad_args(cmd, &a) {
            return self.toast(why, false);
        }
        self.set_composer("");
        self.edit_from = None;
        if let Some(cid) = self.chat_id.clone() {
            self.drafts.remove(&cid);
            self.draft_pastes.remove(&cid);
        }
        self.command(cmd, &a).await;
    }

    /// Why a command can't use these arguments, checked before the box is cleared.
    fn bad_args(&self, cmd: &Cmd, a: &str) -> Option<String> {
        let low = a.to_lowercase();
        match cmd.name {
            "new" if !a.is_empty() && commands::find_agent(a, &self.agents).is_none() => Some(format!("No agent called “{a}” (Ctrl+K → New agent makes one)")),
            "export" if commands::export_kind(a).is_none() => Some("Export as md or json, like /export json".into()),
            "model" if !a.is_empty() && !self.server.models.is_empty() => commands::pick_model(a, &self.server.models).err(),
            "remember" if a.is_empty() => Some("Say what to remember, like /remember I prefer short answers".into()),
            "permissions" if !matches!(low.as_str(), "" | "ask" | "bypass") => Some("Use /permissions ask or /permissions bypass".into()),
            "theme" if !a.is_empty() && Theme::named(&low).is_none() => Some("Themes: dark, light, plain".into()),
            _ => None,
        }
    }

    /// What a slash command does, with its arguments (pastes expanded), once it may run.
    async fn command(&mut self, cmd: &'static Cmd, a: &str) {
        match cmd.name {
            "help" => self.overlay = Some(self.help_info()),
            "new" if a.is_empty() => {
                let aid = self.chat_agent_id();
                if self.in_chat() && self.agent(&aid).is_some() {
                    self.new_chat(&aid).await;
                } else {
                    self.overlay = Some(Overlay::Pick { sel: 0 });
                }
            }
            "new" => match commands::find_agent(a, &self.agents).map(|x| x.id.clone()) {
                Some(id) => self.new_chat(&id).await,
                None => self.toast(format!("No agent called “{a}” (Ctrl+K → New agent makes one)"), false),
            },
            "resume" => self.palette_search(Overlay::Palette { q: a.to_string(), items: vec![], sel: 0, hits: vec![] }).await,
            "rename" => {
                let Some(c) = self.this_chat() else { return };
                if a.is_empty() {
                    self.overlay = Some(Overlay::prompt("Rename chat", &c.title, Act::RenameChat(c.id.clone())));
                } else if let Some(t) = self.rename_chat(&c.id, a).await {
                    self.toast(format!("Renamed to “{t}”"), false);
                }
            }
            "pin" => {
                if let Some(c) = self.this_chat() {
                    self.pin_chat(c).await;
                }
            }
            "branch" => {
                let n = self.chat.as_ref().and_then(|c| c.get("messages")).and_then(|m| m.as_array()).map(|m| m.len()).unwrap_or(0);
                if n == 0 {
                    self.toast("Nothing to branch yet", false);
                } else {
                    self.branch(n).await;
                }
            }
            "retry" => self.regenerate().await,
            "edit" => self.edit_last().await,
            "stop" if self.running => self.stop().await,
            "stop" => self.toast("Nothing is running", false),
            "compact" => self.compact_with(a).await,
            "export" => match (commands::export_kind(a), self.this_chat()) {
                (Some(kind), Some(c)) => self.export_chat(c, kind).await,
                (None, _) => self.toast("Export as md or json, like /export json", false),
                _ => {}
            },
            "copy" => match self.last_reply_text() {
                Some(t) => {
                    self.copy(&t);
                    self.toast(format!("Copied the last reply ({} characters)", t.chars().count()), false);
                }
                None => self.toast("No reply to copy yet", false),
            },
            "delete" => {
                if let Some(c) = self.this_chat() {
                    self.confirm_delete(&c);
                }
            }
            "agents" => {
                let id = Some(self.chat_agent_id()).filter(|id| self.agent(id).is_some());
                self.open_agent_editor(id);
            }
            "model" => {
                if self.server.models.is_empty() {
                    self.poll_server().await;
                }
                let aid = self.chat_agent_id();
                if self.agent(&aid).is_none() {
                    return self.toast(if self.in_chat() { "This chat's agent was deleted." } else { "Open a chat first" }, false);
                }
                if a.is_empty() {
                    self.overlay = self.models_overlay();
                } else {
                    match commands::pick_model(a, &self.server.models) {
                        Ok(m) => self.set_model(&aid, m).await,
                        Err(which) => self.toast(which, false),
                    }
                }
            }
            "memory" => self.open_memory().await,
            "remember" => self.remember(a).await,
            "permissions" => {
                let on = self.settings.get("bypass_approvals").and_then(|b| b.as_bool()).unwrap_or(false);
                match (a.to_lowercase().as_str(), on) {
                    ("ask", true) => self.set_bypass(false).await,
                    ("ask", false) => self.toast("Agents already ask before running commands or sending email.", false),
                    ("bypass", true) => self.toast("Permissions are already bypassed.", false),
                    ("bypass", false) => self.run_cmd("bypass").await,
                    ("", true) => self.toast("Bypassing permissions: agents run commands and send email without asking. /permissions ask turns that off.", false),
                    ("", false) => self.toast("Agents ask before running shell commands or sending email. /permissions bypass stops the asking.", false),
                    _ => self.toast("Use /permissions ask or /permissions bypass", false),
                }
            }
            "settings" | "routines" | "files" | "verbose" => self.run_cmd(cmd.name).await,
            "theme" => {
                let name = if a.is_empty() { commands::next_theme(self.theme.name).to_string() } else { a.to_lowercase() };
                match Theme::named(&name) {
                    Some(t) => {
                        self.set_theme(t);
                        self.toast(format!("Theme: {name}"), false);
                    }
                    None => self.toast("Themes: dark, light, plain", false),
                }
            }
            "context" => self.overlay = Some(self.context_info()),
            "usage" => self.overlay = Some(self.usage_info()),
            "status" => {
                self.poll_server().await;
                self.overlay = Some(self.status_info());
            }
            "quit" => self.quit = true,
            _ => {}
        }
    }

    /// Switch the palette now and keep it for next time.
    pub fn set_theme(&mut self, t: Theme) {
        self.cfg.theme = t.name.to_string();
        self.theme = t;
        self.save_cfg();
        self.touch(); // the trace's colors are baked into its rendered lines
    }

    fn info(title: &str, lines: Vec<Line<'static>>) -> Overlay {
        Overlay::Info { title: title.into(), lines, scroll: 0 }
    }

    pub fn help_info(&self) -> Overlay {
        Self::info("/help", commands::help_lines(&self.theme))
    }

    pub fn context_info(&self) -> Overlay {
        let window = self.server.n_ctx.or_else(|| self.settings.get("context_size").and_then(|v| v.as_u64()).filter(|n| *n > 0));
        let at = self.settings.get("compact_at").and_then(|v| v.as_u64()).unwrap_or(70);
        let auto = self.settings.get("auto_compact").and_then(|v| v.as_bool()).unwrap_or(true);
        Self::info("/context", commands::context_lines(&self.theme, self.ctx_tokens, window, at, auto, &self.chat_age()))
    }

    pub fn usage_info(&self) -> Overlay {
        let msgs = self.chat.as_ref().and_then(|c| c.get("messages")).and_then(|m| m.as_array()).map_or(&[][..], Vec::as_slice);
        Self::info("/usage", commands::usage_lines(&self.theme, &commands::usage(msgs), &self.chat_age()))
    }

    fn chat_age(&self) -> String {
        trace::chat_age(self.chat.as_ref().unwrap_or(&Value::Null), trace::now())
    }

    pub fn status_info(&self) -> Overlay {
        Self::info("/status", commands::status_lines(&self.theme, &self.server, &self.settings, &self.version))
    }

    /// /model's pick list for this chat's agent, its current model selected.
    pub fn models_overlay(&self) -> Option<Overlay> {
        let agent = self.chat_agent_id();
        let a = self.agent(&agent)?;
        let mut items = vec![String::new()];
        items.extend(self.server.models.iter().cloned());
        if !a.model.is_empty() && !items.contains(&a.model) {
            items.push(a.model.clone());
        }
        let sel = items.iter().position(|m| *m == a.model).unwrap_or(0);
        Some(Overlay::Models { agent, items, sel })
    }

    /// Set an agent's model ("" = the default). The rest of the agent goes back exactly as the
    /// server has it now, so nothing else changes (PUT replaces the whole agent).
    async fn set_model(&mut self, agent_id: &str, model: String) {
        let api = self.api.clone();
        let res: Result<Value> = async {
            let st: Value = api.get("/api/state").await?;
            let mut a = st.get("agents").and_then(|a| a.as_array()).and_then(|a| a.iter().find(|x| x.get("id").and_then(|i| i.as_str()) == Some(agent_id))).cloned().ok_or_else(|| anyhow!("That agent was deleted"))?;
            a["model"] = Value::from(model.clone());
            api.put(&format!("/api/agents/{agent_id}"), a).await
        }
        .await;
        match res {
            Ok(_) => {
                self.reload_state().await;
                let name = agent_who(&self.agents, agent_id).name;
                let listed = self.server.models.contains(&model);
                self.toast(match model.as_str() {
                    "" => format!("{name} uses the default model again"),
                    m => format!("{name} now uses {m}{}", if listed { "" } else { " (the server doesn't list it)" }),
                }, false);
            }
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    async fn remember(&mut self, fact: &str) {
        if fact.is_empty() {
            return self.toast("Say what to remember, like /remember I prefer short answers", false);
        }
        match self.api.post::<Value>("/api/memory", json!({ "fact": fact })).await {
            Ok(m) => {
                let merged = m.get("action").and_then(|a| a.as_str()) == Some("merged");
                let saved = m.get("fact").and_then(|f| f.as_str()).unwrap_or(fact).to_string();
                self.toast(format!("{}: {saved}", if merged { "Updated a memory" } else { "Remembered" }), false);
                self.refresh_memory_count().await;
            }
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    /// The newest reply's text (what /copy copies).
    fn last_reply_text(&self) -> Option<String> {
        self.trace.entries.iter().rev().find_map(|e| match e {
            Entry::Turn(t) => {
                let texts: Vec<&str> = t.steps.iter().filter_map(|s| if let trace::Step::Text { text, .. } = s { Some(text.as_str()) } else { None }).filter(|t| !t.trim().is_empty()).collect();
                if texts.is_empty() { None } else { Some(texts.join("\n\n")) }
            }
            _ => None,
        })
    }

    // ---------------------------------------------------------------- events

    pub async fn on_incoming(&mut self, inc: Incoming) {
        self.on_server(inc).await;
        self.settle_focus();
    }

    async fn on_server(&mut self, inc: Incoming) {
        match inc {
            Incoming::Global(ev) => self.on_global(ev).await,
            Incoming::Chat { chat_id, visit, event } => {
                if visit == self.stream_gen && Some(&chat_id) == self.chat_id.as_ref() {
                    self.on_chat_event(event).await;
                }
            }
            Incoming::StreamEnded { chat_id, visit, error } => {
                if visit != self.stream_gen || Some(&chat_id) != self.chat_id.as_ref() {
                    return; // a stream left behind by an earlier visit
                }
                if error.as_deref() == Some("HTTP 404") {
                    self.toast("That chat no longer exists.", true);
                    self.open_chat(None).await;
                    return;
                }
                if let Some(e) = error {
                    self.toast(format!("Lost the chat stream: {e}"), true);
                }
                self.follow_open_chat(Duration::from_secs(2)); // reconnect after a moment
            }
            Incoming::Server(info) => self.server = info,
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
                    // the one bell for an approval, whichever chat asks (the open one's stream
                    // brings the approval itself too, and stays quiet)
                    let was = before.unwrap_or_else(|| c.status.clone());
                    c.status = status.clone();
                    if status == "waiting" && was != "waiting" {
                        self.bells += 1;
                        if self.bell {
                            print!("\x07");
                        }
                    }
                }
                self.rebuild_sidebar();
            }
            // an agent made, edited or deleted (here, in the web UI, through the API): its name,
            // icon and color everywhere, and "Deleted agent" for one that's gone
            "agents_changed" => {
                self.reload_state().await;
                self.trace.refresh_agents(&self.agents);
                if !self.files.agent_id.is_empty() && self.agent(&self.files.agent_id).is_none() && self.cfg.files {
                    self.refresh_files(true).await;
                }
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
                let text = commands::escape(d.get("text").and_then(|t| t.as_str()).unwrap_or(""));
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
                self.run_notices.clear();
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
                self.run_notices.clear();
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
                let comp = d.get("compaction").cloned().unwrap_or(Value::Null);
                self.trace.compact_end(&path, &comp, &agents);
                if path.is_empty() {
                    self.add_compaction(comp);
                }
                self.touch();
            }
            "approval" => {
                self.trace.approval(&path, d.get("call_id").and_then(|c| c.as_str()).unwrap_or(""), d.get("approval_id").and_then(|a| a.as_str()), None, &agents);
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
                let text = d.get("text").and_then(|t| t.as_str()).unwrap_or("");
                self.trace.notice(&path, text, &agents);
                self.run_notices.push(text.to_string());
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
                // what the run said that the saved chat doesn't keep ("Nothing to compact yet", a
                // failed compaction, the step limit) would go with the live trace: a toast keeps it
                let kept: Vec<String> = self.trace.notices().into_iter().map(String::from).collect();
                for n in std::mem::take(&mut self.run_notices) {
                    if !n.is_empty() && !kept.contains(&n) {
                        self.toast(n, false);
                    }
                }
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

    /// A compaction of the open chat, mid-run: into the chat's own list (the footer and /context
    /// count that list), unless it is there already. A stream that joins a run replays its events
    /// after a snapshot that already holds their compactions.
    fn add_compaction(&mut self, comp: Value) {
        let Some(chat) = self.chat.as_mut().and_then(|c| c.as_object_mut()) else { return };
        let list = chat.entry("compactions").or_insert_with(|| json!([]));
        if !list.is_array() {
            *list = json!([]);
        }
        let Some(list) = list.as_array_mut() else { return };
        let same = |c: &Value| ["at", "upto"].iter().all(|k| c.get(k) == comp.get(k));
        if comp.is_object() && !list.iter().any(same) {
            list.push(comp);
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
                self.pasting = true;
                self.paste(&text).await;
                self.pasting = false;
            }
            _ => {}
        }
    }

    /// A terminal event, stamped with when it arrived (`next`: when the next key did, if it is
    /// already waiting). Keys that come in a burst, faster than anyone types, are a paste from a
    /// terminal without bracketed paste: their text is collected and handled as one paste, so an
    /// Enter inside it is a line break rather than send, and none of its keys act as shortcuts.
    pub async fn on_input(&mut self, at: Instant, ev: TermEvent, next: Option<Instant>) {
        if let TermEvent::Key(k) = &ev
            && k.kind != KeyEventKind::Release
        {
            let prev = self.last_key.replace(at);
            let open = !self.burst.is_empty();
            if text::joins_burst(prev, at, next, open) {
                match text::burst_char(k) {
                    Some(c) => {
                        self.burst.push(c);
                        return;
                    }
                    // a key with no text in it inside a paste (a form feed arrives as Ctrl+L, an
                    // escape sequence's leftovers as Alt+…): pasted, not pressed, so it is
                    // dropped, as a bracketed paste drops it, and the paste goes on
                    None if open => return,
                    None => {}
                }
            }
        }
        if !matches!(ev, TermEvent::Mouse(MouseEvent { kind: MouseEventKind::Moved, .. }) | TermEvent::Resize(..) | TermEvent::FocusGained | TermEvent::FocusLost) {
            self.flush_burst().await;
        }
        self.on_term(ev).await;
        self.settle_focus();
    }

    /// When the open keystroke paste is over if no more keys come.
    pub fn burst_deadline(&self) -> Option<Instant> {
        if self.burst.is_empty() { None } else { self.last_key.map(|t| t + text::BURST_LINGER) }
    }

    pub async fn flush_burst(&mut self) {
        if !self.burst.is_empty() {
            let text = std::mem::take(&mut self.burst);
            self.paste(&text).await;
            self.settle_focus();
        }
    }

    /// Pasted text (bracketed, or a keystroke burst) goes to whatever takes text: an overlay's
    /// input, the sidebar filter while it is being typed, otherwise the open chat's composer, where
    /// a large paste becomes a placeholder token.
    pub async fn paste(&mut self, raw: &str) {
        let text = text::normalize(raw);
        if text.is_empty() {
            return;
        }
        // one-line inputs take it as a single row
        let row = text.trim_end_matches('\n').replace(['\n', '\t'], " ");
        match &mut self.overlay {
            Some(Overlay::Prompt { value, cursor, .. }) => forms::insert_line(value, cursor, &row),
            Some(Overlay::Palette { .. }) => {
                if let Some(Overlay::Palette { mut q, items, hits, .. }) = self.overlay.take() {
                    q.push_str(&row);
                    self.palette_search(Overlay::Palette { q, items, sel: 0, hits }).await;
                }
            }
            Some(Overlay::Memory { q, typing: true, .. }) => {
                q.push_str(&row);
                self.reload_memory().await;
            }
            Some(Overlay::Form { form, .. }) => form.paste(&text),
            Some(_) => {}
            None if self.focus == Focus::Sidebar && self.search_focus => {
                self.search.push_str(&row);
                self.update_search().await;
            }
            None => {
                if self.chat_id.is_none() || self.is_subchat() {
                    return;
                }
                self.focus = Focus::Composer;
                if self.composer_text().is_empty() {
                    self.pastes.clear();
                }
                let mut ins = self.pastes.add(&text);
                // a bracketed paste is text, never a command: at the start of the box, what Enter
                // would run (or send a slash short, like "// a code comment") goes in escaped,
                // the way edit-last puts a message back. (A keystroke paste can't be told from
                // fast typing well enough to do this.)
                if self.pasting && self.composer.cursor() == (0, 0) {
                    let whole = format!("{ins}{}", self.composer_text());
                    if commands::escape(&whole) != whole {
                        ins.insert(0, '/');
                    }
                }
                self.composer.insert_str(ins);
            }
        }
    }

    async fn on_mouse(&mut self, m: MouseEvent) {
        if self.overlay.is_some() {
            return;
        }
        // what was on screen where it landed (the last frame, not the current settings: below 90
        // columns the sidebar and drawer show only while focused)
        let (side, files) = self.drawn_cols;
        let x = m.column;
        let pane = if x < side {
            Focus::Sidebar
        } else if files > 0 && x >= self.last_size.0.saturating_sub(files) {
            Focus::Files
        } else {
            Focus::Messages
        };
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = m.kind == MouseEventKind::ScrollUp;
                match pane {
                    Focus::Sidebar => self.side_scroll = if up { self.side_scroll.saturating_sub(3) } else { self.side_scroll + 3 },
                    Focus::Files => {}
                    _ if up => {
                        self.scroll = self.scroll.saturating_sub(3);
                        self.follow = false;
                    }
                    _ => self.scroll += 3,
                }
            }
            MouseEventKind::Down(MouseButton::Left) => match pane {
                Focus::Sidebar => {
                    if let Some(i) = self.side_row_at(m.row as usize) {
                        self.side_sel = i;
                        self.focus = Focus::Sidebar;
                        self.side_activate().await;
                    }
                }
                Focus::Files => self.focus = Focus::Files,
                // the message box of a chat you write in; the trace of a sub-agent's chat
                _ => self.focus = if self.in_chat() { Focus::Composer } else { Focus::Messages },
            },
            _ => {}
        }
    }

    /// Columns the sidebar and the drawer take at this width: 30 and 32 when shown, and below 90
    /// columns only the focused one (the trace keeps the room). Beside both, the trace keeps 30
    /// (at 90 and 91 columns the drawer gives up the difference); they never add up to more
    /// than the width.
    pub fn pane_cols(&self, width: u16) -> (u16, u16) {
        let small = width < 90;
        let side = if !self.cfg.sidebar || (small && self.focus != Focus::Sidebar) { 0 } else { 30.min(width) };
        let files = if !self.cfg.files || (small && self.focus != Focus::Files) { 0 } else { 32.min(width.saturating_sub(side + if small { 0 } else { 30 })) };
        (side, files)
    }

    /// Which sidebar row is drawn at terminal row `y` (the drawing code records this each frame).
    pub fn side_row_at(&self, y: usize) -> Option<usize> {
        self.side_row_map.get(&y).copied()
    }

    async fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        // a form or prompt holds typing that Ctrl+K or F1 would throw away
        let editing = matches!(self.overlay, Some(Overlay::Form { .. } | Overlay::Prompt { .. }));
        // global
        match (k.code, ctrl, alt) {
            // Ctrl+C cancels what is open or typed: a dialog (as Esc), the sidebar filter, the
            // message box's text; with nothing to cancel it quits
            (KeyCode::Char('c'), true, _) if self.overlay.is_some() => {
                return self.on_overlay_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).await;
            }
            (KeyCode::Char('c'), true, _) if self.focus == Focus::Sidebar && (self.search_focus || !self.search.is_empty()) => {
                return self.clear_search();
            }
            // (from the trace or the sidebar too: a draft is something to clear, not to quit over)
            (KeyCode::Char('c'), true, _) if self.in_chat() && !self.composer_text().is_empty() => {
                self.set_composer("");
                self.edit_from = None;
                return;
            }
            (KeyCode::Char('q'), true, _) | (KeyCode::Char('c'), true, _) => {
                self.quit = true;
                return;
            }
            (KeyCode::Char('k'), true, _) if !editing => {
                self.forget_return();
                if matches!(self.overlay, Some(Overlay::Palette { .. })) {
                    self.overlay = None;
                } else {
                    self.open_palette();
                }
                return;
            }
            (KeyCode::F(1), _, _) if !editing => {
                self.forget_return();
                self.overlay = if matches!(self.overlay, Some(Overlay::Help { .. })) { None } else { Some(Overlay::Help { scroll: 0 }) };
                return;
            }
            _ => {}
        }
        if self.overlay.is_some() {
            self.on_overlay_key(k).await;
            return;
        }
        // approvals: y / a / n with the trace focused, Alt+Y / Alt+A / Alt+N from anywhere (over
        // Alt+A's and Alt+N's usual jobs); never typing in the message box or the sidebar filter
        if !ctrl && !self.trace.pending_approvals().is_empty() {
            let c = match k.code {
                KeyCode::Char(c) if alt => Some(c.to_ascii_lowercase()),
                KeyCode::Char(c) if self.focus == Focus::Messages && !k.modifiers.intersects(KeyModifiers::SUPER | KeyModifiers::META) => Some(c),
                _ => None,
            };
            match c {
                Some('y') => return self.approve(true, false).await,
                Some('a') => return self.approve(true, true).await,
                Some('n') => return self.approve(false, false).await,
                _ => {}
            }
        }
        // the slash-command menu takes its keys before the shortcuts (Tab, Ctrl+N, Esc, ↑)
        if self.focus == Focus::Composer && let Some(items) = self.menu() && self.on_menu_key(k, &items).await {
            return;
        }
        match (k.code, ctrl, alt) {
            (KeyCode::Char('o'), _, true) => return self.toggle_verbose(),
            (KeyCode::Char('b'), _, true) | (KeyCode::Char('b'), true, _) => {
                self.cfg.sidebar = !self.cfg.sidebar;
                self.save_cfg();
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
                let id = self.target_agent();
                return self.open_agent_editor(id);
            }
            (KeyCode::Char('p'), _, true) => return self.target_cmd("pin").await,
            (KeyCode::Char('x'), _, true) => return self.target_cmd("export-md").await,
            (KeyCode::Char('d'), _, true) => return self.target_cmd("delete").await,
            (KeyCode::Char('2'), _, true) | (KeyCode::F(2), _, _) => return self.target_cmd("rename").await,
            (KeyCode::Char('u'), true, _) | (KeyCode::Char('u'), _, true) => {
                if self.in_chat() {
                    self.overlay = Some(Overlay::prompt("Attach a file (path)", "", Act::AttachPath));
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
            (KeyCode::Tab, false, false) => return self.cycle_focus(1),
            (KeyCode::BackTab, _, _) => return self.cycle_focus(-1),
            // the welcome page's numbered agents: a digit starts a chat with that one
            (KeyCode::Char(c @ '1'..='9'), false, false) if self.chat_id.is_none() && !self.search_focus => {
                if let Some(a) = self.agents.get(c as usize - '1' as usize).map(|a| a.id.clone()) {
                    return self.new_chat(&a).await;
                }
            }
            _ => {}
        }
        match self.focus {
            Focus::Sidebar => self.on_sidebar_key(k).await,
            Focus::Messages => self.on_messages_key(k).await,
            Focus::Composer => self.on_composer_key(k).await,
            Focus::Files => self.on_files_key(k).await,
        }
    }

    /// The panes that can take the focus now, in Tab order: the sidebar (unless hidden), the trace
    /// and message box of an open chat (no box in a sub-agent's chat), the files drawer.
    fn panes(&self) -> Vec<Focus> {
        let mut p = vec![];
        if self.cfg.sidebar {
            p.push(Focus::Sidebar);
        }
        if self.chat_id.is_some() {
            p.push(Focus::Messages);
        }
        if self.in_chat() {
            p.push(Focus::Composer);
        }
        if self.cfg.files {
            p.push(Focus::Files);
        }
        p
    }

    fn cycle_focus(&mut self, dir: i32) {
        let p = self.panes();
        if p.is_empty() {
            return;
        }
        let n = p.len() as i32;
        let next = match p.iter().position(|f| *f == self.focus) {
            Some(i) => (i as i32 + dir).rem_euclid(n),
            None => if dir > 0 { 0 } else { n - 1 },
        };
        self.focus = p[next as usize];
    }

    /// Keep the focus on a pane that is there: after the open chat closes, a sub-agent's chat
    /// opens (no message box), or the sidebar or drawer hides, keys would otherwise go to
    /// something invisible (and typed text into a box no one sees).
    pub fn settle_focus(&mut self) {
        let p = self.panes();
        if p.contains(&self.focus) {
            return;
        }
        self.search_focus = false;
        self.focus = [Focus::Composer, Focus::Messages, Focus::Sidebar, Focus::Files].into_iter().find(|f| p.contains(f)).unwrap_or(Focus::Sidebar);
    }

    /// A panel waiting behind a prompt or confirmation stays closed once something else replaces
    /// them.
    fn forget_return(&mut self) {
        self.memory_back = None;
        self.routines_return = false;
    }

    fn clear_search(&mut self) {
        self.search_focus = false;
        self.search.clear();
        self.hits.clear();
        self.rebuild_sidebar();
    }

    async fn on_sidebar_key(&mut self, k: KeyEvent) {
        if self.search_focus {
            match k.code {
                KeyCode::Esc => self.clear_search(),
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
                KeyCode::Char(c) if !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
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
                let id = self.target_agent();
                self.open_agent_editor(id);
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                match self.side_rows.get(self.side_sel).cloned() {
                    Some(SideRow::Chat(_)) => self.target_cmd("delete").await,
                    Some(SideRow::Agent(id)) => {
                        let name = self.agent(&id).map(|a| a.name.clone()).unwrap_or_default();
                        self.overlay = Some(Overlay::Confirm { title: format!("Delete {name}'s idle chats?"), text: "Every idle, unpinned chat with it will be removed. Pinned chats and routine chats stay.".into(), ok: "Delete all".into(), act: Act::DeleteIdle(Some(id)) });
                    }
                    Some(SideRow::AllAgents) => self.overlay = Some(Overlay::Confirm { title: "Delete all idle chats?".into(), text: "Every idle, unpinned chat you started will be removed.".into(), ok: "Delete all".into(), act: Act::DeleteIdle(None) }),
                    _ => {}
                }
            }
            KeyCode::Char('r') => self.target_cmd("rename").await,
            KeyCode::Char('p') => self.target_cmd("pin").await,
            KeyCode::Char('x') => self.target_cmd("export-md").await,
            KeyCode::Char('J') => self.target_cmd("export-json").await,
            KeyCode::Char('m') => {
                for c in &self.chats {
                    self.cfg.seen.insert(c.id.clone(), c.updated);
                }
                self.save_cfg();
                self.toast("Marked all as read", false);
            }
            KeyCode::Esc => {
                if !self.search.is_empty() {
                    self.clear_search();
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
                let text = self.composer_text();
                match commands::parse(&text) {
                    Parsed::Run(name, args) => {
                        if let Some(cmd) = commands::find(name) {
                            let args = args.to_string();
                            return self.run_slash(cmd, &args).await;
                        }
                    }
                    Parsed::Unknown(word) => return self.toast(format!("Unknown command /{word}. /help lists them; start with // to send a message that begins with /"), false),
                    Parsed::Message(_) => {}
                }
                if let Some(idx) = self.edit_from {
                    // the box (and edit mode) go only once the server took it: refused or failed,
                    // the edited message is still there to send again
                    let content = self.pastes.expand(commands::message(&self.composer_text())).trim().to_string();
                    if content.is_empty() {
                        self.edit_from = None;
                    } else if self.run_body(json!({ "content": content, "from_index": idx })).await {
                        self.edit_from = None;
                        self.set_composer("");
                        if let Some(cid) = self.chat_id.clone() {
                            self.drafts.remove(&cid);
                            self.draft_pastes.remove(&cid);
                        }
                    }
                } else {
                    self.send().await;
                }
            }
            KeyCode::Esc => {
                if self.edit_from.take().is_some() {
                    self.set_composer("");
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
            Overlay::Help { scroll } => match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::F(1) | KeyCode::Char('?') => {}
                // (drawing clamps it to the list)
                KeyCode::Up | KeyCode::Char('k') => self.overlay = Some(Overlay::Help { scroll: scroll.saturating_sub(1) }),
                KeyCode::Down | KeyCode::Char('j') => self.overlay = Some(Overlay::Help { scroll: scroll + 1 }),
                KeyCode::PageUp => self.overlay = Some(Overlay::Help { scroll: scroll.saturating_sub(10) }),
                KeyCode::PageDown => self.overlay = Some(Overlay::Help { scroll: scroll + 10 }),
                KeyCode::Home => self.overlay = Some(Overlay::Help { scroll: 0 }),
                KeyCode::End => self.overlay = Some(Overlay::Help { scroll: usize::MAX / 2 }),
                _ => self.overlay = Some(Overlay::Help { scroll }),
            },
            Overlay::Toasts => {
                if !matches!(k.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::F(1) | KeyCode::Char('?')) {
                    self.overlay = Some(overlay);
                }
            }
            Overlay::Confirm { title, text, ok, act } => match k.code {
                KeyCode::Enter | KeyCode::Char('y') => self.do_act(act, None).await,
                KeyCode::Esc | KeyCode::Char('n') => {}
                _ => self.overlay = Some(Overlay::Confirm { title, text, ok, act }),
            },
            Overlay::Prompt { title, mut value, mut cursor, act } => match k.code {
                KeyCode::Enter => self.do_act(act, Some(value)).await,
                KeyCode::Esc => {}
                _ => {
                    forms::edit_line(&mut value, &mut cursor, k);
                    self.overlay = Some(Overlay::Prompt { title, value, cursor, act });
                }
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
            Overlay::Palette { mut q, items, mut sel, hits } => {
                match k.code {
                    KeyCode::Esc => return,
                    KeyCode::Up | KeyCode::Down | KeyCode::Tab => {
                        sel = if k.code == KeyCode::Up { sel.saturating_sub(1) } else { (sel + 1).min(items.len().saturating_sub(1)) };
                        self.overlay = Some(Overlay::Palette { q, items, sel, hits });
                        return;
                    }
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
                                PaletteAction::Slash(name) => {
                                    if let Some(cmd) = commands::find(name) {
                                        let args = commands::args_of(q.trim()).to_string();
                                        if let Err(why) = commands::check(cmd, self.in_chat(), self.running) {
                                            return self.toast(why, false);
                                        }
                                        self.command(cmd, &args).await;
                                    }
                                }
                            }
                        }
                        return;
                    }
                    KeyCode::Backspace => {
                        q.pop();
                    }
                    KeyCode::Char(c) if !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => q.push(c),
                    _ => {
                        self.overlay = Some(Overlay::Palette { q, items, sel, hits });
                        return;
                    }
                }
                self.palette_search(Overlay::Palette { q, items, sel, hits }).await;
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
            Overlay::Info { title, lines, mut scroll } => {
                match k.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char(' ') => return,
                    KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => scroll += 1, // drawing clamps it
                    KeyCode::PageUp => scroll = scroll.saturating_sub(10),
                    KeyCode::PageDown => scroll += 10,
                    KeyCode::Home => scroll = 0,
                    _ => {}
                }
                self.overlay = Some(Overlay::Info { title, lines, scroll });
            }
            Overlay::Models { agent, items, mut sel } => match k.code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Enter => {
                    if let Some(m) = items.get(sel).cloned() {
                        self.set_model(&agent, m).await;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    sel = sel.saturating_sub(1);
                    self.overlay = Some(Overlay::Models { agent, items, sel });
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                    sel = (sel + 1).min(items.len().saturating_sub(1));
                    self.overlay = Some(Overlay::Models { agent, items, sel });
                }
                _ => self.overlay = Some(Overlay::Models { agent, items, sel }),
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
                        // the prompt stands in for the panel, which comes back after it
                        self.memory_back = Some(Box::new(Overlay::Memory { data, cats, total, sel, q, cat, typing }));
                        self.overlay = Some(Overlay::prompt("Remember something (e.g. “I prefer short answers”)", "", Act::AddMemory));
                        return;
                    }
                    KeyCode::Char('d') | KeyCode::Delete => {
                        if let Some(m) = data.get(sel) {
                            // like every other delete, it asks first
                            let title = format!("Forget “{}”?", trace::one_line(&m.fact, 50));
                            let act = Act::DeleteMemory(m.id);
                            self.memory_back = Some(Box::new(Overlay::Memory { data, cats, total, sel, q, cat, typing }));
                            self.overlay = Some(Overlay::Confirm { title, text: "Every agent stops knowing it. A fact the model learned may be learned again.".into(), ok: "Forget".into(), act });
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
                        self.memory_back = Some(Box::new(Overlay::Memory { data, cats, total, sel, q, cat, typing }));
                        self.overlay = Some(Overlay::Confirm { title: "Tidy up memory?".into(), text: "The model merges duplicates, resolves contradictions (newest wins) and drops trivia. Facts you typed in yourself are never changed.".into(), ok: "Tidy up".into(), act: Act::Tidy });
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
                // (Esc in the editor comes back here, as saving does)
                KeyCode::Char('n') => {
                    self.open_routine_editor(None);
                    self.routines_return = true;
                }
                KeyCode::Enter | KeyCode::Char('e') => {
                    match items.get(sel).cloned() {
                        Some(r) => self.open_routine_editor(Some(&r)),
                        None => self.open_routine_editor(None),
                    }
                    self.routines_return = true;
                }
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
            if let Some(panel) = self.memory_back.take() {
                // back to the panel as it was (search, kind, selection), its list fresh
                self.overlay = Some(*panel);
                self.reload_memory().await;
            } else if self.routines_return {
                self.routines_return = false;
                self.open_routines().await;
            }
        }
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        self.toasts.retain(|t| now.duration_since(t.at) < Duration::from_millis(if t.err { 7000 } else { 3500 }));
        // /context and /usage say how long the chat has been going: an open one keeps counting
        if let Some(Overlay::Info { title, scroll, .. }) = &self.overlay {
            let scroll = *scroll;
            let fresh = match title.as_str() {
                "/context" => self.context_info(),
                "/usage" => self.usage_info(),
                _ => return,
            };
            if let Overlay::Info { title, lines, .. } = fresh {
                self.overlay = Some(Overlay::Info { title, lines, scroll });
            }
        }
    }
}

/// Write a chat's export into `dir` as `name.kind`, or `name-2.kind` and on when that is taken:
/// an existing file is never replaced, unless it is this chat's export written earlier
/// (`exported` keeps which). Returns where it went.
fn save_export(dir: &std::path::Path, name: &str, kind: &str, text: &str, chat_id: &str, exported: &mut HashMap<PathBuf, String>) -> std::io::Result<PathBuf> {
    for n in 1..1000 {
        let full = dir.join(if n == 1 { format!("{name}.{kind}") } else { format!("{name}-{n}.{kind}") });
        let res = if exported.get(&full).is_some_and(|id| id == chat_id) {
            std::fs::write(&full, text)
        } else {
            std::fs::OpenOptions::new().write(true).create_new(true).open(&full).and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()))
        };
        match res {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue, // (someone else's: the next name)
            Err(e) => return Err(e),
            Ok(()) => {
                exported.insert(full.clone(), chat_id.to_string());
                return Ok(full);
            }
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "every name is taken"))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An app with a chat open and nothing behind it (the API points at a closed port), with a
    /// state.json of its own (tests run side by side, and one hiding the sidebar mustn't hide
    /// another's).
    fn chat_app() -> App {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("agent-chat-tui-app-test-{}-{n}-{t}", std::process::id()));
        let mut app = App::new(Api::new("http://127.0.0.1:9"), Theme::dark(), dir.join("state.json"));
        app.agents = vec![Agent { id: "a1".into(), name: "Assistant".into(), ..Default::default() }];
        app.chats = vec![ChatSummary { id: "c1".into(), title: "chat".into(), agent_id: "a1".into(), status: "idle".into(), ..Default::default() }];
        app.chat_id = Some("c1".into());
        app.chat = Some(json!({ "id": "c1", "agent_id": "a1", "messages": [] }));
        app.rebuild_sidebar();
        app.focus = Focus::Composer;
        app
    }

    fn key(c: char) -> TermEvent {
        TermEvent::Key(match c {
            '\r' => KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            '\n' => KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
            '\t' => KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            c => KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        })
    }

    /// Feed text as keys that all arrived together at `at` (one read from the terminal).
    async fn keys_at(app: &mut App, text: &str, at: Instant) {
        let n = text.chars().count();
        for (i, c) in text.chars().enumerate() {
            app.on_input(at, key(c), if i + 1 < n { Some(at) } else { None }).await;
        }
    }

    #[tokio::test]
    async fn keystroke_paste_is_one_text_not_several_sends() {
        let mut app = chat_app();
        let t0 = Instant::now();
        keys_at(&mut app, "one\rtwo\r\tthree\nfour", t0).await;
        assert_eq!(app.composer_text(), "", "held until the burst ends");
        assert_eq!(app.burst_deadline(), Some(t0 + text::BURST_LINGER));
        app.flush_burst().await;
        assert_eq!(app.composer_text(), "one\ntwo\n\tthree\nfour");
        assert_eq!(app.focus, Focus::Composer, "a Tab inside a paste is text, not a focus change");
        assert!(app.toasts.is_empty(), "nothing was sent or queued");
        // a person pressing Enter a moment later sends (which fails here: there is no server)
        app.on_input(t0 + Duration::from_millis(400), key('\r'), None).await;
        assert!(app.toasts.iter().any(|t| t.err), "Enter on its own should have tried to send");
    }

    #[tokio::test]
    async fn typing_is_not_a_burst() {
        let mut app = chat_app();
        let t0 = Instant::now();
        for (i, c) in "hi".chars().enumerate() {
            app.on_input(t0 + Duration::from_millis(150 * i as u64), key(c), None).await;
        }
        assert_eq!(app.composer_text(), "hi");
        assert!(app.burst_deadline().is_none());
    }

    #[tokio::test]
    async fn large_keystroke_paste_becomes_a_token() {
        let mut app = chat_app();
        let body: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        keys_at(&mut app, &body.join("\r"), Instant::now()).await;
        app.flush_burst().await;
        assert_eq!(app.composer_text(), "[Pasted text #1 +30 lines]");
        assert_eq!(app.pastes.expand(&app.composer_text()), body.join("\n"));
    }

    #[tokio::test]
    async fn burst_outside_the_composer_triggers_no_shortcuts() {
        let mut app = chat_app();
        app.focus = Focus::Sidebar;
        app.select_chat_row("c1");
        // "d" would ask to delete the chat and Enter would confirm it, if these were keystrokes
        keys_at(&mut app, "do this\r", Instant::now()).await;
        app.flush_burst().await;
        assert!(app.overlay.is_none());
        assert_eq!(app.focus, Focus::Composer);
        assert_eq!(app.composer_text(), "do this\n");
    }

    #[tokio::test]
    async fn bracketed_paste_is_normalised_and_large_ones_held_aside() {
        let mut app = chat_app();
        app.on_input(Instant::now(), TermEvent::Paste("a\r\nb\rc\x1b[31m red\x1b[0m\td".into()), None).await;
        assert_eq!(app.composer_text(), "a\nb\nc red\td");
        let big: String = (0..200).map(|i| format!("row {i}\r\n")).collect();
        app.on_input(Instant::now(), TermEvent::Paste(big.clone()), None).await;
        app.on_input(Instant::now(), TermEvent::Paste("x".repeat(1500)), None).await;
        assert_eq!(app.composer_text(), "a\nb\nc red\td[Pasted text #1 +200 lines][Pasted text #2, 1500 chars]");
        let sent = app.pastes.expand(&app.composer_text());
        assert_eq!(sent, format!("a\nb\nc red\td{}{}", big.replace("\r\n", "\n"), "x".repeat(1500)));
        // Ctrl+C empties the box and forgets the pastes; numbering starts again
        app.on_input(Instant::now(), TermEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), None).await;
        assert!(!app.quit);
        assert_eq!(app.composer_text(), "");
        app.on_input(Instant::now(), TermEvent::Paste(big), None).await;
        assert_eq!(app.composer_text(), "[Pasted text #1 +200 lines]");
        // a paste into a prompt is one line
        app.overlay = Some(Overlay::prompt("Attach", "", Act::AttachPath));
        app.on_input(Instant::now(), TermEvent::Paste("/tmp/some file\n".into()), None).await;
        assert!(matches!(&app.overlay, Some(Overlay::Prompt { value, .. }) if value == "/tmp/some file"));
    }

    #[tokio::test]
    async fn drafts_keep_their_pastes() {
        let mut app = chat_app();
        app.chats.push(ChatSummary { id: "c2".into(), title: "other".into(), agent_id: "a1".into(), status: "idle".into(), ..Default::default() });
        let big = vec!["p"; 20].join("\n");
        app.paste(&big).await;
        app.open_chat(Some("c2".into())).await;
        assert_eq!(app.composer_text(), "");
        app.open_chat(Some("c1".into())).await;
        assert_eq!(app.composer_text(), "[Pasted text #1 +20 lines]");
        assert_eq!(app.pastes.expand(&app.composer_text()), big);
    }

    /// Type text the way a person does: one key at a time, far apart (never a keystroke paste).
    async fn type_slow(app: &mut App, text: &str) {
        for c in text.chars() {
            let at = app.last_key.map(|t| t + Duration::from_millis(200)).unwrap_or_else(Instant::now);
            app.on_input(at, key(c), None).await;
        }
    }

    async fn press(app: &mut App, code: KeyCode, mods: KeyModifiers) {
        let at = app.last_key.map(|t| t + Duration::from_millis(200)).unwrap_or_else(Instant::now);
        app.on_input(at, TermEvent::Key(KeyEvent::new(code, mods)), None).await;
    }

    fn menu_names(app: &mut App) -> Option<Vec<&'static str>> {
        app.menu().map(|m| m.iter().map(|c| c.name).collect())
    }

    #[tokio::test]
    async fn the_menu_opens_filters_moves_completes_and_closes() {
        let mut app = chat_app();
        assert!(app.menu().is_none());
        type_slow(&mut app, "/").await;
        assert_eq!(app.menu().map(|m| m.len()), Some(commands::COMMANDS.len()));
        type_slow(&mut app, "re").await;
        assert_eq!(menu_names(&mut app).unwrap(), ["resume", "rename", "retry", "remember"]);
        assert_eq!(app.menu_sel, 0);
        press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
        press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
        assert_eq!(app.menu_sel, 2);
        assert!(app.overlay.is_none(), "Ctrl+N moved the selection instead of starting a chat");
        press(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL).await;
        press(&mut app, KeyCode::Up, KeyModifiers::NONE).await;
        press(&mut app, KeyCode::Up, KeyModifiers::NONE).await;
        assert_eq!(app.menu_sel, 3, "↑ wraps around");
        assert_eq!(app.focus, Focus::Composer, "↑ stayed in the menu");
        // Tab completes the selected command (it takes arguments, so a space) and the menu closes
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE).await;
        assert_eq!(app.composer_text(), "/remember ");
        assert_eq!(app.composer.cursor(), (0, 10));
        assert_eq!(app.focus, Focus::Composer, "Tab completed instead of moving the focus");
        assert!(app.menu().is_none());
        // Esc closes it and keeps the text; typing on reopens it with the selection back on top
        app.set_composer("");
        type_slow(&mut app, "/he").await;
        assert!(app.menu().is_some());
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert!(app.menu().is_none());
        assert_eq!(app.composer_text(), "/he");
        assert_eq!(app.focus, Focus::Composer, "Esc only closed the menu");
        type_slow(&mut app, "l").await;
        assert_eq!(menu_names(&mut app).unwrap(), ["help"]);
        // Enter runs the selected one: the box empties and its answer shows
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert_eq!(app.composer_text(), "");
        assert!(matches!(&app.overlay, Some(Overlay::Info { title, .. }) if title == "/help"));
        assert!(app.toasts.is_empty());
    }

    #[tokio::test]
    async fn the_menu_runs_with_what_follows_and_aliases_show_the_command() {
        let mut app = chat_app();
        type_slow(&mut app, "/stats").await;
        assert_eq!(menu_names(&mut app).unwrap(), ["usage"]);
        // the cursor back in the word with arguments after it: Enter runs the pick with them
        app.set_composer("/ren x");
        app.composer.move_cursor(CursorMove::Jump(0, 2));
        assert_eq!(menu_names(&mut app).unwrap(), ["rename"]);
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE).await;
        assert_eq!(app.composer_text(), "/rename x");
        assert_eq!(app.composer.cursor(), (0, 8));
        // a path or "//" never opens it
        app.set_composer("/etc/ho");
        assert!(app.menu().is_none());
        app.set_composer("//he");
        assert!(app.menu().is_none());
        // nor outside the composer
        app.set_composer("/he");
        app.focus = Focus::Messages;
        assert!(app.menu().is_none());
    }

    #[tokio::test]
    async fn commands_never_reach_the_agent() {
        let mut app = chat_app();
        // while it works, a command runs now instead of queuing (a send would fail loudly here)
        app.running = true;
        type_slow(&mut app, "/usage").await;
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await; // menu closed: Enter parses the text
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(matches!(&app.overlay, Some(Overlay::Info { title, .. }) if title == "/usage"));
        assert!(app.toasts.is_empty(), "nothing was sent or queued: {:?}", app.toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
        assert_eq!(app.composer_text(), "");
        app.overlay = None;
        // one that would change what it works on (or that the server refuses mid-run, like /pin)
        // says so, never queues, and keeps the text to run once it's done
        for text in ["/compact keep the API notes", "/pin", "/rename later"] {
            app.toasts.clear();
            app.set_composer(text);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
            assert!(app.toasts.last().is_some_and(|t| !t.err && t.text.contains("working") && t.text.contains("/stop")), "{text}: {:?}", app.toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
            assert_eq!(app.composer_text(), text, "a refused command stays in the box");
        }
        // picked from the menu, the same
        app.set_composer("/compa");
        assert_eq!(app.menu().map(|m| m[0].name), Some("compact"));
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.toasts.last().is_some_and(|t| t.text.contains("/compact")));
        assert_eq!(app.composer_text(), "/compa");
        app.set_composer("");
        app.running = false;
        app.toasts.clear();
        // an unknown one keeps the text and says how to send it as a message
        app.set_composer("/nosuch thing");
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.toasts.last().is_some_and(|t| t.text.starts_with("Unknown command /nosuch") && t.text.contains("//")));
        assert_eq!(app.composer_text(), "/nosuch thing");
        app.toasts.clear();
        // "//" and paths are messages: Enter tries to send them (there is no server, so it fails)
        for text in ["//hi", "/etc/hosts is broken"] {
            app.set_composer(text);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
            assert!(app.overlay.is_none());
            assert!(app.toasts.last().is_some_and(|t| t.err), "{text} should have been sent");
            assert_eq!(commands::message(&app.composer_text()), &text[usize::from(text.starts_with("//"))..]);
            app.toasts.clear();
        }
    }

    #[tokio::test]
    async fn a_pasted_command_is_only_text() {
        let mut app = chat_app();
        app.paste("/help\n").await;
        assert!(app.overlay.is_none());
        assert_eq!(app.composer_text(), "/help\n");
        app.set_composer("");
        keys_at(&mut app, "/quit\r", Instant::now()).await;
        app.flush_burst().await;
        assert!(!app.quit, "a keystroke paste with a line break ran /quit");
        assert_eq!(app.composer_text(), "/quit\n");
        assert!(app.menu().is_none(), "the cursor is past the first line");
    }

    #[tokio::test]
    async fn availability_theme_and_escapes() {
        let mut app = chat_app();
        let cmd = |n| commands::find(n).unwrap();
        // a chat-only command with no chat open
        app.chat_id = None;
        app.run_slash(cmd("pin"), "").await;
        assert_eq!(app.toasts.last().map(|t| t.text.as_str()), Some("Open a chat first"));
        app.chat_id = Some("c1".into());
        // /theme cycles, names one, and is remembered
        app.run_slash(cmd("theme"), "").await;
        assert_eq!((app.theme.name, app.cfg.theme.as_str()), ("light", "light"));
        app.run_slash(cmd("theme"), "PLAIN").await;
        assert_eq!(app.theme.name, "plain");
        app.run_slash(cmd("theme"), "neon").await;
        assert_eq!(app.theme.name, "plain");
        assert_eq!(app.toasts.last().map(|t| t.text.as_str()), Some("Themes: dark, light, plain"));
        // /stop with nothing running, /remember with nothing to remember, a bad /export
        app.run_slash(cmd("stop"), "").await;
        assert_eq!(app.toasts.last().map(|t| t.text.as_str()), Some("Nothing is running"));
        app.run_slash(cmd("remember"), " ").await;
        assert!(app.toasts.last().unwrap().text.starts_with("Say what to remember"));
        app.run_slash(cmd("export"), "pdf").await;
        assert!(app.toasts.last().unwrap().text.starts_with("Export as md or json"));
        // a message of yours that looks like a command comes back escaped for editing
        let chat = json!({ "id": "c1", "agent_id": "a1", "messages": [{ "role": "user", "content": "/hi there" }, { "role": "assistant", "content": "ok" }] });
        app.trace = Trace::from_chat(&chat, &app.agents, None);
        app.chat = Some(chat);
        app.run_slash(cmd("edit"), "").await;
        assert_eq!(app.composer_text(), "//hi there");
        assert_eq!(app.edit_from, Some(0));
        assert_eq!(commands::message(&app.composer_text()), "/hi there");
        // /copy takes the last reply
        app.run_slash(cmd("copy"), "").await;
        assert_eq!(app.toasts.last().map(|t| t.text.as_str()), Some("Copied the last reply (2 characters)"));
        assert_eq!(app.composer_text(), "", "running a command also leaves edit mode");
        assert_eq!(app.edit_from, None);
        // /model lists the default first, then the server's models, the current one selected
        app.server.models = vec!["m1".into(), "m2".into()];
        app.agents[0].model = "m2".into();
        assert!(matches!(app.models_overlay(), Some(Overlay::Models { items, sel: 2, .. }) if items == ["", "m1", "m2"]));
        app.agents[0].model = "elsewhere".into();
        assert!(matches!(app.models_overlay(), Some(Overlay::Models { items, sel: 3, .. }) if items == ["", "m1", "m2", "elsewhere"]));
    }

    fn alt(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    /// What a failed request toasts here (nothing listens on the test API's port): the sign that
    /// the app tried to answer an approval, start a chat, add a calendar, …
    fn tried(app: &App) -> bool {
        app.toasts.last().is_some_and(|t| t.err && t.text.contains("request failed"))
    }

    /// chat_app with a shell command waiting for approval in the open chat, and a second chat.
    fn approval_app() -> App {
        let mut app = chat_app();
        app.chats[0].title = "first chat".into();
        app.chats.push(ChatSummary { id: "c2".into(), title: "second chat".into(), agent_id: "a1".into(), status: "idle".into(), ..Default::default() });
        app.rebuild_sidebar();
        app.select_chat_row("c1");
        let ag = app.agents.clone();
        app.trace.start_turn(agent_who(&ag, "a1"), 1);
        app.trace.tool_start(&[], "t1", "run_shell", json!({ "command": "echo hi" }), &ag);
        app.trace.approval(&[], "t1", Some("ap1"), None, &ag);
        assert_eq!(app.trace.pending_approvals().len(), 1);
        app
    }

    #[tokio::test]
    async fn approvals_take_y_a_n_only_in_the_trace_or_with_alt() {
        let mut app = approval_app();
        // the message box: an empty one takes the letters as text
        type_slow(&mut app, "no thanks, all good yes").await;
        assert_eq!(app.composer_text(), "no thanks, all good yes");
        assert!(app.toasts.is_empty(), "nothing answered: {:?}", app.toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
        // Alt+A and Alt+N answer it, over the agent editor and the new-chat picker
        for c in ['a', 'n', 'y', 'Y'] {
            app.toasts.clear();
            app.on_input(Instant::now() + Duration::from_secs(1), TermEvent::Key(alt(c)), None).await;
            assert!(tried(&app), "Alt+{c} should answer the approval");
            assert!(app.overlay.is_none(), "Alt+{c} opened {:?}", app.overlay.as_ref().map(|_| "an overlay"));
        }
        assert_eq!(app.composer_text(), "no thanks, all good yes");
        // the sidebar filter takes them as text, and the sidebar's own n starts a chat
        app.toasts.clear();
        app.focus = Focus::Sidebar;
        type_slow(&mut app, "/yan").await;
        assert_eq!(app.search, "yan");
        assert!(app.toasts.is_empty());
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        app.select_chat_row("c1");
        type_slow(&mut app, "n").await;
        assert!(matches!(app.overlay, Some(Overlay::Pick { .. })), "n in the sidebar picks an agent for a new chat");
        assert!(app.toasts.is_empty());
        app.overlay = None;
        // in the trace the plain letters answer
        app.focus = Focus::Messages;
        type_slow(&mut app, "y").await;
        assert!(tried(&app));
        // a keystroke paste there is text for the message box, not three answers
        app.toasts.clear();
        app.set_composer("");
        app.focus = Focus::Messages;
        keys_at(&mut app, "yes", Instant::now() + Duration::from_secs(5)).await;
        app.flush_burst().await;
        assert_eq!((app.composer_text().as_str(), app.focus), ("yes", Focus::Composer));
        assert!(app.toasts.is_empty());
    }

    #[tokio::test]
    async fn shortcuts_act_on_the_open_chat_except_in_the_sidebar() {
        let mut app = approval_app();
        app.trace = Trace::empty();
        app.select_chat_row("c2"); // highlighted in the sidebar, while c1 is open
        let confirm_title = |app: &App| match &app.overlay {
            Some(Overlay::Confirm { title, .. }) => title.clone(),
            _ => String::new(),
        };
        press(&mut app, KeyCode::Char('d'), KeyModifiers::ALT).await;
        assert!(confirm_title(&app).contains("first chat"), "from the message box Alt+D deletes the open chat: {}", confirm_title(&app));
        app.overlay = None;
        app.focus = Focus::Sidebar;
        press(&mut app, KeyCode::Char('d'), KeyModifiers::ALT).await;
        assert!(confirm_title(&app).contains("second chat"), "in the sidebar it is the highlighted one");
        app.overlay = None;
        // the palette's "This chat" group is the open chat, wherever the focus was
        app.run_cmd("delete").await;
        assert!(confirm_title(&app).contains("first chat"));
        app.overlay = None;
        press(&mut app, KeyCode::F(2), KeyModifiers::NONE).await;
        assert!(matches!(&app.overlay, Some(Overlay::Prompt { value, .. }) if value == "second chat"));
    }

    #[tokio::test]
    async fn the_focus_stays_on_panes_that_are_on_screen() {
        let mut app = chat_app();
        let tabs = |app: &mut App| {
            let mut seen = vec![app.focus];
            for _ in 0..4 {
                app.cycle_focus(1);
                seen.push(app.focus);
            }
            seen
        };
        use Focus::*;
        app.focus = Sidebar;
        assert_eq!(tabs(&mut app), [Sidebar, Messages, Composer, Sidebar, Messages]);
        app.cfg.files = true;
        assert_eq!(tabs(&mut app), [Messages, Composer, Files, Sidebar, Messages]);
        // a sub-agent's chat has no message box: Tab, Shift+Tab, Esc in the drawer and a click skip it
        app.chat = Some(json!({ "id": "c1", "agent_id": "a1", "parent": { "root_chat_id": "c0", "caller_id": "a1" }, "messages": [] }));
        app.focus = Sidebar;
        assert_eq!(tabs(&mut app), [Sidebar, Messages, Files, Sidebar, Messages]);
        app.focus = Sidebar;
        press(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT).await;
        assert_eq!(app.focus, Files);
        press(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT).await;
        assert_eq!(app.focus, Messages, "Shift+Tab from the drawer skips the hidden message box");
        app.focus = Files;
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert_eq!(app.focus, Messages);
        app.drawn_cols = (30, 32);
        app.last_size = (140, 40);
        app.on_input(Instant::now() + Duration::from_secs(9), TermEvent::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 60, row: 10, modifiers: KeyModifiers::NONE }), None).await;
        assert_eq!(app.focus, Messages, "a click in the trace of a sub-agent's chat");
        // the open chat closes (deleted, say) with the focus in its message box: keys must not go
        // into a box no one sees, to turn up in the next chat
        app.chat = Some(json!({ "id": "c1", "agent_id": "a1", "messages": [] }));
        app.cfg.files = false;
        app.focus = Composer;
        app.open_chat(None).await;
        app.settle_focus();
        assert_eq!(app.focus, Sidebar);
        type_slow(&mut app, "x").await;
        assert_eq!(app.composer_text(), "");
        // the sidebar hidden with nothing else on screen: it keeps the keys (n, digits) anyway
        press(&mut app, KeyCode::Char('b'), KeyModifiers::ALT).await;
        assert_eq!(app.focus, Sidebar);
    }

    #[tokio::test]
    async fn the_welcome_pages_numbers_start_chats() {
        let mut app = chat_app();
        app.open_chat(None).await;
        app.focus = Focus::Sidebar;
        type_slow(&mut app, "9").await;
        assert!(app.toasts.is_empty(), "no ninth agent");
        type_slow(&mut app, "1").await;
        assert!(tried(&app), "1 starts a chat with the first agent");
        // with a chat open the digits are no shortcut
        let mut app = chat_app();
        app.focus = Focus::Sidebar;
        type_slow(&mut app, "1").await;
        assert!(app.toasts.is_empty());
    }

    #[tokio::test]
    async fn one_stream_per_visit_and_a_deleted_chat_closes() {
        let mut app = approval_app();
        app.open_chat(Some("c2".into())).await;
        app.open_chat(Some("c1".into())).await;
        let visit = app.stream_gen;
        let snapshot = |title: &str| Event { kind: "snapshot".into(), data: json!({ "type": "snapshot", "chat": { "id": "c1", "title": title, "agent_id": "a1", "messages": [] }, "running": false }) };
        // an earlier visit's stream is ignored, this one's is applied
        app.on_incoming(Incoming::Chat { chat_id: "c1".into(), visit: visit - 2, event: snapshot("stale") }).await;
        assert_eq!(app.chat.as_ref().and_then(|c| c["title"].as_str()), Some("first chat"), "from the chat list until the snapshot");
        app.on_incoming(Incoming::Chat { chat_id: "c1".into(), visit, event: snapshot("fresh") }).await;
        assert_eq!(app.chat.as_ref().and_then(|c| c["title"].as_str()), Some("fresh"));
        app.on_incoming(Incoming::StreamEnded { chat_id: "c1".into(), visit: visit - 2, error: Some("HTTP 404".into()) }).await;
        assert_eq!((app.chat_id.as_deref(), app.toasts.len()), (Some("c1"), 0), "an old stream ending changes nothing");
        // this visit's stream dropping: say so and reconnect
        app.on_incoming(Incoming::StreamEnded { chat_id: "c1".into(), visit, error: Some("connection reset".into()) }).await;
        assert!(app.toasts.last().is_some_and(|t| t.text.starts_with("Lost the chat stream")));
        assert!(app.chat_stream.is_some());
        // the chat is gone: once, and back to the welcome page
        app.toasts.clear();
        app.on_incoming(Incoming::StreamEnded { chat_id: "c1".into(), visit, error: Some("HTTP 404".into()) }).await;
        assert_eq!(app.chat_id, None);
        assert_eq!(app.toasts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), ["That chat no longer exists."]);
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[tokio::test]
    async fn a_chat_reads_as_itself_before_it_loads() {
        let mut app = chat_app();
        app.chats.push(ChatSummary { id: "s1".into(), title: "the sub".into(), agent_id: "a1".into(), parent: Some(crate::api::Parent { root_chat_id: "c1".into(), caller_id: "a1".into() }), ..Default::default() });
        app.open_chat(Some("s1".into())).await;
        assert_eq!(app.chat_agent_id(), "a1", "not a deleted agent");
        assert!(app.is_subchat());
        app.settle_focus();
        assert_eq!(app.focus, Focus::Messages, "a sub-agent's chat has no message box");
    }

    #[tokio::test]
    async fn edit_mode_stays_with_its_chat() {
        let mut app = approval_app();
        app.set_composer("my last message");
        app.edit_from = Some(0);
        app.open_chat(Some("c2".into())).await;
        assert_eq!(app.edit_from, None, "Enter in another chat would resend from this one's index");
    }

    #[tokio::test]
    async fn edit_last_counts_the_messages_it_replaces() {
        let mut app = chat_app();
        let chat = json!({ "id": "c1", "agent_id": "a1", "messages": [
            { "role": "user", "content": "run it" },
            { "role": "assistant", "content": "", "tool_calls": [{ "id": "t1", "function": { "name": "run_shell", "arguments": "{\"command\":\"ls\"}" } }] },
            { "role": "tool", "tool_call_id": "t1", "content": "exit code 0" },
            { "role": "assistant", "content": "done" },
        ] });
        app.trace = Trace::from_chat(&chat, &app.agents, None);
        app.chat = Some(chat);
        press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL).await;
        assert_eq!(app.composer_text(), "run it");
        assert!(app.toasts.last().unwrap().text.contains("(the 3 messages after it"), "{}", app.toasts.last().unwrap().text);
        // not in a sub-agent's chat, which has no message box to edit in
        app.set_composer("");
        app.edit_from = None;
        app.chat.as_mut().unwrap()["parent"] = json!({ "root_chat_id": "c0", "caller_id": "a1" });
        press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL).await;
        assert_eq!((app.composer_text().as_str(), app.edit_from), ("", None));
    }

    #[tokio::test]
    async fn dialogs_keep_typing_from_global_keys_and_ctrl_c_cancels() {
        let mut app = chat_app();
        app.open_settings().await;
        for (code, mods) in [(KeyCode::Char('k'), KeyModifiers::CONTROL), (KeyCode::F(1), KeyModifiers::NONE)] {
            press(&mut app, code, mods).await;
            assert!(matches!(app.overlay, Some(Overlay::Form { .. })), "{code:?} would throw the form's edits away");
        }
        press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL).await;
        assert!(app.overlay.is_none() && !app.quit, "Ctrl+C closes the form instead of quitting");
        app.overlay = Some(Overlay::prompt("Rename chat", "draft", Act::RenameChat("c1".into())));
        press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL).await;
        assert!(matches!(&app.overlay, Some(Overlay::Prompt { value, .. }) if value == "draft"));
        // elsewhere Ctrl+K and F1 still switch
        app.overlay = Some(Overlay::Help { scroll: 0 });
        press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL).await;
        assert!(matches!(app.overlay, Some(Overlay::Palette { .. })));
        press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL).await;
        assert!(app.overlay.is_none() && !app.quit);
        // the sidebar filter clears, then (nothing left to clear) Ctrl+C quits
        app.focus = Focus::Sidebar;
        type_slow(&mut app, "/zz").await;
        press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL).await;
        assert!((app.search.as_str(), app.search_focus, app.quit) == ("", false, false));
        press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL).await;
        assert!(app.quit);
    }

    #[tokio::test]
    async fn prompts_edit_at_the_cursor() {
        let mut app = chat_app();
        app.overlay = Some(Overlay::prompt("Rename chat", "abc", Act::RenameChat("c1".into())));
        for code in [KeyCode::Left, KeyCode::Left, KeyCode::Char('X'), KeyCode::Home, KeyCode::Delete, KeyCode::End, KeyCode::Left] {
            press(&mut app, code, KeyModifiers::NONE).await;
        }
        app.paste("1\n").await;
        assert!(matches!(&app.overlay, Some(Overlay::Prompt { value, cursor: 3, .. }) if value == "Xb1c"), "{:?}", match &app.overlay { Some(Overlay::Prompt { value, cursor, .. }) => (value.clone(), *cursor), _ => (String::new(), 0) });
    }

    #[tokio::test]
    async fn memory_delete_asks_first_and_the_panel_comes_back() {
        let mut app = chat_app();
        let mem = Memory { id: 7, fact: "likes tea".into(), category: "prefs".into(), importance: 5, ..Default::default() };
        let panel = || Overlay::Memory { data: vec![mem.clone()], cats: vec!["prefs".into()], total: 1, sel: 0, q: "tea".into(), cat: 1, typing: false };
        app.overlay = Some(panel());
        type_slow(&mut app, "d").await;
        assert!(matches!(&app.overlay, Some(Overlay::Confirm { title, act: Act::DeleteMemory(7), .. }) if title.contains("likes tea")));
        assert!(app.toasts.is_empty(), "nothing deleted yet");
        type_slow(&mut app, "n").await;
        assert!(matches!(&app.overlay, Some(Overlay::Memory { q, cat: 1, data, .. }) if q == "tea" && data.len() == 1), "back to the panel as it was");
        type_slow(&mut app, "d").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(tried(&app), "Enter deletes");
        assert!(matches!(&app.overlay, Some(Overlay::Memory { q, .. }) if q == "tea"));
        // a / t come back the same way; Ctrl+K away from the question leaves the panel closed
        type_slow(&mut app, "a").await;
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert!(matches!(app.overlay, Some(Overlay::Memory { .. })));
        type_slow(&mut app, "t").await;
        press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL).await;
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert!(app.overlay.is_none());
    }

    #[tokio::test]
    async fn calendars_enter_adds_the_calendar_typed() {
        let mut app = chat_app();
        app.open_calendars().await;
        // (with calendars connected their Disconnect buttons come first; it opens on Name anyway)
        if let Some(Overlay::Form { form, .. }) = &mut app.overlay {
            form.items.insert(1, crate::forms::Item { key: "rm:x".into(), label: "Disconnect X".into(), hint: String::new(), field: crate::forms::Field::Button { label: "Disconnect X".into() } });
            form.focus_on("name");
        }
        type_slow(&mut app, "Work").await; // the form opens on its note: typing reaches Name
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        let at = |app: &App| match &app.overlay {
            Some(Overlay::Form { form, .. }) => form.items[form.focus].key.clone(),
            _ => "closed".into(),
        };
        assert_eq!(at(&app), "url", "Enter in a filled-in name goes on to the link");
        type_slow(&mut app, "/tmp/x.ics").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(tried(&app), "Enter in the link adds it");
        assert_eq!(at(&app), "url", "and the form stays (it reopens with the calendar listed when the server takes it)");
    }

    #[tokio::test]
    async fn the_palette_starts_on_the_best_match() {
        let mut app = chat_app();
        app.open_palette();
        type_slow(&mut app, "Calendars").await;
        let picked = |app: &App| match &app.overlay {
            Some(Overlay::Palette { items, sel, .. }) => items.get(*sel).map(|i| i.title.clone()).unwrap_or_default(),
            _ => String::new(),
        };
        assert_eq!(picked(&app), "Calendars", "not Settings, whose description mentions calendars");
        press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
        assert_eq!(picked(&app), "Settings", "the arrows still move, without searching again");
    }

    #[tokio::test]
    async fn the_drawers_refresh_row_refreshes() {
        let mut app = chat_app();
        app.cfg.files = true;
        app.focus = Focus::Files;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(tried(&app), "Enter on ↻ refresh lists the folder again");
    }

    #[tokio::test]
    async fn clicks_land_on_what_was_drawn() {
        let mut app = approval_app();
        app.trace = Trace::empty();
        let c2 = app.side_rows.iter().position(|r| matches!(r, SideRow::Chat(c) if c == "c2")).unwrap();
        app.side_row_map = HashMap::from([(10, c2)]); // left from a frame that had the sidebar
        let click = |x| TermEvent::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: 10, modifiers: KeyModifiers::NONE });
        app.last_size = (80, 30);
        app.drawn_cols = (0, 0); // 80 columns: the sidebar hides while the message box has the focus
        app.on_input(Instant::now(), click(5), None).await;
        assert_eq!((app.chat_id.as_deref(), app.focus), (Some("c1"), Focus::Composer));
        app.drawn_cols = (30, 0);
        app.on_input(Instant::now() + Duration::from_secs(1), click(5), None).await;
        assert_eq!(app.chat_id.as_deref(), Some("c2"), "where the sidebar was drawn a click opens the row");
        // below 90 columns only the focused one of sidebar and drawer is drawn
        app.cfg.files = true;
        app.focus = Focus::Files;
        assert_eq!(app.pane_cols(80), (0, 32));
        app.focus = Focus::Composer;
        assert_eq!(app.pane_cols(80), (0, 0));
        assert_eq!(app.pane_cols(140), (30, 32));
    }

    #[test]
    fn a_deleted_agents_filter_goes() {
        let mut app = chat_app();
        app.filter = "ghost".into();
        app.cfg.filter = "ghost".into();
        app.drop_missing_filter();
        assert_eq!((app.filter.as_str(), app.cfg.filter.as_str()), ("", ""));
        app.filter = "a1".into();
        app.drop_missing_filter();
        assert_eq!(app.filter, "a1");
    }

    #[tokio::test]
    async fn the_compaction_count_follows_the_chat() {
        let mut app = chat_app();
        let ev = |kind: &str, data: Value| Event { kind: kind.into(), data };
        let comp = |upto: u64, at: f64| json!({ "upto": upto, "at": at, "summary": "s", "reason": "auto", "before_tokens": 900, "after_tokens": 90 });
        let chat = |comps: Vec<Value>| json!({ "id": "c1", "agent_id": "a1", "created": 100.0, "messages": [{ "role": "user", "content": "a" }, { "role": "assistant", "content": "b" }, { "role": "user", "content": "c" }], "compactions": comps });
        let count = |app: &App| app.chat.as_ref().and_then(|c| c["compactions"].as_array()).map_or(0, Vec::len);
        // joining a run: the snapshot holds its first compaction, then the run's events replay it
        app.on_chat_event(ev("snapshot", json!({ "chat": chat(vec![comp(2, 1.5)]), "running": true, "run_base": 3 }))).await;
        assert_eq!(count(&app), 1);
        app.on_chat_event(ev("compact_end", json!({ "path": [], "compaction": comp(2, 1.5) }))).await;
        assert_eq!(count(&app), 1, "a replayed compaction isn't counted twice");
        // a new one mid-run counts at once; a sub-agent's (it has a path) belongs to its own chat
        app.on_chat_event(ev("compact_end", json!({ "path": ["t1"], "compaction": comp(1, 2.5) }))).await;
        assert_eq!(count(&app), 1);
        app.on_chat_event(ev("compact_end", json!({ "path": [], "compaction": comp(3, 3.5) }))).await;
        assert_eq!(count(&app), 2);
        assert_eq!(trace::footer(app.chat.as_ref().unwrap(), 145.0).as_deref(), Some("compacted 2× · going for 45s"));
        // a chat without the list yet gets one
        app.chat.as_mut().unwrap().as_object_mut().unwrap().remove("compactions");
        app.on_chat_event(ev("compact_end", json!({ "path": [], "compaction": comp(3, 3.5) }))).await;
        assert_eq!(count(&app), 1);
        // the chat as the server sends it wins: a regenerate dropped the ones past it, …
        app.on_chat_event(ev("run_start", json!({ "chat": chat(vec![comp(2, 1.5)]), "run_base": 3, "agent_id": "a1" }))).await;
        assert_eq!(count(&app), 1);
        // … an edit all of them
        app.on_chat_event(ev("done", json!({ "chat": chat(vec![]) }))).await;
        assert_eq!(count(&app), 0);
        assert_eq!(trace::footer(app.chat.as_ref().unwrap(), 340.0).as_deref(), Some("not compacted yet · going for 4m"));
    }

    #[tokio::test]
    async fn context_and_usage_say_how_long_the_chat_has_gone_and_keep_counting() {
        let mut app = chat_app();
        let now = trace::now();
        app.chat = Some(json!({ "id": "c1", "agent_id": "a1", "created": now - 125.0, "messages": [{ "role": "user", "content": "hi" }], "compactions": [{}, {}] }));
        let shown = |app: &App| match &app.overlay {
            Some(Overlay::Info { lines, .. }) => lines.iter().map(|l| l.spans.iter().map(|s| s.content.to_string()).collect::<String>()).collect::<Vec<_>>().join("\n"),
            _ => String::new(),
        };
        let at = trace::clock(Some(now - 125.0));
        for name in ["usage", "context"] {
            app.run_slash(commands::find(name).unwrap(), "").await;
            assert!(shown(&app).contains(&format!("compacted 2× · started {at} (going for 2m)")), "/{name}: {}", shown(&app));
            // an open one keeps counting, where it was scrolled to
            if let Some(Overlay::Info { scroll, .. }) = &mut app.overlay {
                *scroll = 1;
            }
            app.chat.as_mut().unwrap()["created"] = json!(now - 3700.0);
            app.tick();
            assert!(shown(&app).contains("(going for 1h 1m)"), "/{name}: {}", shown(&app));
            assert!(matches!(&app.overlay, Some(Overlay::Info { title, scroll: 1, .. }) if *title == format!("/{name}")));
            app.chat.as_mut().unwrap()["created"] = json!(now - 125.0);
            app.overlay = None;
        }
        // other panels are left alone
        app.overlay = Some(app.help_info());
        app.tick();
        assert!(matches!(&app.overlay, Some(Overlay::Info { title, .. }) if title == "/help"));
    }

    #[test]
    fn toasts_merge_repeats_and_keep_the_newest_few() {
        let t0 = Instant::now();
        let mut list = vec![];
        for i in 0..5 {
            push_toast(&mut list, "Queued: the agent reads it at its next step".into(), false, t0 + Duration::from_millis(i));
        }
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].count, 5);
        assert_eq!(list[0].at, t0 + Duration::from_millis(4), "a repeat restarts the timer");
        // same text but an error is a different toast
        push_toast(&mut list, "Queued: the agent reads it at its next step".into(), true, t0);
        assert_eq!(list.len(), 2);
        for i in 0..6 {
            push_toast(&mut list, format!("toast {i}"), false, t0);
        }
        assert_eq!(list.len(), TOASTS_SHOWN);
        let texts: Vec<&str> = list.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["toast 3", "toast 4", "toast 5"]);
        // only consecutive repeats merge
        push_toast(&mut list, "toast 4".into(), false, t0);
        assert_eq!(list.len(), TOASTS_SHOWN);
        assert_eq!(list.last().unwrap().count, 1);
    }

    // ------------------------------------------------------- server events, end to end

    fn lines_of(t: &Trace, verbose: bool) -> Vec<String> {
        let th = Theme::dark();
        let r = trace::render(t, &trace::RenderOpts { theme: &th, width: 80, verbose, selected_turn: None });
        r.lines.iter().map(|l| l.spans.iter().map(|s| s.content.to_string()).collect()).collect()
    }

    /// The times a live trace takes from the clock: when each turn started (now, where the saved
    /// chat has its first reply's time) and how long each thought took (measured as the deltas
    /// arrive, and a replay here arrives all at once). Everything else must match.
    fn untimed(mut t: Trace) -> Trace {
        fn steps(list: &mut [trace::Step]) {
            for s in list {
                match s {
                    trace::Step::Thinking { secs, started, .. } => (*secs, *started) = (None, None),
                    trace::Step::Tool(t) => {
                        if let Some(sub) = &mut t.sub {
                            steps(&mut sub.steps);
                        }
                    }
                    _ => {}
                }
            }
        }
        for e in t.entries.iter_mut() {
            if let Entry::Turn(turn) = e {
                turn.ts = None;
                steps(&mut turn.steps);
            }
        }
        t
    }

    fn chat_event(app: &App, ev: &Value) -> Incoming {
        Incoming::Chat { chat_id: app.chat_id.clone().unwrap(), visit: app.stream_gen, event: Event { kind: ev["type"].as_str().unwrap().into(), data: ev.clone() } }
    }

    /// An app watching the chat of a captured run (crate::api::tests::captured) that has been
    /// sent every event of it but `done`; and that `done` event.
    async fn replayed(name: &str) -> (App, Value) {
        let fx = crate::api::tests::captured();
        let events = fx["runs"][name].as_array().unwrap().clone();
        let done = events.iter().find(|e| e["type"] == "done").cloned().expect("a done event");
        let mut app = chat_app();
        app.bell = false;
        app.agents = serde_json::from_value(fx["state"]["agents"].clone()).unwrap();
        app.chat_id = done["chat"]["id"].as_str().map(String::from);
        for ev in events.iter().filter(|e| e["type"] != "done") {
            let inc = chat_event(&app, ev);
            app.on_incoming(inc).await;
        }
        (app, done)
    }

    #[tokio::test]
    async fn a_run_seen_live_draws_what_its_saved_chat_draws() {
        // real runs: a handoff whose agent runs a command after an approval, a denied command,
        // hostile text in thinking and reply, and a reply that filled the context and went on
        for name in ["delegate", "deny", "verbatim", "cutoff"] {
            let (mut app, done) = replayed(name).await;
            assert!(app.running, "{name}: still running before done");
            let live = untimed(std::mem::replace(&mut app.trace, Trace::empty()));
            let rebuilt = untimed(Trace::from_chat(&done["chat"], &app.agents, None));
            for verbose in [false, true] {
                assert_eq!(lines_of(&live, verbose), lines_of(&rebuilt, verbose), "{name} (verbose {verbose}): live, then rebuilt from the saved chat");
            }
            // done: the saved chat replaces what was built live
            app.trace = live;
            let inc = chat_event(&app, &done);
            app.on_incoming(inc).await;
            assert!(!app.running && app.queue.is_empty());
            assert_eq!(lines_of(&untimed(std::mem::replace(&mut app.trace, Trace::empty())), false), lines_of(&rebuilt, false), "{name}");
        }
    }

    #[tokio::test]
    async fn joining_a_running_chat_replays_to_the_same_trace() {
        let (mut live, done) = replayed("delegate").await;
        // a second client opens the chat late: the snapshot (the chat as it is, the run's base),
        // then every event of the run so far
        let fx = crate::api::tests::captured();
        let mut late = chat_app();
        late.bell = false;
        late.agents = live.agents.clone();
        late.chat_id = live.chat_id.clone();
        let snap = json!({ "type": "snapshot", "chat": done["chat"], "running": true, "run_base": 1, "queue": [] });
        let inc = chat_event(&late, &snap);
        late.on_incoming(inc).await;
        assert!(late.running);
        for ev in fx["runs"]["delegate"].as_array().unwrap().iter().filter(|e| e["type"] != "done") {
            let inc = chat_event(&late, ev);
            late.on_incoming(inc).await;
        }
        let a = untimed(std::mem::replace(&mut live.trace, Trace::empty()));
        let b = untimed(std::mem::replace(&mut late.trace, Trace::empty()));
        assert_eq!(lines_of(&b, true), lines_of(&a, true));
        assert_eq!((late.ctx_tokens, late.tok_per_s), (live.ctx_tokens, live.tok_per_s));
    }

    #[tokio::test]
    async fn a_chat_opened_mid_run_says_how_long_its_finished_thoughts_took() {
        // the reply that filled the context had thought for a while (its saved _stats.think_s);
        // a client that opens the chat while the run goes on gets the run's events replayed in
        // one go, so it can't time the thinking itself: the server's measure is what it has
        let fx = crate::api::tests::captured();
        let events = fx["runs"]["cutoff"].as_array().unwrap();
        let first_end = events.iter().find(|e| e["type"] == "assistant_end").unwrap();
        let secs = first_end["message"]["_stats"]["think_s"].as_f64().unwrap();
        assert!(secs >= 2.0, "the capture thought for {secs} s");
        let done = events.iter().find(|e| e["type"] == "done").unwrap();
        let mut late = chat_app();
        late.agents = serde_json::from_value(fx["state"]["agents"].clone()).unwrap();
        late.chat_id = done["chat"]["id"].as_str().map(String::from);
        let snap = json!({ "type": "snapshot", "chat": done["chat"], "running": true, "run_base": 1, "queue": [] });
        for ev in std::iter::once(&snap).chain(events.iter().filter(|e| e["type"] != "done")) {
            let inc = chat_event(&late, ev);
            late.on_incoming(inc).await;
        }
        let want = format!("○ thought for {} s", secs.round() as u64);
        let lines = lines_of(&late.trace, false);
        assert!(lines.iter().any(|l| l.starts_with(&want)), "{want:?} in {lines:#?}");
    }

    #[tokio::test]
    async fn a_compaction_shows_live_then_as_a_divider() {
        let (mut app, done) = replayed("compact").await;
        let comp = done["chat"]["compactions"][0].clone();
        let (before, after) = (comp["before_tokens"].as_u64().unwrap(), comp["after_tokens"].as_u64().unwrap());
        let live = lines_of(&app.trace, false);
        let note = format!("Earlier messages summarized to free up context ({} → {} tokens).", trace::fmt_k(before), trace::fmt_k(after));
        assert!(live.iter().any(|l| l.contains(&note)), "{note:?} in {live:#?}");
        assert!(!live.iter().any(|l| l.contains("compacting")), "the progress line goes when it ends");
        assert_eq!(trace::footer(app.chat.as_ref().unwrap(), trace::now()).map(|f| f.starts_with("compacted 1×")), Some(true), "counted at once");
        let inc = chat_event(&app, &done);
        app.on_incoming(inc).await;
        let saved = lines_of(&app.trace, false);
        assert!(saved.contains(&format!("── earlier messages summarized on request · {} → {} tokens ──", trace::fmt_k(before), trace::fmt_k(after))), "{saved:#?}");
        assert!(saved.iter().all(|l| !l.contains(&note)));
        assert_eq!(trace::footer(app.chat.as_ref().unwrap(), trace::now()).map(|f| f.starts_with("compacted 1×")), Some(true));
        // while it summarizes, the trace says so and counts what it wrote
        let mut app = chat_app();
        let evs = crate::api::tests::captured()["runs"]["compact"].clone();
        for ev in evs.as_array().unwrap().iter().take_while(|e| e["type"] != "compact_end") {
            let inc = chat_event(&app, ev);
            app.on_incoming(inc).await;
        }
        let n = evs.as_array().unwrap().iter().filter(|e| e["kind"] == "compact").map(|e| e["text"].as_str().unwrap().len()).sum::<usize>();
        assert!(lines_of(&app.trace, false).iter().any(|l| l.contains(&format!("compacting: summarizing older messages… {} chars", trace::fmt_k(n as u64)))));
    }

    #[tokio::test]
    async fn every_chat_event_lands_where_it_belongs() {
        let mut app = chat_app();
        app.bell = false;
        async fn send(app: &mut App, kind: &str, mut data: Value) {
            data["type"] = json!(kind);
            let inc = chat_event(app, &data);
            app.on_incoming(inc).await;
        }
        let chat = json!({ "id": "c1", "agent_id": "a1", "messages": [{ "role": "user", "content": "go" }], "stats": { "context_tokens": 500, "tok_per_s": 7.5 } });
        send(&mut app, "snapshot", json!({ "chat": chat, "running": true, "run_base": 1, "queue": [{ "id": "q1", "content": "waiting" }] })).await;
        assert!(app.running && app.queue.len() == 1);
        assert_eq!((app.ctx_tokens, app.tok_per_s), (Some(500), Some(7.5)), "the snapshot's stats");
        send(&mut app, "assistant_start", json!({ "path": [], "agent_id": "a1" })).await;
        send(&mut app, "delta", json!({ "path": [], "kind": "content", "text": "first" })).await;
        // the meter is the open chat's own: a sub-agent's (with a path) is its chat's
        send(&mut app, "ctx", json!({ "path": [], "tokens": 900 })).await;
        send(&mut app, "ctx", json!({ "path": ["x"], "tokens": 5 })).await;
        send(&mut app, "assistant_end", json!({ "path": ["x"], "message": { "content": "", "_stats": { "prompt_tokens": 1, "tok_per_s": 1.0 } } })).await;
        assert_eq!((app.ctx_tokens, app.tok_per_s), (Some(900), Some(7.5)));
        send(&mut app, "assistant_end", json!({ "path": [], "message": { "role": "assistant", "content": "first", "_stats": { "prompt_tokens": 1000, "completion_tokens": 24, "tok_per_s": 30.5 } } })).await;
        assert_eq!((app.ctx_tokens, app.tok_per_s), (Some(1024), Some(30.5)), "prompt and completion of the last reply");
        send(&mut app, "queue", json!({ "items": [] })).await;
        assert!(app.queue.is_empty());
        // a message delivered mid-run starts the next turn
        send(&mut app, "user_message", json!({ "path": [], "message": { "role": "user", "content": "aside", "_queued": true }, "index": 2 })).await;
        send(&mut app, "assistant_start", json!({ "path": [], "agent_id": "a1" })).await;
        send(&mut app, "delta", json!({ "path": [], "kind": "content", "text": "second" })).await;
        for kind in ["subagent_end", "stopped", "mystery"] {
            send(&mut app, kind, json!({ "path": ["nope"] })).await;
        }
        send(&mut app, "error", json!({ "message": "boom" })).await;
        let shape: Vec<String> = app.trace.entries.iter().map(|e| match e {
            Entry::User(u) => format!("user {} {}{}", u.index, u.text, if u.queued { " (queued)" } else { "" }),
            Entry::Turn(t) => format!("turn {} {}", t.start, t.steps.len()),
            Entry::Divider { .. } => "divider".into(),
        }).collect();
        assert_eq!(shape, ["user 0 go", "turn 1 1", "user 2 aside (queued)", "turn 3 2"]);
        let Some(Entry::Turn(t)) = app.trace.entries.last() else { panic!() };
        assert!(matches!(&t.steps[..], [trace::Step::Text { text, live: true }, trace::Step::Error { text: e, .. }] if text == "second" && e == "boom"), "{:?}", t.steps);
        // done: the chat as saved, nothing running or queued
        app.queue = vec![json!({ "content": "x" })];
        send(&mut app, "done", json!({ "chat": { "id": "c1", "agent_id": "a1", "messages": [{ "role": "user", "content": "go" }, { "role": "assistant", "content": "ok" }] } })).await;
        assert!(!app.running && app.queue.is_empty());
        assert_eq!(app.trace.entries.len(), 2);
        // a different chat's events never touch this one
        let before = lines_of(&app.trace, true);
        app.on_incoming(Incoming::Chat { chat_id: "other".into(), visit: app.stream_gen, event: Event { kind: "delta".into(), data: json!({ "path": [], "kind": "content", "text": "NOT HERE" }) } }).await;
        assert_eq!(lines_of(&app.trace, true), before);
    }

    #[tokio::test]
    async fn global_events_update_statuses_titles_drafts_and_memory() {
        let mut app = chat_app();
        app.bell = false;
        app.chats.push(ChatSummary { id: "c2".into(), title: "two".into(), agent_id: "a1".into(), status: "idle".into(), ..Default::default() });
        let global = |kind: &str, data: Value| Incoming::Global(Event { kind: kind.into(), data });
        app.on_incoming(global("chat_status", json!({ "chat_id": "c2", "status": "waiting" }))).await;
        assert_eq!((app.chats[1].status.as_str(), app.statuses.get("c2").map(String::as_str)), ("waiting", Some("waiting")));
        assert_eq!(app.state_word("a1"), "needs you");
        app.on_incoming(global("chat_status", json!({ "chat_id": "c2" }))).await;
        assert_eq!(app.chats[1].status, "idle", "no status reads as idle");
        app.on_incoming(global("chat_title", json!({ "chat_id": "c1", "title": "Named by the model" }))).await;
        assert_eq!((app.chats[0].title.as_str(), app.chat.as_ref().unwrap()["title"].as_str()), ("Named by the model", Some("Named by the model")));
        app.on_incoming(global("chat_title", json!({ "chat_id": "c2", "title": "Other" }))).await;
        assert_eq!(app.chat.as_ref().unwrap()["title"], "Named by the model", "another chat's title stays there");
        // queued messages a run ended without: into the box of the open chat (escaped, ahead of
        // what was typed), or kept as the other chat's draft
        app.set_composer("typed");
        app.on_incoming(global("queue_returned", json!({ "chat_id": "c1", "text": "/hi\n\nsecond" }))).await;
        assert_eq!(app.composer_text(), "//hi\n\nsecond\n\ntyped");
        assert!(app.toasts.last().unwrap().text.contains("back in the message box"));
        app.on_incoming(global("queue_returned", json!({ "chat_id": "c2", "text": "one" }))).await;
        app.on_incoming(global("queue_returned", json!({ "chat_id": "c2", "text": "two" }))).await;
        assert_eq!(app.drafts.get("c2").map(String::as_str), Some("two\n\none"));
        // memory: working or not, and who learned what
        app.on_incoming(global("memory_status", json!({ "state": "working" }))).await;
        assert!(app.memory_working);
        app.on_incoming(global("memory_status", json!({ "state": "idle" }))).await;
        assert!(!app.memory_working);
        app.toasts.clear();
        for (data, want) in [
            (json!({ "by": "auto", "added": ["likes tea"], "updated": ["lives in Abu Dhabi"] }), Some("Remembered: likes tea (+1 more)")),
            (json!({ "by": "auto", "added": [] }), None),
            (json!({ "by": "Coder", "summary": "Remembered: uses vim" }), Some("Coder: Remembered: uses vim")),
            (json!({ "by": "you" }), None),
            (json!({ "by": "tidy", "added": ["x"] }), None),
        ] {
            app.toasts.clear();
            app.on_incoming(global("memory_changed", data.clone())).await;
            assert_eq!(app.toasts.last().map(|t| t.text.as_str()), want, "{data}");
        }
        app.on_incoming(global("mystery", json!({}))).await;
        app.on_incoming(global("routines_changed", json!({}))).await;
        assert!(app.overlay.is_none(), "routines reload only while their panel is open");
    }

    // ----------------------------------------------------------------- the sidebar

    /// Noon `days` ago, local time (clear of midnight and of daylight-saving jumps).
    fn noon(days: i64) -> f64 {
        let d = chrono::Local::now().date_naive() - chrono::Duration::days(days);
        d.and_hms_opt(12, 0, 0).unwrap().and_local_timezone(chrono::Local).earliest().unwrap().timestamp() as f64
    }

    #[test]
    fn chats_group_by_pin_and_day_and_filter_by_agent_or_text() {
        assert_eq!([0, 1, 3, 10, 40].map(|d| day_group(noon(d))), ["Today", "Yesterday", "This week", "This month", "Older"]);
        assert_eq!(day_group(trace::now() + 86_400.0), "Today", "a clock ahead of ours is still today");
        let now = trace::now();
        assert_eq!([now - 30.0, now - 150.0, now - 7300.0, now - 2.5 * 86_400.0].map(time_ago), ["just now", "2 min ago", "2 h ago", "2 d ago"]);
        assert!(time_ago(noon(40)).chars().next().unwrap().is_ascii_alphabetic(), "a date once it's a week old");
        let mut app = chat_app();
        app.agents.push(Agent { id: "a2".into(), name: "Second".into(), ..Default::default() });
        let chat = |id: &str, agent: &str, title: &str, updated: f64| ChatSummary { id: id.into(), title: title.into(), agent_id: agent.into(), updated, status: "idle".into(), ..Default::default() };
        app.chats = vec![
            ChatSummary { pinned: true, ..chat("p", "a1", "pinned one", noon(40)) },
            chat("t", "a1", "today's", now),
            chat("y", "a2", "yesterday's", noon(1)),
            chat("o", "a2", "old", noon(40)),
            ChatSummary { parent: Some(crate::api::Parent { root_chat_id: "t".into(), caller_id: "a1".into() }), preview: "sub work".into(), ..chat("s", "a2", "a sub-chat", now) },
        ];
        app.rebuild_sidebar();
        let rows = |app: &App| app.side_rows.iter().map(|r| match r {
            SideRow::AllAgents => "all".to_string(),
            SideRow::Agent(a) => format!("agent {a}"),
            SideRow::Group(g) => format!("[{g}]"),
            SideRow::Chat(c) => c.clone(),
        }).collect::<Vec<_>>();
        assert_eq!(rows(&app), ["all", "agent a1", "agent a2", "[Pinned]", "p", "[Today]", "t", "[Yesterday]", "y", "[Older]", "o"], "sub-chats only under their agent");
        app.filter = "a2".into();
        app.rebuild_sidebar();
        assert_eq!(rows(&app)[3..], ["[Yesterday]", "y", "[Older]", "o", "[Today]", "s"], "an agent's own sub-chats show under it (in the order the server lists them)");
        // text (title or preview) wins over the agent filter
        app.search = "SUB WORK".into();
        app.rebuild_sidebar();
        assert_eq!(rows(&app)[3..], ["[Matches]", "s"]);
        app.search = "zzz".into();
        app.rebuild_sidebar();
        assert_eq!(rows(&app).len(), 3);
        app.search.clear();
        app.filter.clear();
        app.rebuild_sidebar();
        // moving skips the group headings; it stops at either end
        app.side_sel = 4; // "p"
        app.side_move(-1);
        assert_eq!(rows(&app)[app.side_sel], "agent a2");
        app.side_move(1);
        app.side_move(1);
        assert_eq!(rows(&app)[app.side_sel], "t");
        for _ in 0..20 {
            app.side_move(1);
        }
        assert_eq!(rows(&app)[app.side_sel], "o");
        for _ in 0..20 {
            app.side_move(-1);
        }
        assert_eq!(app.side_sel, 0);
        // a shrinking list keeps the selection on a row
        app.side_sel = 10;
        app.chats.truncate(1);
        app.rebuild_sidebar();
        assert!(app.side_sel < app.side_rows.len());
    }

    #[test]
    fn unread_and_busy_words_follow_the_chats() {
        let mut app = chat_app();
        app.agents.push(Agent { id: "a2".into(), name: "Helper".into(), ..Default::default() });
        let base = ChatSummary { id: "u".into(), title: "t".into(), agent_id: "a1".into(), updated: 100.0, last_role: Some("assistant".into()), status: "idle".into(), ..Default::default() };
        app.cfg.seen.insert("u".into(), 50.0);
        assert!(app.is_unread(&base));
        app.cfg.seen.insert("u".into(), 99.5);
        assert!(!app.is_unread(&base), "within a second of being seen");
        app.cfg.seen.clear();
        for (c, why) in [
            (ChatSummary { last_role: Some("user".into()), ..base.clone() }, "your own message last"),
            (ChatSummary { status: "running".into(), ..base.clone() }, "still working"),
            (ChatSummary { id: "c1".into(), ..base.clone() }, "the open chat"),
            (ChatSummary { parent: Some(Default::default()), ..base.clone() }, "a sub-chat"),
        ] {
            assert!(!app.is_unread(&c), "{why}");
        }
        assert_eq!(app.state_word("a1"), "");
        app.chats = vec![
            ChatSummary { status: "delegated".into(), ..base.clone() },
            ChatSummary { id: "s".into(), agent_id: "a2".into(), status: "running".into(), parent: Some(crate::api::Parent { root_chat_id: "u".into(), caller_id: "a1".into() }), ..base.clone() },
        ];
        assert_eq!((app.state_word("a1"), app.state_word("a2")), ("→ Helper".to_string(), "working".to_string()));
        assert_eq!(app.busy_state(None), Some("running"));
        app.chats[1].status = "waiting".into();
        assert_eq!(app.busy_state(None), Some("waiting"), "waiting outranks the rest");
        app.chats[1].status = "idle".into();
        assert_eq!(app.state_word("a1"), "waiting", "delegated, but the helper isn't on it any more");
    }

    // --------------------------------------------------------------- overlays and keys

    #[tokio::test]
    async fn every_panel_moves_scrolls_and_closes_with_its_keys() {
        let mut app = chat_app();
        let k = |c: KeyCode| (c, KeyModifiers::NONE);
        // help: arrows, pages, ends; Esc closes
        app.overlay = Some(Overlay::Help { scroll: 0 });
        for (code, want) in [(KeyCode::Down, 1), (KeyCode::Char('j'), 2), (KeyCode::PageDown, 12), (KeyCode::Up, 11), (KeyCode::PageUp, 1), (KeyCode::Home, 0), (KeyCode::End, usize::MAX / 2), (KeyCode::Char('x'), usize::MAX / 2)] {
            press(&mut app, code, KeyModifiers::NONE).await;
            assert!(matches!(app.overlay, Some(Overlay::Help { scroll }) if scroll == want), "{code:?}");
        }
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert!(app.overlay.is_none());
        // an info panel: scrolls, Space closes
        app.overlay = Some(app.help_info());
        press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
        press(&mut app, KeyCode::PageDown, KeyModifiers::NONE).await;
        assert!(matches!(app.overlay, Some(Overlay::Info { scroll: 11, .. })));
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE).await;
        assert!(app.overlay.is_none());
        // the file viewer stops at its last line
        let lines: Vec<String> = (0..30).map(|i| format!("l{i}")).collect();
        app.overlay = Some(Overlay::Viewer { title: "f".into(), lines, scroll: 0 });
        for (code, want) in [(KeyCode::Down, 1), (KeyCode::PageDown, 21), (KeyCode::PageDown, 29), (KeyCode::Down, 29), (KeyCode::PageUp, 9), (KeyCode::Home, 0), (KeyCode::End, 29), (KeyCode::Up, 28)] {
            press(&mut app, code, KeyModifiers::NONE).await;
            assert!(matches!(app.overlay, Some(Overlay::Viewer { scroll, .. }) if scroll == want), "{code:?}");
        }
        press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE).await;
        assert_eq!(app.toasts.last().map(|t| t.text.as_str()), Some("Copied the file"));
        press(&mut app, KeyCode::Char('q'), KeyModifiers::NONE).await;
        assert!(app.overlay.is_none());
        // the agent picker: no further than its list; a digit past it does nothing
        app.agents.push(Agent { id: "a2".into(), name: "Two".into(), ..Default::default() });
        app.overlay = Some(Overlay::Pick { sel: 0 });
        for (code, want) in [k(KeyCode::Down), k(KeyCode::Tab), k(KeyCode::Up), k(KeyCode::Char('0')), k(KeyCode::Char('3')), k(KeyCode::Char('j'))].iter().zip([1, 1, 0, 0, 0, 1]) {
            press(&mut app, code.0, code.1).await;
            assert!(matches!(app.overlay, Some(Overlay::Pick { sel }) if sel == want), "{code:?}");
        }
        app.toasts.clear();
        press(&mut app, KeyCode::Char('2'), KeyModifiers::NONE).await;
        assert!(tried(&app), "a digit starts a chat with that agent");
        // the model list
        app.overlay = Some(Overlay::Models { agent: "a1".into(), items: vec!["".into(), "m".into()], sel: 0 });
        for (code, want) in [(KeyCode::Down, 1), (KeyCode::Down, 1), (KeyCode::Char('k'), 0), (KeyCode::Up, 0)] {
            press(&mut app, code, KeyModifiers::NONE).await;
            assert!(matches!(app.overlay, Some(Overlay::Models { sel, .. }) if sel == want), "{code:?}");
        }
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert!(app.overlay.is_none());
        // a confirmation: n (or Esc) cancels and does nothing
        app.toasts.clear();
        app.overlay = Some(Overlay::Confirm { title: "Delete?".into(), text: String::new(), ok: "Delete".into(), act: Act::DeleteChat("c1".into()) });
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE).await;
        assert!(matches!(app.overlay, Some(Overlay::Confirm { .. })), "another key keeps it open");
        press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE).await;
        assert!(app.overlay.is_none() && app.toasts.is_empty());
    }

    #[tokio::test]
    async fn the_routines_panel_opens_editors_and_comes_back_after_a_delete_question() {
        let mut app = chat_app();
        let r = |id: &str, name: &str, sched: Value| Routine { id: id.into(), name: name.into(), agent_id: "a1".into(), prompt: "p".into(), schedule: sched, enabled: true, ..Default::default() };
        let items = vec![r("r1", "Daily", json!({ "type": "daily", "time": "07:15", "days": [5, 6] })), r("r2", "Often", json!({ "type": "interval", "minutes": 120 }))];
        app.overlay = Some(Overlay::Routines { items: items.clone(), sel: 0 });
        press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
        press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
        assert!(matches!(app.overlay, Some(Overlay::Routines { sel: 1, .. })));
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        let Some(Overlay::Form { form, kind: FormKind::Routine(Some(id)) }) = &app.overlay else { panic!("the editor") };
        assert_eq!((id.as_str(), form.title.as_str()), ("r2", "Edit routine “Often”"));
        assert_eq!((form.string("type"), form.string("minutes"), form.string("time"), form.boolean("day:0")), ("interval".into(), "120".into(), "08:00".into(), true), "an interval keeps the daily fields' defaults");
        assert!(form.get("template").is_none(), "no templates when editing");
        app.overlay = Some(Overlay::Routines { items: items.clone(), sel: 0 });
        press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE).await;
        let Some(Overlay::Form { form, .. }) = &app.overlay else { panic!() };
        assert_eq!((form.string("time"), form.boolean("day:5"), form.boolean("day:0"), form.boolean("enabled")), ("07:15".into(), true, false, true));
        // a new one starts from a blank sheet or a template, on every day at 08:00
        app.overlay = Some(Overlay::Routines { items: vec![], sel: 0 });
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        let Some(Overlay::Form { form, kind: FormKind::Routine(None) }) = &app.overlay else { panic!("an empty list's Enter makes one") };
        assert_eq!((form.string("template"), form.string("agent_id"), form.string("time")), ("".into(), "a1".into(), "08:00".into()));
        assert!((0..7).all(|d| form.boolean(&format!("day:{d}"))));
        // delete asks first; answering brings the panel back (here: its reload fails, and says so)
        app.overlay = Some(Overlay::Routines { items, sel: 1 });
        press(&mut app, KeyCode::Char('d'), KeyModifiers::NONE).await;
        assert!(matches!(&app.overlay, Some(Overlay::Confirm { title, act: Act::DeleteRoutine(id), .. }) if title.contains("Often") && id == "r2"));
        app.toasts.clear();
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert!(tried(&app) && !app.routines_return, "it went back for the panel");
        for (name, agent, sched, prompt) in routine_templates() {
            assert!(["assistant", "mail", "planner", "researcher"].contains(&agent), "{name}: a starter agent");
            assert!(crate::ui::schedule_text(&sched).contains(" at "), "{name}");
            assert!(!prompt.is_empty());
        }
    }

    #[tokio::test]
    async fn the_palette_filters_as_you_type_and_runs_the_pick() {
        let mut app = chat_app();
        app.open_palette();
        let titles = |app: &App| match &app.overlay {
            Some(Overlay::Palette { items, .. }) => items.iter().map(|i| i.title.clone()).collect::<Vec<_>>(),
            _ => vec![],
        };
        assert!(titles(&app).contains(&"New Assistant chat".to_string()) && titles(&app).contains(&"chat".to_string()) && titles(&app).contains(&"Delete chat".to_string()));
        type_slow(&mut app, "shortc").await;
        assert_eq!(titles(&app), ["Keyboard shortcuts"]);
        press(&mut app, KeyCode::Backspace, KeyModifiers::NONE).await;
        assert!(matches!(&app.overlay, Some(Overlay::Palette { q, .. }) if q == "short"));
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(matches!(app.overlay, Some(Overlay::Help { .. })), "Enter runs it");
        app.open_palette();
        type_slow(&mut app, "zzzz").await;
        assert!(titles(&app).is_empty());
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.overlay.is_none(), "nothing to run: it just closes");
        // a sub-agent's chat has no "This chat" commands
        app.chat = Some(json!({ "id": "c1", "agent_id": "a1", "parent": { "root_chat_id": "r", "caller_id": "a1" } }));
        app.open_palette();
        assert!(!titles(&app).contains(&"Delete chat".to_string()));
        // a paste goes into the search
        app.paste("Keyboard\n").await;
        assert_eq!(titles(&app), ["Keyboard shortcuts"]);
    }

    #[test]
    fn the_agent_editor_reads_back_what_was_entered() {
        let mut app = chat_app();
        app.tools = vec![ToolInfo { name: "run_shell".into(), label: "Run".into(), group: "Shell".into(), danger: true }, ToolInfo { name: "read_file".into(), label: "Read".into(), group: "Files".into(), danger: false }];
        app.agents[0] = Agent { id: "a1".into(), name: "Assistant".into(), emoji: "💬".into(), color: "#112233".into(), tools: vec!["read_file".into()], delegate_all: true, confirm_shell: true, memory: true, temperature: Some(0.2), model: "gone-model".into(), ..Default::default() };
        app.agents.push(Agent { id: "a2".into(), name: "Two".into(), ..Default::default() });
        app.server.models = vec!["m1".into()];
        app.open_agent_editor(Some("a1".into()));
        let Some(Overlay::Form { form, kind: FormKind::Agent(Some(id)) }) = &mut app.overlay else { panic!() };
        assert_eq!((id.as_str(), form.title.as_str()), ("a1", "Edit Assistant"));
        assert_eq!((form.string("model"), form.string("temperature"), form.boolean("tool:read_file"), form.boolean("tool:run_shell"), form.boolean("del:a2")), ("gone-model".into(), "0.2".into(), true, false, true), "a model the server doesn't list stays selectable");
        assert!(form.get("del:a1").is_none(), "no switch for talking to itself");
        assert!(form.items.iter().any(|i| i.key == "duplicate") && form.items.iter().any(|i| i.key == "delete"));
        form.set_text("name", "  Renamed  ");
        form.set_text("emoji", " ");
        form.set_text("color", "#zzzzzz");
        form.set_text("temperature", "");
        form.set_bool("tool:run_shell", true);
        form.set_bool("delegate_all", false);
        let form = match app.overlay.take() { Some(Overlay::Form { form, .. }) => form, _ => unreachable!() };
        let body = app.agent_from_form(&form);
        assert_eq!(body, json!({
            "name": "Renamed", "emoji": "🤖", "color": "#7c6cff", "purpose": "", "system_prompt": "",
            "tools": ["run_shell", "read_file"], "delegate_all": false, "delegates": ["a2"], "model": "gone-model",
            "temperature": null, "workspace": "", "confirm_shell": true, "memory": true,
        }));
        // a new agent: the defaults, no duplicate or delete
        app.open_agent_editor(None);
        let Some(Overlay::Form { form, kind: FormKind::Agent(None) }) = &app.overlay else { panic!() };
        assert_eq!((form.title.as_str(), form.string("emoji"), form.string("color"), form.boolean("delegate_all"), form.boolean("confirm_shell"), form.boolean("memory")), ("New agent", "🤖".into(), "#7c6cff".into(), true, true, true));
        assert!(form.get("delete").is_none());
        let mut form = match app.overlay.take() { Some(Overlay::Form { form, .. }) => form, _ => unreachable!() };
        form.set_text("temperature", "0.9");
        form.set_text("color", " #AbCdEf ");
        let body = app.agent_from_form(&form);
        assert_eq!((body["temperature"].clone(), body["color"].clone(), body["delegates"].clone()), (json!(0.9), json!("#AbCdEf"), json!([])));
    }

    #[tokio::test]
    async fn settings_open_with_the_servers_values() {
        let mut app = chat_app();
        app.settings = json!({ "base_url": "http://x/v1", "api_key": "k", "model": "pinned", "context_size": 4096, "auto_compact": true, "compact_at": 70, "max_steps": 30, "shell_timeout": 180, "bypass_approvals": true, "browser_headless": false });
        app.server = ServerInfo { ok: true, models: vec!["m1".into()], n_ctx: Some(8192), kind: "llama".into(), ..Default::default() };
        app.version = "0.2.0".into();
        app.run_slash(commands::find("settings").unwrap(), "").await;
        let Some(Overlay::Form { form, kind: FormKind::Settings }) = &app.overlay else { panic!() };
        for (k, v) in [("base_url", "http://x/v1"), ("api_key", "k"), ("model", "pinned"), ("context_size", "4096"), ("compact_at", "70"), ("max_steps", "30"), ("shell_timeout", "180"), ("search_url", "")] {
            assert_eq!(form.string(k), v, "{k}");
        }
        assert!(form.boolean("auto_compact") && form.boolean("bypass_approvals") && !form.boolean("browser_headless") && !form.boolean("auto_title"));
        assert!(form.danger_keys.contains(&"bypass_approvals".to_string()));
        assert!(form.items.iter().any(|i| i.label.contains("connected (llama-server), 1 model(s), context 8192") && i.label.contains("Agent Chat 0.2.0")));
    }

    // ----------------------------------------------------------- composer and trace keys

    #[tokio::test]
    async fn the_message_box_keys() {
        let mut app = chat_app();
        // Enter on nothing (or only spaces) sends nothing
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        type_slow(&mut app, "   ").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.toasts.is_empty(), "nothing was sent: {:?}", app.toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
        // a line break with Alt+Enter, Shift+Enter, Ctrl+J
        app.set_composer("a");
        press(&mut app, KeyCode::Enter, KeyModifiers::ALT).await;
        type_slow(&mut app, "b").await;
        press(&mut app, KeyCode::Enter, KeyModifiers::SHIFT).await;
        press(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL).await;
        assert_eq!(app.composer_text(), "a\nb\n\n");
        // an unknown command keeps the text and says how to send it as a message
        app.set_composer("/nosuch thing");
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert_eq!(app.composer_text(), "/nosuch thing");
        assert!(app.toasts.last().is_some_and(|t| t.text.starts_with("Unknown command /nosuch.") && t.text.contains("//")));
        // Esc leaves edit mode (emptying the box), else moves to the trace
        app.edit_from = Some(0);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert_eq!((app.composer_text().as_str(), app.edit_from, app.focus), ("", None, Focus::Composer));
        assert_eq!(app.toasts.last().map(|t| t.text.as_str()), Some("Edit cancelled"));
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert_eq!(app.focus, Focus::Messages);
        // Backspace in an empty box drops the last attachment; ↑ on the first line goes up
        app.focus = Focus::Composer;
        app.attachments = vec![Upload { name: "a".into(), ..Default::default() }, Upload { name: "b".into(), ..Default::default() }];
        press(&mut app, KeyCode::Backspace, KeyModifiers::NONE).await;
        assert_eq!(app.attachments.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(), ["a"]);
        app.set_composer("one line");
        app.scroll = 5;
        press(&mut app, KeyCode::Up, KeyModifiers::NONE).await;
        assert_eq!((app.focus, app.follow, app.scroll), (Focus::Messages, false, 4));
        app.focus = Focus::Composer;
        app.set_composer("two\nlines");
        press(&mut app, KeyCode::Up, KeyModifiers::NONE).await;
        assert_eq!((app.focus, app.composer.cursor().0), (Focus::Composer, 0), "↑ moves inside a longer text first");
        press(&mut app, KeyCode::PageUp, KeyModifiers::NONE).await;
        assert_eq!(app.focus, Focus::Messages);
    }

    #[tokio::test]
    async fn the_trace_keys_pick_replies_scroll_and_copy() {
        let mut app = chat_app();
        let chat = json!({ "id": "c1", "agent_id": "a1", "messages": [
            { "role": "user", "content": "q1" }, { "role": "assistant", "content": "a1" },
            { "role": "user", "content": "q2" }, { "role": "assistant", "content": "a2" },
            { "role": "user", "content": "q3" }, { "role": "assistant", "content": "" , "_error": "boom" },
        ] });
        app.trace = Trace::from_chat(&chat, &app.agents, None);
        app.chat = Some(chat);
        app.focus = Focus::Messages;
        let turns: Vec<usize> = app.trace.entries.iter().enumerate().filter(|(_, e)| matches!(e, Entry::Turn(_))).map(|(i, _)| i).collect();
        type_slow(&mut app, "[").await;
        assert_eq!(app.selected_turn, Some(turns[2]), "the newest first");
        type_slow(&mut app, "[[[").await;
        assert_eq!(app.selected_turn, Some(turns[0]), "and no further than the oldest");
        type_slow(&mut app, "]").await;
        assert_eq!(app.selected_turn, Some(turns[1]));
        type_slow(&mut app, "c").await;
        assert_eq!(app.toasts.last().map(|t| t.text.as_str()), Some("Copied the reply"));
        app.selected_turn = Some(turns[2]);
        app.toasts.clear();
        type_slow(&mut app, "c").await;
        assert!(app.toasts.is_empty(), "an error has no reply to copy");
        // r on an earlier reply asks first (it deletes what came after)
        app.selected_turn = Some(turns[0]);
        type_slow(&mut app, "r").await;
        assert!(matches!(app.overlay, Some(Overlay::Confirm { act: Act::Regenerate(1), .. })));
        app.overlay = None;
        for (key, scroll, follow) in [("g", 0, false), ("j", 1, false), ("G", usize::MAX / 2, true), ("k", usize::MAX / 2 - 1, false)] {
            type_slow(&mut app, key).await;
            assert_eq!((app.scroll, app.follow), (scroll, follow), "{key}");
        }
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert_eq!(app.focus, Focus::Composer);
        app.focus = Focus::Messages;
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[tokio::test]
    async fn pastes_go_to_what_takes_text() {
        let mut app = chat_app();
        // the memory panel's search while typing it; not otherwise
        app.overlay = Some(Overlay::Memory { data: vec![], cats: vec![], total: 0, sel: 0, q: String::new(), cat: 0, typing: true });
        app.paste("tea\tand\ncake").await;
        assert!(matches!(&app.overlay, Some(Overlay::Memory { q, .. }) if q == "tea and cake"));
        app.overlay = Some(Overlay::Memory { data: vec![], cats: vec![], total: 0, sel: 0, q: String::new(), cat: 0, typing: false });
        app.paste("ignored").await;
        assert!(matches!(&app.overlay, Some(Overlay::Memory { q, .. }) if q.is_empty()));
        // a form's focused field
        app.open_agent_editor(None);
        app.paste("Pasted\nName").await;
        assert!(matches!(&app.overlay, Some(Overlay::Form { form, .. }) if form.string("name") == "Pasted Name"));
        app.overlay = None;
        // the sidebar filter while it is being typed
        app.focus = Focus::Sidebar;
        app.search_focus = true;
        app.paste("x").await;
        assert_eq!(app.search, "x");
        app.search_focus = false;
        // no chat open, or a sub-agent's: nowhere to put it
        app.focus = Focus::Composer;
        app.chat_id = None;
        app.paste("lost").await;
        app.chat_id = Some("c1".into());
        app.chat = Some(json!({ "id": "c1", "agent_id": "a1", "parent": { "root_chat_id": "r", "caller_id": "a1" } }));
        app.paste("lost").await;
        assert_eq!(app.composer_text(), "");
        // an empty paste, or one that is only escape codes, adds nothing
        app.chat = Some(json!({ "id": "c1", "agent_id": "a1", "messages": [] }));
        app.paste("").await;
        app.paste("\x1b[31m\x1b[0m").await;
        assert_eq!(app.composer_text(), "");
    }

    // ------------------------------------------------------------- time, state, helpers

    #[test]
    fn toasts_expire_errors_later() {
        let mut app = chat_app();
        let now = Instant::now();
        let ago = |s: u64| now.checked_sub(Duration::from_secs(s)).unwrap_or(now);
        for (text, err, at) in [("old info", false, ago(4)), ("old error", true, ago(8)), ("recent error", true, ago(4)), ("fresh", false, now)] {
            app.toasts.push(Toast { text: text.into(), err, at, count: 1 });
        }
        app.tick();
        assert_eq!(app.toasts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), ["recent error", "fresh"]);
        // a toast's text is one clean line
        app.toast("line\none\t\x1b[31mred", false);
        assert_eq!(app.toasts.last().unwrap().text, "line one red");
    }

    #[test]
    fn what_the_client_remembers_comes_back_next_time() {
        let dir = std::env::temp_dir().join(format!("agent-chat-tui-cfg-test-{}-{}", std::process::id(), trace::now()));
        let path = dir.join("state.json");
        let mut app = App::new(Api::new("http://127.0.0.1:9"), Theme::dark(), path.clone());
        assert!(app.cfg.sidebar && !app.cfg.verbose && !app.cfg.files && app.cfg.theme.is_empty(), "no file: the defaults");
        app.cfg.seen.insert("c1".into(), 42.0);
        app.cfg.filter = "a1".into();
        app.toggle_verbose();
        app.set_theme(Theme::light());
        let again = App::new(Api::new("http://127.0.0.1:9"), Theme::dark(), path.clone());
        assert_eq!((again.cfg.verbose, again.cfg.theme.as_str(), again.cfg.seen.get("c1").copied(), again.filter.as_str()), (true, "light", Some(42.0), "a1"));
        assert!(again.render_key.1, "the trace draws verbose from the start");
        // a damaged or partial file: whatever can be read, defaults for the rest
        std::fs::write(&path, "{not json").unwrap();
        assert!(App::new(Api::new("http://127.0.0.1:9"), Theme::dark(), path.clone()).cfg.sidebar);
        std::fs::write(&path, r#"{ "sidebar": false, "files": true, "unknown": 1 }"#).unwrap();
        let partial = App::new(Api::new("http://127.0.0.1:9"), Theme::dark(), path.clone());
        assert!(!partial.cfg.sidebar && partial.cfg.files && partial.cfg.seen.is_empty());
    }

    #[test]
    fn small_helpers() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(shellexpand("~/notes/a.txt"), home.join("notes/a.txt").to_string_lossy());
        for p in ["/abs/path", "rel/path", "~user/x", "~"] {
            assert_eq!(shellexpand(p), p);
        }
        assert_eq!(agent_color(&Agent { color: "#010203".into(), ..Default::default() }), Color::Rgb(1, 2, 3));
        assert_eq!(agent_color(&Agent { color: "blue".into(), ..Default::default() }), Color::Gray);
        assert_eq!(agent_who(&[], "x").name, "Deleted agent");
        assert!(matches!(Overlay::prompt("t", "日本", Act::Tidy), Overlay::Prompt { cursor: 2, .. }), "the cursor after the text, in characters");
    }

    // ------------------------------------------------------------ found in review

    #[tokio::test]
    async fn an_edit_resent_that_fails_or_is_refused_stays_to_send_again() {
        // the request fails (nothing listens): the edited text, its pastes and edit mode stay
        let mut app = chat_app();
        let big = (0..20).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        app.set_composer("my carefully edited message ");
        app.edit_from = Some(0);
        app.on_term(TermEvent::Paste(big.clone())).await;
        let shown = app.composer_text();
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(tried(&app));
        assert_eq!((app.composer_text(), app.edit_from), (shown.clone(), Some(0)), "the edited message after a failed resend");
        assert_eq!(app.pastes.expand(&shown), format!("my carefully edited message {big}"), "its paste is still held");
        // a run started meanwhile (another window, a routine): refused, the same
        app.running = true;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.toasts.last().is_some_and(|t| t.text.starts_with("Wait for the current reply")));
        assert_eq!((app.composer_text(), app.edit_from), (shown, Some(0)));
        // Esc still cancels the edit
        app.running = false;
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
        assert_eq!((app.composer_text().as_str(), app.edit_from), ("", None));
    }

    #[tokio::test]
    async fn ctrl_c_clears_the_box_from_any_pane_before_it_quits() {
        let mut app = chat_app();
        app.cfg.sidebar = true;
        for focus in [Focus::Messages, Focus::Sidebar, Focus::Composer] {
            app.set_composer("a long draft I have not sent");
            app.edit_from = Some(0);
            app.focus = focus;
            press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL).await;
            assert!(!app.quit, "{focus:?}: Ctrl+C quit with text in the box");
            assert_eq!((app.composer_text().as_str(), app.edit_from), ("", None), "{focus:?}");
        }
        // the sidebar's filter goes first, then the box, then it quits
        app.set_composer("draft");
        app.focus = Focus::Sidebar;
        app.search = "fil".into();
        press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL).await;
        assert!(app.search.is_empty() && app.composer_text() == "draft" && !app.quit);
        press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL).await;
        assert!(app.composer_text().is_empty() && !app.quit);
        press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL).await;
        assert!(app.quit);
    }

    #[tokio::test]
    async fn keys_with_no_text_inside_a_keystroke_paste_do_nothing() {
        // a form feed (it arrives as Ctrl+L, which would compact) and ESC d (Alt+D: delete) in a
        // paste from a terminal without bracketed paste: dropped, and the paste goes on
        let mut app = chat_app();
        let t0 = Instant::now();
        let ctrl_l = TermEvent::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        let alt_d = TermEvent::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT));
        let ctrl_q = TermEvent::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
        let mut evs: Vec<TermEvent> = vec![key('a'), key('\r'), ctrl_l, key('\r'), key('b'), key(' ')];
        evs.extend("see ".chars().map(key));
        evs.extend([alt_d, ctrl_q, TermEvent::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))]);
        evs.extend("the rest of the paste".chars().map(key));
        let n = evs.len();
        for (i, e) in evs.into_iter().enumerate() {
            app.on_input(t0, e, if i + 1 < n { Some(t0) } else { None }).await;
        }
        app.flush_burst().await;
        assert!(app.toasts.is_empty() && app.overlay.is_none() && !app.quit, "a key inside the paste acted: {:?}", app.toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
        assert_eq!(app.composer_text(), "a\n\nb see the rest of the paste");
        // typed on their own, the same keys act as usual
        press(&mut app, KeyCode::Char('d'), KeyModifiers::ALT).await;
        assert!(matches!(app.overlay, Some(Overlay::Confirm { act: Act::DeleteChat(_), .. })));
    }

    #[tokio::test]
    async fn mojibake_keeps_the_text_after_its_c1_bytes() {
        // UTF-8 read as Latin-1 (a page with the wrong charset): ” is "â€\u{9d}", 😀 "ð\u{9f}\u{98}\u{80}"
        let mut app = chat_app();
        app.paste("He said â€œhiâ€\u{9d} and then left the room\nsecond line ð\u{9f}\u{98}\u{80} still here").await;
        assert_eq!(app.composer_text(), "He said â€œhiâ€ and then left the room\nsecond line ð still here");
    }

    #[tokio::test]
    async fn a_bracketed_paste_at_the_start_of_the_box_is_a_message_as_pasted() {
        let mut app = chat_app();
        // a code comment keeps both its slashes; "/*" is no unknown command; a command is text
        for (pasted, shown) in [
            ("// fix the off-by-one below\nfor i in 0..=n {}", "/// fix the off-by-one below\nfor i in 0..=n {}"),
            ("/* comment */ int x;", "//* comment */ int x;"),
            ("/help", "//help"),
            ("/etc/hosts is broken", "/etc/hosts is broken"),
            ("plain text", "plain text"),
        ] {
            app.set_composer("");
            app.on_term(TermEvent::Paste(pasted.into())).await;
            assert_eq!(app.composer_text(), shown, "{pasted:?}");
            assert_eq!(commands::parse(&app.composer_text()), Parsed::Message(pasted), "Enter sends {pasted:?} as it was pasted");
        }
        // after typed text it is part of what was typed: "/remember " then a paste
        app.set_composer("/remember ");
        app.on_term(TermEvent::Paste("/usr/share is full".into())).await;
        assert_eq!(app.composer_text(), "/remember /usr/share is full");
        // pasted before a command already in the box, the slash it adds is escaped
        app.set_composer("help me");
        app.composer.move_cursor(CursorMove::Head);
        app.on_term(TermEvent::Paste("/".into())).await;
        assert_eq!(app.composer_text(), "//help me");
        // a keystroke paste can't be told from fast typing: it stays as it came (and never runs)
        app.set_composer("");
        keys_at(&mut app, "/stat", Instant::now()).await;
        app.flush_burst().await;
        assert_eq!(app.composer_text(), "/stat");
    }

    #[tokio::test]
    async fn the_bar_under_the_box_says_what_enter_will_do() {
        use ratatui::backend::TestBackend;
        let mut app = chat_app();
        let bar = |app: &mut App| {
            let mut term = ratatui::Terminal::new(TestBackend::new(120, 30)).unwrap();
            term.draw(|f| crate::ui::draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..30).map(|y| (0..120).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).find(|l| l.contains("asks before acting")).unwrap_or_default()
        };
        // mid-reply, a command that waits for the agent doesn't say Enter runs it
        app.running = true;
        for text in ["/compact keep the API notes", "/pin", "/rename later", "/retry", "/edit", "/delete"] {
            app.set_composer(text);
            app.menu_closed = Some(app.composer_text());
            let b = bar(&mut app);
            assert!(!b.contains("Enter runs") && b.contains("waits for the agent") && b.contains("Stop ■"), "{text}: {b}");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
            assert!(app.toasts.last().is_some_and(|t| t.text.contains("working")), "{text}");
        }
        app.set_composer("/usage");
        app.menu_closed = Some(app.composer_text());
        assert!(bar(&mut app).contains("Enter runs /usage now (not queued)"));
        // an unknown /word: neither sent nor run, and the bar says so
        app.running = false;
        for text in ["/nosuch thing", "/ spaced"] {
            app.set_composer(text);
            let b = bar(&mut app);
            assert!(b.contains("Unknown command /") && b.contains("//") && !b.contains("Enter sends") && !b.contains("Send ↑"), "{text}: {b}");
        }
        app.set_composer("hello");
        assert!(bar(&mut app).contains("Send ↑"));
    }

    #[tokio::test]
    async fn a_command_with_arguments_it_cant_use_keeps_the_text() {
        let mut app = chat_app();
        for text in ["/export pdf", "/theme neon", "/permissions maybe", "/new Nobody Here", "/remember"] {
            app.toasts.clear();
            app.set_composer(text);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
            assert_eq!(app.composer_text(), text, "the text stays to fix");
            assert!(!app.toasts.is_empty(), "{text}: says why");
        }
    }

    #[tokio::test]
    async fn pin_and_rename_wait_for_the_agent_as_their_commands_do() {
        let mut app = chat_app();
        app.running = true;
        app.chats[0].status = "running".into();
        for (code, mods) in [(KeyCode::Char('p'), KeyModifiers::ALT), (KeyCode::F(2), KeyModifiers::NONE), (KeyCode::Char('2'), KeyModifiers::ALT)] {
            app.toasts.clear();
            press(&mut app, code, mods).await;
            assert!(app.overlay.is_none(), "{code:?}: no request, no prompt");
            assert!(app.toasts.last().is_some_and(|t| !t.err && t.text.starts_with("The agent is working") && t.text.contains("/stop")), "{code:?}: {:?}", app.toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
        }
        // the palette's, and the sidebar's for a chat that works in the background
        app.toasts.clear();
        app.run_cmd("pin").await;
        app.run_cmd("rename").await;
        assert_eq!(app.toasts.len(), 2, "{:?}", app.toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
        app.running = false;
        app.chats.push(ChatSummary { id: "c2".into(), title: "busy elsewhere".into(), agent_id: "a1".into(), status: "waiting".into(), ..Default::default() });
        app.rebuild_sidebar();
        app.select_chat_row("c2");
        app.focus = Focus::Sidebar;
        app.toasts.clear();
        press(&mut app, KeyCode::Char('p'), KeyModifiers::NONE).await;
        assert!(app.toasts.last().is_some_and(|t| t.text.contains("then pin it")));
        // a rename prompt answered once a run has started: refused, not sent
        app.overlay = Some(Overlay::prompt("Rename chat", "new name", Act::RenameChat("c2".into())));
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(app.toasts.last().is_some_and(|t| t.text.contains("then rename it")) && !tried(&app));
        // idle, they go to the server (which isn't there)
        app.chats[0].status = "idle".into();
        app.focus = Focus::Composer;
        press(&mut app, KeyCode::Char('p'), KeyModifiers::ALT).await;
        assert!(tried(&app));
    }

    #[tokio::test]
    async fn esc_in_the_routine_editor_goes_back_to_the_routines() {
        let mut app = chat_app();
        let r = Routine { id: "r1".into(), name: "Daily".into(), agent_id: "a1".into(), prompt: "p".into(), schedule: json!({ "type": "daily", "time": "07:15", "days": [0] }), enabled: true, ..Default::default() };
        for k in [KeyCode::Char('n'), KeyCode::Enter, KeyCode::Char('e')] {
            app.overlay = Some(Overlay::Routines { items: vec![r.clone()], sel: 0 });
            press(&mut app, k, KeyModifiers::NONE).await;
            assert!(matches!(app.overlay, Some(Overlay::Form { kind: FormKind::Routine(_), .. })), "{k:?}");
            app.toasts.clear();
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
            // (it goes back for the list: here that fails, and says so)
            assert!(tried(&app) && !app.routines_return, "{k:?}: Esc closed the Routines too");
        }
        // and so does Ctrl+C, which cancels the editor as Esc does
        app.overlay = Some(Overlay::Routines { items: vec![r], sel: 0 });
        press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE).await;
        app.toasts.clear();
        press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL).await;
        assert!(tried(&app) && !app.routines_return && !app.quit);
    }

    #[tokio::test]
    async fn an_approval_rings_once() {
        let mut app = chat_app();
        app.bell = false; // (counted, not rung)
        let ag = app.agents.clone();
        app.trace.start_turn(agent_who(&ag, "a1"), 1);
        app.trace.tool_start(&[], "t1", "run_shell", json!({ "command": "echo bell" }), &ag);
        let visit = app.stream_gen;
        let chat_ev = |kind: &str, data: Value| Incoming::Chat { chat_id: "c1".into(), visit, event: Event { kind: kind.into(), data } };
        let status = |s: &str| Incoming::Global(Event { kind: "chat_status".into(), data: json!({ "chat_id": "c1", "status": s }) });
        app.on_incoming(status("running")).await;
        app.on_incoming(chat_ev("approval", json!({ "path": [], "call_id": "t1", "approval_id": "ap1" }))).await;
        app.on_incoming(status("waiting")).await;
        assert_eq!(app.bells, 1, "the approval and the chat turning 'waiting' both rang");
        app.on_incoming(status("waiting")).await;
        assert_eq!(app.bells, 1, "still the same approval");
        app.on_incoming(status("running")).await;
        // a chat in the background that asks rings too, once
        app.chats.push(ChatSummary { id: "c2".into(), title: "other".into(), agent_id: "a1".into(), status: "running".into(), ..Default::default() });
        app.on_incoming(Incoming::Global(Event { kind: "chat_status".into(), data: json!({ "chat_id": "c2", "status": "waiting" }) })).await;
        assert_eq!(app.bells, 2);
    }

    #[tokio::test]
    async fn what_a_run_said_that_its_chat_keeps_no_trace_of_stays_as_a_toast() {
        let mut app = chat_app();
        app.bell = false;
        let visit = app.stream_gen;
        let ev = |kind: &str, data: Value| Incoming::Chat { chat_id: "c1".into(), visit, event: Event { kind: kind.into(), data } };
        let chat = json!({ "id": "c1", "agent_id": "a1", "messages": [{ "role": "user", "content": "hi" }, { "role": "assistant", "content": "cut", "_truncated": true }, { "role": "assistant", "content": "rest" }] });
        app.on_incoming(ev("run_start", json!({ "chat": chat, "run_base": 3, "agent_id": "a1" }))).await;
        app.on_incoming(ev("notice", json!({ "path": [], "text": "Nothing to compact yet: the conversation is still short." }))).await;
        app.on_incoming(ev("notice", json!({ "path": [], "text": trace::CUT_OFF }))).await;
        app.on_incoming(ev("done", json!({ "chat": chat }))).await;
        let toasts: Vec<&str> = app.toasts.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(toasts, ["Nothing to compact yet: the conversation is still short."], "the cut-off notice is kept in the trace, the other only in a toast");
        assert_eq!(app.trace.notices(), [trace::CUT_OFF]);
    }

    #[test]
    fn exports_never_replace_a_file_that_isnt_theirs() {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("agent-chat-tui-export-test-{}-{t}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("README.md"), "precious").unwrap();
        let mut exported = HashMap::new();
        // a chat titled README beside a project's README.md: README-2.md
        assert_eq!(save_export(&dir, "README", "md", "export 1", "c1", &mut exported).unwrap(), dir.join("README-2.md"));
        assert_eq!(std::fs::read_to_string(dir.join("README.md")).unwrap(), "precious");
        // exported again: the same file, rewritten
        assert_eq!(save_export(&dir, "README", "md", "export 2", "c1", &mut exported).unwrap(), dir.join("README-2.md"));
        assert_eq!(std::fs::read_to_string(dir.join("README-2.md")).unwrap(), "export 2");
        // another chat of the same title gets a name of its own; json is its own file
        assert_eq!(save_export(&dir, "README", "md", "other", "c2", &mut exported).unwrap(), dir.join("README-3.md"));
        assert_eq!(save_export(&dir, "README", "json", "{}", "c1", &mut exported).unwrap(), dir.join("README.json"));
        assert_eq!(std::fs::read_to_string(dir.join("README-2.md")).unwrap(), "export 2");
        // a folder that can't be written: the error
        assert!(save_export(&dir.join("missing"), "x", "md", "", "c1", &mut exported).is_err());
    }

    #[tokio::test]
    async fn slash_commands_run_from_the_palette_with_no_chat_open() {
        let mut app = chat_app();
        app.chat_id = None;
        app.chat = None;
        app.focus = Focus::Sidebar;
        press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL).await;
        for c in "/he".chars() {
            press(&mut app, KeyCode::Char(c), KeyModifiers::NONE).await;
        }
        let Some(Overlay::Palette { items, sel, .. }) = &app.overlay else { panic!("the palette") };
        assert_eq!(items[*sel].title, "/help");
        assert!(items.iter().all(|i| i.group == "Slash commands"));
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(matches!(&app.overlay, Some(Overlay::Info { title, .. }) if title == "/help"));
        // chat-only ones aren't listed, and typed anyway say to open a chat
        app.overlay = None;
        press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL).await;
        for c in "/pin".chars() {
            press(&mut app, KeyCode::Char(c), KeyModifiers::NONE).await;
        }
        assert!(matches!(&app.overlay, Some(Overlay::Palette { items, .. }) if items.iter().all(|i| i.title != "/pin")));
        app.overlay = None;
        app.open_palette();
        if let Some(Overlay::Palette { q, .. }) = &mut app.overlay {
            *q = "/new".into();
        }
        let o = app.overlay.take().unwrap();
        app.palette_search(o).await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert!(matches!(app.overlay, Some(Overlay::Pick { .. })), "/new with no chat: the agent picker");
        // with arguments, and the message box untouched
        app.chat_id = Some("c1".into());
        app.chat = Some(json!({ "id": "c1", "agent_id": "a1", "messages": [] }));
        app.set_composer("a draft");
        app.overlay = None;
        app.open_palette();
        if let Some(Overlay::Palette { q, .. }) = &mut app.overlay {
            *q = "/theme light".into();
        }
        let o = app.overlay.take().unwrap();
        app.palette_search(o).await;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
        assert_eq!((app.theme.name, app.composer_text().as_str()), ("light", "a draft"));
        app.chat_id = None;
        app.open_palette();
        if let Some(Overlay::Palette { q, .. }) = &mut app.overlay {
            *q = "/rename x".into();
        }
        let o = app.overlay.take().unwrap();
        app.palette_search(o).await;
        assert!(matches!(&app.overlay, Some(Overlay::Palette { items, .. }) if items.is_empty()));
    }

    #[tokio::test]
    async fn a_chat_opened_on_start_takes_what_is_typed_in_its_box() {
        // what main does for --chat
        let mut app = chat_app();
        app.focus = Focus::Sidebar;
        app.open_chat(Some("c1".into())).await;
        app.focus = if app.in_chat() { Focus::Composer } else { Focus::Messages };
        type_slow(&mut app, "hello there").await;
        assert_eq!(app.composer_text(), "hello there");
        assert!(app.overlay.is_none(), "a letter ran a sidebar shortcut");
    }
}
