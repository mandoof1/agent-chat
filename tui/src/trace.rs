//! The conversation as a trace: entries (you, an agent's turn, a compaction divider), where a turn
//! is a list of steps (thinking, text, tool calls with nested handoffs). Built from a chat's saved
//! messages and updated live from the run's events, then rendered to styled, pre-wrapped lines.

use crate::api::Agent;
use crate::md;
use crate::theme::{dim, hex, Theme};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Who {
    pub id: String,
    pub name: String,
    pub color: Color,
    pub emoji: String,
}

impl Who {
    pub fn from(agent: Option<&Agent>, id: &str) -> Who {
        match agent {
            Some(a) => Who { id: a.id.clone(), name: a.name.clone(), color: hex(&a.color).unwrap_or(Color::Gray), emoji: a.emoji.clone() },
            None => Who { id: id.to_string(), name: "Deleted agent".into(), color: Color::Gray, emoji: "❔".into() },
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub tok_per_s: Option<f64>,
    pub prompt: Option<u64>,
    pub completion: Option<u64>,
    pub think_s: Option<f64>,
}

impl Stats {
    pub fn parse(v: Option<&Value>) -> Stats {
        let v = match v {
            Some(v) => v,
            None => return Stats::default(),
        };
        Stats {
            tok_per_s: v.get("tok_per_s").and_then(|x| x.as_f64()),
            prompt: v.get("prompt_tokens").and_then(|x| x.as_u64()),
            completion: v.get("completion_tokens").and_then(|x| x.as_u64()),
            think_s: v.get("think_s").and_then(|x| x.as_f64()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolState {
    Running,
    Writing,
    Done,
    Failed(String),
    NeedsApproval,
}

#[derive(Clone, Debug)]
pub struct Sub {
    pub who: Who,
    pub chat_id: Option<String>,
    pub continued: bool,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug)]
pub struct ToolStep {
    pub call_id: String,
    pub name: String,
    pub args: Value,
    pub raw_args: String, // while the model is still writing the call
    pub state: ToolState,
    pub result: Option<String>,
    pub live_out: Option<String>,
    pub approval: Option<String>,
    pub bypassed: Option<String>,
    pub sub: Option<Sub>,
}

#[derive(Clone, Debug)]
pub enum Step {
    Thinking { text: String, live: bool, secs: Option<f64>, started: Option<std::time::Instant> },
    Text { text: String, live: bool },
    Tool(ToolStep),
    Notice(String),
    Error { text: String, index: usize },
    Stopped,
    Waiting,
    Compacting { chars: usize },
}

#[derive(Clone, Debug)]
pub struct Turn {
    pub who: Who,
    pub ts: Option<f64>,
    pub stats: Stats,
    pub steps: Vec<Step>,
    pub start: usize, // message index this turn begins at (for regenerate)
    pub end: usize,   // one past its last message (for branching)
}

#[derive(Clone, Debug)]
pub struct UserEntry {
    pub text: String,
    pub ts: Option<f64>,
    pub from: Option<Who>,
    pub queued: bool,
    pub images: Vec<String>,
    pub recalled: usize,
    pub index: usize,
}

#[derive(Clone, Debug)]
pub enum Entry {
    User(UserEntry),
    Turn(Turn),
    Divider { summary: String, reason: String, before: Option<u64>, after: Option<u64> },
}

pub struct Trace {
    pub entries: Vec<Entry>,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn tool_state_for(name: &str, text: &str) -> ToolState {
    if let Some(rest) = text.strip_prefix("exit code ") {
        let code: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect();
        return if code == "0" { ToolState::Done } else { ToolState::Failed(format!("exit {code}")) };
    }
    if text.starts_with("Error") {
        return ToolState::Failed("failed".into());
    }
    if text.starts_with("The user denied") || text.starts_with("The user did not approve") {
        return ToolState::Failed("denied".into());
    }
    if text.starts_with("[cancelled") {
        return ToolState::Failed("cancelled".into());
    }
    if name == "run_shell" && text.starts_with("Command timed out") {
        return ToolState::Failed("timed out".into());
    }
    ToolState::Done
}

impl Trace {
    pub fn empty() -> Self {
        Trace { entries: Vec::new() }
    }

    /// The saved messages of a chat as entries (`up_to` leaves out the part a live run is replaying).
    pub fn from_chat(chat: &Value, agents: &[Agent], up_to: Option<usize>) -> Self {
        let mut t = Trace::empty();
        let msgs = chat.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
        let msgs = &msgs[..up_to.map(|n| n.min(msgs.len())).unwrap_or(msgs.len())];
        let agent_id = s(chat, "agent_id");
        let who = Who::from(agents.iter().find(|a| a.id == agent_id), &agent_id);
        let caller = chat.get("parent").and_then(|p| p.get("caller_id")).and_then(|c| c.as_str()).map(|cid| Who::from(agents.iter().find(|a| a.id == cid), cid));
        let results: std::collections::HashMap<String, &Value> = msgs.iter().filter(|m| s(m, "role") == "tool").map(|m| (s(m, "tool_call_id"), m)).collect();
        let dividers: std::collections::HashMap<usize, &Value> = chat
            .get("compactions")
            .and_then(|c| c.as_array())
            .map(|cs| cs.iter().filter_map(|c| c.get("upto").and_then(|u| u.as_u64()).map(|u| (u as usize, c))).collect())
            .unwrap_or_default();
        let mut turn_open = false;
        for (i, m) in msgs.iter().enumerate() {
            if let Some(c) = dividers.get(&i) {
                t.entries.push(Entry::Divider {
                    summary: s(c, "summary"),
                    reason: s(c, "reason"),
                    before: c.get("before_tokens").and_then(|x| x.as_u64()),
                    after: c.get("after_tokens").and_then(|x| x.as_u64()),
                });
                turn_open = false;
            }
            match s(m, "role").as_str() {
                "user" => {
                    turn_open = false;
                    t.entries.push(Entry::User(user_entry(m, i, caller.clone())));
                }
                "assistant" => {
                    if !turn_open {
                        t.entries.push(Entry::Turn(Turn { who: who.clone(), ts: m.get("_ts").and_then(|x| x.as_f64()), stats: Stats::default(), steps: vec![], start: i, end: i + 1 }));
                        turn_open = true;
                    }
                    if let Some(Entry::Turn(turn)) = t.entries.last_mut() {
                        append_assistant(&mut turn.steps, m, &results, agents, i);
                        if m.get("_stats").is_some() {
                            turn.stats = Stats::parse(m.get("_stats"));
                        }
                        turn.end = i + 1;
                    }
                }
                _ => {}
            }
        }
        t
    }

    pub fn last_turn_mut(&mut self) -> Option<&mut Turn> {
        self.entries.iter_mut().rev().find_map(|e| if let Entry::Turn(t) = e { Some(t) } else { None })
    }

    /// The step list a run event with `path` (a chain of ask_agent call ids) belongs to.
    pub fn container(&mut self, path: &[String], agents: &[Agent]) -> Option<&mut Vec<Step>> {
        let turn = self.last_turn_mut()?;
        descend(&mut turn.steps, path, agents)
    }

    pub fn find_tool(&mut self, path: &[String], call_id: &str, agents: &[Agent]) -> Option<&mut ToolStep> {
        let steps = self.container(path, agents)?;
        steps.iter_mut().rev().find_map(|st| match st {
            Step::Tool(t) if t.call_id == call_id => Some(t),
            _ => None,
        })
    }
}

fn descend<'a>(steps: &'a mut Vec<Step>, path: &[String], agents: &[Agent]) -> Option<&'a mut Vec<Step>> {
    if path.is_empty() {
        return Some(steps);
    }
    let tool = steps.iter_mut().rev().find_map(|st| match st {
        Step::Tool(t) if t.call_id == path[0] => Some(t),
        _ => None,
    })?;
    if tool.sub.is_none() {
        let name = tool.args.get("agent").and_then(|a| a.as_str()).unwrap_or("");
        let agent = agents.iter().find(|a| a.name.eq_ignore_ascii_case(name));
        tool.sub = Some(Sub { who: Who::from(agent, name), chat_id: None, continued: false, steps: vec![] });
    }
    descend(&mut tool.sub.as_mut().unwrap().steps, &path[1..], agents)
}

fn user_entry(m: &Value, index: usize, from: Option<Who>) -> UserEntry {
    UserEntry {
        text: s(m, "content"),
        ts: m.get("_ts").and_then(|x| x.as_f64()),
        from,
        queued: m.get("_queued").and_then(|q| q.as_bool()).unwrap_or(false),
        images: m.get("_images").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
        recalled: m.get("_recall").and_then(|r| r.as_str()).map(|r| r.lines().filter(|l| !l.trim().is_empty()).count()).unwrap_or(0),
        index,
    }
}

pub fn user_from_event(m: &Value, index: usize) -> UserEntry {
    user_entry(m, index, None)
}

fn append_assistant(steps: &mut Vec<Step>, m: &Value, results: &std::collections::HashMap<String, &Value>, agents: &[Agent], index: usize) {
    if let Some(err) = m.get("_error").and_then(|e| e.as_str()) {
        steps.push(Step::Error { text: err.to_string(), index });
    }
    if let Some(r) = m.get("reasoning_content").and_then(|r| r.as_str()) {
        if !r.is_empty() {
            steps.push(Step::Thinking { text: r.to_string(), live: false, secs: m.get("_stats").and_then(|st| st.get("think_s")).and_then(|x| x.as_f64()), started: None });
        }
    }
    let content = s(m, "content");
    if !content.is_empty() {
        steps.push(Step::Text { text: content, live: false });
    }
    if m.get("_stopped").and_then(|x| x.as_bool()).unwrap_or(false) {
        steps.push(Step::Stopped);
    }
    if m.get("_truncated").and_then(|x| x.as_bool()).unwrap_or(false) {
        steps.push(Step::Notice("This reply filled the context window before it finished; the agent continues from where it stopped.".into()));
    }
    for tc in m.get("tool_calls").and_then(|t| t.as_array()).cloned().unwrap_or_default() {
        let id = s(&tc, "id");
        let name = tc.get("function").map(|f| s(f, "name")).unwrap_or_default();
        let raw = tc.get("function").map(|f| s(f, "arguments")).unwrap_or_default();
        let args = serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| serde_json::json!({ "_raw": raw }));
        let mut tool = ToolStep { call_id: id.clone(), name: name.clone(), args: args.clone(), raw_args: String::new(), state: ToolState::Running, result: None, live_out: None, approval: None, bypassed: None, sub: None };
        if let Some(res) = results.get(&id) {
            let text = s(res, "content");
            tool.state = tool_state_for(&name, &text);
            if res.get("_bypassed").and_then(|b| b.as_bool()).unwrap_or(false) {
                tool.bypassed = Some("settings".into());
            }
            if let Some(sub) = res.get("_sub") {
                let aid = s(sub, "agent_id");
                let who = Who::from(agents.iter().find(|a| a.id == aid), &aid);
                let mut sub_steps = Vec::new();
                let sub_msgs = sub.get("messages").and_then(|a| a.as_array()).cloned().unwrap_or_default();
                let sub_results: std::collections::HashMap<String, &Value> = sub_msgs.iter().filter(|m| s(m, "role") == "tool").map(|m| (s(m, "tool_call_id"), m)).collect();
                for (j, sm) in sub_msgs.iter().enumerate() {
                    if s(sm, "role") == "assistant" {
                        append_assistant(&mut sub_steps, sm, &sub_results, agents, j);
                    }
                }
                tool.sub = Some(Sub { who, chat_id: sub.get("chat_id").and_then(|c| c.as_str()).map(String::from), continued: sub.get("continued").and_then(|c| c.as_bool()).unwrap_or(false), steps: sub_steps });
                if matches!(tool.state, ToolState::Failed(_)) {
                    tool.result = Some(text);
                }
            } else {
                tool.result = Some(text);
            }
        } else {
            tool.state = ToolState::Failed("cancelled".into());
        }
        steps.push(Step::Tool(tool));
    }
}

// ------------------------------------------------------------------ live updates

impl Trace {
    /// A new turn for the chat's agent (run start, or after a queued message was delivered).
    pub fn start_turn(&mut self, who: Who, start: usize) {
        self.entries.push(Entry::Turn(Turn { who, ts: Some(now()), stats: Stats::default(), steps: vec![], start, end: start }));
    }

    pub fn assistant_start(&mut self, path: &[String], agents: &[Agent]) {
        if let Some(c) = self.container(path, agents) {
            c.push(Step::Waiting);
        }
    }

    pub fn delta(&mut self, path: &[String], kind: &str, text: &str, index: Option<u64>, name: Option<&str>, agents: &[Agent]) {
        let Some(c) = self.container(path, agents) else { return };
        match kind {
            "reasoning" => {
                if matches!(c.last(), Some(Step::Waiting)) {
                    c.pop();
                }
                match c.last_mut() {
                    Some(Step::Thinking { text: t, live: true, .. }) => t.push_str(text),
                    _ => c.push(Step::Thinking { text: text.to_string(), live: true, secs: None, started: Some(std::time::Instant::now()) }),
                }
            }
            "content" => {
                if matches!(c.last(), Some(Step::Waiting)) {
                    c.pop();
                }
                if let Some(Step::Thinking { live, secs, started, .. }) = c.last_mut() {
                    if *live {
                        *live = false;
                        *secs = started.map(|s| s.elapsed().as_secs_f64());
                    }
                }
                match c.last_mut() {
                    Some(Step::Text { text: t, live: true }) => t.push_str(text),
                    _ => c.push(Step::Text { text: text.to_string(), live: true }),
                }
            }
            "tool_args" => {
                if matches!(c.last(), Some(Step::Waiting)) {
                    c.pop();
                }
                if let Some(Step::Thinking { live, secs, started, .. }) = c.last_mut() {
                    if *live {
                        *live = false;
                        *secs = started.map(|s| s.elapsed().as_secs_f64());
                    }
                }
                let idx = index.unwrap_or(0);
                let key = format!("draft:{idx}");
                let existing = c.iter_mut().rev().find_map(|st| match st {
                    Step::Tool(t) if t.call_id == key => Some(t),
                    _ => None,
                });
                match existing {
                    Some(t) => {
                        t.raw_args.push_str(text);
                        if let Some(n) = name {
                            if !n.is_empty() {
                                t.name = n.to_string();
                            }
                        }
                    }
                    None => c.push(Step::Tool(ToolStep { call_id: key, name: name.unwrap_or("tool").to_string(), args: Value::Null, raw_args: text.to_string(), state: ToolState::Writing, result: None, live_out: None, approval: None, bypassed: None, sub: None })),
                }
            }
            _ => {}
        }
    }

    pub fn assistant_end(&mut self, path: &[String], message: &Value, agents: &[Agent]) {
        let stats = Stats::parse(message.get("_stats"));
        let is_root = path.is_empty();
        let Some(c) = self.container(path, agents) else { return };
        c.retain(|st| !matches!(st, Step::Waiting) && !matches!(st, Step::Tool(t) if t.state == ToolState::Writing));
        for st in c.iter_mut() {
            match st {
                Step::Thinking { live, secs, started, text } if *live => {
                    *live = false;
                    *secs = stats.think_s.or_else(|| started.map(|s| s.elapsed().as_secs_f64()));
                    if let Some(r) = message.get("reasoning_content").and_then(|r| r.as_str()) {
                        *text = r.to_string();
                    }
                }
                Step::Text { live, text } if *live => {
                    *live = false;
                    if let Some(ct) = message.get("content").and_then(|r| r.as_str()) {
                        *text = ct.to_string();
                    }
                }
                _ => {}
            }
        }
        if message.get("_truncated").and_then(|x| x.as_bool()).unwrap_or(false) {
            c.push(Step::Notice("The reply filled the context window before it finished. Continuing from where it stopped…".into()));
        }
        if is_root {
            if let Some(t) = self.last_turn_mut() {
                if stats.tok_per_s.is_some() || stats.prompt.is_some() {
                    t.stats = stats;
                }
            }
        }
    }

    pub fn tool_start(&mut self, path: &[String], call_id: &str, name: &str, args: Value, agents: &[Agent]) {
        if let Some(c) = self.container(path, agents) {
            c.push(Step::Tool(ToolStep { call_id: call_id.to_string(), name: name.to_string(), args, raw_args: String::new(), state: ToolState::Running, result: None, live_out: None, approval: None, bypassed: None, sub: None }));
        }
    }

    pub fn tool_progress(&mut self, path: &[String], call_id: &str, text: &str, agents: &[Agent]) {
        if let Some(t) = self.find_tool(path, call_id, agents) {
            t.live_out = Some(text.to_string());
        }
    }

    pub fn tool_result(&mut self, path: &[String], call_id: &str, text: &str, agents: &[Agent]) {
        if let Some(t) = self.find_tool(path, call_id, agents) {
            t.live_out = None;
            t.state = tool_state_for(&t.name, text);
            if t.sub.is_none() || matches!(t.state, ToolState::Failed(_)) {
                t.result = Some(text.to_string());
            }
        }
    }

    pub fn subagent_start(&mut self, path: &[String], agent_id: &str, chat_id: &str, continued: bool, agents: &[Agent]) {
        let Some((last, parent)) = path.split_last() else { return };
        if let Some(t) = self.find_tool(parent, last, agents) {
            let who = Who::from(agents.iter().find(|a| a.id == agent_id), agent_id);
            match &mut t.sub {
                Some(sub) => {
                    sub.who = who;
                    sub.chat_id = Some(chat_id.to_string());
                    sub.continued = continued;
                }
                None => t.sub = Some(Sub { who, chat_id: Some(chat_id.to_string()), continued, steps: vec![] }),
            }
        }
    }

    pub fn approval(&mut self, path: &[String], call_id: &str, approval_id: Option<&str>, approved: Option<bool>, agents: &[Agent]) {
        if let Some(t) = self.find_tool(path, call_id, agents) {
            match (approval_id, approved) {
                (Some(id), _) => {
                    t.approval = Some(id.to_string());
                    t.state = ToolState::NeedsApproval;
                }
                (None, Some(ok)) => {
                    t.approval = None;
                    t.state = if ok { ToolState::Running } else { ToolState::Failed("denied".into()) };
                }
                _ => {}
            }
        }
    }

    pub fn bypassed(&mut self, path: &[String], call_id: &str, reason: &str, agents: &[Agent]) {
        if let Some(t) = self.find_tool(path, call_id, agents) {
            t.bypassed = Some(reason.to_string());
        }
    }

    pub fn notice(&mut self, path: &[String], text: &str, agents: &[Agent]) {
        if let Some(c) = self.container(path, agents) {
            c.push(Step::Notice(text.to_string()));
        }
    }

    pub fn error(&mut self, text: &str, index: usize) {
        if let Some(t) = self.last_turn_mut() {
            t.steps.push(Step::Error { text: text.to_string(), index });
        }
    }

    pub fn compact_start(&mut self, path: &[String], agents: &[Agent]) {
        if let Some(c) = self.container(path, agents) {
            c.push(Step::Compacting { chars: 0 });
        }
    }

    pub fn compact_delta(&mut self, path: &[String], n: usize, agents: &[Agent]) {
        if let Some(c) = self.container(path, agents) {
            if let Some(Step::Compacting { chars }) = c.iter_mut().rev().find(|s| matches!(s, Step::Compacting { .. })) {
                *chars += n;
            }
        }
    }

    pub fn compact_end(&mut self, path: &[String], compaction: &Value, agents: &[Agent]) {
        let before = compaction.get("before_tokens").and_then(|x| x.as_u64());
        let after = compaction.get("after_tokens").and_then(|x| x.as_u64());
        if let Some(c) = self.container(path, agents) {
            c.retain(|s| !matches!(s, Step::Compacting { .. }));
            let size = match (before, after) {
                (Some(b), Some(a)) => format!(" ({} → {} tokens)", fmt_k(b), fmt_k(a)),
                _ => String::new(),
            };
            c.push(Step::Notice(format!("Earlier messages summarized to free up context{size}.")));
        }
    }

    /// Every pending approval, outermost first: (approval id, tool name, a one-line summary).
    pub fn pending_approvals(&self) -> Vec<(String, String, String)> {
        fn walk(steps: &[Step], out: &mut Vec<(String, String, String)>) {
            for st in steps {
                if let Step::Tool(t) = st {
                    if let Some(id) = &t.approval {
                        out.push((id.clone(), t.name.clone(), arg_summary(&t.name, &t.args)));
                    }
                    if let Some(sub) = &t.sub {
                        walk(&sub.steps, out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        for e in &self.entries {
            if let Entry::Turn(t) = e {
                walk(&t.steps, &mut out);
            }
        }
        out
    }
}

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub fn fmt_k(n: u64) -> String {
    if n >= 1000 {
        let s = format!("{:.1}", n as f64 / 1000.0);
        format!("{}k", s.trim_end_matches(".0"))
    } else {
        n.to_string()
    }
}

pub fn clock(ts: Option<f64>) -> String {
    let Some(ts) = ts else { return String::new() };
    let dt = chrono::DateTime::from_timestamp(ts as i64, 0).map(|d| d.with_timezone(&chrono::Local));
    match dt {
        Some(d) => {
            let today = chrono::Local::now().date_naive();
            if d.date_naive() == today {
                d.format("%H:%M").to_string()
            } else {
                d.format("%b %-d, %H:%M").to_string()
            }
        }
        None => String::new(),
    }
}

pub fn one_line(s: &str, n: usize) -> String {
    let joined: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() > n {
        let cut: String = joined.chars().take(n.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        joined
    }
}

pub fn arg_summary(name: &str, a: &Value) -> String {
    let g = |k: &str| a.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    if a.get("_raw").is_some() {
        return "(invalid arguments)".into();
    }
    match name {
        "run_shell" => one_line(&g("command"), 80),
        "web_search" | "email_search" | "recall_memory" => g("query"),
        "web_fetch" | "browser_navigate" => g("url"),
        "browser_click" => if g("text").is_empty() { g("selector") } else { g("text") },
        "browser_type" => format!("{}: {}", if g("label").is_empty() { g("selector") } else { g("label") }, one_line(&g("text"), 40)),
        "list_dir" => if g("path").is_empty() { ".".into() } else { g("path") },
        "remember" => one_line(&g("fact"), 80),
        "forget_memory" => format!("#{}", a.get("id").map(|v| v.to_string()).unwrap_or_default()),
        "email_read" => format!("#{}", g("uid")),
        "email_list" => format!("{}{}", if g("folder").is_empty() { "INBOX".to_string() } else { g("folder") }, if a.get("unread_only").and_then(|v| v.as_bool()).unwrap_or(false) { " · unread" } else { "" }),
        "email_send" | "email_draft" => format!("{} · {}", g("to"), g("subject")),
        "ask_agent" => one_line(&if g("message").is_empty() { g("task") } else { g("message") }, 80),
        "calendar_events" => {
            let mut parts = vec![];
            if !g("query").is_empty() { parts.push(format!("“{}”", g("query"))); }
            parts.push(if g("start").is_empty() { "today".into() } else { g("start") });
            if let Some(d) = a.get("days").and_then(|v| v.as_u64()) { parts.push(format!("{d} days")); }
            parts.join(" · ")
        }
        _ => {
            if !g("path").is_empty() {
                g("path")
            } else {
                one_line(&a.to_string(), 80)
            }
        }
    }
}

// ----------------------------------------------------------------------- rendering

/// Wrap a line's spans to `width` columns, splitting on spaces (and inside long words).
pub fn wrap_spans(spans: &[Span<'static>], width: usize) -> Vec<Vec<Span<'static>>> {
    let width = width.max(4);
    let mut out: Vec<Vec<Span<'static>>> = vec![vec![]];
    let mut col = 0usize;
    for sp in spans {
        let style = sp.style;
        let text = sp.content.to_string();
        // split into words keeping the spaces attached to the word before them
        let mut word = String::new();
        let push_word = |word: &mut String, out: &mut Vec<Vec<Span<'static>>>, col: &mut usize| {
            if word.is_empty() {
                return;
            }
            let mut w = word.chars().count();
            if *col + w > width && *col > 0 {
                out.push(vec![]);
                *col = 0;
                let trimmed = word.trim_start().to_string();
                w = trimmed.chars().count();
                *word = trimmed;
            }
            while w > width {
                let head: String = word.chars().take(width - *col).collect();
                let rest: String = word.chars().skip(width - *col).collect();
                out.last_mut().unwrap().push(Span::styled(head, style));
                out.push(vec![]);
                *col = 0;
                *word = rest;
                w = word.chars().count();
            }
            if !word.is_empty() {
                out.last_mut().unwrap().push(Span::styled(std::mem::take(word), style));
                *col += w;
            }
        };
        for ch in text.chars() {
            word.push(ch);
            if ch == ' ' {
                push_word(&mut word, &mut out, &mut col);
            }
        }
        push_word(&mut word, &mut out, &mut col);
    }
    out
}

pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    /// Which turn (by index into entries) each line belongs to, for the copy/branch actions.
    pub owners: Vec<Option<usize>>,
}

pub struct RenderOpts<'a> {
    pub theme: &'a Theme,
    pub width: usize,
    pub verbose: bool,
    pub selected_turn: Option<usize>,
}

/// Render the whole trace into pre-wrapped lines.
pub fn render(trace: &Trace, opts: &RenderOpts) -> Rendered {
    let mut r = Rendered { lines: vec![], owners: vec![] };
    let th = opts.theme;
    for (i, e) in trace.entries.iter().enumerate() {
        match e {
            Entry::User(u) => {
                r.push(Line::from(""), None);
                let mut who = vec![];
                match &u.from {
                    Some(f) => who.push(Span::styled(format!("{} asked", f.name), Style::default().fg(f.color).add_modifier(Modifier::BOLD))),
                    None => who.push(Span::styled("You", Style::default().fg(th.muted).add_modifier(Modifier::BOLD))),
                }
                if let Some(ts) = u.ts {
                    who.push(Span::styled(format!("  {}", clock(Some(ts))), Style::default().fg(th.faint)));
                }
                if u.queued {
                    who.push(Span::styled("  · sent while it worked", Style::default().fg(th.faint)));
                }
                if u.recalled > 0 {
                    who.push(Span::styled(format!("  · {} memor{} recalled", u.recalled, if u.recalled == 1 { "y" } else { "ies" }), Style::default().fg(th.faint)));
                }
                r.push(Line::from(who), None);
                let bar = Span::styled("▎ ", Style::default().fg(u.from.as_ref().map(|f| f.color).unwrap_or(th.line)));
                for img in &u.images {
                    r.push(Line::from(vec![bar.clone(), Span::styled(format!("🖼 {img}"), Style::default().fg(th.muted))]), None);
                }
                for raw in u.text.lines() {
                    let wrapped = wrap_spans(&[Span::raw(raw.to_string())], opts.width.saturating_sub(2));
                    for w in wrapped {
                        let mut l = vec![bar.clone()];
                        l.extend(w);
                        r.push(Line::from(l), None);
                    }
                }
            }
            Entry::Divider { summary, reason, before, after } => {
                r.push(Line::from(""), None);
                let why = match reason.as_str() {
                    "auto" => "automatically",
                    "manual" => "on request",
                    "overflow" => "because the context was full",
                    "cutoff" => "because a reply filled the context",
                    _ => "",
                };
                let size = match (before, after) {
                    (Some(b), Some(a)) => format!(" · {} → {} tokens", fmt_k(*b), fmt_k(*a)),
                    _ => String::new(),
                };
                r.push(Line::from(Span::styled(format!("── earlier messages summarized {why}{size} ──"), Style::default().fg(th.faint))), None);
                if opts.verbose {
                    for l in md::render(summary, th, opts.width.saturating_sub(4)) {
                        let mut spans = vec![Span::styled("  ", Style::default())];
                        spans.extend(l.spans);
                        r.push(Line::from(spans), None);
                    }
                }
            }
            Entry::Turn(t) => {
                r.push(Line::from(""), Some(i));
                let selected = opts.selected_turn == Some(i);
                let mut head = vec![
                    Span::styled(if selected { "▶ " } else { "" }, Style::default().fg(th.accent)),
                    Span::styled(format!("{} ", t.who.emoji), Style::default()),
                    Span::styled(t.who.name.clone(), Style::default().fg(t.who.color).add_modifier(Modifier::BOLD)),
                ];
                let mut bits = vec![];
                if let Some(v) = t.stats.tok_per_s { bits.push(format!("{v:.1} tok/s")); }
                if let Some(v) = t.stats.prompt { bits.push(format!("{} in", fmt_k(v))); }
                if let Some(v) = t.stats.completion { bits.push(format!("{} out", fmt_k(v))); }
                if !bits.is_empty() {
                    head.push(Span::styled(format!("  {}", bits.join(" · ")), Style::default().fg(th.faint)));
                }
                if let Some(ts) = t.ts {
                    head.push(Span::styled(format!("  {}", clock(Some(ts))), Style::default().fg(th.faint)));
                }
                r.push(Line::from(head), Some(i));
                render_steps(&t.steps, &[t.who.color], opts, &mut r, Some(i));
            }
        }
    }
    r
}

impl Rendered {
    fn push(&mut self, line: Line<'static>, owner: Option<usize>) {
        self.lines.push(line);
        self.owners.push(owner);
    }
}

fn prefix(colors: &[Color], th: &Theme, marker: Option<(&str, Color)>) -> Vec<Span<'static>> {
    let mut spans = vec![];
    let n = colors.len();
    for (k, c) in colors.iter().enumerate() {
        let line_color = dim(*c, 2);
        if k + 1 == n {
            match marker {
                Some((m, mc)) => spans.push(Span::styled(format!("{m} "), Style::default().fg(mc))),
                None => spans.push(Span::styled("│ ", Style::default().fg(line_color))),
            }
        } else {
            spans.push(Span::styled("│ ", Style::default().fg(line_color)));
        }
    }
    let _ = th;
    spans
}

fn render_steps(steps: &[Step], colors: &[Color], opts: &RenderOpts, r: &mut Rendered, owner: Option<usize>) {
    let th = opts.theme;
    let depth = colors.len();
    let inner = opts.width.saturating_sub(2 * depth + 1);
    let color = *colors.last().unwrap_or(&th.muted);
    let body = |text_lines: Vec<Line<'static>>, r: &mut Rendered| {
        for l in text_lines {
            for w in wrap_spans(&l.spans, inner) {
                let mut spans = prefix(colors, th, None);
                spans.extend(w);
                r.push(Line::from(spans), owner);
            }
        }
    };
    for st in steps {
        match st {
            Step::Waiting => {
                let mut spans = prefix(colors, th, Some(("◌", th.accent)));
                spans.push(Span::styled("working…", Style::default().fg(th.muted)));
                r.push(Line::from(spans), owner);
            }
            Step::Thinking { text, live, secs, .. } => {
                let label = if *live { "thinking…".to_string() } else if let Some(s) = secs { format!("thought for {}", fmt_secs(*s)) } else { "thoughts".to_string() };
                let mut spans = prefix(colors, th, Some(("○", if *live { th.accent } else { dim(color, 2) })));
                spans.push(Span::styled(label, Style::default().fg(if *live { th.accent } else { th.muted })));
                if !opts.verbose && !*live {
                    spans.push(Span::styled("  (Alt+O to show)", Style::default().fg(th.faint)));
                }
                r.push(Line::from(spans), owner);
                if opts.verbose || *live {
                    let tail: String = if *live { text.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n") } else { text.clone() };
                    for raw in tail.lines() {
                        for w in wrap_spans(&[Span::styled(raw.to_string(), Style::default().fg(th.muted).add_modifier(Modifier::ITALIC))], inner.saturating_sub(2)) {
                            let mut spans = prefix(colors, th, None);
                            spans.push(Span::styled("  ", Style::default()));
                            spans.extend(w);
                            r.push(Line::from(spans), owner);
                        }
                    }
                }
            }
            Step::Text { text, live } => {
                let md_lines = md::render(text, th, inner);
                let mut first = true;
                for l in md_lines {
                    for w in wrap_spans(&l.spans, inner) {
                        let mut spans = if first { prefix(colors, th, Some(("●", color))) } else { prefix(colors, th, None) };
                        first = false;
                        spans.extend(w);
                        r.push(Line::from(spans), owner);
                    }
                }
                if *live {
                    let mut spans = prefix(colors, th, None);
                    spans.push(Span::styled("▍", Style::default().fg(th.accent)));
                    r.push(Line::from(spans), owner);
                }
            }
            Step::Tool(t) => render_tool(t, colors, opts, r, owner),
            Step::Notice(n) => {
                let mut spans = prefix(colors, th, Some(("◦", th.amber)));
                let wrapped = wrap_spans(&[Span::styled(n.clone(), Style::default().fg(th.amber))], inner);
                for (k, w) in wrapped.into_iter().enumerate() {
                    let mut s = if k == 0 { std::mem::take(&mut spans) } else { prefix(colors, th, None) };
                    s.extend(w);
                    r.push(Line::from(s), owner);
                }
            }
            Step::Error { text, .. } => {
                let mut spans = prefix(colors, th, Some(("✖", th.danger)));
                spans.push(Span::styled("Error", Style::default().fg(th.danger).add_modifier(Modifier::BOLD)));
                r.push(Line::from(spans), owner);
                for raw in text.lines() {
                    for w in wrap_spans(&[Span::styled(raw.to_string(), Style::default().fg(th.danger))], inner.saturating_sub(2)) {
                        let mut s = prefix(colors, th, None);
                        s.push(Span::raw("  "));
                        s.extend(w);
                        r.push(Line::from(s), owner);
                    }
                }
                let mut s = prefix(colors, th, None);
                s.push(Span::styled("  Ctrl+R tries again", Style::default().fg(th.faint)));
                r.push(Line::from(s), owner);
            }
            Step::Stopped => {
                let mut spans = prefix(colors, th, Some(("■", th.muted)));
                spans.push(Span::styled("stopped here", Style::default().fg(th.muted)));
                r.push(Line::from(spans), owner);
            }
            Step::Compacting { chars } => {
                let mut spans = prefix(colors, th, Some(("◌", th.accent)));
                spans.push(Span::styled(format!("compacting: summarizing older messages… {} chars", fmt_k(*chars as u64)), Style::default().fg(th.muted)));
                r.push(Line::from(spans), owner);
            }
        }
    }
    let _ = body;
}

fn fmt_secs(s: f64) -> String {
    if s >= 60.0 {
        format!("{} min {} s", (s / 60.0).floor() as u64, (s % 60.0).round() as u64)
    } else {
        format!("{} s", s.round().max(1.0) as u64)
    }
}

fn render_tool(t: &ToolStep, colors: &[Color], opts: &RenderOpts, r: &mut Rendered, owner: Option<usize>) {
    let th = opts.theme;
    let depth = colors.len();
    let inner = opts.width.saturating_sub(2 * depth + 1);
    let _color = *colors.last().unwrap_or(&th.muted);
    let (marker, mcolor, state_text, state_color) = match &t.state {
        ToolState::Writing => ("◐", th.accent, format!("writing · {} chars", t.raw_args.len()), th.accent),
        ToolState::Running => ("◐", th.accent, "running".to_string(), th.accent),
        ToolState::Done => ("●", th.ok, "done".to_string(), th.ok),
        ToolState::Failed(l) => ("●", th.danger, l.clone(), th.danger),
        ToolState::NeedsApproval => ("◆", th.amber, "needs approval".to_string(), th.amber),
    };
    let is_handoff = t.name == "ask_agent";
    let mut spans = prefix(colors, th, Some((marker, mcolor)));
    if is_handoff {
        let who = t.sub.as_ref().map(|s| s.who.clone());
        spans.push(Span::styled("↦ handoff ", Style::default().fg(th.muted)));
        if let Some(w) = &who {
            spans.push(Span::styled(format!("{} {}", w.emoji, w.name), Style::default().fg(w.color).add_modifier(Modifier::BOLD)));
        } else {
            spans.push(Span::styled(t.args.get("agent").and_then(|a| a.as_str()).unwrap_or("…").to_string(), Style::default().add_modifier(Modifier::BOLD)));
        }
        spans.push(Span::styled(format!("  {}", arg_summary(&t.name, &t.args)), Style::default().fg(th.muted)));
    } else {
        spans.push(Span::styled(t.name.clone(), Style::default().fg(th.text).add_modifier(Modifier::BOLD)));
        let summary = if t.state == ToolState::Writing { one_line(&t.raw_args, 60) } else { arg_summary(&t.name, &t.args) };
        if !summary.is_empty() {
            spans.push(Span::styled(format!("  {summary}"), Style::default().fg(th.muted)));
        }
    }
    if let Some(reason) = &t.bypassed {
        spans.push(Span::styled(format!("  auto-approved{}", if reason == "run" { " (this run)" } else { "" }), Style::default().fg(th.amber)));
    }
    spans.push(Span::styled(format!("  {state_text}"), Style::default().fg(state_color)));
    r.push(Line::from(spans), owner);

    let show_body = opts.verbose || t.state == ToolState::NeedsApproval || t.live_out.is_some() || t.state == ToolState::Writing;
    if show_body {
        let detail: Vec<(String, String)> = match t.name.as_str() {
            "run_shell" => vec![("command".into(), t.args.get("command").and_then(|v| v.as_str()).unwrap_or("").to_string())],
            "write_file" => vec![(format!("content → {}", t.args.get("path").and_then(|v| v.as_str()).unwrap_or("")), t.args.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string())],
            "edit_file" => vec![("replace".into(), t.args.get("old_text").and_then(|v| v.as_str()).unwrap_or("").to_string()), ("with".into(), t.args.get("new_text").and_then(|v| v.as_str()).unwrap_or("").to_string())],
            "email_send" | "email_draft" => vec![("email".into(), format!("To: {}\nSubject: {}\n\n{}", t.args.get("to").and_then(|v| v.as_str()).unwrap_or(""), t.args.get("subject").and_then(|v| v.as_str()).unwrap_or(""), t.args.get("body").and_then(|v| v.as_str()).unwrap_or("")))],
            "ask_agent" => vec![("message".into(), t.args.get("message").or(t.args.get("task")).and_then(|v| v.as_str()).unwrap_or("").to_string())],
            _ if t.state == ToolState::Writing => vec![("arguments".into(), t.raw_args.clone())],
            _ => vec![],
        };
        for (label, text) in detail {
            if text.is_empty() {
                continue;
            }
            let mut s = prefix(colors, th, None);
            s.push(Span::styled(format!("  {label}"), Style::default().fg(th.faint)));
            r.push(Line::from(s), owner);
            for raw in text.lines().take(60) {
                for w in wrap_spans(&[Span::styled(raw.to_string(), Style::default().fg(th.code))], inner.saturating_sub(4)) {
                    let mut s = prefix(colors, th, None);
                    s.push(Span::styled("  │ ", Style::default().fg(th.faint)));
                    s.extend(w);
                    r.push(Line::from(s), owner);
                }
            }
        }
        if let Some(out) = &t.live_out {
            let mut s = prefix(colors, th, None);
            s.push(Span::styled("  output so far", Style::default().fg(th.faint)));
            r.push(Line::from(s), owner);
            for raw in out.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev() {
                let mut s = prefix(colors, th, None);
                s.push(Span::styled("  │ ", Style::default().fg(th.accent)));
                s.push(Span::styled(one_line(raw, inner.saturating_sub(4)), Style::default().fg(th.code)));
                r.push(Line::from(s), owner);
            }
        }
        if t.state == ToolState::NeedsApproval {
            let mut s = prefix(colors, th, None);
            s.push(Span::styled("  Let the agent do this?  ", Style::default().fg(th.amber).add_modifier(Modifier::BOLD)));
            s.push(Span::styled("y", Style::default().fg(th.ok).add_modifier(Modifier::BOLD)));
            s.push(Span::styled(" approve · ", Style::default().fg(th.muted)));
            s.push(Span::styled("a", Style::default().fg(th.ok).add_modifier(Modifier::BOLD)));
            s.push(Span::styled(" approve all this run · ", Style::default().fg(th.muted)));
            s.push(Span::styled("n", Style::default().fg(th.danger).add_modifier(Modifier::BOLD)));
            s.push(Span::styled(" deny", Style::default().fg(th.muted)));
            r.push(Line::from(s), owner);
        }
    }
    if let Some(sub) = &t.sub {
        if sub.continued {
            let mut s = prefix(colors, th, None);
            s.push(Span::styled("  ↩ continues its earlier conversation with this agent", Style::default().fg(th.accent)));
            r.push(Line::from(s), owner);
        }
        let mut nested = colors.to_vec();
        nested.push(sub.who.color);
        render_steps(&sub.steps, &nested, opts, r, owner);
    }
    if opts.verbose {
        if let Some(res) = &t.result {
            if !is_handoff {
                let mut s = prefix(colors, th, None);
                s.push(Span::styled("  result", Style::default().fg(th.faint)));
                r.push(Line::from(s), owner);
                for raw in res.lines().take(40) {
                    for w in wrap_spans(&[Span::styled(raw.to_string(), Style::default().fg(th.muted))], inner.saturating_sub(4)) {
                        let mut s = prefix(colors, th, None);
                        s.push(Span::styled("  │ ", Style::default().fg(th.faint)));
                        s.extend(w);
                        r.push(Line::from(s), owner);
                    }
                }
                if res.lines().count() > 40 {
                    let mut s = prefix(colors, th, None);
                    s.push(Span::styled(format!("  … {} more lines", res.lines().count() - 40), Style::default().fg(th.faint)));
                    r.push(Line::from(s), owner);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agents() -> Vec<Agent> {
        vec![
            Agent { id: "coder".into(), name: "Coder".into(), color: "#3fb27f".into(), emoji: "💻".into(), ..Default::default() },
            Agent { id: "orchestrator".into(), name: "Orchestrator".into(), color: "#f2a541".into(), emoji: "🧭".into(), ..Default::default() },
        ]
    }

    #[test]
    fn builds_history_with_nested_handoff() {
        let chat = json!({
            "agent_id": "orchestrator",
            "messages": [
                {"role": "user", "content": "do it", "_ts": 1.0},
                {"role": "assistant", "content": "", "reasoning_content": "plan", "tool_calls": [{"id": "c1", "function": {"name": "ask_agent", "arguments": "{\"agent\": \"Coder\", \"message\": \"build\"}"}}]},
                {"role": "tool", "tool_call_id": "c1", "content": "done!", "_sub": {"agent_id": "coder", "chat_id": "sub1", "messages": [
                    {"role": "assistant", "content": "", "tool_calls": [{"id": "s1", "function": {"name": "run_shell", "arguments": "{\"command\": \"ls\"}"}}]},
                    {"role": "tool", "tool_call_id": "s1", "content": "exit code 0\nfoo"},
                    {"role": "assistant", "content": "done!"}
                ]}},
                {"role": "assistant", "content": "All done.", "_stats": {"tok_per_s": 30.0, "prompt_tokens": 1000}}
            ]
        });
        let t = Trace::from_chat(&chat, &agents(), None);
        assert_eq!(t.entries.len(), 2);
        let Entry::Turn(turn) = &t.entries[1] else { panic!() };
        assert_eq!(turn.steps.len(), 3); // thinking, handoff, text
        let Step::Tool(tool) = &turn.steps[1] else { panic!() };
        let sub = tool.sub.as_ref().unwrap();
        assert_eq!(sub.who.name, "Coder");
        assert_eq!(sub.steps.len(), 2);
        assert!(matches!(&sub.steps[0], Step::Tool(x) if x.state == ToolState::Done));
        let th = Theme::dark();
        let out = render(&t, &RenderOpts { theme: &th, width: 80, verbose: true, selected_turn: None });
        let text: Vec<String> = out.lines.iter().map(|l| l.spans.iter().map(|s| s.content.to_string()).collect()).collect();
        assert!(text.iter().any(|l| l.contains("handoff") && l.contains("Coder")));
        assert!(text.iter().any(|l| l.contains("run_shell") && l.contains("done")));
        assert!(text.iter().any(|l| l.contains("All done.")));
    }

    #[test]
    fn live_events_build_the_same_shape() {
        let ag = agents();
        let mut t = Trace::empty();
        t.start_turn(Who::from(ag.iter().find(|a| a.id == "orchestrator"), "orchestrator"), 1);
        t.assistant_start(&[], &ag);
        t.delta(&[], "reasoning", "hmm", None, None, &ag);
        t.delta(&[], "tool_args", "{\"agent\": \"Co", Some(0), Some("ask_agent"), &ag);
        t.assistant_end(&[], &json!({"role": "assistant", "content": ""}), &ag);
        t.tool_start(&[], "c1", "ask_agent", json!({"agent": "Coder", "message": "build"}), &ag);
        t.subagent_start(&["c1".into()], "coder", "sub1", false, &ag);
        t.assistant_start(&["c1".into()], &ag);
        t.delta(&["c1".into()], "content", "working", None, None, &ag);
        t.tool_start(&["c1".into()], "s1", "run_shell", json!({"command": "ls"}), &ag);
        t.approval(&["c1".into()], "s1", Some("ap1"), None, &ag);
        assert_eq!(t.pending_approvals().len(), 1);
        t.approval(&["c1".into()], "s1", None, Some(true), &ag);
        t.tool_progress(&["c1".into()], "s1", "foo\n", &ag);
        t.tool_result(&["c1".into()], "s1", "exit code 0\nfoo", &ag);
        let turn = t.last_turn_mut().unwrap();
        let Step::Tool(tool) = &turn.steps[1] else { panic!("{:?}", turn.steps) };
        let sub = tool.sub.as_ref().unwrap();
        assert_eq!(sub.chat_id.as_deref(), Some("sub1"));
        assert!(matches!(&sub.steps[1], Step::Tool(x) if x.state == ToolState::Done && x.live_out.is_none()));
    }

    #[test]
    fn wrapping_keeps_words() {
        let w = wrap_spans(&[Span::raw("one two three four five".to_string())], 9);
        let lines: Vec<String> = w.iter().map(|l| l.iter().map(|s| s.content.to_string()).collect()).collect();
        assert_eq!(lines, vec!["one two ", "three ", "four five"]);
    }
}
