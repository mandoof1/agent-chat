//! Slash commands, like Claude Code: `/` at the start of the message box opens a menu of these
//! above it. The registry is data in the web UI's shape (static/js/commands.js; a test checks both
//! list the same names and aliases, so keep one entry per line), plus the pure parts: parsing what
//! was typed, matching and completing in the menu, and the text the informational commands show.
//! Running one is `App::run_slash`.

use crate::api::{Agent, ServerInfo};
use crate::text::line as clean;
use crate::theme::Theme;
use crate::trace::fmt_k;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

pub struct Cmd {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub args: &'static str,
    /// Only in a chat you write in (listed only then; typed anyway, it says to open one).
    pub chat: bool,
    /// Refused while the agent works: it would change what the agent is working on, or the server
    /// refuses it mid-run (pinning gets "chat is busy").
    pub idle: bool,
    pub desc: &'static str,
}

#[rustfmt::skip]
pub const COMMANDS: &[Cmd] = &[
    Cmd { name: "help", aliases: &[], args: "", chat: false, idle: false, desc: "List every command and the keyboard shortcuts" },
    Cmd { name: "new", aliases: &["clear"], args: "[agent]", chat: false, idle: false, desc: "New chat with this chat's agent, or the one you name" },
    Cmd { name: "resume", aliases: &["chats"], args: "[search]", chat: false, idle: false, desc: "Find an earlier chat" },
    Cmd { name: "rename", aliases: &[], args: "[title]", chat: true, idle: true, desc: "Rename this chat" },
    Cmd { name: "pin", aliases: &[], args: "", chat: true, idle: true, desc: "Pin or unpin this chat" },
    Cmd { name: "branch", aliases: &["fork"], args: "", chat: true, idle: false, desc: "Branch this chat from the end into a new chat" },
    Cmd { name: "retry", aliases: &["regenerate"], args: "", chat: true, idle: true, desc: "Regenerate the last reply" },
    Cmd { name: "edit", aliases: &[], args: "", chat: true, idle: true, desc: "Edit your last message and send it again" },
    Cmd { name: "stop", aliases: &[], args: "", chat: true, idle: false, desc: "Stop the running reply" },
    Cmd { name: "compact", aliases: &[], args: "[instructions]", chat: true, idle: true, desc: "Summarize older messages now; say what the summary must keep" },
    Cmd { name: "export", aliases: &[], args: "[md|json]", chat: true, idle: false, desc: "Save this chat in the current folder (Markdown unless you say json)" },
    Cmd { name: "copy", aliases: &[], args: "", chat: true, idle: false, desc: "Copy the last reply (through the terminal, OSC 52)" },
    Cmd { name: "delete", aliases: &[], args: "", chat: true, idle: true, desc: "Delete this chat (asks first)" },
    Cmd { name: "agents", aliases: &[], args: "", chat: false, idle: false, desc: "Edit this chat's agent" },
    Cmd { name: "model", aliases: &[], args: "[name]", chat: false, idle: false, desc: "Show the models, or set this chat's agent's model (default clears it)" },
    Cmd { name: "memory", aliases: &[], args: "", chat: false, idle: false, desc: "Open the memory panel" },
    Cmd { name: "remember", aliases: &[], args: "<fact>", chat: false, idle: false, desc: "Save a fact to the memory every agent shares" },
    Cmd { name: "permissions", aliases: &[], args: "[ask|bypass]", chat: false, idle: false, desc: "Show or switch whether agents ask before acting" },
    Cmd { name: "settings", aliases: &["config"], args: "", chat: false, idle: false, desc: "Open settings" },
    Cmd { name: "routines", aliases: &[], args: "", chat: false, idle: false, desc: "Open routines" },
    Cmd { name: "files", aliases: &[], args: "", chat: false, idle: false, desc: "Show or hide the workspace files" },
    Cmd { name: "verbose", aliases: &[], args: "", chat: false, idle: false, desc: "Show or collapse every thinking block and tool detail" },
    Cmd { name: "theme", aliases: &[], args: "[dark|light|plain]", chat: false, idle: false, desc: "Switch the colors (no name: the next one)" },
    Cmd { name: "context", aliases: &[], args: "", chat: true, idle: false, desc: "How much of the context window this chat uses" },
    Cmd { name: "usage", aliases: &["cost", "stats"], args: "", chat: true, idle: false, desc: "Tokens in and out for this chat, replies, speed" },
    Cmd { name: "status", aliases: &[], args: "", chat: false, idle: false, desc: "Model server, models, context size, app version" },
    Cmd { name: "quit", aliases: &["exit"], args: "", chat: false, idle: false, desc: "Quit the terminal client" },
];

impl Cmd {
    pub fn names(&self) -> impl Iterator<Item = &'static str> {
        std::iter::once(self.name).chain(self.aliases.iter().copied())
    }
}

/// A command by its name or an alias (any case).
pub fn find(word: &str) -> Option<&'static Cmd> {
    let w = word.to_lowercase();
    COMMANDS.iter().find(|c| c.names().any(|n| n == w))
}

/// What the message box holds, as Enter sees it.
#[derive(Debug, PartialEq)]
pub enum Parsed<'a> {
    /// Send this to the agent: plain text, `//text` as `/text`, or a first word that is a path.
    Message(&'a str),
    Run(&'static str, &'a str),
    /// A `/word` that is no command.
    Unknown(&'a str),
}

/// Splits after the `/word` at the start: (word, the rest with the space after the word removed).
fn split_word(rest: &str) -> (&str, &str) {
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    (&rest[..end], rest[end..].trim_start())
}

pub fn parse(text: &str) -> Parsed<'_> {
    let Some(rest) = text.strip_prefix('/') else { return Parsed::Message(text) };
    if rest.starts_with('/') {
        return Parsed::Message(rest);
    }
    let (word, args) = split_word(rest);
    if word.contains('/') {
        return Parsed::Message(text); // a path like /etc/hosts
    }
    match find(word) {
        Some(c) => Parsed::Run(c.name, args.trim_end()),
        None => Parsed::Unknown(word),
    }
}

/// The text a message really sends (`//x` sends `/x`).
pub fn message(text: &str) -> &str {
    match parse(text) {
        Parsed::Message(m) => m,
        _ => text,
    }
}

/// Text going back into the message box as a message (edit-last, a returned queue): if Enter
/// wouldn't send it exactly as it is (it reads as a command, or starts with `//`), escape it.
pub fn escape(text: &str) -> String {
    if parse(text) == Parsed::Message(text) { text.to_string() } else { format!("/{text}") }
}

/// What follows the command word: its arguments.
pub fn args_of(text: &str) -> &str {
    split_word(text.strip_prefix('/').unwrap_or(text)).1.trim_end()
}

/// The command word being typed (without the slash) while the cursor (row, column) is still in
/// it; the menu is open only then.
pub fn typed_word(text: &str, (row, col): (usize, usize)) -> Option<&str> {
    let rest = text.strip_prefix('/')?;
    let (word, _) = split_word(rest);
    if row != 0 || rest.starts_with('/') || word.contains('/') || col > word.chars().count() + 1 {
        return None;
    }
    Some(word)
}

/// The menu for a typed word: an exact name or alias first, then the commands whose own name
/// starts with it, then those with an alias that does ("/sta" is /status before /usage's
/// /stats), then those that only contain it. Registry order within each. Chat-only ones only
/// when a chat is open.
pub fn matching(q: &str, in_chat: bool) -> Vec<&'static Cmd> {
    let q = q.to_lowercase();
    let rank = |c: &Cmd| match () {
        _ if c.names().any(|n| n == q) => 0,
        _ if c.name.starts_with(&q) => 1,
        _ if c.aliases.iter().any(|a| a.starts_with(&q)) => 2,
        _ if c.names().any(|n| n.contains(&q)) => 3,
        _ => 4,
    };
    let mut out: Vec<&'static Cmd> = COMMANDS.iter().filter(|c| (in_chat || !c.chat) && rank(c) < 4).collect();
    out.sort_by_key(|c| rank(c)); // (stable)
    out
}

/// Tab in the menu: the command's full name, a space when it takes arguments (or some follow),
/// then whatever was already typed after the word. Returns the text and the cursor's column.
pub fn complete(text: &str, cmd: &Cmd) -> (String, usize) {
    let rest = args_of(text);
    let head = format!("/{}{}", cmd.name, if cmd.args.is_empty() && rest.is_empty() { "" } else { " " });
    let col = head.chars().count();
    (head + rest, col)
}

/// Whether a command can run now; the reason (for a toast) when it can't.
pub fn check(cmd: &Cmd, in_chat: bool, running: bool) -> Result<(), String> {
    if cmd.chat && !in_chat {
        return Err("Open a chat first".into());
    }
    if cmd.idle && running {
        return Err(format!("The agent is working. Wait for it, or /stop it first, then /{}.", cmd.name));
    }
    Ok(())
}

/// `/model <name>`: the model to set. `default` clears it; an exact name or file name wins, then
/// the one listed model containing it; several matches ask which. Anything else is set as typed.
pub fn pick_model(name: &str, models: &[String]) -> Result<String, String> {
    let low = name.to_lowercase();
    if low == "default" {
        return Ok(String::new());
    }
    let base = |m: &str| m.rsplit('/').next().unwrap_or(m).to_lowercase().trim_end_matches(".gguf").to_string();
    if let Some(m) = models.iter().find(|m| m.to_lowercase() == low || base(m) == low) {
        return Ok(m.clone());
    }
    let hits: Vec<&str> = models.iter().filter(|m| m.to_lowercase().contains(&low)).map(String::as_str).collect();
    match hits.as_slice() {
        [one] => Ok(one.to_string()),
        [] => Ok(name.to_string()),
        many => Err(format!("Which one? {}", many.join(", "))),
    }
}

/// `/new <agent>`: by id, by name, then by the start of a name (any case).
pub fn find_agent<'a>(name: &str, agents: &'a [Agent]) -> Option<&'a Agent> {
    let low = name.to_lowercase();
    agents.iter().find(|a| a.id == name || a.name.to_lowercase() == low).or_else(|| agents.iter().find(|a| a.name.to_lowercase().starts_with(&low)))
}

/// `/export [md|json]`.
pub fn export_kind(arg: &str) -> Option<&'static str> {
    match arg.trim().trim_start_matches('.').to_lowercase().as_str() {
        "" | "md" | "markdown" => Some("md"),
        "json" => Some("json"),
        _ => None,
    }
}

/// `/theme` without a name: the one after `current`.
pub fn next_theme(current: &str) -> &'static str {
    let names = crate::theme::NAMES;
    let i = names.iter().position(|n| *n == current).map(|i| i + 1).unwrap_or(0);
    names[i % names.len()]
}

// ------------------------------------------------- what the informational commands show

fn kv(th: &Theme, k: &str, v: &str) -> Line<'static> {
    Line::from(vec![Span::styled(format!("{k:>11}  "), Style::default().fg(th.muted)), Span::styled(clean(v), Style::default().fg(th.text))])
}

fn note(th: &Theme, s: &str) -> Line<'static> {
    Line::from(Span::styled(clean(s), Style::default().fg(th.faint)))
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// 12345 as "12,345".
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn help_lines(th: &Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = COMMANDS
        .iter()
        .map(|c| {
            let call = format!("/{}{}{}", c.name, if c.args.is_empty() { "" } else { " " }, c.args);
            let mut spans = vec![Span::styled(format!(" {call:<26}"), Style::default().fg(th.accent)), Span::styled(c.desc, Style::default().fg(th.text))];
            if !c.aliases.is_empty() {
                spans.push(Span::styled(format!(" · also {}", c.aliases.iter().map(|a| format!("/{a}")).collect::<Vec<_>>().join(", ")), Style::default().fg(th.faint)));
            }
            Line::from(spans)
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(note(th, " Tab completes a command, ↑↓ pick one in the menu, Esc closes it."));
    lines.push(note(th, " Start a message with // to send one that begins with /. F1 lists the keyboard shortcuts."));
    lines
}

/// The one-line answer of `/context`.
pub fn context_summary(used: Option<u64>, window: Option<u64>, compact_at: u64, auto: bool) -> String {
    let auto = if auto { format!("auto-compacts at {compact_at}%") } else { "auto-compact is off".into() };
    match (used.unwrap_or(0), window) {
        (0, Some(n)) => format!("ctx 0 / {} · nothing sent yet · {auto}", fmt_k(n)),
        (0, None) => "ctx 0 · nothing sent yet".into(),
        (u, Some(n)) => format!("ctx {} / {} ({}%) · {auto}", fmt_k(u), fmt_k(n), (u as f64 / n as f64 * 100.0).round() as u64),
        (u, None) => format!("ctx ~{} · window size unknown (set it in Settings)", fmt_k(u)),
    }
}

/// `age`: the chat's `trace::chat_age` ("compacted 2× · started 09:14 (going for 3h 12m)").
pub fn context_lines(th: &Theme, used: Option<u64>, window: Option<u64>, compact_at: u64, auto: bool, age: &str) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(format!(" {}", context_summary(used, window, compact_at, auto)), Style::default().fg(th.text).add_modifier(Modifier::BOLD)))];
    if let Some(n) = window.filter(|n| *n > 0) {
        let pct = (used.unwrap_or(0) as f64 / n as f64 * 100.0).min(100.0);
        let bar = 40usize;
        let fill = ((pct / 100.0 * bar as f64).round() as usize).max(usize::from(used.unwrap_or(0) > 0));
        let mark = if auto { ((compact_at as f64 / 100.0 * bar as f64).round() as usize).min(bar - 1) } else { bar };
        let color = if pct >= 90.0 { th.danger } else if pct >= compact_at as f64 { th.amber } else { th.ok };
        let mut spans = vec![Span::raw(" ")];
        for i in 0..bar {
            let (ch, fg) = if i < fill { ("▮", color) } else if i == mark { ("┃", th.amber) } else { ("▯", th.line) };
            spans.push(Span::styled(ch, Style::default().fg(fg)));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(Span::styled(format!(" {}", clean(age)), Style::default().fg(th.muted))));
    lines.push(Line::from(""));
    lines.push(note(th, " /compact summarizes older messages now; /compact <what to keep> says what the summary keeps."));
    lines
}

#[derive(Debug, Default, PartialEq)]
pub struct Usage {
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub calls: usize,
    pub replies: usize,
    pub speed: Option<f64>,
}

/// Token totals over a chat's saved messages: every model call's prompt and completion, how many
/// replies (an answer to a message, however many calls it took) and the newest speed.
pub fn usage(messages: &[Value]) -> Usage {
    let mut u = Usage::default();
    let mut prev = "";
    for m in messages {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role == "assistant" {
            if prev != "assistant" && prev != "tool" {
                u.replies += 1;
            }
            if let Some(st) = m.get("_stats") {
                u.calls += 1;
                u.tokens_in += st.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                u.tokens_out += st.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                u.speed = st.get("tok_per_s").and_then(|x| x.as_f64()).or(u.speed);
            }
        }
        prev = role;
    }
    u
}

pub fn usage_lines(th: &Theme, u: &Usage, age: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(format!(" {} in · {} out · {}", fmt_k(u.tokens_in), fmt_k(u.tokens_out), plural(u.replies, "reply", "replies")), Style::default().fg(th.text).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled(format!(" {}", clean(age)), Style::default().fg(th.muted))),
        Line::from(""),
        kv(th, "tokens in", &format!("{} (prompts summed over {})", thousands(u.tokens_in), plural(u.calls, "model call", "model calls"))),
        kv(th, "tokens out", &thousands(u.tokens_out)),
        kv(th, "replies", &u.replies.to_string()),
        kv(th, "last speed", &u.speed.map(|s| format!("{s:.1} tok/s")).unwrap_or_else(|| "not measured yet".into())),
        Line::from(""),
        note(th, " Work other agents did for this chat is counted in their own chats."),
    ]
}

pub fn status_lines(th: &Theme, s: &ServerInfo, settings: &Value, version: &str) -> Vec<Line<'static>> {
    let set = |k: &str| settings.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let window = s.n_ctx.or_else(|| settings.get("context_size").and_then(|v| v.as_u64()).filter(|n| *n > 0));
    let server = if s.ok { "reachable".to_string() } else { format!("not reachable{}", s.error.as_ref().map(|e| format!(": {e}")).unwrap_or_default()) };
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{:>11}  ", "server"), Style::default().fg(th.muted)),
        Span::styled(clean(&server), Style::default().fg(if s.ok { th.ok } else { th.danger })),
    ])];
    lines.push(kv(th, "address", &if !s.base_url.is_empty() { s.base_url.clone() } else if !set("base_url").is_empty() { set("base_url") } else { "not set".into() }));
    if s.ok {
        lines.push(kv(th, "kind", if s.kind == "llama" { "llama-server" } else { "OpenAI-compatible server" }));
    }
    lines.push(kv(th, "models", &if s.models.is_empty() { "none listed".into() } else { s.models.join(", ") }));
    let default = if !set("model").is_empty() { set("model") } else { s.models.first().map(|m| format!("{m} (the server's first)")).unwrap_or_else(|| "the server's first".into()) };
    lines.push(kv(th, "default", &default));
    lines.push(kv(th, "context", &match window {
        Some(n) => format!("{} tokens{}", thousands(n), if s.n_ctx.is_some() { "" } else { " (from Settings)" }),
        None => "unknown".into(),
    }));
    lines.push(kv(th, "app", &format!("Agent Chat {version} · terminal client {}", env!("CARGO_PKG_VERSION"))));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn names(v: &[&Cmd]) -> Vec<&'static str> {
        v.iter().map(|c| c.name).collect()
    }

    #[test]
    fn registry_is_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for c in COMMANDS {
            for n in c.names() {
                assert!(seen.insert(n), "/{n} is listed twice");
                assert!(!n.is_empty() && n.chars().all(|ch| ch.is_ascii_lowercase()), "/{n}");
            }
            assert!(!c.desc.is_empty());
            assert!(c.args.is_empty() || c.args.starts_with('[') || c.args.starts_with('<'), "{}", c.args);
            assert!(!c.idle || c.chat, "/{} needs a chat to be idle", c.name);
        }
        // the chat-only set the spec names
        let chat: Vec<&str> = COMMANDS.iter().filter(|c| c.chat).map(|c| c.name).collect();
        assert_eq!(chat, ["rename", "pin", "branch", "retry", "edit", "stop", "compact", "export", "copy", "delete", "context", "usage"]);
        let idle: Vec<&str> = COMMANDS.iter().filter(|c| c.idle).map(|c| c.name).collect();
        // the spec's, plus pin: the server refuses it mid-run ("chat is busy")
        assert_eq!(idle, ["rename", "pin", "retry", "edit", "compact", "delete"]);
    }

    #[test]
    fn aliases_resolve_to_the_command() {
        for (alias, name) in [("clear", "new"), ("chats", "resume"), ("fork", "branch"), ("regenerate", "retry"), ("config", "settings"), ("cost", "usage"), ("stats", "usage"), ("exit", "quit"), ("HELP", "help"), ("Usage", "usage")] {
            assert_eq!(find(alias).map(|c| c.name), Some(name), "/{alias}");
        }
        assert!(find("nosuch").is_none());
        assert!(find("").is_none());
        assert!(find("he").is_none(), "a prefix is not a name");
        assert_eq!(parse("/clear"), Parsed::Run("new", ""));
        assert_eq!(parse("/stats"), Parsed::Run("usage", ""));
    }

    #[test]
    fn parsing_commands_messages_and_escapes() {
        assert_eq!(parse("hello"), Parsed::Message("hello"));
        assert_eq!(parse(" /help"), Parsed::Message(" /help"), "only at the very start");
        assert_eq!(parse("/help"), Parsed::Run("help", ""));
        assert_eq!(parse("/rename  My new title  "), Parsed::Run("rename", "My new title"));
        assert_eq!(parse("/compact keep the API\ndesign decisions"), Parsed::Run("compact", "keep the API\ndesign decisions"));
        assert_eq!(parse("/compact\nkeep this"), Parsed::Run("compact", "keep this"));
        assert_eq!(parse("/nosuch thing"), Parsed::Unknown("nosuch"));
        assert_eq!(parse("/"), Parsed::Unknown(""));
        // "//" sends the rest as a message that starts with one slash
        assert_eq!(parse("//hi there"), Parsed::Message("/hi there"));
        assert_eq!(parse("//help"), Parsed::Message("/help"));
        assert_eq!(message("//hi"), "/hi");
        assert_eq!(message("plain"), "plain");
        // a first word with another slash in it is a path, so a message
        assert_eq!(parse("/etc/hosts is broken"), Parsed::Message("/etc/hosts is broken"));
        assert_eq!(parse("/usr/bin/env"), Parsed::Message("/usr/bin/env"));
        assert_eq!(parse("/help/me"), Parsed::Message("/help/me"));
        assert_eq!(message("/etc/hosts x"), "/etc/hosts x");
        // a path further along doesn't matter
        assert_eq!(parse("/remember my notes live in /home/me/notes"), Parsed::Run("remember", "my notes live in /home/me/notes"));
    }

    #[test]
    fn escape_round_trips_a_message() {
        for t in ["/hi", "/help me", "/", "/nosuch", "//already", "///x", "plain", "/etc/hosts", " /help", ""] {
            assert_eq!(parse(&escape(t)), Parsed::Message(t), "{t:?} comes back as {:?}", escape(t));
        }
        for t in ["plain", "/etc/hosts x", " /help"] {
            assert_eq!(escape(t), t, "left alone when it already sends as is");
        }
        assert_eq!(escape("/hi"), "//hi");
        assert_eq!(escape("//x"), "///x");
    }

    #[test]
    fn args_are_what_follows_the_word() {
        assert_eq!(args_of("/rename A title "), "A title");
        assert_eq!(args_of("/rename"), "");
        assert_eq!(args_of("/re"), "");
        assert_eq!(args_of("/compact\n  line two"), "line two");
        assert_eq!(args_of("/model   qwen 7b"), "qwen 7b");
    }

    #[test]
    fn the_menu_opens_only_while_typing_the_first_word() {
        assert_eq!(typed_word("/", (0, 1)), Some(""));
        assert_eq!(typed_word("/re", (0, 3)), Some("re"));
        assert_eq!(typed_word("/re", (0, 1)), Some("re"), "cursor anywhere in the word");
        assert_eq!(typed_word("/re", (0, 0)), Some("re"));
        assert_eq!(typed_word("/rename x", (0, 7)), Some("rename"));
        assert_eq!(typed_word("/rename x", (0, 8)), None, "past the word: typing arguments");
        assert_eq!(typed_word("/rename ", (0, 8)), None);
        assert_eq!(typed_word("/re\nmore", (1, 2)), None);
        assert_eq!(typed_word("hi /re", (0, 6)), None);
        assert_eq!(typed_word("//re", (0, 4)), None);
        assert_eq!(typed_word("/etc/ho", (0, 7)), None);
        assert_eq!(typed_word("/日本", (0, 3)), Some("日本"), "the column counts characters");
    }

    #[test]
    fn matching_prefix_first_then_contains() {
        assert_eq!(matching("", true).len(), COMMANDS.len());
        assert_eq!(names(&matching("", false)), COMMANDS.iter().filter(|c| !c.chat).map(|c| c.name).collect::<Vec<_>>());
        // prefixes in registry order, aliases counting (/regenerate brings /retry)
        assert_eq!(names(&matching("re", true)), ["resume", "rename", "retry", "remember"]);
        assert_eq!(names(&matching("reg", true)), ["retry"]);
        assert_eq!(names(&matching("co", true)), ["compact", "copy", "context", "settings", "usage"], "/config and /cost are aliases, after the names");
        // an exact name or alias comes first even when another command sorts earlier
        // a command's own name before another's alias: Enter on "/sta" runs /status
        assert_eq!(names(&matching("stat", true)), ["status", "usage"]);
        assert_eq!(names(&matching("sta", true)), ["status", "usage"]);
        assert_eq!(names(&matching("cl", true)), ["new"], "/clear");
        assert_eq!(names(&matching("stats", true)), ["usage"]);
        assert_eq!(names(&matching("edit", true)), ["edit"]);
        assert_eq!(names(&matching("exit", true)), ["quit"]);
        // then the ones that only contain the text
        assert_eq!(names(&matching("ry", true)), ["retry", "memory"]);
        assert_eq!(names(&matching("m", true)), ["model", "memory", "resume", "rename", "compact", "remember", "permissions", "theme"]);
        assert_eq!(names(&matching("NEW", true)), ["new"], "any case");
        assert!(matching("zzz", true).is_empty());
        // chat-only commands only in a chat
        assert_eq!(names(&matching("re", false)), ["resume", "remember"]);
        assert_eq!(names(&matching("co", false)), ["settings"]);
    }

    #[test]
    fn tab_completes_the_name() {
        let c = |n| find(n).unwrap();
        assert_eq!(complete("/ren", c("rename")), ("/rename ".into(), 8), "a space when it takes arguments");
        assert_eq!(complete("/he", c("help")), ("/help".into(), 5), "none when it takes none");
        assert_eq!(complete("/reg", c("retry")), ("/retry".into(), 6), "an alias completes to the name");
        assert_eq!(complete("/ren my title", c("rename")), ("/rename my title".into(), 8), "what follows is kept");
        assert_eq!(complete("/he x", c("help")), ("/help x".into(), 6));
        assert_eq!(complete("/", c("compact")), ("/compact ".into(), 9));
    }

    #[test]
    fn availability() {
        let c = |n| find(n).unwrap();
        assert!(check(c("help"), false, false).is_ok());
        assert!(check(c("help"), true, true).is_ok());
        assert_eq!(check(c("pin"), false, false), Err("Open a chat first".into()));
        assert!(check(c("pin"), true, true).unwrap_err().contains("The agent is working"), "the server refuses a pin mid-run (409 chat is busy)");
        assert!(check(c("stop"), true, true).is_ok());
        assert!(check(c("compact"), true, false).is_ok());
        let busy = check(c("compact"), true, true).unwrap_err();
        assert!(busy.contains("working") && busy.contains("/stop") && busy.contains("/compact"), "{busy}");
        assert_eq!(check(c("rename"), false, true), Err("Open a chat first".into()), "no chat says so first");
    }

    #[test]
    fn model_names() {
        let models: Vec<String> = ["qwen2.5-7b-instruct", "models/Llama-3.1-8B.Q4.gguf", "qwen2.5-14b"].iter().map(|s| s.to_string()).collect();
        assert_eq!(pick_model("default", &models), Ok(String::new()));
        assert_eq!(pick_model("DEFAULT", &models), Ok(String::new()));
        assert_eq!(pick_model("qwen2.5-14b", &models), Ok("qwen2.5-14b".into()));
        assert_eq!(pick_model("llama-3.1-8b.q4", &models), Ok("models/Llama-3.1-8B.Q4.gguf".into()), "the file name without .gguf");
        assert_eq!(pick_model("14b", &models), Ok("qwen2.5-14b".into()), "the one that contains it");
        assert_eq!(pick_model("qwen", &models), Err("Which one? qwen2.5-7b-instruct, qwen2.5-14b".into()));
        assert_eq!(pick_model("gpt-4o", &models), Ok("gpt-4o".into()), "unlisted: as typed");
        assert_eq!(pick_model("x", &[]), Ok("x".into()));
    }

    #[test]
    fn agents_by_id_name_or_prefix() {
        let a = |id: &str, name: &str| Agent { id: id.into(), name: name.into(), ..Default::default() };
        let agents = vec![a("assistant", "Assistant"), a("coder", "Coder"), a("c2", "Code Reviewer")];
        assert_eq!(find_agent("coder", &agents).map(|a| a.id.as_str()), Some("coder"));
        assert_eq!(find_agent("CODER", &agents).map(|a| a.id.as_str()), Some("coder"));
        assert_eq!(find_agent("code r", &agents).map(|a| a.id.as_str()), Some("c2"));
        assert_eq!(find_agent("ass", &agents).map(|a| a.id.as_str()), Some("assistant"));
        assert_eq!(find_agent("c2", &agents).map(|a| a.id.as_str()), Some("c2"));
        assert!(find_agent("nobody", &agents).is_none());
    }

    #[test]
    fn small_parsers() {
        assert_eq!(export_kind(""), Some("md"));
        assert_eq!(export_kind("MD"), Some("md"));
        assert_eq!(export_kind(".json"), Some("json"));
        assert_eq!(export_kind("markdown"), Some("md"));
        assert_eq!(export_kind("pdf"), None);
        assert_eq!(next_theme("dark"), "light");
        assert_eq!(next_theme("light"), "plain");
        assert_eq!(next_theme("plain"), "dark");
        assert_eq!(next_theme("?"), "dark");
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn context_wording() {
        assert_eq!(context_summary(Some(12_300), Some(85_000), 70, true), "ctx 12.3k / 85k (14%) · auto-compacts at 70%");
        assert_eq!(context_summary(Some(12_300), Some(85_000), 70, false), "ctx 12.3k / 85k (14%) · auto-compact is off");
        assert_eq!(context_summary(None, Some(8192), 70, true), "ctx 0 / 8.2k · nothing sent yet · auto-compacts at 70%");
        assert_eq!(context_summary(Some(900), None, 70, true), "ctx ~900 · window size unknown (set it in Settings)");
        let th = Theme::dark();
        let text = |ls: Vec<Line>| ls.iter().map(|l| l.spans.iter().map(|s| s.content.to_string()).collect::<String>()).collect::<Vec<_>>();
        let age = "compacted 2× · started 09:14 (going for 3h 12m)";
        let lines = text(context_lines(&th, Some(60_000), Some(100_000), 70, true, age));
        assert!(lines[1].contains('┃'), "the auto-compact mark: {:?}", lines[1]);
        assert_eq!(lines[1].chars().filter(|c| *c == '▮').count(), 24);
        assert_eq!(lines[2], format!(" {age}"), "the chat's age right under the bar");
        let lines = text(context_lines(&th, None, None, 70, true, "not compacted yet · started 09:14 (going for 4m)"));
        assert!(!lines.iter().any(|l| l.contains('▯')), "no bar without a window");
        assert_eq!(lines[1], " not compacted yet · started 09:14 (going for 4m)");
    }

    #[test]
    fn usage_totals() {
        let msgs = vec![
            json!({ "role": "user", "content": "hi" }),
            json!({ "role": "assistant", "content": "", "tool_calls": [], "_stats": { "prompt_tokens": 1000, "completion_tokens": 20, "tok_per_s": 30.0 } }),
            json!({ "role": "tool", "content": "ok" }),
            json!({ "role": "assistant", "content": "done", "_stats": { "prompt_tokens": 1100, "completion_tokens": 30 } }),
            json!({ "role": "user", "content": "again" }),
            json!({ "role": "assistant", "content": "", "_error": "boom" }),
            json!({ "role": "user", "content": "and again" }),
            json!({ "role": "assistant", "content": "ok", "_stats": { "prompt_tokens": 1300, "completion_tokens": 5, "tok_per_s": 41.5 } }),
        ];
        assert_eq!(usage(&msgs), Usage { tokens_in: 3400, tokens_out: 55, calls: 3, replies: 3, speed: Some(41.5) });
        assert_eq!(usage(&[]), Usage::default());
        let th = Theme::dark();
        let text = |ls: Vec<Line>| ls.iter().map(|l| l.spans.iter().map(|s| s.content.to_string()).collect::<String>()).collect::<Vec<_>>();
        let lines = text(usage_lines(&th, &usage(&msgs), "compacted 1× · started 09:14 (going for 45s)\x1b[31m"));
        assert_eq!(lines[0], " 3.4k in · 55 out · 3 replies");
        assert_eq!(lines[1], " compacted 1× · started 09:14 (going for 45s)", "the age, cleaned");
    }

    #[test]
    fn status_text() {
        let th = Theme::dark();
        let text = |ls: Vec<Line>| ls.iter().map(|l| l.spans.iter().map(|s| s.content.to_string()).collect::<String>()).collect::<Vec<_>>().join("\n");
        let up = ServerInfo { ok: true, models: vec!["mock-model".into()], n_ctx: Some(85000), kind: "llama".into(), base_url: "http://x/v1".into(), error: None };
        let s = text(status_lines(&th, &up, &json!({ "model": "" }), "0.2.0"));
        for want in ["reachable", "http://x/v1", "llama-server", "mock-model (the server's first)", "85,000 tokens", "Agent Chat 0.2.0"] {
            assert!(s.contains(want), "{want} in {s}");
        }
        let down = ServerInfo { ok: false, error: Some("connection refused\r\n\x1b[31m".into()), ..Default::default() };
        let s = text(status_lines(&th, &down, &json!({ "base_url": "http://y/v1", "context_size": 4096 }), "0.2.0"));
        assert!(s.contains("not reachable: connection refused") && s.contains("http://y/v1") && s.contains("4,096 tokens (from Settings)") && s.contains("none listed"), "{s}");
        assert!(!s.chars().any(|c| c.is_control() && c != '\n'), "server text is cleaned: {s:?}");
    }

    #[test]
    fn help_lists_every_command() {
        let th = Theme::dark();
        let all: String = help_lines(&th).iter().flat_map(|l| l.spans.iter().map(|s| s.content.to_string())).collect();
        for c in COMMANDS {
            assert!(all.contains(&format!("/{}", c.name)), "/{}", c.name);
            for a in c.aliases {
                assert!(all.contains(&format!("/{a}")), "/{a}");
            }
        }
        assert!(all.contains("//"));
    }

    fn plain(ls: &[Line]) -> Vec<String> {
        ls.iter().map(|l| l.spans.iter().map(|s| s.content.to_string()).collect()).collect()
    }

    #[test]
    fn every_name_and_alias_finds_runs_matches_and_completes_its_command() {
        for c in COMMANDS {
            for n in c.names() {
                assert_eq!(find(n).map(|x| x.name), Some(c.name), "/{n}");
                assert_eq!(find(&n.to_uppercase()).map(|x| x.name), Some(c.name), "/{n} in capitals");
                assert_eq!(parse(&format!("/{n}")), Parsed::Run(c.name, ""), "/{n}");
                assert_eq!(parse(&format!("/{n}\targ  ")), Parsed::Run(c.name, "arg"), "/{n} with a tab before its argument");
                assert_eq!(names(&matching(n, true))[0], c.name, "typing /{n} puts /{} first", c.name);
                assert!(names(&matching(&n[..1], true)).contains(&c.name), "/{} is listed for its first letter", c.name);
                let (text, col) = complete(&format!("/{}", &n[..1]), c);
                assert_eq!(text, if c.args.is_empty() { format!("/{}", c.name) } else { format!("/{} ", c.name) });
                assert_eq!(col, text.chars().count());
                assert_eq!(parse(&text), Parsed::Run(c.name, ""), "the completion runs it");
            }
            // chat-only ones are listed only in a chat; the rest everywhere
            assert_eq!(matching(c.name, false).iter().any(|x| x.name == c.name), !c.chat, "/{}", c.name);
            // whether it runs: no chat, a working agent
            for (in_chat, running) in [(false, false), (false, true), (true, false), (true, true)] {
                let ok = check(c, in_chat, running).is_ok();
                assert_eq!(ok, (!c.chat || in_chat) && (!c.idle || !running), "/{} in_chat {in_chat} running {running}", c.name);
            }
        }
        assert!(find(" help").is_none() && find("help ").is_none() && find("/help").is_none());
        assert_eq!(parse("/ help"), Parsed::Unknown(""), "a space after the slash is no command");
    }

    #[test]
    fn random_text_parses_and_escapes_consistently() {
        let mut rng = crate::text::tests::Rng(31);
        let words = ["/", "//", "/help", "/re", "/etc/x", "/compact", "/model", "/x/y", " ", "\n", "\t", "a", "b c", "日本"];
        for case in 0..5000 {
            let t: String = if rng.below(2) == 0 {
                (0..rng.below(6)).map(|_| words[rng.below(words.len())]).collect()
            } else {
                crate::text::tests::random_text(&mut rng, 8)
            };
            // what goes back in the box comes out as the same message
            assert_eq!(parse(&escape(&t)), Parsed::Message(&t), "case {case}: {t:?} escaped as {:?}", escape(&t));
            match parse(&t) {
                Parsed::Run(name, args) => {
                    assert!(find(name).is_some());
                    assert_eq!(args, args_of(&t), "case {case}: {t:?}");
                    assert_eq!(args, args.trim(), "case {case}: arguments are trimmed");
                }
                Parsed::Unknown(w) => assert!(t.starts_with('/') && find(w).is_none() && !w.contains('/') && !w.contains(char::is_whitespace), "case {case}: {t:?}"),
                Parsed::Message(m) => {
                    assert!(t.ends_with(m), "case {case}: a message is the text, or the text less one slash");
                    assert_eq!(message(&t), m);
                }
            }
            // the menu is open only on a first word that would run (or name) a command
            if let Some(w) = typed_word(&t, (0, 1)) {
                assert!(t.starts_with('/') && !t.starts_with("//") && t[1..].starts_with(w), "case {case}: {t:?} → {w:?}");
            }
        }
    }

    #[test]
    fn the_menu_word_follows_the_cursor() {
        // the cursor may sit anywhere from the slash to the end of the word
        for col in 0..=7 {
            assert_eq!(typed_word("/rename title", (0, col)), Some("rename"), "column {col}");
        }
        assert_eq!(typed_word("/rename title", (0, 8)), None);
        assert_eq!(typed_word("/rename", (0, 99)), None, "a cursor past the text is past the word");
        assert_eq!(typed_word("/re\n", (0, 3)), Some("re"), "a line break ends the word too");
        assert_eq!(typed_word("", (0, 0)), None);
        assert_eq!(typed_word("no slash", (0, 0)), None);
    }

    #[test]
    fn model_and_agent_names_resolve_or_ask() {
        let models: Vec<String> = ["org/Qwen2.5-7B.gguf", "qwen2.5-7b", "Llama"].iter().map(|s| s.to_string()).collect();
        // an exact name beats a file name, which beats containing
        assert_eq!(pick_model("qwen2.5-7b", &models), Ok("org/Qwen2.5-7B.gguf".into()), "the file name matches first in list order");
        assert_eq!(pick_model("LLAMA", &models), Ok("Llama".into()));
        assert_eq!(pick_model("org/qwen2.5-7b.gguf", &models), Ok("org/Qwen2.5-7B.gguf".into()));
        assert_eq!(pick_model("lla", &models), Ok("Llama".into()));
        assert!(pick_model("2.5", &models).unwrap_err().starts_with("Which one? "));
        assert_eq!(pick_model(" spaced ", &models), Ok(" spaced ".into()), "set as typed (run_slash trims it first)");
        let a = |id: &str, name: &str| Agent { id: id.into(), name: name.into(), ..Default::default() };
        let agents = vec![a("x1", "Writer"), a("x2", "Write Bot"), a("writer", "Other")];
        assert_eq!(find_agent("writer", &agents).map(|a| a.id.as_str()), Some("x1"), "a name before an id that only differs in case");
        assert_eq!(find_agent("WRITE", &agents).map(|a| a.id.as_str()), Some("x1"), "the first whose name starts with it");
        assert_eq!(find_agent("x2", &agents).map(|a| a.id.as_str()), Some("x2"));
        assert!(find_agent("bot", &agents).is_none(), "not a name's middle");
    }

    #[test]
    fn small_parsers_at_their_edges() {
        for (arg, want) in [(" md ", Some("md")), ("JSON", Some("json")), ("..json", Some("json")), ("Markdown", Some("md")), ("txt", None), ("md json", None)] {
            assert_eq!(export_kind(arg), want, "{arg:?}");
        }
        let mut name = "dark";
        for _ in 0..crate::theme::NAMES.len() {
            name = next_theme(name);
        }
        assert_eq!(name, "dark", "the cycle comes back around");
        assert!(crate::theme::NAMES.iter().all(|n| crate::theme::Theme::named(n).is_some()));
        assert_eq!(thousands(u64::MAX), "18,446,744,073,709,551,615");
        assert_eq!(thousands(100_000), "100,000");
        assert_eq!(thousands(12), "12");
    }

    #[test]
    fn context_and_usage_say_what_they_measure() {
        let th = Theme::dark();
        // over the window still says so, and the bar stays full rather than overflowing
        assert_eq!(context_summary(Some(120_000), Some(100_000), 70, true), "ctx 120k / 100k (120%) · auto-compacts at 70%");
        let over = plain(&context_lines(&th, Some(120_000), Some(100_000), 70, true, ""));
        assert_eq!(over[1].chars().filter(|c| *c == '▮').count(), 40);
        // a little is one cell, the auto-compact mark sits at its percentage, off: no mark
        let little = plain(&context_lines(&th, Some(1), Some(100_000), 70, true, ""));
        assert_eq!(little[1].chars().filter(|c| *c == '▮').count(), 1);
        assert_eq!(little[1].chars().position(|c| c == '┃'), Some(1 + 28));
        let off = plain(&context_lines(&th, Some(1), Some(100_000), 70, false, ""));
        assert!(!off[1].contains('┃'));
        // a window of 0 is no window
        assert_eq!(plain(&context_lines(&th, Some(5), Some(0), 70, true, "age")).len(), 4);
        assert_eq!(context_summary(Some(0), None, 70, false), "ctx 0 · nothing sent yet");
        // usage: replies that start without a message (a sub-chat's), calls without numbers
        let msgs = vec![
            json!({ "role": "assistant", "content": "a", "_stats": {} }),
            json!({ "role": "assistant", "content": "b", "_stats": { "prompt_tokens": 10 } }),
            json!({ "role": "user" }),
            json!({ "content": "no role" }),
            json!({ "role": "assistant", "content": "c" }),
        ];
        assert_eq!(usage(&msgs), Usage { tokens_in: 10, tokens_out: 0, calls: 2, replies: 2, speed: None });
        let lines = plain(&usage_lines(&th, &usage(&msgs), "age"));
        assert!(lines.iter().any(|l| l.contains("last speed  not measured yet")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("10 (prompts summed over 2 model calls)")), "{lines:?}");
        let one = usage(&[json!({ "role": "user" }), json!({ "role": "assistant", "_stats": { "prompt_tokens": 5, "completion_tokens": 1, "tok_per_s": 9.25 } })]);
        let lines = plain(&usage_lines(&th, &one, "age"));
        assert!(lines[0].ends_with("· 1 reply") && lines.iter().any(|l| l.contains("over 1 model call)")) && lines.iter().any(|l| l.ends_with("9.2 tok/s") || l.ends_with("9.3 tok/s")), "{lines:?}");
    }

    #[test]
    fn status_says_which_server_and_where_the_numbers_come_from() {
        let th = Theme::dark();
        let other = ServerInfo { ok: true, models: vec![], n_ctx: None, kind: "other".into(), base_url: String::new(), error: None };
        let s = plain(&status_lines(&th, &other, &json!({ "model": "pinned-model", "base_url": "http://set/v1", "context_size": 0 }), "9.9.9")).join("\n");
        for want in ["reachable", "address  http://set/v1", "kind  OpenAI-compatible server", "models  none listed", "default  pinned-model", "context  unknown", "Agent Chat 9.9.9 · terminal client"] {
            assert!(s.contains(want), "{want:?} in\n{s}");
        }
        let none = plain(&status_lines(&th, &ServerInfo::default(), &Value::Null, "")).join("\n");
        assert!(none.contains("not reachable") && none.contains("address  not set") && none.contains("default  the server's first") && !none.contains("kind"), "{none}");
        // the keys line up: every value starts in the same column
        let lines = plain(&status_lines(&th, &other, &json!({}), "1"));
        assert!(lines.iter().all(|l| l.chars().nth(13).is_some() && l[..13].ends_with("  ")), "{lines:#?}");
    }

    #[test]
    fn help_rows_line_up_and_name_every_alias() {
        let th = Theme::dark();
        let lines = help_lines(&th);
        for (l, c) in lines.iter().zip(COMMANDS) {
            assert_eq!(l.spans[0].content.chars().count(), 27, "/{}: the descriptions start in one column", c.name);
            assert_eq!(l.spans[1].content, c.desc);
            assert_eq!(l.spans.len(), if c.aliases.is_empty() { 2 } else { 3 });
        }
    }
}
