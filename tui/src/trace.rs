//! The conversation as a trace: entries (you, an agent's turn, a compaction divider), where a turn
//! is a list of steps (thinking, text, tool calls with nested handoffs). Built from a chat's saved
//! messages and updated live from the run's events, then rendered to styled, pre-wrapped lines.

use crate::api::Agent;
use crate::md;
use crate::text::{self, sanitize};
use crate::theme::{dim, hex, Theme};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

/// Under a reply that filled the context window: the words of the server's live notice (its
/// "notice" event, app/runner.py), so a chat reads the same live and once it is reloaded.
pub const CUT_OFF: &str = "The reply filled the context window before it finished. Continuing from where it stopped…";

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
            Some(a) => Who { id: a.id.clone(), name: text::line(&a.name), color: hex(&a.color).unwrap_or(Color::Gray), emoji: text::icon(&a.emoji) },
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
    /// Which trace this is: a new one (another chat, a reload) shares no rendered lines with the last.
    pub id: u64,
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
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Trace { entries: Vec::new(), id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) }
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
        steps.push(Step::Notice(CUT_OFF.into()));
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
                // 'run' (approve-all) or 'settings' (bypass on); older chats saved no reason
                tool.bypassed = Some(res.get("_bypass_reason").and_then(|r| r.as_str()).unwrap_or("settings").to_string());
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
                // this reply's thinking, still streaming or ended by its text or a call (`started`
                // is cleared once a reply's end has timed it): the server's measure wins over the
                // one taken here, which a replayed run (a chat opened mid-run) gets as about 0
                Step::Thinking { live, secs, started, text } if *live || started.is_some() => {
                    if *live && let Some(r) = message.get("reasoning_content").and_then(|r| r.as_str()) {
                        *text = r.to_string();
                    }
                    *live = false;
                    *secs = stats.think_s.or_else(|| started.map(|s| s.elapsed().as_secs_f64())).or(*secs);
                    *started = None;
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
    /// Every agent named in it as the agents are now (renamed, given a new icon, or deleted). It
    /// is drawn afresh after (its lines drawn so far named them the old way).
    pub fn refresh_agents(&mut self, agents: &[Agent]) {
        self.id = Trace::empty().id;
        fn again(w: &mut Who, agents: &[Agent]) {
            *w = Who::from(agents.iter().find(|a| a.id == w.id), &w.id.clone());
        }
        fn walk(steps: &mut [Step], agents: &[Agent]) {
            for st in steps {
                if let Step::Tool(ToolStep { sub: Some(sub), .. }) = st {
                    again(&mut sub.who, agents);
                    walk(&mut sub.steps, agents);
                }
            }
        }
        for e in self.entries.iter_mut() {
            match e {
                Entry::Turn(t) => {
                    again(&mut t.who, agents);
                    walk(&mut t.steps, agents);
                }
                Entry::User(UserEntry { from: Some(w), .. }) => again(w, agents),
                _ => {}
            }
        }
    }

    /// Every notice in the trace, handoffs' included.
    pub fn notices(&self) -> Vec<&str> {
        fn walk<'a>(steps: &'a [Step], out: &mut Vec<&'a str>) {
            for st in steps {
                match st {
                    Step::Notice(n) => out.push(n),
                    Step::Tool(ToolStep { sub: Some(sub), .. }) => walk(&sub.steps, out),
                    _ => {}
                }
            }
        }
        let mut out = vec![];
        for e in &self.entries {
            if let Entry::Turn(t) = e {
                walk(&t.steps, &mut out);
            }
        }
        out
    }

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

/// A running time, floored, as the web UI's footer and the export say it: 45s, 12m, 3h 12m, 2d 4h.
pub fn fmt_span(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h {}m", s / 3600, s / 60 % 60),
        _ => format!("{}d {}h", s / 86400, s / 3600 % 24),
    }
}

/// When a chat started: its `created`, else its first message's time, else when it last changed.
pub fn started(chat: &Value) -> Option<f64> {
    let first = || chat.get("messages")?.get(0)?.get("_ts")?.as_f64();
    chat.get("created").and_then(|x| x.as_f64()).or_else(first).or_else(|| chat.get("updated").and_then(|x| x.as_f64()))
}

/// "compacted 2×", from the chat's list as it is now (an edit or regenerate drops the entries
/// past it), or "not compacted yet".
pub fn compacted(chat: &Value) -> String {
    match chat.get("compactions").and_then(|c| c.as_array()).map_or(0, Vec::len) {
        0 => "not compacted yet".into(),
        n => format!("compacted {n}×"),
    }
}

/// The quiet line after an open chat's last turn: "compacted 2× · going for 3h 12m". Only once
/// the chat has a message (the chat list's stub of it, before the snapshot, has none).
pub fn footer(chat: &Value, now: f64) -> Option<String> {
    chat.get("messages").and_then(|m| m.as_array()).filter(|m| !m.is_empty())?;
    Some(format!("{} · going for {}", compacted(chat), fmt_span(now - started(chat).unwrap_or(now))))
}

/// The same facts as /usage and /context give them: "compacted 2× · started 09:14 (going for 3h 12m)".
pub fn chat_age(chat: &Value, now: f64) -> String {
    let start = started(chat).unwrap_or(now);
    format!("{} · started {} (going for {})", compacted(chat), clock(Some(start)), fmt_span(now - start))
}

/// `s` on one line of at most `n` columns, its whitespace squeezed, … where it was cut.
pub fn one_line(s: &str, n: usize) -> String {
    text::cut(&sanitize(s).split_whitespace().collect::<Vec<_>>().join(" "), n)
}

pub fn arg_summary(name: &str, a: &Value) -> String {
    let g = |k: &str| text::line(a.get(k).and_then(|v| v.as_str()).unwrap_or(""));
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

/// Wrap a line's spans to `width` columns, splitting on spaces (and inside long words). Only a
/// character wider than the whole width (a CJK one in one column) overflows it.
pub fn wrap_spans(spans: &[Span<'static>], width: usize) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1);
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
            let mut w = text::width(word);
            if *col + w > width && *col > 0 {
                out.push(vec![]);
                *col = 0;
                let trimmed = word.trim_start().to_string();
                w = text::width(&trimmed);
                *word = trimmed;
            }
            while w > width {
                // split by columns, so wide characters don't overflow the line
                let mut used = 0;
                let cut = word.char_indices().find(|(_, c)| {
                    used += text::width(c.encode_utf8(&mut [0; 4]));
                    used > width
                });
                let rest = word.split_off(cut.map(|(i, _)| i).unwrap_or(word.len()).max(word.chars().next().map(char::len_utf8).unwrap_or(0)));
                out.last_mut().unwrap().push(Span::styled(std::mem::replace(word, rest), style));
                out.push(vec![]);
                *col = 0;
                w = text::width(word);
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
    /// What these lines were drawn for: the trace, width, verbosity, theme, and the day (a clock
    /// label says "Oct 9, 14:02" once it isn't today).
    key: (u64, usize, bool, &'static str, chrono::NaiveDate),
    /// The line each entry starts at, and how many entries at the front are settled: the ones
    /// before the last turn, which a run never changes (its events land in the last turn, or add
    /// entries after it).
    starts: Vec<usize>,
    settled: usize,
    selected: Option<usize>,
}

pub struct RenderOpts<'a> {
    pub theme: &'a Theme,
    pub width: usize,
    pub verbose: bool,
    pub selected_turn: Option<usize>,
}

/// Render the whole trace into pre-wrapped lines (the app keeps what it can with `render_from`).
#[cfg(test)]
pub fn render(trace: &Trace, opts: &RenderOpts) -> Rendered {
    render_from(trace, opts, None)
}

/// `render`, keeping what `prev` already drew of the same trace: its settled entries stay, so a
/// streamed token re-renders the reply, not the whole history (which takes tens of milliseconds
/// a frame in a long chat). A changed selection redraws from the first turn it touches.
pub fn render_from(trace: &Trace, opts: &RenderOpts, prev: Option<Rendered>) -> Rendered {
    let key = (trace.id, opts.width, opts.verbose, opts.theme.name, chrono::Local::now().date_naive());
    let mut r = match prev.filter(|p| p.key == key) {
        Some(mut p) => {
            let mut keep = p.settled.min(trace.entries.len());
            if p.selected != opts.selected_turn {
                keep = [p.selected, opts.selected_turn].into_iter().flatten().fold(keep, usize::min);
            }
            let at = p.starts.get(keep).copied().unwrap_or(p.lines.len());
            p.lines.truncate(at);
            p.owners.truncate(at);
            p.starts.truncate(keep);
            p
        }
        None => Rendered { lines: vec![], owners: vec![], key, starts: vec![], settled: 0, selected: None },
    };
    r.selected = opts.selected_turn;
    r.settled = trace.entries.iter().rposition(|e| matches!(e, Entry::Turn(_))).unwrap_or(trace.entries.len());
    let th = opts.theme;
    for (i, e) in trace.entries.iter().enumerate().skip(r.starts.len()) {
        r.starts.push(r.lines.len());
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
                    r.push(Line::from(vec![bar.clone(), Span::styled(format!("🖼 {}", text::line(img)), Style::default().fg(th.muted))]), None);
                }
                for raw in sanitize(&u.text).lines() {
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
                    let text = sanitize(text);
                    let tail: String = if *live { text.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n") } else { text };
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
                let wrapped = wrap_spans(&[Span::styled(text::line(n), Style::default().fg(th.amber))], inner);
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
                for raw in sanitize(text).lines() {
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

/// Rounded once, so 119.6 s is "2 min", not "1 min 60 s".
fn fmt_secs(s: f64) -> String {
    let t = s.round().max(1.0) as u64;
    match (t / 60, t % 60) {
        (0, s) => format!("{s} s"),
        (m, 0) => format!("{m} min"),
        (m, s) => format!("{m} min {s} s"),
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
            spans.push(Span::styled(text::line(t.args.get("agent").and_then(|a| a.as_str()).unwrap_or("…")), Style::default().add_modifier(Modifier::BOLD)));
        }
        spans.push(Span::styled(format!("  {}", arg_summary(&t.name, &t.args)), Style::default().fg(th.muted)));
    } else {
        spans.push(Span::styled(text::line(&t.name), Style::default().fg(th.text).add_modifier(Modifier::BOLD)));
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
            s.push(Span::styled(format!("  {}", text::line(&label)), Style::default().fg(th.faint)));
            r.push(Line::from(s), owner);
            for raw in sanitize(&text).lines().take(60) {
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
            for raw in sanitize(out).lines().rev().take(12).collect::<Vec<_>>().into_iter().rev() {
                let mut s = prefix(colors, th, None);
                s.push(Span::styled("  │ ", Style::default().fg(th.accent)));
                s.push(Span::styled(one_line(raw, inner.saturating_sub(4)), Style::default().fg(th.code)));
                r.push(Line::from(s), owner);
            }
        }
        if t.state == ToolState::NeedsApproval {
            // Alt+key works from anywhere; the plain letter only with the trace focused (typed
            // into the message box or a filter, it is just a letter)
            let key = |k: &str, c| Span::styled(k.to_string(), Style::default().fg(c).add_modifier(Modifier::BOLD));
            let muted = |t: &str| Span::styled(t.to_string(), Style::default().fg(th.muted));
            let ask = vec![
                Span::styled("Let the agent do this?  ", Style::default().fg(th.amber).add_modifier(Modifier::BOLD)),
                key("Alt+Y", th.ok), muted(" approve · "), key("Alt+A", th.ok), muted(" approve all this run · "), key("Alt+N", th.danger), muted(" deny  "),
                Span::styled("(in the trace: y · a · n)", Style::default().fg(th.faint)),
            ];
            for w in wrap_spans(&ask, inner.saturating_sub(2)) {
                let mut s = prefix(colors, th, None);
                s.push(Span::raw("  "));
                s.extend(w);
                r.push(Line::from(s), owner);
            }
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
                let res = sanitize(res);
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

    #[test]
    fn wrapping_counts_columns_not_characters() {
        let w = wrap_spans(&[Span::raw("日本語のテキストです ok".to_string())], 8);
        let lines: Vec<String> = w.iter().map(|l| l.iter().map(|s| s.content.to_string()).collect()).collect();
        assert_eq!(lines, vec!["日本語の", "テキスト", "です ok"]);
        assert!(lines.iter().all(|l| crate::text::width(l) <= 8));
    }

    fn texts(r: &Rendered) -> Vec<String> {
        r.lines.iter().map(|l| l.spans.iter().map(|s| s.content.to_string()).collect()).collect()
    }

    fn opts(th: &Theme, width: usize, verbose: bool) -> RenderOpts<'_> {
        RenderOpts { theme: th, width, verbose, selected_turn: None }
    }

    fn long_chat(turns: usize) -> Value {
        let reply = "Here is **bold**, `code` and a list:\n\n- one\n- two with a [link](http://x.y)\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nA closing paragraph long enough to wrap at least once at a hundred columns, more or less, it goes on.";
        let mut msgs = vec![];
        for i in 0..turns {
            msgs.push(json!({"role": "user", "content": format!("question {i}")}));
            msgs.push(json!({"role": "assistant", "content": reply, "reasoning_content": "thinking about it"}));
        }
        json!({"agent_id": "coder", "messages": msgs})
    }

    #[test]
    fn a_render_that_reuses_the_last_one_matches_a_fresh_one() {
        let ag = agents();
        let th = Theme::dark();
        let mut t = Trace::from_chat(&long_chat(6), &ag, None);
        let o = opts(&th, 70, false);
        let mut r = render(&t, &o);
        let check = |t: &Trace, r: &Rendered, o: &RenderOpts, what: &str| {
            let fresh = render(t, o);
            assert_eq!(r.lines, fresh.lines, "{what}");
            assert_eq!(r.owners, fresh.owners, "{what}");
        };
        // a run streams into the last turn, adds entries, starts a new turn
        t.start_turn(Who::from(ag.first(), "coder"), 12);
        t.assistant_start(&[], &ag);
        for (i, piece) in ["Partial ", "**markdown", "** and\n\n```\ncode"].iter().enumerate() {
            t.delta(&[], "content", piece, None, None, &ag);
            r = render_from(&t, &o, Some(r));
            check(&t, &r, &o, &format!("after delta {i}"));
        }
        t.tool_start(&[], "c9", "run_shell", json!({"command": "ls"}), &ag);
        t.approval(&[], "c9", Some("ap"), None, &ag);
        r = render_from(&t, &o, Some(r));
        check(&t, &r, &o, "an approval");
        t.entries.push(Entry::User(user_from_event(&json!({"role": "user", "content": "queued one", "_queued": true}), 14)));
        t.start_turn(Who::from(ag.first(), "coder"), 15);
        t.delta(&[], "content", "next turn", None, None, &ag);
        r = render_from(&t, &o, Some(r));
        check(&t, &r, &o, "a queued message and the turn after it");
        // selecting an early turn, and moving off it again, redraws its header
        for sel in [Some(1), Some(3), None] {
            let o = RenderOpts { selected_turn: sel, ..opts(&th, 70, false) };
            r = render_from(&t, &o, Some(r));
            check(&t, &r, &o, &format!("selection {sel:?}"));
        }
        // anything else that changes how it all looks starts over
        let light = Theme::light();
        for o in [opts(&th, 50, false), opts(&th, 50, true), opts(&light, 50, true)] {
            r = render_from(&t, &o, Some(r));
            check(&t, &r, &o, "another width, verbosity or theme");
        }
        // and so does another trace, even one with the same number of entries
        let other = Trace::from_chat(&json!({"agent_id": "orchestrator", "messages": [{"role": "user", "content": "different"}]}), &ag, None);
        let o = opts(&th, 50, true);
        r = render_from(&other, &o, Some(r));
        check(&other, &r, &o, "another trace");
    }

    #[test]
    fn a_streamed_token_redraws_the_reply_not_the_history() {
        let ag = agents();
        let th = Theme::dark();
        let mut t = Trace::from_chat(&long_chat(2000), &ag, None);
        t.start_turn(Who::from(ag.first(), "coder"), 4000);
        t.delta(&[], "content", "streaming", None, None, &ag);
        let o = opts(&th, 100, false);
        let start = std::time::Instant::now();
        let mut r = render(&t, &o);
        let full = start.elapsed();
        let start = std::time::Instant::now();
        for _ in 0..10 {
            t.delta(&[], "content", " more **words**", None, None, &ag);
            r = render_from(&t, &o, Some(r));
        }
        let each = start.elapsed() / 10;
        eprintln!("2000 turns, {} lines: full render {full:?}, each token after {each:?}", r.lines.len());
        // (a full render of this chat takes about 40 ms in a release build, 200 ms in a debug one)
        assert!(each < std::time::Duration::from_millis(50) && each * 4 < full, "a token re-rendered too much: {each:?} vs {full:?} for everything");
        assert_eq!(texts(&r).iter().rev().find(|l| l.contains("streaming")).map(|l| l.contains("more words more words")), Some(true));
    }

    #[test]
    fn helpers_format_numbers_times_and_one_liners() {
        for (n, want) in [(0, "0"), (999, "999"), (1000, "1k"), (1500, "1.5k"), (85000, "85k"), (1_234_567, "1234.6k")] {
            assert_eq!(fmt_k(n), want);
        }
        for (s, want) in [(0.2, "1 s"), (1.4, "1 s"), (59.4, "59 s"), (59.6, "1 min"), (61.0, "1 min 1 s"), (119.6, "2 min"), (3725.0, "62 min 5 s")] {
            assert_eq!(fmt_secs(s), want, "{s}");
        }
        assert_eq!(one_line("a\n  b\tc", 10), "a b c");
        assert_eq!(one_line("abcdef", 4), "abc…");
        assert_eq!(one_line("日本語のテキスト", 7), "日本語…", "cut by columns");
        assert!(text::width(&one_line(&"字".repeat(50), 9)) <= 9);
        assert_eq!(one_line("✍\u{FE0F} note", 20), "✍ note");
        let now = super::now();
        assert!(clock(Some(now)).len() == 5 && clock(Some(now)).contains(':'));
        assert!(clock(Some(now - 3.0 * 86400.0)).contains(','));
        assert_eq!(clock(None), "");
    }

    #[test]
    fn the_footer_counts_compactions_and_how_long_the_chat_has_gone() {
        // floored, the web UI's and the export's way: 45s, 12m, 3h 12m, 2d 4h
        for (s, want) in [(-5.0, "0s"), (0.0, "0s"), (45.0, "45s"), (59.9, "59s"), (60.0, "1m"), (720.0, "12m"), (3599.0, "59m"), (3600.0, "1h 0m"), (11_520.0, "3h 12m"),
                          (86_399.0, "23h 59m"), (86_400.0, "1d 0h"), (187_200.0, "2d 4h"), (40.0 * 86_400.0 + 3599.0, "40d 0h"), (f64::NAN, "0s")] {
            assert_eq!(fmt_span(s), want, "{s}");
        }
        let t0 = 1_760_000_000.0;
        let chat = |comps: usize| json!({ "id": "c", "created": t0, "updated": t0 + 50.0, "messages": [{ "role": "user", "content": "hi", "_ts": t0 + 5.0 }], "compactions": vec![json!({ "upto": 1 }); comps] });
        assert_eq!(footer(&chat(0), t0 + 240.0).as_deref(), Some("not compacted yet · going for 4m"));
        assert_eq!(footer(&chat(1), t0 + 45.0).as_deref(), Some("compacted 1× · going for 45s"));
        assert_eq!(footer(&chat(3), t0 + 11_530.0).as_deref(), Some("compacted 3× · going for 3h 12m"));
        assert_eq!(footer(&chat(2), t0 + 187_200.0).as_deref(), Some("compacted 2× · going for 2d 4h"));
        // no list at all reads as none; only once the chat has a message (not the chat list's stub)
        assert_eq!(footer(&json!({ "created": t0, "messages": [{ "role": "user" }] }), t0 + 1.0).as_deref(), Some("not compacted yet · going for 1s"));
        assert_eq!(footer(&json!({ "created": t0, "messages": [], "compactions": [{}] }), t0), None);
        assert_eq!(footer(&json!({ "id": "c", "title": "t", "agent_id": "a" }), t0), None);
        // started: created, else the first message's time, else when it last changed
        assert_eq!(started(&chat(0)), Some(t0));
        let mut c = chat(0);
        c.as_object_mut().unwrap().remove("created");
        assert_eq!(started(&c), Some(t0 + 5.0));
        c["messages"][0].as_object_mut().unwrap().remove("_ts");
        assert_eq!(started(&c), Some(t0 + 50.0));
        // /usage and /context: the start time as the trace shows times, then how long
        assert_eq!(chat_age(&chat(2), t0 + 11_530.0), format!("compacted 2× · started {} (going for 3h 12m)", clock(Some(t0))));
        let now = super::now();
        let today = json!({ "created": now - 240.0, "messages": [] });
        assert_eq!(chat_age(&today, now), format!("not compacted yet · started {} (going for 4m)", clock(Some(now - 240.0))));
    }

    #[test]
    fn tool_states_come_from_the_result() {
        for (name, result, want) in [
            ("run_shell", "exit code 0\nok", ToolState::Done),
            ("run_shell", "exit code 2\nno", ToolState::Failed("exit 2".into())),
            ("run_shell", "exit code -9", ToolState::Failed("exit -9".into())),
            ("web_fetch", "Error: 404", ToolState::Failed("failed".into())),
            ("run_shell", "The user denied this", ToolState::Failed("denied".into())),
            ("run_shell", "The user did not approve", ToolState::Failed("denied".into())),
            ("run_shell", "[cancelled by the user]", ToolState::Failed("cancelled".into())),
            ("run_shell", "Command timed out after 1 s", ToolState::Failed("timed out".into())),
            ("web_fetch", "Command timed out", ToolState::Done),
            ("read_file", "plain text", ToolState::Done),
        ] {
            assert_eq!(tool_state_for(name, result), want, "{name}: {result}");
        }
    }

    #[test]
    fn every_tool_gets_a_one_line_summary() {
        for (name, args, want) in [
            ("run_shell", json!({"command": "ls\n-la"}), "ls -la"),
            ("web_search", json!({"query": "rust tui"}), "rust tui"),
            ("email_search", json!({"query": "invoice"}), "invoice"),
            ("recall_memory", json!({"query": "name"}), "name"),
            ("web_fetch", json!({"url": "http://a.b"}), "http://a.b"),
            ("browser_navigate", json!({"url": "http://c.d"}), "http://c.d"),
            ("browser_click", json!({"text": "Sign in", "selector": "#x"}), "Sign in"),
            ("browser_click", json!({"selector": "#x"}), "#x"),
            ("browser_type", json!({"label": "Email", "text": "me@x.y"}), "Email: me@x.y"),
            ("browser_type", json!({"selector": "#q", "text": "hi"}), "#q: hi"),
            ("list_dir", json!({}), "."),
            ("list_dir", json!({"path": "src"}), "src"),
            ("remember", json!({"fact": "likes tea"}), "likes tea"),
            ("forget_memory", json!({"id": 7}), "#7"),
            ("email_read", json!({"uid": "42"}), "#42"),
            ("email_list", json!({"unread_only": true}), "INBOX · unread"),
            ("email_list", json!({"folder": "Sent"}), "Sent"),
            ("email_send", json!({"to": "a@b.c", "subject": "Hi"}), "a@b.c · Hi"),
            ("email_draft", json!({"to": "a@b.c", "subject": "Re"}), "a@b.c · Re"),
            ("ask_agent", json!({"agent": "Coder", "message": "build it"}), "build it"),
            ("ask_agent", json!({"agent": "Coder", "task": "old style"}), "old style"),
            ("calendar_events", json!({"query": "dentist", "days": 10}), "“dentist” · today · 10 days"),
            ("calendar_events", json!({"start": "2026-10-11"}), "2026-10-11"),
            ("write_file", json!({"path": "a.py", "content": "x"}), "a.py"),
            ("mystery", json!({"k": 1}), "{\"k\":1}"),
            ("run_shell", json!({"_raw": "{bad"}), "(invalid arguments)"),
        ] {
            assert_eq!(arg_summary(name, &args), want, "{name}");
        }
    }

    #[test]
    fn dividers_cancelled_tools_and_verbose_results() {
        let ag = agents();
        let th = Theme::dark();
        let mut msgs = vec![json!({"role": "user", "content": "hi"}), json!({"role": "assistant", "content": "", "tool_calls": [{"id": "n", "function": {"name": "run_shell", "arguments": "{\"command\": \"sleep 9\"}"}}]})];
        let out: String = (1..=50).map(|i| format!("{i}\n")).collect();
        msgs.push(json!({"role": "assistant", "content": "", "tool_calls": [{"id": "s", "function": {"name": "run_shell", "arguments": "{\"command\": \"seq 50\"}"}}]}));
        msgs.push(json!({"role": "tool", "tool_call_id": "s", "content": format!("exit code 0\n{out}")}));
        let mut chat = json!({"agent_id": "coder", "messages": msgs});
        for (reason, why) in [("auto", "automatically"), ("manual", "on request"), ("overflow", "because the context was full"), ("cutoff", "because a reply filled the context")] {
            chat["compactions"] = json!([{"upto": 1, "summary": "the gist", "reason": reason, "before_tokens": 9000, "after_tokens": 1200}]);
            let t = Trace::from_chat(&chat, &ag, None);
            let lines = texts(&render(&t, &opts(&th, 80, false)));
            assert!(lines.contains(&format!("── earlier messages summarized {why} · 9k → 1.2k tokens ──")), "{lines:?}");
        }
        chat["compactions"] = json!([{"upto": 1, "summary": "the gist", "reason": "manual"}]);
        let t = Trace::from_chat(&chat, &ag, None);
        let quiet = texts(&render(&t, &opts(&th, 80, false)));
        assert!(quiet.contains(&"── earlier messages summarized on request ──".to_string()));
        // a saved call that never got its result was cut short
        assert!(quiet.iter().any(|l| l.contains("run_shell  sleep 9  cancelled")), "{quiet:?}");
        assert!(!quiet.iter().any(|l| l.contains("result") || l.contains("the gist")));
        // verbose: the summary, and results up to 40 lines
        let loud = texts(&render(&t, &opts(&th, 80, true)));
        assert!(loud.iter().any(|l| l.contains("the gist")));
        assert!(loud.iter().any(|l| l.ends_with("  result")));
        assert!(loud.iter().any(|l| l.ends_with("│ 39")) && !loud.iter().any(|l| l.ends_with("│ 40")));
        assert!(loud.iter().any(|l| l.ends_with("… 11 more lines")), "exit line + 50 numbers = 51 lines");
    }

    #[test]
    fn bodies_fit_their_width_at_any_depth() {
        let ag = agents();
        let th = Theme::dark();
        let url = format!("http://example.com/{}", "x".repeat(280));
        let mut t = Trace::from_chat(&json!({"agent_id": "coder", "messages": [{"role": "user", "content": format!("{url}\n\nafter a blank")}]}), &ag, None);
        t.start_turn(Who::from(ag.first(), "coder"), 1);
        t.delta(&[], "content", &"你好".repeat(40), None, None, &ag);
        // a handoff four deep, each with a reply and thinking
        let mut path: Vec<String> = vec![];
        for d in 0..4 {
            let id = format!("h{d}");
            t.tool_start(&path, &id, "ask_agent", json!({"agent": "Coder", "message": "go"}), &ag);
            path.push(id);
            t.delta(&path, "reasoning", "thinking it through at length, more words than fit", None, None, &ag);
            t.delta(&path, "content", &format!("depth {d} says a fair amount of text here too 日本語"), None, None, &ag);
        }
        for width in [12, 20, 30, 40] {
            let r = render(&t, &opts(&th, width, true));
            let lines = texts(&r);
            // the wrapped text (one-line headers and labels, a turn's or a tool's, may run on: the
            // pane cuts them)
            for l in lines.iter().filter(|l| ["depth", "says", "amount", "through", "words", "fit", "日", "本", "語", "你", "after", "xxx"].iter().any(|w| l.contains(w))) {
                assert!(text::width(l) <= width, "{l:?} is wider than {width}");
            }
            if width == 40 {
                let joined: String = lines.iter().filter(|l| l.starts_with("▎ ")).map(|l| l.trim_start_matches("▎ ")).collect::<Vec<_>>().join("");
                assert!(joined.starts_with(&url), "the long word comes back whole: {joined:?}");
                assert_eq!(lines.iter().filter(|l| *l == "▎ ").count(), 1, "the blank line between kept");
            }
            let chars: usize = lines.iter().map(|l| l.matches('你').count()).sum();
            assert_eq!(chars, 40, "every wide character kept at {width}");
        }
        for w in [0, 1, 3] {
            let out = wrap_spans(&[Span::raw("日本 word abcdefgh".to_string())], w);
            let back: String = out.iter().flatten().map(|s| s.content.to_string()).collect();
            assert_eq!(back.replace(' ', ""), "日本wordabcdefgh", "at {w}");
        }
    }

    #[test]
    fn live_steps_show_what_is_happening() {
        let ag = agents();
        let th = Theme::dark();
        let mut t = Trace::empty();
        t.start_turn(Who::from(ag.first(), "coder"), 1);
        t.assistant_start(&[], &ag);
        assert!(texts(&render(&t, &opts(&th, 60, false))).iter().any(|l| l.ends_with("working…")));
        let thought: String = (1..=10).map(|i| format!("Step {i}\n")).collect();
        t.delta(&[], "reasoning", &thought, None, None, &ag);
        let lines = texts(&render(&t, &opts(&th, 60, false)));
        assert!(!lines.iter().any(|l| l.ends_with("working…")));
        let steps: Vec<&String> = lines.iter().filter(|l| l.contains("Step ")).collect();
        assert_eq!(steps.len(), 6, "only the last six lines while it thinks: {lines:?}");
        assert!(steps[5].ends_with("Step 10") && steps[0].ends_with("Step 5"));
        // a call still being written shows its arguments; done, it goes
        t.delta(&[], "tool_args", "{\"query\": \"rust t", Some(0), Some("web_search"), &ag);
        let lines = texts(&render(&t, &opts(&th, 80, false)));
        assert!(lines.iter().any(|l| l.contains("web_search") && l.contains("writing · 17 chars")), "{lines:?}");
        assert!(lines.iter().any(|l| l.ends_with("  arguments")));
        t.assistant_end(&[], &json!({"content": ""}), &ag);
        assert!(!texts(&render(&t, &opts(&th, 80, false))).iter().any(|l| l.contains("writing")));
        // a body is capped at 60 lines; live output shows the last 12
        let content: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        t.tool_start(&[], "w", "write_file", json!({"path": "big.txt", "content": content}), &ag);
        let lines = texts(&render(&t, &opts(&th, 80, true)));
        assert!(lines.iter().any(|l| l.ends_with("content → big.txt")));
        assert_eq!(lines.iter().filter(|l| l.contains("│ line ")).count(), 60);
        t.tool_start(&[], "r", "run_shell", json!({"command": "seq 20"}), &ag);
        t.tool_progress(&[], "r", &(1..=20).map(|i| format!("out{i}")).collect::<Vec<_>>().join("\n"), &ag);
        let lines = texts(&render(&t, &opts(&th, 80, false)));
        let out: Vec<&String> = lines.iter().filter(|l| l.contains("│ out")).collect();
        assert_eq!(out.len(), 12);
        assert!(out[0].ends_with("out9") && out[11].ends_with("out20"));
    }

    #[test]
    fn agent_icons_keep_names_in_one_column() {
        let ag = [
            Agent { id: "w".into(), name: "Writer".into(), emoji: "✍\u{FE0F}".into(), ..Default::default() },
            Agent { id: "c".into(), name: "Coder".into(), emoji: "💻".into(), ..Default::default() },
        ];
        let th = Theme::dark();
        let mut t = Trace::empty();
        for id in ["w", "c", "gone"] {
            t.start_turn(Who::from(ag.iter().find(|a| a.id == id), id), 0);
            if let Some(Entry::Turn(turn)) = t.entries.last_mut() {
                turn.ts = None;
            }
        }
        let heads: Vec<String> = texts(&render(&t, &opts(&th, 60, false))).into_iter().filter(|l| !l.is_empty()).collect();
        assert_eq!(heads, vec!["✍  Writer", "💻 Coder", "❔ Deleted agent"]);
        for h in &heads {
            let name = h.trim_start_matches(|c: char| !c.is_ascii_alphabetic());
            assert_eq!(text::width(&h[..h.len() - name.len()]), 3, "the name starts in column 3: {h:?}");
        }
    }

    fn turn(t: &Trace) -> &Turn {
        t.entries.iter().rev().find_map(|e| if let Entry::Turn(t) = e { Some(t) } else { None }).expect("a turn")
    }

    fn sub_steps<'a>(steps: &'a [Step], call: &str) -> &'a [Step] {
        steps.iter().find_map(|s| match s {
            Step::Tool(t) if t.call_id == call => t.sub.as_ref().map(|s| s.steps.as_slice()),
            _ => None,
        }).unwrap_or_else(|| panic!("no handoff {call} in {steps:?}"))
    }

    fn kinds(steps: &[Step]) -> Vec<String> {
        steps.iter().map(|s| match s {
            Step::Thinking { live, .. } => format!("thinking{}", if *live { "…" } else { "" }),
            Step::Text { live, .. } => format!("text{}", if *live { "…" } else { "" }),
            Step::Tool(t) => format!("tool:{}:{:?}", t.name, t.state),
            Step::Notice(_) => "notice".into(),
            Step::Error { .. } => "error".into(),
            Step::Stopped => "stopped".into(),
            Step::Waiting => "waiting".into(),
            Step::Compacting { chars } => format!("compacting:{chars}"),
        }).collect()
    }

    #[test]
    fn every_event_kind_folds_into_its_step() {
        let ag = agents();
        let mut t = Trace::empty();
        // before a turn exists (a stream joining late), events have nowhere to go and change nothing
        t.assistant_start(&[], &ag);
        t.delta(&[], "content", "x", None, None, &ag);
        t.tool_start(&[], "c0", "run_shell", json!({}), &ag);
        t.notice(&[], "n", &ag);
        t.error("e", 0);
        t.compact_start(&[], &ag);
        assert!(t.entries.is_empty());

        t.start_turn(Who::from(ag.first(), "coder"), 1);
        t.assistant_start(&[], &ag);
        assert_eq!(kinds(&turn(&t).steps), ["waiting"]);
        t.delta(&[], "reasoning", "let me ", None, None, &ag);
        t.delta(&[], "reasoning", "think", None, None, &ag);
        assert_eq!(kinds(&turn(&t).steps), ["thinking…"], "the waiting mark gives way, pieces join");
        t.delta(&[], "content", "Here", None, None, &ag);
        t.delta(&[], "content", " it is", None, None, &ag);
        assert_eq!(kinds(&turn(&t).steps), ["thinking", "text…"], "text ends the thinking");
        // two calls written at once, by index; the name arrives with the first piece or later
        t.delta(&[], "tool_args", "{\"comm", Some(0), None, &ag);
        t.delta(&[], "tool_args", "{\"path\"", Some(1), Some("read_file"), &ag);
        t.delta(&[], "tool_args", "and\": 1}", Some(0), Some("run_shell"), &ag);
        t.delta(&[], "tool_args", "", Some(0), Some(""), &ag);
        let steps = &turn(&t).steps;
        assert_eq!(kinds(steps), ["thinking", "text…", "tool:run_shell:Writing", "tool:read_file:Writing"]);
        assert!(matches!(&steps[2], Step::Tool(x) if x.raw_args == "{\"command\": 1}"), "{steps:?}");
        // an unknown kind (or a compaction's, which the app routes to compact_delta) is no step
        t.delta(&[], "mystery", "?", None, None, &ag);
        assert_eq!(turn(&t).steps.len(), 4);
        // the reply ends: drafts go, the saved message's text and thinking win, its stats show
        t.assistant_end(&[], &json!({ "content": "Here it is, final", "reasoning_content": "let me think, saved", "_stats": { "think_s": 4.4, "tok_per_s": 12.0, "prompt_tokens": 900, "completion_tokens": 20 } }), &ag);
        let tt = turn(&t);
        assert_eq!(kinds(&tt.steps), ["thinking", "text"]);
        assert!(matches!(&tt.steps[1], Step::Text { text, .. } if text == "Here it is, final"));
        assert!(matches!(&tt.steps[0], Step::Thinking { text, .. } if text == "let me think"), "thinking that already ended keeps what streamed");
        // but takes the server's measure of how long it took (the reply's end times it once)
        assert!(matches!(&tt.steps[0], Step::Thinking { secs: Some(s), started: None, .. } if *s == 4.4), "{:?}", tt.steps[0]);
        assert_eq!((tt.stats.tok_per_s, tt.stats.prompt, tt.stats.completion), (Some(12.0), Some(900), Some(20)));
        // tools: start, live output, approval asked and answered, bypassed, result
        t.tool_start(&[], "c1", "run_shell", json!({ "command": "make" }), &ag);
        t.tool_start(&[], "c2", "web_fetch", json!({ "url": "http://a.b" }), &ag);
        t.tool_progress(&[], "c1", "building…", &ag);
        t.approval(&[], "c2", Some("ap2"), None, &ag);
        assert_eq!(t.pending_approvals(), vec![("ap2".to_string(), "web_fetch".to_string(), "http://a.b".to_string())]);
        t.approval(&[], "c2", None, Some(false), &ag);
        assert!(t.pending_approvals().is_empty());
        t.bypassed(&[], "c1", "run", &ag);
        // events for calls that don't exist (or a path that leads nowhere) change nothing
        t.tool_result(&[], "nope", "x", &ag);
        t.tool_progress(&[], "nope", "x", &ag);
        t.approval(&[], "nope", Some("x"), None, &ag);
        t.notice(&["nope".into()], "lost", &ag);
        t.subagent_start(&["nope".into()], "coder", "s", false, &ag);
        t.subagent_start(&[], "coder", "s", false, &ag);
        t.tool_result(&[], "c1", "exit code 0\nok", &ag);
        let tt = turn(&t);
        assert_eq!(kinds(&tt.steps), ["thinking", "text", "tool:run_shell:Done", "tool:web_fetch:Failed(\"denied\")"]);
        let Step::Tool(c1) = &tt.steps[2] else { panic!() };
        assert_eq!((c1.live_out.as_deref(), c1.result.as_deref(), c1.bypassed.as_deref()), (None, Some("exit code 0\nok"), Some("run")));
        // notices, errors, and a compaction that counts what it writes
        t.notice(&[], "heads up", &ag);
        t.compact_start(&[], &ag);
        t.compact_delta(&[], 1200, &ag);
        t.compact_delta(&[], 34, &ag);
        assert_eq!(kinds(&turn(&t).steps)[5], "compacting:1234");
        t.compact_end(&[], &json!({ "before_tokens": 9000, "after_tokens": 1200 }), &ag);
        t.compact_start(&[], &ag);
        t.compact_end(&[], &json!({}), &ag);
        t.error("boom", 7);
        let tt = turn(&t);
        assert_eq!(kinds(&tt.steps)[4..], ["notice", "notice", "notice", "error"]);
        assert!(matches!(&tt.steps[5], Step::Notice(n) if n == "Earlier messages summarized to free up context (9k → 1.2k tokens)."));
        assert!(matches!(&tt.steps[6], Step::Notice(n) if n == "Earlier messages summarized to free up context."));
        assert!(matches!(&tt.steps[7], Step::Error { text, index: 7 } if text == "boom"));
    }

    #[test]
    fn handoffs_nest_by_path_at_any_depth() {
        let ag = agents();
        let mut t = Trace::empty();
        t.start_turn(Who::from(ag.get(1), "orchestrator"), 1);
        // the call's arguments name the agent before the handoff starts; its sub-steps follow the path
        t.tool_start(&[], "h1", "ask_agent", json!({ "agent": "coder", "message": "build" }), &ag);
        t.assistant_start(&["h1".into()], &ag);
        assert_eq!(kinds(sub_steps(&turn(&t).steps, "h1")), ["waiting"], "a sub-step before subagent_start makes the sub from the arguments");
        t.subagent_start(&["h1".into()], "coder", "sub-1", true, &ag);
        t.delta(&["h1".into()], "content", "on it", None, None, &ag);
        t.tool_start(&["h1".into()], "h2", "ask_agent", json!({ "agent": "Orchestrator", "task": "old style" }), &ag);
        t.subagent_start(&["h1".into(), "h2".into()], "orchestrator", "sub-2", false, &ag);
        let deep: Vec<String> = vec!["h1".into(), "h2".into()];
        t.tool_start(&deep, "s1", "run_shell", json!({ "command": "ls" }), &ag);
        t.approval(&deep, "s1", Some("ap-deep"), None, &ag);
        t.tool_start(&[], "s0", "run_shell", json!({ "command": "pwd" }), &ag);
        t.approval(&[], "s0", Some("ap-top"), None, &ag);
        // in step order, depth first: the handoff came first, so the call two handoffs down
        assert_eq!(t.pending_approvals().iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), ["ap-deep", "ap-top"]);
        t.approval(&deep, "s1", None, Some(true), &ag);
        t.tool_result(&deep, "s1", "exit code 3\nno", &ag);
        t.tool_result(&["h1".into()], "h2", "Error: depth", &ag);
        let steps = &turn(&t).steps;
        let Step::Tool(h1) = &steps[0] else { panic!() };
        let sub = h1.sub.as_ref().unwrap();
        assert_eq!((sub.who.name.as_str(), sub.chat_id.as_deref(), sub.continued), ("Coder", Some("sub-1"), true));
        let two = sub_steps(&sub.steps, "h2");
        assert_eq!(kinds(two), ["tool:run_shell:Failed(\"exit 3\")"]);
        let Step::Tool(h2) = &sub.steps[1] else { panic!() };
        assert_eq!(h2.result.as_deref(), Some("Error: depth"), "a failed handoff keeps its error to show");
        assert_eq!(h2.sub.as_ref().unwrap().who.name, "Orchestrator");
        // drawn: each level behind one more bar in its agent's color, the handoff names who
        let th = Theme::dark();
        let lines = texts(&render(&t, &opts(&th, 80, false)));
        assert!(lines.iter().any(|l| l.contains("↦ handoff 💻 Coder  build")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("↩ continues its earlier conversation")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("│ │ ● run_shell  ls  exit 3") || l.starts_with("│ │ ●") && l.contains("exit 3")), "{lines:?}");
    }

    #[test]
    fn pending_approvals_list_in_step_order() {
        let ag = agents();
        let mut t = Trace::empty();
        t.start_turn(Who::from(ag.first(), "coder"), 1);
        t.tool_start(&[], "h", "ask_agent", json!({ "agent": "Coder", "message": "go" }), &ag);
        t.tool_start(&["h".into()], "in", "run_shell", json!({ "command": "inner" }), &ag);
        t.approval(&["h".into()], "in", Some("ap-in"), None, &ag);
        t.tool_start(&[], "out", "email_send", json!({ "to": "a@b.c", "subject": "Hi" }), &ag);
        t.approval(&[], "out", Some("ap-out"), None, &ag);
        // (steps in order: the handoff came first, so its inner call is listed before the later one)
        let got: Vec<(String, String, String)> = t.pending_approvals();
        assert_eq!(got.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), ["ap-in", "ap-out"]);
        assert_eq!(got[1].2, "a@b.c · Hi");
        // the handoff's own approval comes before its sub's
        t.approval(&[], "h", Some("ap-h"), None, &ag);
        assert_eq!(t.pending_approvals().iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), ["ap-h", "ap-in", "ap-out"]);
    }

    #[test]
    fn a_saved_approve_all_keeps_its_reason() {
        let ag = agents();
        let call = |id: &str| json!({ "id": id, "function": { "name": "run_shell", "arguments": "{\"command\": \"ls\"}" } });
        let chat = json!({ "agent_id": "coder", "messages": [
            { "role": "user", "content": "go" },
            { "role": "assistant", "content": "", "tool_calls": [call("t1"), call("t2"), call("t3")] },
            { "role": "tool", "tool_call_id": "t1", "content": "ok", "_bypassed": true, "_bypass_reason": "run" },
            { "role": "tool", "tool_call_id": "t2", "content": "ok", "_bypassed": true, "_bypass_reason": "settings" },
            { "role": "tool", "tool_call_id": "t3", "content": "ok", "_bypassed": true },
        ] });
        let t = Trace::from_chat(&chat, &ag, None);
        let Entry::Turn(turn) = &t.entries[1] else { panic!("{:?}", t.entries.len()) };
        let reasons: Vec<Option<String>> = turn.steps.iter().filter_map(|s| match s { Step::Tool(t) => Some(t.bypassed.clone()), _ => None }).collect();
        assert_eq!(reasons, [Some("run".to_string()), Some("settings".to_string()), Some("settings".to_string())], "older chats saved no reason");
    }

    #[test]
    fn history_rebuilds_every_kind_of_saved_message() {
        let ag = agents();
        let chat = json!({
            "agent_id": "coder",
            "parent": { "root_chat_id": "r", "caller_id": "orchestrator" },
            "compactions": [{ "upto": 4, "summary": "s", "reason": "auto", "before_tokens": 5000, "after_tokens": 700 }],
            "messages": [
                { "role": "user", "content": "first", "_ts": 10.0, "_images": ["uploads/a.png", 7], "_recall": "fact one\n\nfact two\n" },
                { "role": "assistant", "content": "", "tool_calls": [
                    { "id": "t1", "function": { "name": "run_shell", "arguments": "{\"command\": \"ls\"}" } },
                    { "id": "t2", "function": { "name": "web_fetch", "arguments": "{not json" } },
                    { "id": "t3", "function": { "name": "run_shell", "arguments": "{\"command\": \"rm -rf /\"}" } },
                ], "_ts": 11.0 },
                { "role": "tool", "tool_call_id": "t1", "content": "exit code 0\nok", "_bypassed": true },
                { "role": "tool", "tool_call_id": "t2", "content": "Error: tool arguments were not valid JSON" },
                { "role": "assistant", "content": "partial", "reasoning_content": "hm", "_stopped": true, "_stats": { "think_s": 2.0 } },
                { "role": "user", "content": "aside", "_queued": true },
                { "role": "assistant", "content": "cut", "_truncated": true },
                { "role": "assistant", "content": "rest", "_stats": { "tok_per_s": 5.5, "prompt_tokens": 2000, "completion_tokens": 9 } },
                { "role": "assistant", "content": "", "_error": "Can't reach the model server" },
                { "role": "system", "content": "ignored" },
            ],
        });
        let t = Trace::from_chat(&chat, &ag, None);
        let shape: Vec<String> = t.entries.iter().map(|e| match e {
            Entry::User(u) => format!("user {} {}", u.index, u.text),
            Entry::Turn(t) => format!("turn {}..{} {:?}", t.start, t.end, kinds(&t.steps)),
            Entry::Divider { reason, .. } => format!("divider {reason}"),
        }).collect();
        assert_eq!(shape, [
            "user 0 first",
            "turn 1..2 [\"tool:run_shell:Done\", \"tool:web_fetch:Failed(\\\"failed\\\")\", \"tool:run_shell:Failed(\\\"cancelled\\\")\"]",
            "divider auto",
            "turn 4..5 [\"thinking\", \"text\", \"stopped\"]",
            "user 5 aside",
            "turn 6..9 [\"text\", \"notice\", \"text\", \"error\"]",
        ]);
        let Entry::User(u) = &t.entries[0] else { panic!() };
        assert_eq!((u.images.clone(), u.recalled, u.queued, u.from.as_ref().map(|w| w.name.as_str())), (vec!["uploads/a.png".to_string()], 2, false, Some("Orchestrator")));
        let Entry::Turn(first) = &t.entries[1] else { panic!() };
        let Step::Tool(t1) = &first.steps[0] else { panic!() };
        assert_eq!(t1.bypassed.as_deref(), Some("settings"));
        let Step::Tool(t2) = &first.steps[1] else { panic!() };
        assert_eq!(arg_summary(&t2.name, &t2.args), "(invalid arguments)");
        assert_eq!(first.ts, Some(11.0));
        let Entry::Turn(last) = &t.entries[5] else { panic!() };
        assert_eq!((last.stats.tok_per_s, last.stats.prompt), (Some(5.5), Some(2000)), "the turn's stats come from its last measured reply");
        // `up_to` leaves out what a live run replays (the run's base)
        let cut = Trace::from_chat(&chat, &ag, Some(5));
        assert_eq!(cut.entries.len(), 4);
        assert_eq!(Trace::from_chat(&chat, &ag, Some(999)).entries.len(), t.entries.len());
        assert!(Trace::from_chat(&json!({}), &ag, None).entries.is_empty());
        assert!(Trace::from_chat(&json!({ "messages": "nope" }), &ag, Some(3)).entries.is_empty());
        // each build is a new trace for the render cache
        assert_ne!(Trace::from_chat(&chat, &ag, None).id, t.id);
        // drawn: who asked, queued, recalled, images, the stopped mark, the error's retry hint
        let th = Theme::dark();
        let lines = texts(&render(&t, &opts(&th, 80, false)));
        for want in ["Orchestrator asked", "2 memories recalled", "▎ 🖼 uploads/a.png", "· sent while it worked", "■ stopped here", "✖ Error", "Ctrl+R tries again", "thought for 2 s  (Alt+O to show)", "auto-approved", "5.5 tok/s · 2k in · 9 out"] {
            assert!(lines.iter().any(|l| l.contains(want)), "{want:?} in {lines:#?}");
        }
    }

    #[test]
    fn a_turn_header_and_a_user_entry_draw_their_details() {
        let ag = [Agent { id: "x".into(), name: "Na\tme\x1b[31m".into(), color: "not a color".into(), emoji: "✍\u{FE0F}🤖".into(), ..Default::default() }];
        let who = Who::from(ag.first(), "x");
        assert_eq!((who.name.as_str(), who.color, who.emoji.as_str()), ("Na  me", Color::Gray, "✍ "));
        let gone = Who::from(None, "zz");
        assert_eq!((gone.id.as_str(), gone.name.as_str()), ("zz", "Deleted agent"));
        let mut t = Trace::empty();
        t.entries.push(Entry::User(user_from_event(&json!({ "role": "user", "content": "one\n\ttwo", "_queued": true, "_recall": "a\n" }), 0)));
        t.start_turn(who, 1);
        let th = Theme::dark();
        let r = render(&t, &RenderOpts { theme: &th, width: 40, verbose: false, selected_turn: Some(1) });
        let lines = texts(&r);
        assert!(lines.iter().any(|l| l.starts_with("You") && l.ends_with("· sent while it worked  · 1 memory recalled")), "{lines:?}");
        assert!(lines.contains(&"▎ one".to_string()) && lines.contains(&"▎     two".to_string()), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("▶ ✍  Na  me")), "the selected turn is marked: {lines:?}");
        assert_eq!(r.owners.iter().filter(|o| **o == Some(1)).count(), 2, "the turn's blank line and header belong to it");
    }

    #[test]
    fn every_step_kind_draws_what_it_is() {
        let ag = agents();
        let th = Theme::dark();
        let mut t = Trace::empty();
        t.start_turn(Who::from(ag.first(), "coder"), 1);
        let steps = &mut t.last_turn_mut().unwrap().steps;
        steps.push(Step::Waiting);
        steps.push(Step::Thinking { text: "deep\nthoughts".into(), live: false, secs: None, started: None });
        steps.push(Step::Thinking { text: "x".into(), live: false, secs: Some(125.0), started: None });
        steps.push(Step::Compacting { chars: 12_345 });
        steps.push(Step::Stopped);
        steps.push(Step::Notice("a notice long enough that it has to wrap onto a second line".into()));
        steps.push(Step::Tool(ToolStep { call_id: "w".into(), name: "write_file".into(), args: json!({ "path": "a.txt", "content": "hello" }), raw_args: String::new(), state: ToolState::Running, result: Some("Wrote 5 bytes".into()), live_out: None, approval: None, bypassed: Some("settings".into()), sub: None }));
        steps.push(Step::Tool(ToolStep { call_id: "e".into(), name: "edit_file".into(), args: json!({ "path": "b.py", "old_text": "a = 1", "new_text": "a = 2" }), raw_args: String::new(), state: ToolState::NeedsApproval, result: None, live_out: None, approval: Some("ap".into()), bypassed: None, sub: None }));
        steps.push(Step::Tool(ToolStep { call_id: "m".into(), name: "email_send".into(), args: json!({ "to": "a@b.c", "subject": "Hi", "body": "Dear you" }), raw_args: String::new(), state: ToolState::Done, result: None, live_out: None, approval: None, bypassed: Some("run".into()), sub: None }));
        let quiet = texts(&render(&t, &opts(&th, 60, false)));
        for want in ["◌ working…", "○ thoughts  (Alt+O to show)", "○ thought for 2 min 5 s", "compacting: summarizing older messages… 12.3k chars", "■ stopped here", "◦ a notice long enough", "write_file  a.txt  auto-approved  running", "edit_file  b.py  needs approval", "email_send  a@b.c · Hi  auto-approved (this run)  done"] {
            assert!(quiet.iter().any(|l| l.contains(want)), "{want:?} in {quiet:#?}");
        }
        assert!(quiet.iter().any(|l| l.contains("replace")) && quiet.iter().any(|l| l.contains("│ a = 2")), "an approval shows what it would do: {quiet:#?}");
        assert!(!quiet.iter().any(|l| l.contains("deep")) && !quiet.iter().any(|l| l.contains("Dear you")), "{quiet:#?}");
        let loud = texts(&render(&t, &opts(&th, 60, true)));
        for want in ["deep", "thoughts", "content → a.txt", "│ hello", "  result", "│ Wrote 5 bytes", "To: a@b.c", "│ Dear you"] {
            assert!(loud.iter().any(|l| l.contains(want)), "verbose: {want:?} in {loud:#?}");
        }
        assert!(!loud.iter().any(|l| l.contains("(Alt+O to show)")));
    }

    #[test]
    fn a_run_drawn_live_matches_the_same_run_rebuilt_from_its_chat() {
        // the events a run sends (app/runner.py) for: thinking, a handoff whose agent runs a shell
        // command after an approval, then a reply that was cut off and continued; and the chat
        // the server saves for it. Live and rebuilt, the trace must draw the same lines.
        let ag = agents();
        let sub_msgs = json!([
            { "role": "user", "content": "build" },
            { "role": "assistant", "content": "", "reasoning_content": "on it", "tool_calls": [{ "id": "s1", "function": { "name": "run_shell", "arguments": "{\"command\": \"make\"}" } }], "_stats": { "think_s": 0.2 } },
            { "role": "tool", "tool_call_id": "s1", "content": "exit code 0\nbuilt" },
            { "role": "assistant", "content": "Built it.", "_stats": { "prompt_tokens": 50 } },
        ]);
        let msgs = json!([
            { "role": "user", "content": "please build" },
            { "role": "assistant", "content": "", "reasoning_content": "delegate", "tool_calls": [{ "id": "h1", "function": { "name": "ask_agent", "arguments": "{\"agent\": \"Coder\", \"message\": \"build\"}" } }], "_stats": { "think_s": 0.3, "prompt_tokens": 100, "completion_tokens": 10, "tok_per_s": 20.0 } },
            { "role": "tool", "tool_call_id": "h1", "content": "Built it.", "_sub": { "agent_id": "coder", "chat_id": "sub1", "messages": sub_msgs, "continued": false } },
            { "role": "assistant", "content": "The first half", "_truncated": true, "_stats": { "prompt_tokens": 150, "completion_tokens": 400 } },
            { "role": "assistant", "content": "and the rest.", "_stats": { "prompt_tokens": 160, "completion_tokens": 20, "tok_per_s": 25.0 } },
        ]);
        let chat = json!({ "agent_id": "orchestrator", "messages": msgs });
        let at = |i: usize| msgs[i].clone();
        let p1: Vec<String> = vec!["h1".into()];
        let mut live = Trace::from_chat(&chat, &ag, Some(1));
        live.start_turn(Who::from(ag.get(1), "orchestrator"), 1);
        live.assistant_start(&[], &ag);
        live.delta(&[], "reasoning", "delegate", None, None, &ag);
        live.delta(&[], "tool_args", "{\"agent\": \"Coder\", \"message\": \"build\"}", Some(0), Some("ask_agent"), &ag);
        live.assistant_end(&[], &at(1), &ag);
        live.tool_start(&[], "h1", "ask_agent", json!({ "agent": "Coder", "message": "build" }), &ag);
        live.subagent_start(&p1, "coder", "sub1", false, &ag);
        live.assistant_start(&p1, &ag);
        live.delta(&p1, "reasoning", "on it", None, None, &ag);
        live.delta(&p1, "tool_args", "{\"command\": \"make\"}", Some(0), Some("run_shell"), &ag);
        live.assistant_end(&p1, &sub_msgs[1], &ag);
        live.tool_start(&p1, "s1", "run_shell", json!({ "command": "make" }), &ag);
        live.approval(&p1, "s1", Some("ap"), None, &ag);
        live.approval(&p1, "s1", None, Some(true), &ag);
        live.tool_progress(&p1, "s1", "building", &ag);
        live.tool_result(&p1, "s1", "exit code 0\nbuilt", &ag);
        live.assistant_start(&p1, &ag);
        live.delta(&p1, "content", "Built it.", None, None, &ag);
        live.assistant_end(&p1, &sub_msgs[3], &ag);
        live.tool_result(&[], "h1", "Built it.", &ag);
        live.assistant_start(&[], &ag);
        live.delta(&[], "content", "The first half", None, None, &ag);
        live.assistant_end(&[], &at(3), &ag);
        live.notice(&[], "The reply filled the context window before it finished. Continuing from where it stopped…", &ag);
        live.assistant_start(&[], &ag);
        live.delta(&[], "content", "and the rest.", None, None, &ag);
        live.assistant_end(&[], &at(4), &ag);
        let mut rebuilt = Trace::from_chat(&chat, &ag, None);
        // (when the turn started is the one thing that differs: now, or its first reply's time)
        for t in [&mut live, &mut rebuilt] {
            if let Some(tt) = t.last_turn_mut() {
                tt.ts = None;
            }
        }
        let th = Theme::dark();
        for verbose in [false, true] {
            let a = texts(&render(&live, &opts(&th, 70, verbose)));
            let b = texts(&render(&rebuilt, &opts(&th, 70, verbose)));
            assert_eq!(a, b, "verbose {verbose}: live\n{a:#?}\nrebuilt\n{b:#?}");
        }
    }

    #[test]
    fn random_spans_wrap_within_their_width_and_keep_every_character_and_style() {
        use crate::text::tests::{random_text, Rng};
        let mut rng = Rng(13);
        let styles = [Style::default(), Style::default().fg(Color::Red), Style::default().add_modifier(Modifier::BOLD)];
        for case in 0..3000 {
            let spans: Vec<Span<'static>> = (0..1 + rng.below(4)).map(|i| Span::styled(text::line(&random_text(&mut rng, 10)), styles[i % 3])).collect();
            let width = rng.below(24);
            let out = wrap_spans(&spans, width);
            // only the white space where a line was broken may go; every other character keeps
            // its style
            let cells = |l: &mut dyn Iterator<Item = &Span<'static>>| l.flat_map(|s| s.content.chars().filter(|c| !c.is_whitespace()).map(move |c| (c, s.style))).collect::<Vec<_>>();
            assert_eq!(cells(&mut out.iter().flatten()), cells(&mut spans.iter()), "case {case}: {spans:?} at {width}");
            for l in &out {
                let w: usize = l.iter().map(|s| text::width(&s.content)).sum();
                let visible = l.iter().flat_map(|s| s.content.chars()).filter(|c| text::width(&c.to_string()) > 0).count();
                assert!(w <= width.max(1) || visible == 1, "case {case}: a line {w} wide at {width}: {l:?}");
            }
        }
    }

    #[test]
    fn a_render_that_reuses_the_last_one_always_matches_a_fresh_one() {
        use crate::text::tests::{random_text, Rng};
        let ag = agents();
        let (dark, light) = (Theme::dark(), Theme::light());
        let mut rng = Rng(17);
        for run in 0..25 {
            let mut t = Trace::from_chat(&long_chat(3), &ag, None);
            let mut prev: Option<Rendered> = None;
            for step in 0..60usize {
                let s = random_text(&mut rng, 5);
                let last_tool = t.last_turn_mut().and_then(|tt| tt.steps.iter().rev().find_map(|st| if let Step::Tool(x) = st { Some(x.call_id.clone()) } else { None }));
                match rng.below(12) {
                    0 => t.start_turn(Who::from(ag.get(step % 2), "coder"), step),
                    1 | 2 => t.delta(&[], "content", &s, None, None, &ag),
                    3 => t.delta(&[], "reasoning", &s, None, None, &ag),
                    4 => t.tool_start(&[], &format!("c{step}"), ["run_shell", "ask_agent", "web_fetch"][step % 3], json!({ "command": s, "agent": "Coder", "url": s }), &ag),
                    5 => t.entries.push(Entry::User(user_from_event(&json!({ "role": "user", "content": s, "_queued": step % 2 == 0 }), step))),
                    6 => t.assistant_end(&[], &json!({ "content": s, "_stats": { "tok_per_s": step as f64 } }), &ag),
                    7 => t.notice(&[], &s, &ag),
                    8 => t.delta(&[], "tool_args", &s, Some((step % 2) as u64), Some("run_shell"), &ag),
                    9 => {
                        if let Some(id) = &last_tool {
                            match step % 3 {
                                0 => t.approval(&[], id, Some("ap"), None, &ag),
                                1 => t.tool_progress(&[], id, &s, &ag),
                                _ => t.tool_result(&[], id, &s, &ag),
                            }
                        }
                    }
                    10 => {
                        if let Some(id) = &last_tool {
                            t.delta(std::slice::from_ref(id), "content", &s, None, None, &ag);
                        }
                    }
                    _ => t.error(&s, step),
                }
                let turns: Vec<usize> = t.entries.iter().enumerate().filter(|(_, e)| matches!(e, Entry::Turn(_))).map(|(i, _)| i).collect();
                let o = RenderOpts {
                    theme: if rng.below(15) == 0 { &light } else { &dark },
                    width: if rng.below(10) == 0 { 41 } else { 60 },
                    verbose: rng.below(8) == 0,
                    selected_turn: if rng.below(3) == 0 { turns.get(rng.below(turns.len())).copied() } else { None },
                };
                let r = render_from(&t, &o, prev.take());
                let fresh = render(&t, &o);
                assert_eq!(r.lines, fresh.lines, "run {run} step {step}");
                assert_eq!(r.owners, fresh.owners, "run {run} step {step}");
                prev = Some(r);
            }
        }
    }

    #[test]
    fn agents_renamed_or_deleted_elsewhere_are_named_again_everywhere() {
        let mut ag = agents();
        let chat = json!({ "agent_id": "orchestrator", "messages": [
            { "role": "user", "content": "build" },
            { "role": "assistant", "content": "", "tool_calls": [{ "id": "h1", "function": { "name": "ask_agent", "arguments": "{\"agent\": \"Coder\", \"message\": \"go\"}" } }] },
            { "role": "tool", "tool_call_id": "h1", "content": "done", "_sub": { "agent_id": "coder", "chat_id": "s1", "messages": [{ "role": "user", "content": "go" }, { "role": "assistant", "content": "done" }] } },
            { "role": "assistant", "content": "All built." },
        ] });
        let mut t = Trace::from_chat(&chat, &ag, None);
        let th = Theme::dark();
        let shown = |t: &Trace| texts(&render(t, &opts(&th, 80, true))).join("\n");
        assert!(shown(&t).contains("Orchestrator") && shown(&t).contains("Coder"));
        let id = t.id;
        ag[0].name = "Builder".into();
        ag.remove(1);
        t.refresh_agents(&ag);
        let now = shown(&t);
        assert!(now.contains("Builder") && !now.contains("Coder") && now.contains("Deleted agent") && !now.contains("Orchestrator"), "{now}");
        assert_ne!(t.id, id, "drawn afresh, not from the lines that named them the old way");
    }
}
