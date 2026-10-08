//! The Agent Chat HTTP API: JSON calls plus the two server-sent-event streams.

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
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

/// Something that arrived from the server on a background task.
#[derive(Debug)]
pub enum Incoming {
    Global(Event),
    Chat { chat_id: String, event: Event },
    StreamEnded { chat_id: String, error: Option<String> },
    GlobalEnded(Option<String>),
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
        let http = reqwest::Client::builder()
            .default_headers(headers)
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

    pub async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let resp = self.http.get(self.url(path)).send().await.context("request failed")?;
        Ok(Self::check(resp).await?.json().await?)
    }

    pub async fn get_text(&self, path: &str) -> Result<String> {
        let resp = self.http.get(self.url(path)).send().await.context("request failed")?;
        Ok(Self::check(resp).await?.text().await?)
    }

    pub async fn get_bytes(&self, path: &str) -> Result<(Vec<u8>, String)> {
        let resp = self.http.get(self.url(path)).send().await.context("request failed")?;
        let resp = Self::check(resp).await?;
        let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        Ok((resp.bytes().await?.to_vec(), ctype))
    }

    pub async fn send_json<T: for<'de> Deserialize<'de>>(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<T> {
        let mut req = self.http.request(method, self.url(path));
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.context("request failed")?;
        Ok(Self::check(resp).await?.json().await?)
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
        let url = self.url(&format!("/api/chats/{chat_id}/upload"));
        let resp = self.http.post(url).query(&[("name", name)]).body(bytes).send().await.context("upload failed")?;
        Ok(Self::check(resp).await?.json().await?)
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
        let mut buf = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(ended(Some(e.to_string())));
                    return;
                }
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find("\n\n") {
                let block = buf[..pos].to_string();
                buf.drain(..pos + 2);
                let mut data = String::new();
                for line in block.lines() {
                    if let Some(rest) = line.strip_prefix("data:") {
                        if !data.is_empty() {
                            data.push('\n');
                        }
                        data.push_str(rest.trim_start());
                    }
                }
                if data.is_empty() {
                    continue; // a ping
                }
                if let Ok(v) = serde_json::from_str::<Value>(&data) {
                    let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
                    if tx.send(map(Event { kind, data: v })).is_err() {
                        return;
                    }
                }
            }
        }
        let _ = tx.send(ended(None));
    }

    pub async fn follow_global(&self, tx: mpsc::UnboundedSender<Incoming>) {
        self.follow("/api/events", tx, Incoming::Global, Incoming::GlobalEnded).await;
    }

    pub async fn follow_chat(&self, chat_id: String, tx: mpsc::UnboundedSender<Incoming>) {
        let id = chat_id.clone();
        let id2 = chat_id.clone();
        self.follow(
            &format!("/api/chats/{chat_id}/stream"),
            tx,
            move |event| Incoming::Chat { chat_id: id.clone(), event },
            move |error| Incoming::StreamEnded { chat_id: id2.clone(), error },
        )
        .await;
    }
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
