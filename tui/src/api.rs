//! The Agent Chat HTTP API: JSON calls plus the two server-sent-event streams.

use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub emoji: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default = "yes")]
    pub delegate_all: bool,
    #[serde(default)]
    pub delegates: Vec<String>,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub workspace: String,
    #[serde(default = "yes")]
    pub confirm_shell: bool,
    #[serde(default = "yes")]
    pub memory: bool,
    #[serde(default)]
    pub order: Option<i64>,
}
fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Parent {
    pub root_chat_id: String,
    pub caller_id: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ChatSummary {
    pub id: String,
    pub title: String,
    pub agent_id: String,
    #[serde(default)]
    pub updated: f64,
    #[serde(default)]
    pub parent: Option<Parent>,
    #[serde(default)]
    pub routine_id: Option<String>,
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub messages: usize,
    #[serde(default)]
    pub last_role: Option<String>,
    #[serde(default = "idle")]
    pub status: String,
}
fn idle() -> String {
    "idle".into()
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub label: String,
    pub group: String,
    #[serde(default)]
    pub danger: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct State {
    pub agents: Vec<Agent>,
    pub chats: Vec<ChatSummary>,
    pub settings: Value,
    pub tools: Vec<ToolInfo>,
    #[serde(default)]
    pub default_workspace: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ServerInfo {
    pub ok: bool,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub n_ctx: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub base_url: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Memory {
    pub id: i64,
    pub fact: String,
    pub category: String,
    pub importance: i64,
    #[serde(default)]
    pub pinned: i64,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub uses: i64,
    #[serde(default)]
    pub updated: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct MemoryList {
    pub memories: Vec<Memory>,
    pub categories: Vec<String>,
    pub total: i64,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Routine {
    pub id: String,
    pub name: String,
    pub agent_id: String,
    pub prompt: String,
    pub schedule: Value,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub last_run: Option<f64>,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub next_run: Option<f64>,
    #[serde(default)]
    pub chat_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub dir: bool,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub mtime: f64,
    pub path: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Listing {
    pub workspace: String,
    pub path: String,
    pub entries: Vec<FileEntry>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Upload {
    pub name: String,
    pub path: String,
    pub size: u64,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub image: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SearchHit {
    pub chat_id: String,
    pub count: i64,
    pub snippet: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Calendar {
    pub id: String,
    pub name: String,
    pub link: String,
}

/// One server-sent event, already parsed. `type` is pulled out for matching.
#[derive(Clone, Debug)]
pub struct Event {
    pub kind: String,
    pub data: Value,
}

/// Something that arrived from the server on a background task. A chat stream's events carry the
/// visit (`App::stream_gen`) it was opened for, so a stream left from an earlier visit is ignored.
#[derive(Debug)]
pub enum Incoming {
    Global(Event),
    Chat { chat_id: String, visit: u64, event: Event },
    StreamEnded { chat_id: String, visit: u64, error: Option<String> },
    GlobalEnded(Option<String>),
    /// The model server's state, from the regular check (made off the loop that reads the keys).
    Server(ServerInfo),
}

/// SSE framing: append a chunk to `buf` and take out every complete event (`data:` lines joined;
/// comments such as `: ping` skipped). Bytes, not text: a chunk can end inside a UTF-8 character,
/// and only a whole event is decoded.
pub fn feed(buf: &mut Vec<u8>, chunk: &[u8]) -> Vec<Value> {
    let mut from = buf.len().saturating_sub(1); // what was already there holds no complete event
    buf.extend(chunk.iter().filter(|b| **b != b'\r')); // CRLF line ends are allowed too
    let mut out = vec![];
    while let Some(pos) = buf[from..].windows(2).position(|w| w == b"\n\n").map(|p| p + from) {
        from = 0;
        let block: Vec<u8> = buf.drain(..pos + 2).collect();
        let mut data = String::new();
        for line in String::from_utf8_lossy(&block[..pos]).lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            }
        }
        if let Ok(v) = serde_json::from_str::<Value>(&data) {
            out.push(v);
        }
    }
    out
}

#[derive(Clone)]
pub struct Api {
    base: String,
    http: reqwest::Client,
}

impl Api {
    pub fn new(base: &str) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert("X-Agent-Chat", HeaderValue::from_static("1"));
        // (no overall timeout here: the event streams last as long as the app does; each call has
        // its own, see `limit`)
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("http client");
        Self { base: base.trim_end_matches('/').to_string(), http }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("detail").map(|d| d.as_str().map(String::from).unwrap_or_else(|| d.to_string())))
            .unwrap_or(text);
        Err(anyhow!("{} {}", status.as_u16(), detail))
    }

    /// A GET of `path`, with its time limit.
    fn get_req(&self, path: &str) -> reqwest::RequestBuilder {
        self.http.get(self.url(path)).timeout(limit(path))
    }

    pub async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let resp = self.get_req(path).send().await.map_err(unsent)?;
        Self::check(resp).await?.json().await.map_err(unread)
    }

    pub async fn get_text(&self, path: &str) -> Result<String> {
        let resp = self.get_req(path).send().await.map_err(unsent)?;
        Self::check(resp).await?.text().await.map_err(unread)
    }

    pub async fn get_bytes(&self, path: &str) -> Result<(Vec<u8>, String)> {
        let resp = self.get_req(path).send().await.map_err(unsent)?;
        let resp = Self::check(resp).await?;
        let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        Ok((resp.bytes().await.map_err(unread)?.to_vec(), ctype))
    }

    pub async fn send_json<T: for<'de> Deserialize<'de>>(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<T> {
        let mut req = self.http.request(method, self.url(path)).timeout(limit(path));
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.map_err(unsent)?;
        Self::check(resp).await?.json().await.map_err(unread)
    }

    pub async fn post<T: for<'de> Deserialize<'de>>(&self, path: &str, body: Value) -> Result<T> {
        self.send_json(reqwest::Method::POST, path, Some(body)).await
    }
    pub async fn put<T: for<'de> Deserialize<'de>>(&self, path: &str, body: Value) -> Result<T> {
        self.send_json(reqwest::Method::PUT, path, Some(body)).await
    }
    pub async fn patch<T: for<'de> Deserialize<'de>>(&self, path: &str, body: Value) -> Result<T> {
        self.send_json(reqwest::Method::PATCH, path, Some(body)).await
    }
    pub async fn delete<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        self.send_json(reqwest::Method::DELETE, path, None).await
    }

    pub async fn upload(&self, chat_id: &str, name: &str, bytes: Vec<u8>) -> Result<Upload> {
        let path = format!("/api/chats/{chat_id}/upload");
        let resp = self.http.post(self.url(&path)).timeout(limit(&path)).query(&[("name", name)]).body(bytes).send().await.map_err(|e| if e.is_timeout() { timed_out() } else { anyhow::Error::new(e).context("upload failed") })?;
        Self::check(resp).await?.json().await.map_err(unread)
    }

    // -------------------------------------------------------------- shortcuts

    pub async fn state(&self) -> Result<State> {
        self.get("/api/state").await
    }
    pub async fn chats(&self) -> Result<Vec<ChatSummary>> {
        self.get("/api/chats").await
    }
    pub async fn chat(&self, id: &str) -> Result<Value> {
        self.get(&format!("/api/chats/{id}")).await
    }
    pub async fn server(&self) -> Result<ServerInfo> {
        self.get("/api/server").await
    }
    pub async fn new_chat(&self, agent_id: &str) -> Result<Value> {
        self.post("/api/chats", json!({ "agent_id": agent_id })).await
    }
    pub async fn run(&self, chat_id: &str, body: Value) -> Result<Value> {
        self.post(&format!("/api/chats/{chat_id}/run"), body).await
    }
    pub async fn stop(&self, chat_id: &str) -> Result<Value> {
        self.post(&format!("/api/chats/{chat_id}/stop"), json!({})).await
    }
    pub async fn approve(&self, chat_id: &str, approval_id: &str, approve: bool, all: bool) -> Result<Value> {
        self.post(&format!("/api/chats/{chat_id}/approvals/{approval_id}"), json!({ "approve": approve, "all": all })).await
    }
    pub async fn settings(&self, body: Value) -> Result<Value> {
        self.put("/api/settings", body).await
    }
    pub async fn search(&self, q: &str) -> Result<Vec<SearchHit>> {
        self.get(&format!("/api/search?q={}", urlencode(q))).await
    }
    pub async fn memory(&self, q: &str, category: &str) -> Result<MemoryList> {
        self.get(&format!("/api/memory?q={}&category={}", urlencode(q), urlencode(category))).await
    }
    pub async fn routines(&self) -> Result<Vec<Routine>> {
        self.get("/api/routines").await
    }
    pub async fn workspace(&self, agent_id: &str, path: &str) -> Result<Listing> {
        self.get(&format!("/api/workspace?agent_id={}&path={}", urlencode(agent_id), urlencode(path))).await
    }
    pub async fn workspace_file(&self, agent_id: &str, path: &str) -> Result<(Vec<u8>, String)> {
        self.get_bytes(&format!("/api/workspace/file?agent_id={}&path={}", urlencode(agent_id), urlencode(path))).await
    }
    pub async fn calendars(&self) -> Result<Vec<Calendar>> {
        self.get("/api/calendars").await
    }
    pub async fn email(&self) -> Result<Value> {
        self.get("/api/email").await
    }

    // ---------------------------------------------------------------- streams

    /// Follow one SSE stream, forwarding every event to `tx` until it ends. `map` turns a parsed
    /// event into the Incoming variant for this stream.
    async fn follow(&self, path: &str, tx: mpsc::UnboundedSender<Incoming>, map: impl Fn(Event) -> Incoming, ended: impl Fn(Option<String>) -> Incoming) {
        let resp = match self.http.get(self.url(path)).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                let _ = tx.send(ended(Some(format!("HTTP {}", r.status().as_u16()))));
                return;
            }
            Err(e) => {
                let _ = tx.send(ended(Some(e.to_string())));
                return;
            }
        };
        let mut stream = resp.bytes_stream();
        let mut buf = vec![];
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(ended(Some(e.to_string())));
                    return;
                }
            };
            for v in feed(&mut buf, &chunk) {
                let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
                if tx.send(map(Event { kind, data: v })).is_err() {
                    return;
                }
            }
        }
        let _ = tx.send(ended(None));
    }

    pub async fn follow_global(&self, tx: mpsc::UnboundedSender<Incoming>) {
        self.follow("/api/events", tx, Incoming::Global, Incoming::GlobalEnded).await;
    }

    pub async fn follow_chat(&self, chat_id: String, visit: u64, tx: mpsc::UnboundedSender<Incoming>) {
        let id = chat_id.clone();
        let id2 = chat_id.clone();
        self.follow(
            &format!("/api/chats/{chat_id}/stream"),
            tx,
            move |event| Incoming::Chat { chat_id: id.clone(), visit, event },
            move |error| Incoming::StreamEnded { chat_id: id2.clone(), visit, error },
        )
        .await;
    }
}

/// How long a call may take before it counts as failed (a stalled app would otherwise hold the
/// client up for good): what makes the model or another server work (tidying the memory,
/// testing the mail and calendar accounts) and uploads get longer than the rest.
pub fn limit(path: &str) -> Duration {
    let p = path.split('?').next().unwrap_or(path);
    Duration::from_secs(match p {
        "/api/memory/tidy" => 600,
        "/api/email/test" | "/api/calendars/test" => 120,
        _ if p.ends_with("/upload") => 300,
        _ => 20,
    })
}

fn timed_out() -> anyhow::Error {
    anyhow!("The app didn't answer in time (request timed out)")
}

/// A request that got no answer.
fn unsent(e: reqwest::Error) -> anyhow::Error {
    if e.is_timeout() { timed_out() } else { anyhow::Error::new(e).context("request failed") }
}

/// An answer that didn't arrive whole (or wasn't what was expected).
fn unread(e: reqwest::Error) -> anyhow::Error {
    if e.is_timeout() { timed_out() } else { e.into() }
}

pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Settings as a map, so the editor can show every key the server knows.
pub fn settings_map(v: &Value) -> HashMap<String, Value> {
    v.as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn every_call_has_a_time_limit_and_slow_ones_a_longer_one() {
        assert_eq!(limit("/api/state"), Duration::from_secs(20));
        assert_eq!(limit("/api/server"), Duration::from_secs(20), "longer than the app's own checks of the model server (3 s each)");
        assert_eq!(limit("/api/memory/tidy"), Duration::from_secs(600), "the model merges the memory meanwhile");
        assert_eq!(limit("/api/email/test"), Duration::from_secs(120));
        assert_eq!(limit("/api/chats/abc/upload"), Duration::from_secs(300));
        assert_eq!(limit("/api/memory?q=tidy&category="), Duration::from_secs(20));
        assert!(timed_out().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn an_app_that_takes_the_call_and_never_answers_times_out() {
        // a listener that accepts and says nothing (the app stopped with SIGSTOP behaves so); the
        // limit is shortened for the test through a request of our own
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let _keep = tokio::spawn(async move {
            let mut held = vec![];
            while let Ok((sock, _)) = l.accept().await {
                held.push(sock);
            }
        });
        let api = Api::new(&format!("http://127.0.0.1:{port}"));
        let t0 = std::time::Instant::now();
        let e = api.http.get(api.url("/api/state")).timeout(Duration::from_millis(300)).send().await.map_err(unsent).unwrap_err();
        assert!(e.to_string().contains("timed out") && t0.elapsed() < Duration::from_secs(5), "{e}");
    }

    #[test]
    fn sse_events_survive_any_chunking() {
        // the server sends JSON with raw UTF-8 (ensure_ascii=False); a chunk may end mid-character
        let text = "中文—…🎉 é";
        let stream = format!(": ping\n\ndata: {}\n\n: ping\n\ndata: {}\r\n\r\n", json!({ "type": "delta", "text": text }), json!({ "type": "done" }));
        let bytes = stream.as_bytes();
        for cut in 0..=bytes.len() {
            let mut buf = vec![];
            let mut got = feed(&mut buf, &bytes[..cut]);
            got.extend(feed(&mut buf, &bytes[cut..]));
            assert_eq!(got.len(), 2, "cut at {cut}");
            assert_eq!(got[0]["text"], text, "cut at {cut}");
            assert_eq!(got[1]["type"], "done");
            assert!(buf.is_empty());
        }
        // one byte at a time, and several events in one chunk
        let mut buf = vec![];
        let got: Vec<Value> = bytes.iter().flat_map(|b| feed(&mut buf, std::slice::from_ref(b))).collect();
        assert_eq!(got.len(), 2);
        assert!(feed(&mut vec![], b": ping\n\n: ping\n\n").is_empty(), "comments are no events");
        // data split over lines is joined with a line break
        assert_eq!(feed(&mut vec![], b"data: [1,\ndata: 2]\n\n"), vec![json!([1, 2])]);
    }

    #[test]
    fn query_strings_are_encoded() {
        assert_eq!(urlencode("a b&c#d/é"), "a%20b%26c%23d%2F%C3%A9");
    }

    /// Real payloads: every endpoint the client reads into a type, and the events of a few runs
    /// (a handoff with an approval, a denied command, a cut-off reply, hostile text, /compact),
    /// captured from app/main.py driven by tests/mock_llm.py. Consecutive deltas of one kind are
    /// joined (the trace joins them anyway); `ctx` events are left out.
    pub(crate) fn captured() -> Value {
        serde_json::from_str(include_str!("testdata/captured.json")).expect("testdata/captured.json")
    }

    fn typed<T: for<'de> Deserialize<'de>>(v: &Value) -> T {
        serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{e}: {v}"))
    }

    #[test]
    fn every_payload_the_server_returns_reads_into_its_type() {
        let fx = captured();
        let st: State = typed(&fx["state"]);
        assert!(st.agents.len() >= 9 && st.agents.iter().all(|a| !a.id.is_empty() && !a.name.is_empty() && crate::theme::hex(&a.color).is_some()), "{:?}", st.agents);
        let asst = st.agents.iter().find(|a| a.id == "assistant").unwrap();
        assert_eq!((asst.temperature, asst.order, asst.delegate_all, asst.memory, asst.confirm_shell), (None, None, true, true, true));
        assert!(st.tools.iter().any(|t| t.name == "run_shell" && t.danger) && st.tools.iter().all(|t| !t.label.is_empty() && !t.group.is_empty()));
        assert_eq!(st.settings["compact_at"], 70);
        assert_eq!((st.version.as_str(), st.default_workspace.is_empty()), ("0.2.0", false));
        assert_eq!(st.chats.len(), fx["chats"].as_array().unwrap().len());
        let chats: Vec<ChatSummary> = typed(&fx["chats"]);
        assert!(chats.iter().all(|c| c.status == "idle" && c.last_role.as_deref() == Some("assistant") && c.messages >= 2 && c.updated > 0.0));
        let sub = chats.iter().find(|c| c.parent.is_some()).expect("the handoff's sub-chat");
        assert_eq!((sub.agent_id.as_str(), sub.parent.as_ref().unwrap().caller_id.as_str()), ("coder", "orchestrator"));
        let up: ServerInfo = typed(&fx["server"]);
        assert_eq!((up.ok, up.models.clone(), up.n_ctx, up.kind.as_str(), up.error.clone()), (true, vec!["mock-model".to_string()], Some(85000), "llama", None));
        let down: ServerInfo = typed(&fx["server_down"]);
        assert!(!down.ok && down.n_ctx.is_none() && down.kind == "other" && down.error.as_deref().is_some_and(|e| e.contains("ConnectError")) && down.base_url.ends_with("/v1"));
        let mem: MemoryList = typed(&fx["memory"]);
        assert_eq!((mem.total, mem.memories[0].fact.as_str(), mem.memories[0].source.as_str(), mem.memories[0].pinned), (1, "I prefer short answers", "user", 0));
        assert!(mem.categories.contains(&"preference".to_string()));
        let routines: Vec<Routine> = typed(&fx["routines"]);
        assert!(routines.iter().any(|r| r.schedule["type"] == "interval" && !r.enabled && r.next_run.is_some()));
        assert!(routines.iter().all(|r| r.last_run.is_none() && r.chat_id.is_none() && r.last_status.is_none()));
        assert_eq!(crate::ui::schedule_text(&routines[0].schedule), "weekdays at 08:00");
        assert_eq!(crate::ui::schedule_text(&routines[1].schedule), "every 90 min");
        let ls: Listing = typed(&fx["workspace"]);
        assert!(ls.path == "." && ls.entries.iter().any(|e| e.dir && e.size.is_none() && e.name == "uploads"));
        let ups: Vec<Upload> = typed(&fx["upload"]);
        assert_eq!((ups[0].text.as_deref(), ups[0].image, ups[1].text.as_deref(), ups[1].image), (Some("hello\nfile"), false, None, true));
        assert!(ups[0].path.starts_with("uploads/") && ups[1].size > 0);
        let hits: Vec<SearchHit> = typed(&fx["search"]);
        assert!(hits.iter().all(|h| h.count > 0 && h.snippet.contains("question")));
        let cals: Vec<Calendar> = typed(&fx["calendars"]);
        assert!(cals.iter().all(|c| c.name == "Local" && c.link.ends_with("cal.ics")));
        // the email settings are read as a Value: the fields the form takes are there
        for k in ["username", "imap_host", "imap_port", "imap_security", "smtp_host", "smtp_port", "smtp_security", "from_name", "has_password"] {
            assert!(fx["email"].get(k).is_some(), "{k}");
        }
        // every run's events carry a type, and those the trace reads carry their fields
        for (name, run) in fx["runs"].as_object().unwrap() {
            for ev in run.as_array().unwrap() {
                let kind = ev["type"].as_str().unwrap_or_else(|| panic!("{name}: {ev}"));
                let need: &[&str] = match kind {
                    "snapshot" => &["chat", "running", "run_base"],
                    "run_start" => &["chat", "run_base", "agent_id"],
                    "delta" => &["path", "kind", "text"],
                    "assistant_start" => &["path", "agent_id"],
                    "assistant_end" => &["path", "message"],
                    "tool_start" => &["path", "call_id", "name", "args"],
                    "tool_result" => &["path", "call_id", "content"],
                    "subagent_start" => &["path", "agent_id", "chat_id", "continued"],
                    "approval" => &["path", "approval_id", "call_id", "name", "args"],
                    "approval_done" => &["path", "approval_id", "call_id", "approved"],
                    "compact_start" => &["path", "reason", "before_tokens"],
                    "compact_end" => &["path", "compaction"],
                    "notice" => &["path", "text"],
                    "done" => &["chat"],
                    _ => &[],
                };
                for k in need {
                    assert!(ev.get(*k).is_some(), "{name}: {kind} without {k}: {ev}");
                }
            }
        }
    }

    #[test]
    fn missing_optional_fields_take_their_defaults() {
        let a: Agent = typed(&json!({ "id": "x", "name": "X" }));
        assert!(a.delegate_all && a.confirm_shell && a.memory && a.tools.is_empty() && a.model.is_empty() && a.temperature.is_none());
        let c: ChatSummary = typed(&json!({ "id": "c", "title": "t", "agent_id": "x" }));
        assert_eq!((c.status.as_str(), c.pinned, c.messages, c.updated, c.parent.is_none()), ("idle", false, 0, 0.0, true));
        let s: ServerInfo = typed(&json!({ "ok": false }));
        assert!(s.models.is_empty() && s.kind.is_empty());
        let m: Memory = typed(&json!({ "id": 3, "fact": "f", "category": "general", "importance": 5 }));
        assert_eq!((m.pinned, m.uses, m.source.as_str()), (0, 0, ""));
        let r: Routine = typed(&json!({ "id": "r", "name": "n", "agent_id": "a", "prompt": "p", "schedule": null }));
        assert!(!r.enabled && r.schedule.is_null());
        // numbers the server may write as floats or ints
        let a: Agent = typed(&json!({ "id": "x", "name": "X", "temperature": 1, "order": 3 }));
        assert_eq!((a.temperature, a.order), (Some(1.0), Some(3)));
        let e: FileEntry = typed(&json!({ "name": "f", "dir": false, "size": 10, "mtime": 5, "path": "f" }));
        assert_eq!((e.size, e.mtime), (Some(10), 5.0));
        // an agent goes back to the server with every field it came with
        let full = fx_agent();
        let back = serde_json::to_value(typed::<Agent>(&full)).unwrap();
        for (k, v) in full.as_object().unwrap() {
            assert_eq!(&back[k], v, "{k}");
        }
        // what the client can't do without is an error, not a blank
        assert!(serde_json::from_value::<Agent>(json!({ "id": "x" })).is_err());
        assert!(serde_json::from_value::<ServerInfo>(json!({})).is_err());
    }

    fn fx_agent() -> Value {
        captured()["state"]["agents"].as_array().unwrap().iter().find(|a| a["id"] == "coder").cloned().unwrap()
    }

    #[test]
    fn sse_framing_survives_any_chunking_of_any_events() {
        let mut rng = crate::text::tests::Rng(5);
        for case in 0..300 {
            // a few events with random text (escapes, CJK, emoji, line breaks), comments between,
            // LF or CRLF line ends
            let crlf = rng.below(2) == 0;
            let nl = if crlf { "\r\n" } else { "\n" };
            let events: Vec<Value> = (0..1 + rng.below(4)).map(|i| json!({ "type": "delta", "i": i, "text": crate::text::tests::random_text(&mut rng, 10) })).collect();
            let mut stream = String::new();
            for e in &events {
                if rng.below(3) == 0 {
                    stream.push_str(&format!(": ping{nl}{nl}"));
                }
                stream.push_str(&format!("data: {e}{nl}{nl}"));
            }
            let bytes = stream.as_bytes();
            let mut buf = vec![];
            let mut got = vec![];
            let mut at = 0;
            while at < bytes.len() {
                let n = 1 + rng.below(9);
                let end = (at + n).min(bytes.len());
                got.extend(feed(&mut buf, &bytes[at..end]));
                at = end;
            }
            assert_eq!(got, events, "case {case}: {stream:?}");
            assert!(buf.is_empty(), "case {case}");
        }
        // an event cut off when the stream ends stays in the buffer; one that isn't JSON is skipped
        let mut buf = vec![];
        assert!(feed(&mut buf, b"data: {\"type\":\"x\"}\n").is_empty());
        assert!(!buf.is_empty());
        assert_eq!(feed(&mut vec![], b"data: not json\n\ndata:{\"a\":1}\n\nevent: x\nid: 3\n\n"), vec![json!({ "a": 1 })], "no space after data: is fine too");
    }

    #[test]
    fn urls_are_built_and_encoded() {
        assert_eq!(Api::new("http://h:1/").base(), "http://h:1");
        assert_eq!(Api::new("http://h:1///").base(), "http://h:1");
        assert_eq!(Api::new("http://h:1").url("/api/x"), "http://h:1/api/x");
        // every byte comes back out of the encoding, and only unreserved ones stay as they are
        let mut rng = crate::text::tests::Rng(77);
        for case in 0..2000 {
            let s = crate::text::tests::random_text(&mut rng, 8);
            let e = urlencode(&s);
            assert!(e.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.~%".contains(&b)), "case {case}: {e}");
            let mut back = vec![];
            let mut it = e.bytes();
            while let Some(b) = it.next() {
                if b == b'%' {
                    let hex: String = [it.next().unwrap(), it.next().unwrap()].iter().map(|b| *b as char).collect();
                    back.push(u8::from_str_radix(&hex, 16).unwrap());
                } else {
                    back.push(b);
                }
            }
            assert_eq!(back, s.as_bytes(), "case {case}");
        }
        assert_eq!(settings_map(&json!({ "a": 1, "b": "x" })).len(), 2);
        assert!(settings_map(&json!([1, 2])).is_empty() && settings_map(&Value::Null).is_empty());
    }
}
