"use strict";

// ------------------------------------------------------------------ state

const S = {
  agents: [], chats: [], settings: {}, tools: [], defaultWorkspace: "",
  server: { ok: false, models: [] },
  chatId: null, chat: null, es: null, running: false, live: null, follow: true,
  drafts: {}, filter: "", attachments: [], statuses: {}, hits: new Map(), verbose: false,
  queue: [],  // messages sent while the agent works, not delivered to it yet (server-side, per run)
};
try { S.filter = localStorage.getItem("agentFilter") || ""; S.verbose = localStorage.getItem("verbose") === "1"; } catch {}

// Drawn icons from the sprite in index.html (agents keep their own emoji).
function svgIcon(name, cls = "ic") {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", cls);
  svg.setAttribute("aria-hidden", "true");
  const use = document.createElementNS("http://www.w3.org/2000/svg", "use");
  use.setAttribute("href", `#i-${name}`);
  svg.append(use);
  return svg;
}

const APPROVAL_TEXT = {
  run_shell: "Let the agent run this command on your machine?",
  email_send: "Send this email?",
};

const $ = (sel, root = document) => root.querySelector(sel);
const messagesEl = $("#messages");

marked.use({ gfm: true });
hljs.configure({ ignoreUnescapedHTML: true });

// ---------------------------------------------------------------- helpers

function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k === "class") node.className = v;
    else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
    else node.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children.flat()) {
    if (c != null && c !== false) node.append(c instanceof Node ? c : String(c));
  }
  return node;
}

function colored(node, color) {
  node.style.setProperty("--c", color || "#7c6cff");
  return node;
}

async function api(method, url, body) {
  const res = await fetch(url, {
    method,
    headers: body ? { "Content-Type": "application/json", "X-Agent-Chat": "1" } : { "X-Agent-Chat": "1" },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) {
    let msg = `${res.status} ${res.statusText}`;
    try {
      const j = await res.json();
      msg = typeof j.detail === "string" ? j.detail : JSON.stringify(j.detail ?? j);
    } catch {}
    throw new Error(msg);
  }
  return res.json();
}

let toastTimer;
function toast(msg, isError = false) {
  const t = $("#toast");
  t.textContent = msg;
  t.className = isError ? "err" : "";
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.hidden = true), isError ? 6000 : 2500);
}

async function copyText(text, btn) {
  try {
    await navigator.clipboard.writeText(text);
    if (btn) {
      const old = btn.textContent;
      btn.textContent = "Copied";
      setTimeout(() => (btn.textContent = old), 1200);
    } else toast("Copied");
  } catch {
    toast("Copy failed", true);
  }
}

const fmtK = (n) => (n >= 1000 ? (n / 1000).toFixed(1).replace(/\.0$/, "") + "k" : String(n));
const oneLine = (s, n = 140) => String(s ?? "").replace(/\s+/g, " ").trim().slice(0, n);
const pkey = (path) => (path || []).join("/");

function timeAgo(ts) {
  if (!ts) return "";
  const s = Date.now() / 1000 - ts;
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  if (s < 7 * 86400) return `${Math.floor(s / 86400)}d ago`;
  return new Date(ts * 1000).toLocaleDateString();
}

const MISSING_AGENT = { id: "", name: "Deleted agent", emoji: "❔", color: "#888888", purpose: "", tools: [], delegates: [] };
const agentById = (id) => S.agents.find((a) => a.id === id) || { ...MISSING_AGENT, id };
const agentByName = (name) =>
  S.agents.find((a) => a.name.toLowerCase() === String(name || "").trim().toLowerCase()) ||
  { ...MISSING_AGENT, name: name || "Unknown agent" };

// An agent's jack: a socket ring in its color with its own icon inside.
function avatar(agent, small = false) {
  return colored(el("div", { class: "jack" + (small ? " sm" : "") }, agent.emoji || "🤖"), agent.color);
}

function renderMd(node, text, final) {
  node.innerHTML = DOMPurify.sanitize(marked.parse(text || ""));
  for (const a of node.querySelectorAll("a")) {
    a.target = "_blank";
    a.rel = "noopener noreferrer";
  }
  if (!final) return;
  for (const code of node.querySelectorAll("pre code")) {
    try { hljs.highlightElement(code); } catch {}
    const btn = el("button", { class: "copy-code", type: "button" }, "Copy");
    btn.onclick = () => copyText(code.innerText, btn);
    code.parentElement.append(btn);
  }
}

function scrollBottom() {
  messagesEl.scrollTop = messagesEl.scrollHeight;
}
messagesEl.addEventListener("scroll", () => {
  S.follow = messagesEl.scrollHeight - messagesEl.scrollTop - messagesEl.clientHeight < 80;
});

// --------------------------------------------------------------- sidebar

function toolTags(agent) {
  const groups = new Map();
  for (const t of agent.tools) {
    const info = S.tools.find((x) => x.name === t);
    if (!info) continue;
    const label = info.name === "run_shell" ? "Shell" : info.group;
    groups.set(label, info.danger);
  }
  if (!groups.size) return [el("span", { class: "tag" }, "Chat only")];
  return [...groups].map(([label, danger]) => el("span", { class: "tag" + (danger ? " danger" : "") }, label));
}

function setFilter(agentId) {
  S.filter = agentId || "";
  try { localStorage.setItem("agentFilter", S.filter); } catch {}
  renderSidebar();
}

function renderSidebar() {
  if (S.filter && !S.agents.some((a) => a.id === S.filter)) S.filter = "";

  // jack panel: "all chats" + one jack per agent; the lamp shows what its chats are doing
  const rail = $("#rail-agents");
  rail.innerHTML = "";
  const railItem = (agent, title) => {
    const b = busyState(agent?.id);
    const hint = { running: " (working)", waiting: " (needs your approval)", delegated: " (waiting on another agent)" }[b] || "";
    return el("button", {
      type: "button", "data-agent": agent?.id || "",
      class: "jack-item" + (agent ? "" : " all") + ((agent?.id || "") === S.filter ? " active" : "") + (b ? ` is-${b}` : ""),
      title: title + hint,
      onclick: () => { setFilter(agent?.id); if (agent) openActiveChat(agent.id); },
    }, el("span", { class: "lamp" }),
      agent ? avatar(agent) : el("span", { class: "jack" }, svgIcon("grid")),
      el("span", { class: "strip" }, agent ? agent.name : "All"));
  };
  rail.append(railItem(null, "All chats"));
  for (const a of S.agents) rail.append(railItem(a, `${a.name}${a.purpose ? ": " + a.purpose : ""}`));
  requestAnimationFrame(drawCords);

  // call log header: which agent's chats we are looking at
  const head = $("#side-head");
  head.innerHTML = "";
  const fa = S.filter ? agentById(S.filter) : null;
  if (fa) {
    head.append(el("div", { class: "meta" },
      el("div", { class: "name" }, fa.name), el("div", { class: "purpose", title: fa.purpose }, fa.purpose)),
      el("button", { class: "icon-btn", type: "button", title: "Edit agent", onclick: () => openAgentDialog(fa.id) }, svgIcon("pencil")));
    $("#new-chat-btn").replaceChildren(svgIcon("plus"), `New ${fa.name} chat`);
  } else {
    head.append(el("div", { class: "meta" }, el("div", { class: "name" }, "All chats"),
      el("div", { class: "purpose" }, "Every chat you started. Pick an agent on the left to see its own work.")));
    $("#new-chat-btn").replaceChildren(svgIcon("plus"), "New chat");
  }

  const q = $("#chat-search").value.trim().toLowerCase();
  const list = $("#chat-list");
  list.innerHTML = "";
  // "All chats" lists only chats you started; an agent's list also shows work it did for other agents.
  // A search always looks through every chat.
  const chats = S.chats.filter((c) => q
    ? `${c.title} ${c.preview || ""}`.toLowerCase().includes(q) || S.hits.has(c.id)
    : (S.filter ? c.agent_id === S.filter : !c.parent));
  if (!chats.length) list.append(el("div", { class: "empty-note" }, q ? "No matches." : "No chats yet. Start one above."));
  for (const c of chats) {
    const a = agentById(c.agent_id);
    const status = c.status && c.status !== "idle" ? el("span", { class: `status ${c.status}`, title: STATUS_TEXT[c.status] }) : el("span");
    const forWho = c.parent ? `for ${agentById(c.parent.caller_id).name}` : "";
    const hit = q ? S.hits.get(c.id) : null;
    const sub = hit ? `${a.name}: “${hit.snippet}”` : [S.filter && !q ? "" : a.name, forWho, timeAgo(c.updated)].filter(Boolean).join(" · ");
    list.append(el("div", {
      class: "chat-item" + (c.id === S.chatId ? " active" : "") + (c.parent ? " sub-chat" : ""),
      title: c.preview || "",
      onclick: () => openChat(c.id),
    }, avatar(a, true),
      el("div", { class: "meta" },
        el("div", { class: "title" }, c.title),
        el("div", { class: "sub" + (hit ? " snippet" : "") }, sub)),
      status));
  }
}

// Patch cords: while an agent waits on another, draw a cord between their jacks
// (from caller to callee, with a slow flow so you can see which way the call goes).
function drawCords() {
  const svg = $("#cords");
  const rail = $("#rail");
  const ns = "http://www.w3.org/2000/svg";
  svg.replaceChildren();
  svg.style.height = rail.scrollHeight + "px";
  const pairs = new Map();
  for (const c of S.chats) {
    if (c.parent && c.status && c.status !== "idle") pairs.set(`${c.parent.caller_id}>${c.agent_id}`, [c.parent.caller_id, c.agent_id]);
  }
  const box = rail.getBoundingClientRect();
  let n = 0;
  for (const [from, to] of pairs.values()) {
    const a = rail.querySelector(`.jack-item[data-agent="${from}"] .jack`);
    const b = rail.querySelector(`.jack-item[data-agent="${to}"] .jack`);
    if (!a || !b) continue;
    const ra = a.getBoundingClientRect(), rb = b.getBoundingClientRect();
    const x = ra.right - box.left - 3;
    const y0 = ra.top - box.top + rail.scrollTop + ra.height / 2;
    const y1 = rb.top - box.top + rail.scrollTop + rb.height / 2;
    const reach = Math.min(box.width - 5, x + 20 + n * 6);
    const sag = Math.abs(y1 - y0) * 0.12;
    const d = `M ${x} ${y0} C ${reach} ${y0 + sag}, ${reach} ${y1 + sag}, ${x} ${y1}`;
    const color = agentById(from).color || "#f0a43c";
    for (const [cls, attrs] of [["cord", { d, stroke: color }], ["flow", { d }]]) {
      const path = document.createElementNS(ns, "path");
      path.setAttribute("class", cls);
      for (const [k, v] of Object.entries(attrs)) path.setAttribute(k, v);
      svg.append(path);
    }
    for (const y of [y0, y1]) {
      const plug = document.createElementNS(ns, "rect");
      for (const [k, v] of Object.entries({ class: "plug", x: x - 4, y: y - 3.5, width: 8, height: 7, rx: 1.5, fill: color })) plug.setAttribute(k, v);
      svg.append(plug);
    }
    n++;
  }
}
window.addEventListener("resize", () => requestAnimationFrame(drawCords));

const STATUS_TEXT = {
  running: "Working…", waiting: "Waiting for your approval", delegated: "Waiting on another agent",
};

// The most urgent state among an agent's chats (or all chats when agentId is empty).
function busyState(agentId) {
  const st = new Set(S.chats.filter((c) => !agentId || c.agent_id === agentId).map((c) => c.status));
  return ["waiting", "running", "delegated"].find((s) => st.has(s)) || null;
}

// Clicking a busy agent shows what it is doing right now.
function openActiveChat(agentId) {
  const mine = S.chats.filter((c) => c.agent_id === agentId);
  const target = mine.find((c) => c.status === "running" || c.status === "waiting") || mine.find((c) => c.status === "delegated");
  if (target && target.id !== S.chatId) openChat(target.id);
}

function agentCards(container, onPick) {
  container.innerHTML = "";
  const card = (cls, onPickIt, ...children) => el("div", {
    class: cls, tabindex: 0, role: "button", onclick: onPickIt,
    onkeydown: (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onPickIt(); } },
  }, ...children);
  for (const a of S.agents) {
    container.append(colored(card("agent-card", () => onPick(a),
      el("span", { class: "lampdot" }),
      el("div", { class: "top" }, avatar(a), el("span", { class: "call" }, a.name)),
      el("div", { class: "purpose" }, a.purpose || "No job description yet."),
      el("div", { class: "tags" }, toolTags(a))), a.color));
  }
  container.append(card("agent-card new", () => { $("#pick-dialog").close(); openAgentDialog(null); },
    el("div", { class: "top" }, el("span", { class: "jack" }, svgIcon("plus")), el("span", { class: "call" }, "New agent")),
    el("div", { class: "purpose" }, "Give it a job, instructions and tools. Start from a template or a blank sheet.")));
}

async function refreshChats() {
  try {
    S.chats = await api("GET", "/api/chats");
    renderSidebar();
  } catch {}
}

async function reloadState() {
  const st = await api("GET", "/api/state");
  Object.assign(S, { agents: st.agents, chats: st.chats, settings: st.settings, tools: st.tools, defaultWorkspace: st.default_workspace });
  renderSidebar();
  renderMode();
  agentCards($("#welcome-agents"), (a) => newChat(a.id));
  if (S.chat) renderHeader();
}

async function pollServer() {
  try {
    S.server = await api("GET", "/api/server");
  } catch {
    S.server = { ok: false, models: [], error: "Agent Chat backend unreachable" };
  }
  const box = $("#server-status");
  box.className = "server-status " + (S.server.ok ? "ok" : "down");
  const model = (S.settings.model || S.server.models[0] || "online").split("/").pop().replace(/\.gguf$/i, "");
  box.querySelector("span").textContent = S.server.ok ? model : "model offline";
  $("#welcome-readout").textContent = S.server.ok
    ? `Local · ${model} · ${S.agents.length} agents${S.server.n_ctx ? ` · ${fmtK(S.server.n_ctx)} ctx` : ""}`
    : `Model server offline · ${S.agents.length} agents`;
  box.title = S.server.ok
    ? `Connected to ${S.settings.base_url}\nModels: ${S.server.models.join(", ") || "?"}${S.server.n_ctx ? `\nContext: ${S.server.n_ctx} tokens` : ""}`
    : `Can't reach ${S.settings.base_url}\n${S.server.error || ""}`;
  if (S.chat) renderStats();
}

function connectGlobalEvents() {
  const es = new EventSource("/api/events");
  es.onmessage = (e) => {
    const ev = JSON.parse(e.data);
    if (ev.type === "chat_status") {
      const c = S.chats.find((x) => x.id === ev.chat_id);
      const before = S.statuses[ev.chat_id];
      S.statuses[ev.chat_id] = ev.status;
      if (c) {
        c.status = ev.status;
        renderSidebar();
        notifyStatus(c, before, ev.status);
      }
    } else if (ev.type === "chats_changed" || ev.type === "hello") {
      refreshChats();
      if (ev.type === "hello") refreshMemoryCount();
    } else if (ev.type === "queue_returned") {
      returnToInput(ev.chat_id, ev.text);
      if (ev.chat_id === S.chatId) toast("The run ended before the agent read your queued message, so it's back in the message box.");
    } else if (ev.type === "routines_changed") {
      if ($("#routines-dialog").open) loadRoutines();
    } else if (ev.type === "memory_status") {
      $("#memory-btn").classList.toggle("is-running", ev.state === "working");
      $("#memory-btn").title = ev.state === "working" ? "Memory: reviewing the last reply for things to remember…" : "Memory: what your agents know about you";
    } else if (ev.type === "memory_changed") {
      refreshMemoryCount();
      if ($("#memory-dialog").open) loadMemory();
      const learned = [...(ev.added || []), ...(ev.updated || [])];
      if (ev.by === "auto" && learned.length) toast(`Remembered: ${learned[0]}${learned.length > 1 ? ` (+${learned.length - 1} more)` : ""}`);
      else if (ev.by && ev.by !== "you" && ev.by !== "auto" && ev.summary) toast(`${ev.by}: ${ev.summary}`);
    }
  };
}

// -------------------------------------------------------------- navigation

function showView(view) {
  const chat = view === "chat";
  $("#welcome").hidden = chat;
  $("#chat-header").hidden = !chat;
  $("#messages").hidden = !chat;
  $("#composer").hidden = !chat;
}

function openChat(id) {
  if (S.chatId) S.drafts[S.chatId] = $("#input").value;
  if (S.es) { S.es.close(); S.es = null; }
  Object.assign(S, { chatId: id, chat: null, running: false, live: null, follow: true });
  history.replaceState(null, "", id ? `#/chat/${id}` : location.pathname);
  document.body.classList.remove("side-open");
  showView(id ? "chat" : "welcome");
  if (id) applyChatMode(S.chats.find((c) => c.id === id)?.parent);
  renderSidebar();
  $("#history").innerHTML = "";
  $("#live").innerHTML = "";
  if (!id) { document.title = "Agent Chat"; return; }

  $("#input").value = S.drafts[id] || "";
  S.attachments = [];
  renderAttachments();
  S.queue = [];
  renderQueue();
  autoGrow();
  setRunning(false);
  const es = new EventSource(`/api/chats/${id}/stream`);
  S.es = es;
  es.onmessage = (e) => { if (S.es === es) handleEvent(JSON.parse(e.data)); };
  es.onerror = () => {
    if (S.es === es && es.readyState === EventSource.CLOSED) {
      toast("That chat no longer exists.", true);
      openChat(null);
    }
  };
  if (matchMedia("(min-width: 761px)").matches) $("#input").focus();
}

async function newChat(agentId) {
  try {
    const chat = await api("POST", "/api/chats", { agent_id: agentId });
    S.chats.unshift({ ...chat, preview: "", status: "idle" });
    if (S.filter && S.filter !== agentId) S.filter = agentId;
    openChat(chat.id);
  } catch (e) {
    toast(e.message, true);
  }
}

// ------------------------------------------------------------------ header

function renderHeader() {
  const a = agentById(S.chat.agent_id);
  const av = $("#hdr-avatar");
  av.textContent = a.emoji;
  colored(av, a.color);
  if ($("#hdr-title").contentEditable !== "true") $("#hdr-title").textContent = S.chat.title;
  const parent = S.chat.parent;
  $("#hdr-sub").textContent = parent ? `${a.name} · working for ${agentById(parent.caller_id).name}`
    : a.purpose ? `${a.name} · ${a.purpose}` : a.name;
  $("#hdr-edit-agent").hidden = !S.agents.some((x) => x.id === a.id);
  $("#hdr-compact").hidden = !!parent;
  $("#hdr-export").href = `/api/chats/${S.chat.id}/export.md`;
  applyChatMode(parent);
  document.title = `${S.chat.title} · Agent Chat`;
  renderStats();
}

function renderStats() {
  const box = $("#hdr-stats");
  box.innerHTML = "";
  const st = S.chat?.stats || {};
  if (st.context_tokens) {
    const n = S.server.n_ctx;
    const pct = n ? Math.min(100, (st.context_tokens / n) * 100) : 0;
    const meter = el("div", { class: "ctx-meter", title: "Context window used by this chat. Updates live while the agent works" });
    if (n) {
      const segs = el("div", { class: "segs" });
      for (let i = 0; i < 20; i++) {
        const lit = pct >= (i + 0.5) * 5;
        segs.append(el("i", { class: lit ? `on${i >= 18 ? " full" : i >= 14 ? " hi" : ""}` : "" }));
      }
      meter.append(segs);
    }
    meter.append(el("span", { class: "label" }, `ctx ${fmtK(st.context_tokens)}${n ? " / " + fmtK(n) : ""}`));
    box.append(meter);
  }
  if (st.tok_per_s) {
    box.append(el("div", { class: "speed", title: "Generation speed of the last reply" }, el("b", {}, st.tok_per_s.toFixed(1)), "tok/s"));
  }
}

$("#hdr-title").addEventListener("click", () => {
  const t = $("#hdr-title");
  if (t.contentEditable === "true" || !S.chat) return;
  t.contentEditable = "true";
  t.focus();
  getSelection().selectAllChildren(t);
});
$("#hdr-title").addEventListener("keydown", (e) => {
  if (e.key === "Enter") { e.preventDefault(); e.target.blur(); }
  if (e.key === "Escape") { e.target.textContent = S.chat.title; e.target.blur(); }
});
$("#hdr-title").addEventListener("blur", async (e) => {
  const t = e.target;
  t.contentEditable = "false";
  const title = t.textContent.trim();
  if (!S.chat || !title || title === S.chat.title) { t.textContent = S.chat?.title || ""; return; }
  try {
    await api("PATCH", `/api/chats/${S.chatId}`, { title });
    S.chat.title = title;
    renderHeader();
  } catch (err) {
    t.textContent = S.chat.title;
    toast(err.message, true);
  }
});
$("#hdr-delete").onclick = async () => {
  if (!S.chat || !confirm(`Delete “${S.chat.title}”? This can't be undone.`)) return;
  const id = S.chatId;
  try {
    await api("DELETE", `/api/chats/${id}`);
    S.chats = S.chats.filter((c) => c.id !== id);
    delete S.drafts[id];
    S.chatId = null;
    openChat(null);
  } catch (e) {
    toast(e.message, true);
  }
};
$("#hdr-edit-agent").onclick = () => S.chat && openAgentDialog(S.chat.agent_id);
$("#hdr-compact").onclick = async () => {
  if (!S.chat || S.running) return toast("Wait for the current reply or stop it first.");
  try {
    await api("POST", `/api/chats/${S.chatId}/compact`);
  } catch (e) {
    toast(e.message, true);
  }
};

// A sub-chat shows an agent working for another agent: read-only, replies go through the chat that started it.
function applyChatMode(parent) {
  $("#composer").hidden = !!parent;
  $("#sub-banner").hidden = !parent;
  if (!parent) return;
  const me = agentById(S.chat?.agent_id || S.chats.find((c) => c.id === S.chatId)?.agent_id);
  const caller = agentById(parent.caller_id);
  const root = S.chats.find((c) => c.id === parent.root_chat_id);
  $("#sub-banner-text").textContent = `${me.name} is working for ${caller.name}. You can watch here; to reply, use the chat that started it.`;
  $("#sub-banner-open").textContent = root ? `Open “${root.title}”` : "Open the original chat";
  $("#sub-banner-open").onclick = () => openChat(parent.root_chat_id);
}
$("#sub-banner-stop").onclick = () => S.chatId && api("POST", `/api/chats/${S.chatId}/stop`).catch((e) => toast(e.message, true));
$("#menu-btn").onclick = () => document.body.classList.add("side-open");

// ---------------------------------------------------------- message pieces

function thinkingEl(text, live) {
  const label = el("span", { class: "label" }, live ? "Thinking" : "Thoughts");
  const body = el("div", { class: "think-text" }, text || "");
  const node = el("details", { class: "thinking" + (live ? " live" : ""), open: S.verbose }, el("summary", {}, label), body);
  let follow = true; // keep the box scrolled to the newest text unless the user scrolls up inside it
  body.addEventListener("scroll", () => {
    follow = body.scrollHeight - body.scrollTop - body.clientHeight < 24;
  });
  node.addEventListener("toggle", () => {
    if (!node.open) return;
    follow = true;
    body.scrollTop = body.scrollHeight;
    if (live && S.follow) requestAnimationFrame(scrollBottom);
  });
  return {
    el: node, body,
    update(t) {
      body.textContent = t;
      if (node.open && follow) body.scrollTop = body.scrollHeight;
    },
    done(seconds) {
      node.classList.remove("live");
      label.textContent = seconds != null ? `Thought for ${seconds}s` : "Thoughts";
    },
  };
}

function compactDivider(c) {
  const why = { auto: "automatically", manual: "on request", overflow: "because the context was full", cutoff: "because a reply filled the context" }[c.reason] || "";
  const size = c.before_tokens && c.after_tokens ? ` · ~${fmtK(c.before_tokens)} → ${fmtK(c.after_tokens)} tokens` : "";
  const body = el("div", { class: "md" });
  renderMd(body, c.summary, true);
  return el("details", { class: "compact-divider" },
    el("summary", {}, `Earlier messages summarized ${why}${size}`), body);
}

const errorBox = (text) => el("div", { class: "error-box" }, text);
const noticeEl = (text) => el("div", { class: "notice" }, text);

// ------------------------------------------------- live tool calls + code

const LANG_BY_EXT = {
  py: "python", js: "javascript", mjs: "javascript", cjs: "javascript", jsx: "javascript", ts: "typescript", tsx: "typescript",
  html: "xml", htm: "xml", xml: "xml", svg: "xml", vue: "xml", css: "css", scss: "scss", json: "json", sh: "bash", bash: "bash",
  zsh: "bash", fish: "bash", md: "markdown", rs: "rust", go: "go", c: "c", h: "c", cpp: "cpp", cc: "cpp", hpp: "cpp",
  java: "java", kt: "kotlin", rb: "ruby", php: "php", sql: "sql", yaml: "yaml", yml: "yaml", toml: "ini", ini: "ini",
  lua: "lua", swift: "swift", cs: "csharp", dockerfile: "dockerfile", makefile: "makefile",
};

function langFor(path) {
  const base = String(path || "").split("/").pop().toLowerCase();
  return LANG_BY_EXT[base] || LANG_BY_EXT[base.split(".").pop()] || null;
}

// Fill a <code> element, highlighted when the language is known (hljs escapes the text).
function setCode(code, text, lang) {
  if (lang && hljs.getLanguage(lang)) {
    code.innerHTML = hljs.highlight(text, { language: lang, ignoreIllegals: true }).value;
    code.className = `hljs language-${lang}`;
  } else {
    code.textContent = text;
  }
}

function codeBlock(text, lang) {
  const code = el("code");
  setCode(code, text ?? "", lang);
  return el("pre", { class: "code" }, code);
}

// Read the fields of a tool call's JSON arguments while they are still being written
// (unterminated strings and objects are fine). Returns what is known so far.
function partialArgs(raw) {
  const out = {};
  let i = raw.indexOf("{");
  if (i < 0) return out;
  i++;
  const ws = () => { while (i < raw.length && /[\s,]/.test(raw[i])) i++; };
  const str = () => {  // at an opening quote; returns [value, complete]
    let v = "";
    i++;
    while (i < raw.length) {
      const ch = raw[i];
      if (ch === '"') { i++; return [v, true]; }
      if (ch === "\\") {
        const n = raw[i + 1];
        if (n === undefined) return [v, false];
        if (n === "u") {
          const hex = raw.slice(i + 2, i + 6);
          if (hex.length < 4) return [v, false];
          v += String.fromCharCode(parseInt(hex, 16));
          i += 6;
          continue;
        }
        v += { n: "\n", t: "\t", r: "\r", b: "\b", f: "\f" }[n] ?? n;
        i += 2;
        continue;
      }
      v += ch;
      i++;
    }
    return [v, false];
  };
  while (i < raw.length) {
    ws();
    if (raw[i] !== '"') break;
    const [key, keyDone] = str();
    if (!keyDone) break;
    ws();
    if (raw[i] !== ":") break;
    i++;
    ws();
    if (raw[i] === '"') {
      const [val, done] = str();
      out[key] = val;
      if (!done) break;
    } else {
      const m = /^[^,}]*/.exec(raw.slice(i));
      const token = m[0].trim();
      i += m[0].length;
      if (token) { try { out[key] = JSON.parse(token); } catch { out[key] = token; } }
    }
  }
  return out;
}

// What to show for a tool call's arguments: [label, text, language] blocks.
function argBlocks(name, a) {
  if (name === "write_file") return [[`Writing ${a.path ?? "…"}`, a.content, langFor(a.path)]];
  if (name === "edit_file") return [[`Replace in ${a.path ?? "…"}`, a.old_text, langFor(a.path)], ["With", a.new_text, langFor(a.path)]];
  if (name === "run_shell") return [["Command", a.command, "bash"]];
  if (name === "email_send" || name === "email_draft") {
    const head = [a.to && `To: ${a.to}`, a.cc && `Cc: ${a.cc}`, a.subject && `Subject: ${a.subject}`].filter(Boolean).join("\n");
    return [[name === "email_send" ? "Email" : "Draft", `${head}${a.body !== undefined ? `\n\n${a.body}` : ""}`, null]];
  }
  if (name === "ask_agent") return [[`Message to ${a.agent ?? "…"}`, a.message ?? a.task, null]];
  return null;
}

// A card for a tool call that is still being written, shown live as the arguments stream in.
function draftCard(seg, index) {
  const nameEl = el("span", { class: "t-name" }, "tool");
  const argEl = el("span", { class: "t-arg" });
  const count = el("span", { class: "t-state run" }, "writing");
  const body = el("div", { class: "t-body" });
  const card = el("details", { class: "tool-card draft", open: true }, el("summary", {}, nameEl, argEl, count), body);
  seg.el.append(card);
  const d = { index, name: "", raw: "", el: card, blocks: [] };
  d.render = () => {
    nameEl.textContent = d.name || "tool";
    const args = partialArgs(d.raw);
    argEl.textContent = d.name === "ask_agent" ? `→ ${args.agent ?? "…"}` : argSummary(d.name, args) || "";
    count.textContent = `writing · ${fmtK(d.raw.length)} chars`;
    const blocks = argBlocks(d.name, args) || [["Arguments", d.raw, "json"]];
    blocks.forEach(([label, text, lang], k) => {
      let b = d.blocks[k];
      if (!b) {
        const code = el("code");
        b = d.blocks[k] = { label: el("div", { class: "t-label" }), code, pre: el("pre", { class: "code" }, code), follow: true };
        b.pre.addEventListener("scroll", () => { b.follow = b.pre.scrollHeight - b.pre.scrollTop - b.pre.clientHeight < 30; });
        body.append(b.label, b.pre);
      }
      b.label.textContent = label;
      if (b.text !== text) {
        b.text = text;
        setCode(b.code, text ?? "", lang);
        if (b.follow) b.pre.scrollTop = b.pre.scrollHeight;
      }
    });
  };
  return d;
}

function argSummary(name, a) {
  if (a._raw !== undefined) return "(invalid arguments)";
  switch (name) {
    case "run_shell": return oneLine(a.command);
    case "web_search": return a.query;
    case "web_fetch": return a.url;
    case "list_dir": return a.path || ".";
    case "remember": return a.fact;
    case "recall_memory": return a.query;
    case "forget_memory": return `#${a.id}`;
    case "email_list": return `${a.folder || "INBOX"}${a.unread_only ? " · unread" : ""}`;
    case "email_search": return a.query;
    case "email_read": return `#${a.uid}`;
    case "calendar_events": return [a.query && `“${a.query}”`, a.start || "today", a.days && `${a.days} days`].filter(Boolean).join(" · ");
    case "email_send": case "email_draft": return `${a.to} · ${a.subject || ""}`;
    case "read_file": return a.start_line ? `${a.path}:${a.start_line}-${a.end_line ?? ""}` : a.path;
    default: return a.path ?? oneLine(JSON.stringify(a));
  }
}

function argDetails(name, a) {
  const label = (t) => el("div", { class: "t-label" }, t);
  const pre = (t) => el("pre", {}, t ?? "");
  if (a._raw !== undefined) return [label("Raw arguments"), pre(a._raw)];
  if (name === "run_shell") return [label("Command"), codeBlock(a.command, "bash")];
  if (name === "email_send" || name === "email_draft") {
    const head = [`To: ${a.to}`, a.cc && `Cc: ${a.cc}`, `Subject: ${a.subject}`, a.reply_to_uid && `In reply to email #${a.reply_to_uid}`];
    return [label(name === "email_send" ? "Email to send" : "Draft"), pre(`${head.filter(Boolean).join("\n")}\n\n${a.body ?? ""}`)];
  }
  if (name === "write_file") return [label(`Content → ${a.path}`), codeBlock(a.content, langFor(a.path))];
  if (name === "edit_file") return [label(`Replace in ${a.path}`), codeBlock(a.old_text, langFor(a.path)), label("With"), codeBlock(a.new_text, langFor(a.path))];
  if (["read_file", "list_dir", "web_search", "web_fetch", "remember", "recall_memory", "forget_memory",
       "email_list", "email_search", "email_read", "calendar_events"].includes(name)) return [];
  return [label("Arguments"), pre(JSON.stringify(a, null, 2))];
}

function toolCard(name, args, live) {
  const isAsk = name === "ask_agent";
  const state = el("span", { class: "t-state" + (live ? " run" : "") }, live ? "running" : "");
  const summary = el("summary");
  const body = el("div", { class: "t-body" });
  const card = el("details", { class: "tool-card" + (isAsk ? " sub" : "") }, summary, body);
  let subBody = null;
  let approvalBox = null;

  if (isAsk) {
    const target = agentByName(args.agent);
    const text = args.message ?? args.task ?? "";
    colored(card, target.color);
    summary.append(el("span", { class: "patch" }, "Patch →"), avatar(target, true), el("span", { class: "t-name" }, target.name),
      el("span", { class: "t-arg" }, oneLine(text)), state);
    subBody = el("div", { class: "sub-body" });
    body.append(el("div", { class: "t-label" }, "Message"), el("div", { class: "sub-task" }, text), subBody);
    if (live || S.verbose) card.open = true;
  } else {
    summary.append(el("span", { class: "t-name" }, name),
      el("span", { class: "t-arg" }, argSummary(name, args) || ""), state);
    body.append(...argDetails(name, args));
    if (S.verbose) card.open = true;
  }

  return {
    el: card, subBody,
    setChat(chatId) {
      if (!subBody || !chatId || card.querySelector(".open-sub")) return;
      const target = agentByName(args.agent);
      subBody.before(el("button", {
        class: "btn ghost sm open-sub", type: "button",
        onclick: (e) => { e.preventDefault(); openChat(chatId); },
      }, `Open ${target.name}'s chat`, svgIcon("arrow")));
    },
    setContinued() {
      if (!subBody || card.querySelector(".continued")) return;
      subBody.before(el("div", { class: "t-label continued" }, "↩ continues its earlier conversation with this agent"));
    },
    setResult(text) {
      text = String(text ?? "");
      const failed = /^Error\b/.test(text) || text.startsWith("The user denied") || text.startsWith("[cancelled");
      let label = !failed ? "done"
        : text.startsWith("[cancelled") ? "cancelled"
        : text.startsWith("The user denied") ? "denied" : "failed";
      let cls = failed ? "err" : "ok";
      const exit = name === "run_shell" && /^exit code (-?\d+)/.exec(text);
      if (exit) { label = `exit ${exit[1]}`; cls = exit[1] === "0" ? "ok" : "err"; }
      state.className = "t-state " + cls;
      state.textContent = label;
      if (isAsk && !failed) return; // the sub-agent's final reply is already in its transcript
      body.append(el("div", { class: "t-label" }, "Result"), el("pre", {}, text));
    },
    askApproval(approvalId) {
      card.classList.add("approval");
      card.open = true;
      state.className = "t-state";
      state.textContent = "needs approval";
      const yes = el("button", { class: "btn ok sm", type: "button" }, "Approve");
      const no = el("button", { class: "btn danger sm", type: "button" }, "Deny");
      const answer = async (approve) => {
        yes.disabled = no.disabled = true;
        try {
          await api("POST", `/api/chats/${S.chatId}/approvals/${approvalId}`, { approve });
        } catch (e) {
          toast(e.message, true);
          yes.disabled = no.disabled = false;
        }
      };
      yes.onclick = () => answer(true);
      no.onclick = () => answer(false);
      approvalBox = el("div", { class: "approval-box" },
        el("div", { class: "q" }, APPROVAL_TEXT[name] || "Allow this?"), el("div", { class: "row" }, yes, no));
      body.append(approvalBox);
      if (document.hidden) document.title = "Approval needed · Agent Chat";
    },
    markBypassed() {
      if (!summary.querySelector(".t-auto")) state.before(el("span", { class: "t-auto", title: "Ran without asking: permissions are bypassed" }, "auto-approved"));
    },
    approvalDone(approved) {
      approvalBox?.remove();
      approvalBox = null;
      card.classList.remove("approval");
      state.className = "t-state " + (approved ? "run" : "err");
      state.textContent = approved ? "running" : "denied";
    },
  };
}

function makeTurn(agent) {
  const stat = el("span", { class: "stat" });
  const content = el("div");
  const actions = el("div", { class: "turn-actions" });
  const node = colored(el("div", { class: "turn" }, avatar(agent),
    el("div", { class: "turn-body" }, el("div", { class: "turn-name" }, el("span", { class: "call" }, agent.name), stat), content, actions)), agent.color);
  return {
    el: node, body: content, actions, texts: [],
    setStat(tps) { if (tps) stat.textContent = `${tps.toFixed(1)} tok/s`; },
  };
}

function iconBtn(icon, title, onclick) {
  return el("button", { class: "icon-btn", type: "button", title, "aria-label": title, onclick }, svgIcon(icon));
}

// ------------------------------------------------------------ history view

function appendAssistant(container, m, results) {
  if (m._error) container.append(errorBox(m._error));
  if (m.reasoning_content) container.append(thinkingEl(m.reasoning_content, false).el);
  if (m.content) {
    const md = el("div", { class: "md segment" });
    renderMd(md, m.content, true);
    container.append(md);
  }
  if (m._stopped) container.append(el("div", { class: "stopped-tag" }, "Stopped"));
  if (m._truncated) container.append(noticeEl("This reply filled the context window before it finished. The agent continues from where it stopped in the next reply."));
  for (const tc of m.tool_calls || []) {
    let args;
    try { args = JSON.parse(tc.function.arguments || "{}"); } catch { args = { _raw: tc.function.arguments }; }
    const card = toolCard(tc.function.name, args, false);
    const res = results.get(tc.id);
    if (res?._bypassed) card.markBypassed();
    if (res?._sub && card.subBody) {
      if (res._sub.continued) card.setContinued();
      card.setChat(res._sub.chat_id);
      renderSubTranscript(card.subBody, res._sub.messages);
    }
    if (res) card.setResult(res.content);
    container.append(card.el);
  }
}

function renderSubTranscript(container, msgs) {
  const results = new Map(msgs.filter((m) => m.role === "tool").map((m) => [m.tool_call_id, m]));
  for (const m of msgs) if (m.role === "assistant") appendAssistant(container, m, results);
}

function renderHistory(upTo) {
  const h = $("#history");
  h.innerHTML = "";
  if (!S.chat) return;
  const msgs = S.chat.messages.slice(0, upTo ?? undefined);
  const agent = agentById(S.chat.agent_id);
  const caller = S.chat.parent ? agentById(S.chat.parent.caller_id) : null;
  const results = new Map(msgs.filter((m) => m.role === "tool").map((m) => [m.tool_call_id, m]));
  const dividers = new Map((S.chat.compactions || []).map((c) => [c.upto, c]));
  const turns = [];
  let turn = null;
  msgs.forEach((m, i) => {
    if (dividers.has(i)) {
      h.append(compactDivider(dividers.get(i)));
      turn = null;
    }
    if (m.role === "user") {
      turn = null;
      h.append(userBubble(m.content, i, caller, m._recall, m._queued));
    } else if (m.role === "assistant") {
      if (!turn) {
        turn = makeTurn(agent);
        turn.start = i;
        turns.push(turn);
        h.append(turn.el);
      }
      appendAssistant(turn.body, m, results);
      if (m.content) turn.texts.push(m.content);
      if (m._stats?.tok_per_s) turn.setStat(m._stats.tok_per_s);
    }
  });
  turns.forEach((t, idx) => {
    const last = idx === turns.length - 1;
    if (t.texts.length) t.actions.append(iconBtn("copy", "Copy reply", () => copyText(t.texts.join("\n\n"))));
    if (!caller) t.actions.append(iconBtn("redo", "Regenerate", () => {
      if (!last && !confirm("Regenerating this reply deletes every message after it. Continue?")) return;
      runChat({ from_index: t.start });
    }));
    if (last) t.actions.classList.add("show");
  });
  if (!msgs.length && !S.running && !caller) {
    const a = agentById(S.chat.agent_id);
    h.append(el("div", { class: "turn" }, avatar(a), el("div", { class: "turn-body" },
      el("div", { class: "turn-name" }, a.name),
      el("div", { class: "md muted" }, a.purpose ? `Ready. My purpose: ${a.purpose}.` : "Ready when you are."))));
  }
}

function userBubble(text, index, from = null, recall = "", queued = false) {
  const wrap = el("div", { class: "msg-user" + (from ? " from-agent" : "") });
  const show = () => {
    wrap.innerHTML = "";
    const actions = el("div", { class: "actions" }, iconBtn("copy", "Copy", () => copyText(text)));
    if (index >= 0 && !from) actions.prepend(iconBtn("pencil", "Edit and resend", edit));
    const long = text.length > 1500;
    const bubble = el("div", { class: "bubble" }, long ? text.slice(0, 1200) + "…" : text);
    if (long) {
      const more = el("button", { type: "button", class: "more-btn" }, "Show all");
      more.onclick = () => {
        const open = more.textContent === "Show all";
        bubble.firstChild.textContent = open ? text : text.slice(0, 1200) + "…";
        more.textContent = open ? "Show less" : "Show all";
      };
      bubble.append(more);
    }
    const col = el("div", { class: "bubble-col" });
    if (from) col.append(el("div", { class: "from" }, avatar(from, true), from.name));
    col.append(bubble);
    if (queued) col.append(el("div", { class: "queued-tag", title: "You sent this while the agent was working; it read it at its next step" }, "Sent while it worked"));
    if (recall) {
      const lines = recall.split("\n").filter(Boolean);
      const list = el("div", { class: "recall-list", hidden: true }, lines.map((l) => el("div", {}, l.replace(/^- \[\d+\] /, "• "))));
      col.append(el("button", {
        class: "recall-chip", type: "button", title: "Memories the agent was reminded of with this message",
        onclick: () => (list.hidden = !list.hidden),
      }, svgIcon("memory"), `${lines.length} ${lines.length === 1 ? "memory" : "memories"} recalled`), list);
    }
    wrap.append(actions, col);
  };
  const edit = () => {
    if (S.running) return toast("Wait for the current reply or stop it first.");
    const ta = el("textarea", {}, text);
    const send = el("button", { class: "btn primary sm", type: "button" }, "Send");
    const cancel = el("button", { class: "btn ghost sm", type: "button", onclick: show }, "Cancel");
    send.onclick = () => {
      const content = ta.value.trim();
      if (content) runChat({ content, from_index: index });
    };
    ta.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); send.click(); }
      if (e.key === "Escape") show();
    });
    wrap.innerHTML = "";
    wrap.append(el("div", { class: "edit-box" }, ta, el("div", { class: "row" }, cancel, send)));
    ta.focus();
  };
  show();
  return wrap;
}

// --------------------------------------------------------------- live view

let flushQueued = false;
const dirty = new Set();
function markDirty(seg) {
  dirty.add(seg);
  if (!flushQueued) {
    flushQueued = true;
    requestAnimationFrame(() => {
      flushQueued = false;
      for (const s of dirty) s.render();
      dirty.clear();
      if (S.follow) scrollBottom();
    });
  }
}

function resetLive() {
  $("#live").innerHTML = "";
  S.live = null;
}

function startLive(agentId) {
  const turn = makeTurn(agentById(agentId));
  $("#live").append(turn.el);
  S.live = { turn, containers: new Map([["", { body: turn.body, seg: null }]]), cards: new Map() };
}

const liveContainer = (path) => S.live?.containers.get(pkey(path));

function newSegment(container) {
  const seg = {
    text: "", reasoning: "", args: 0, t0: Date.now(), thinkT0: null, thinkT1: null,
    el: el("div", { class: "segment" }),
    wait: el("div", { class: "waiting-dots" }, "Working"),
    think: null, md: null, drafts: new Map(),
  };
  seg.el.append(seg.wait);
  container.body.append(seg.el);
  seg.render = () => {
    seg.wait?.remove();
    seg.wait = null;
    if (seg.reasoning) {
      if (!seg.think) { seg.think = thinkingEl("", true); seg.el.prepend(seg.think.el); }
      seg.think.update(seg.reasoning);
    }
    if (seg.text) {
      if (!seg.md) { seg.md = el("div", { class: "md" }); seg.el.append(seg.md); }
      renderMd(seg.md, seg.text, false);
    }
    for (const d of seg.drafts.values()) d.render();
    if (seg.think && seg.thinkT1 == null && (seg.text || seg.args)) seg.thinkT1 = Date.now();
    if (seg.think && seg.thinkT1 != null) seg.think.done(Math.max(1, Math.round((seg.thinkT1 - seg.thinkT0) / 1000)));
  };
  return seg;
}

function liveAssistantStart(ev) {
  const c = liveContainer(ev.path);
  if (c) c.seg = newSegment(c);
}

function liveDelta(ev) {
  if (ev.kind === "compact") {
    const c = liveContainer(ev.path);
    if (c?.compact) {
      c.compact.n += ev.text.length;
      c.compact.count.textContent = ` ${fmtK(c.compact.n)} chars`;
    }
    return;
  }
  const seg = liveContainer(ev.path)?.seg;
  if (!seg) return;
  if (ev.kind === "reasoning") {
    seg.thinkT0 ??= Date.now();
    seg.reasoning += ev.text;
  } else if (ev.kind === "content") seg.text += ev.text;
  else if (ev.kind === "tool_args") {
    const index = ev.index ?? 0;
    let d = seg.drafts.get(index);
    if (!d) { d = draftCard(seg, index); seg.drafts.set(index, d); }
    if (ev.name) d.name = ev.name;
    d.raw += ev.text;
    seg.args += ev.text.length;
  }
  markDirty(seg);
}

function liveAssistantEnd(ev) {
  const c = liveContainer(ev.path);
  const seg = c?.seg;
  if (!seg) return;
  dirty.delete(seg);
  seg.wait?.remove();
  for (const d of seg.drafts.values()) d.render();
  // the finished calls arrive next as tool_start events, in index order; each replaces its draft
  c.drafts = [...seg.drafts.values()].sort((a, b) => a.index - b.index);
  const m = ev.message;
  if (m._truncated) {  // calls cut off mid-way are dropped by the server, so no tool_start will replace them
    for (const d of c.drafts) d.el.remove();
    c.drafts = [];
  }
  if (m.reasoning_content) {
    if (!seg.think) { seg.think = thinkingEl("", false); seg.el.prepend(seg.think.el); }
    seg.think.update(m.reasoning_content);
    seg.thinkT1 ??= Date.now();
    seg.think.done(seg.thinkT0 ? Math.max(1, Math.round((seg.thinkT1 - seg.thinkT0) / 1000)) : null);
  }
  if (m.content) {
    if (!seg.md) { seg.md = el("div", { class: "md" }); seg.el.append(seg.md); }
    renderMd(seg.md, m.content, true);
  }
  if (!ev.path?.length && m._stats) {
    S.live.turn.setStat(m._stats.tok_per_s);
    if (m._stats.prompt_tokens) {
      S.chat.stats = { context_tokens: m._stats.prompt_tokens + (m._stats.completion_tokens || 0), tok_per_s: m._stats.tok_per_s };
      renderStats();
    }
  }
  c.seg = null;
}

function liveUserMessage(ev) {
  if (!S.live) return;
  const m = ev.message;
  $("#live").append(userBubble(m.content, ev.index, null, m._recall, m._queued));
  const turn = makeTurn(agentById(S.chat.agent_id));  // the agent's output after it continues here
  $("#live").append(turn.el);
  S.live.turn = turn;
  S.live.containers.set("", { body: turn.body, seg: null });
}

function liveToolStart(ev) {
  const c = liveContainer(ev.path);
  if (!c) return;
  const card = toolCard(ev.name, ev.args || {}, true);
  const draft = c.drafts?.shift();
  if (draft?.el.isConnected) draft.el.replaceWith(card.el);
  else c.body.append(card.el);
  S.live.cards.set(ev.call_id, card);
  if (card.subBody) S.live.containers.set(pkey([...(ev.path || []), ev.call_id]), { body: card.subBody, seg: null });
}

function handleEvent(ev) {
  switch (ev.type) {
    case "snapshot":
      S.chat = ev.chat;
      renderHeader();
      renderHistory(ev.running ? ev.run_base : null);
      resetLive();
      setRunning(ev.running);
      S.queue = ev.queue || [];
      renderQueue();
      requestAnimationFrame(scrollBottom);
      break;
    case "run_start":
      S.chat = ev.chat;
      renderHeader();
      renderHistory(ev.run_base);
      resetLive();
      startLive(ev.agent_id);
      setRunning(true);
      S.follow = true;
      break;
    case "assistant_start": liveAssistantStart(ev); break;
    case "delta": liveDelta(ev); return; // scroll handled by the frame flush
    case "assistant_end": liveAssistantEnd(ev); break;
    case "tool_start": liveToolStart(ev); break;
    case "user_message": liveUserMessage(ev); break;
    case "queue": S.queue = ev.items; renderQueue(); return;
    case "ctx":  // live context size while the agent works (the exact count still arrives with assistant_end)
      if (!ev.path?.length && S.chat) { S.chat.stats = { ...S.chat.stats, context_tokens: ev.tokens }; renderStats(); }
      return;
    case "tool_result": S.live?.cards.get(ev.call_id)?.setResult(ev.content); break;
    case "subagent_start": {
      const card = S.live?.cards.get(ev.path.at(-1));
      if (ev.continued) card?.setContinued();
      card?.setChat(ev.chat_id);
      break;
    }
    case "compact_start": {
      const c = liveContainer(ev.path);
      if (!c) break;
      const count = el("span", { class: "muted" });
      c.compact = { n: 0, count, el: el("div", { class: "compact-live" }, "Compacting: summarizing older messages to free up context", el("span", { class: "waiting-dots" }), count) };
      c.body.append(c.compact.el);
      break;
    }
    case "compact_end": {
      const c = liveContainer(ev.path);
      c?.compact?.el.replaceWith(compactDivider(ev.compaction));
      if (c) c.compact = null;
      break;
    }
    case "approval": S.live?.cards.get(ev.call_id)?.askApproval(ev.approval_id); break;
    case "approval_done": S.live?.cards.get(ev.call_id)?.approvalDone(ev.approved); break;
    case "approval_bypassed": S.live?.cards.get(ev.call_id)?.markBypassed(); break;
    case "notice": liveContainer(ev.path)?.body.append(noticeEl(ev.text)); break;
    case "error": S.live?.turn.body.append(errorBox(ev.message)); break;
    case "done": {
      const keepScroll = !S.follow && messagesEl.scrollTop;
      S.chat = ev.chat;
      renderHeader();
      renderHistory(null);
      resetLive();
      setRunning(false);
      S.queue = [];  // delivered, or handed back to the message box (queue_returned)
      renderQueue();
      if (keepScroll) messagesEl.scrollTop = keepScroll;
      break;
    }
  }
  if (S.follow) scrollBottom();
}

// ---------------------------------------------------------------- composer

function setRunning(running) {
  S.running = running;
  const send = $("#send-btn");
  send.firstChild.textContent = running ? "Queue " : "Send ";
  send.title = running ? "Queue (Enter): the agent reads it at its next step" : "Send (Enter)";
  $("#input").placeholder = running ? "Queue a message: the agent reads it at its next step" : "Message";
  $("#stop-btn").hidden = !running;
  $("#sub-banner-stop").hidden = !running;
  $("#hdr-compact").disabled = running;
}

async function runChat(body) {
  if (S.running) return toast("Wait for the current reply or stop it first.");
  setRunning(true);
  try {
    return await api("POST", `/api/chats/${S.chatId}/run`, body);
  } catch (e) {
    setRunning(false);
    toast(e.message, true);
    return false;
  }
}

// ------------------------------------------------------------ message queue
// Like Claude Code: messages sent while the agent works wait here, and the server hands them to
// the agent between steps. Edit or ✕ takes one back; ↑ in an empty box takes them all back.

function renderQueue() {
  const box = $("#queue");
  box.innerHTML = "";
  box.hidden = !S.queue.length;
  if (!S.queue.length) return;
  box.append(el("div", { class: "queue-head" }, `Queued (${S.queue.length})`,
    el("span", { class: "muted" }, "the agent reads these at its next step"), el("span", { class: "grow" }),
    el("span", { class: "muted" }, el("kbd", {}, "↑"), " edit")));
  for (const item of S.queue) {
    box.append(el("div", { class: "queue-item" }, svgIcon("clock", "ic lead"),
      el("div", { class: "q-text", title: item.content }, item.content),
      iconBtn("pencil", "Edit (takes it out of the queue)", () => unqueue(item.id)),
      el("button", { type: "button", class: "icon-btn", title: "Remove from the queue", "aria-label": "Remove from the queue",
        onclick: () => unqueue(item.id, true) }, "✕")));
  }
}

// Put text back in a chat's message box, ahead of anything typed there since.
function returnToInput(chatId, text) {
  if (!text) return;
  if (chatId !== S.chatId) {
    S.drafts[chatId] = S.drafts[chatId] ? `${text}\n\n${S.drafts[chatId]}` : text;
    return;
  }
  const input = $("#input");
  input.value = input.value.trim() ? `${text}\n\n${input.value}` : text;
  autoGrow();
  input.focus();
  input.setSelectionRange(text.length, text.length);
}

async function unqueue(itemId, discard = false) {
  const chatId = S.chatId;
  try {
    const url = `/api/chats/${chatId}/queue` + (itemId ? `/${itemId}` : "");
    const res = await api("DELETE", url);
    const items = itemId ? [res] : res.items;
    if (!discard) returnToInput(chatId, items.map((i) => i.content).join("\n\n"));
  } catch (e) {
    toast(e.message, true);
  }
}

function autoGrow() {
  const ta = $("#input");
  ta.style.height = "auto";
  ta.style.height = Math.min(ta.scrollHeight, 260) + "px";
}

$("#input").addEventListener("input", autoGrow);
$("#input").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    $("#composer").requestSubmit();
  } else if (e.key === "ArrowUp" && !e.isComposing && !e.target.value && S.queue.length) {
    e.preventDefault();  // like Claude Code: pull the queued messages back to edit them
    unqueue(null);
  }
});
$("#composer").addEventListener("submit", async (e) => {
  e.preventDefault();
  const input = $("#input");
  const typed = input.value.trim();
  const content = (typed + attachmentText()).trim();
  if (!content || !S.chatId) return;
  if ("Notification" in window && Notification.permission === "default") Notification.requestPermission();
  input.value = "";
  S.attachments = [];
  renderAttachments();
  autoGrow();
  delete S.drafts[S.chatId];
  if (S.running) {  // the server queues it (or starts a run with it, if the run just ended)
    try {
      const res = await api("POST", `/api/chats/${S.chatId}/run`, { content });
      if (!res.queued) setRunning(true);
    } catch (e) {
      input.value = typed;
      autoGrow();
      toast(e.message, true);
    }
    return;
  }
  const bubble = userBubble(content, -1);
  $("#history").append(bubble);
  S.follow = true;
  scrollBottom();
  const res = await runChat({ content });
  if (!res) {
    input.value = typed;
    autoGrow();
    renderHistory(null);
  } else if (res.queued) bubble.remove();  // a run had started elsewhere (another tab, a routine)
});
$("#stop-btn").onclick = () => S.chatId && api("POST", `/api/chats/${S.chatId}/stop`).catch((e) => toast(e.message, true));

// --------------------------------------------------------------- dialogs

function fillModelSelect(select, current, defaultLabel) {
  select.innerHTML = "";
  const models = [...S.server.models];
  if (current && !models.includes(current)) models.push(current);
  select.append(el("option", { value: "" }, defaultLabel));
  for (const m of models) select.append(el("option", { value: m, title: m }, m.split("/").pop()));
  select.value = current || "";
}

const AGENT_TEMPLATES = [
  { name: "Personal Assistant", emoji: "🗓️", color: "#4fb3a9", purpose: "Keeps track of your plans, tasks and the people in your life",
    tools: ["ask_agent", "read_file", "write_file", "calendar_events"],
    system_prompt: "You are the user's personal assistant. Help them plan their day, keep track of tasks and commitments, think through decisions, and remember what matters to them (people, routines, goals, deadlines). Keep answers practical and short. Keep a running to-do list in todo.md in the workspace when it helps. For email, ask the Mail agent; for research, ask the Researcher." },
  { name: "Tutor", emoji: "🎓", color: "#6c8cff", purpose: "Teaches any topic step by step",
    tools: ["ask_agent", "web_search", "web_fetch"],
    system_prompt: "You are a patient tutor. Find out what the user already knows, explain step by step with small concrete examples, and check understanding with a quick question before moving on. Adapt to how they learn best and remember it. Don't just hand over homework answers; explain the reasoning." },
  { name: "Translator", emoji: "🌐", color: "#2bb3d9", purpose: "Translates text naturally between languages", tools: [],
    system_prompt: "Translate the user's text faithfully and naturally, keeping formatting, names and tone. If the target language isn't stated, translate into English, or from English into the language the user used most recently. Briefly point out idioms or phrases that don't translate directly." },
  { name: "Security Analyst", emoji: "🛡️", color: "#d95f5f", purpose: "Analyzes logs, alerts and configs; explains attacks and defenses",
    tools: ["ask_agent", "web_search", "web_fetch", "list_dir", "read_file"],
    system_prompt: "You are a defensive security analyst. Help analyze logs, alerts, malware reports and configurations; explain how attacks work and how to detect and mitigate them; review code and setups for weaknesses; map findings to MITRE ATT&CK when useful. Be precise about what the evidence shows versus what is suspicion, and give concrete next steps." },
  { name: "Brainstormer", emoji: "💡", color: "#f2c14e", purpose: "Generates and sharpens ideas with you", tools: ["ask_agent"],
    system_prompt: "You are a creative partner. Generate many varied ideas quickly, then help the user pick and refine the best ones. Build on their ideas, offer unexpected angles, and keep momentum. Use what you know about the user's interests." },
];

let editingAgentId = null;
function openAgentDialog(id) {
  editingAgentId = id;
  const a = id ? S.agents.find((x) => x.id === id) : {
    name: "", emoji: "🤖", color: "#7c6cff", purpose: "", system_prompt: "", tools: ["ask_agent"], delegates: [],
    delegate_all: true, model: "", temperature: null, workspace: "", confirm_shell: true, memory: true,
  };
  if (!a) return;
  const form = $("#agent-form");
  const f = (n) => form.elements.namedItem(n);
  $("#agent-dialog-title").textContent = id ? `Edit ${a.name}` : "New agent";
  f("emoji").value = a.emoji;
  f("name").value = a.name;
  f("color").value = /^#[0-9a-f]{6}$/i.test(a.color) ? a.color : "#7c6cff";
  f("purpose").value = a.purpose;
  f("system_prompt").value = a.system_prompt;
  f("temperature").value = a.temperature ?? "";
  f("workspace").value = a.workspace;
  f("workspace").placeholder = S.defaultWorkspace + " (shared default)";
  f("confirm_shell").checked = a.confirm_shell;
  f("delegate_all").checked = a.delegate_all !== false;
  f("memory").checked = a.memory !== false;
  fillModelSelect(f("model"), a.model, "Default (from Settings)");

  const toolBox = $("#tool-checks");
  toolBox.innerHTML = "";
  for (const t of S.tools) {
    toolBox.append(el("label", { class: "check" },
      el("input", { type: "checkbox", "data-tool": t.name, checked: a.tools.includes(t.name) }),
      t.label,
      t.danger ? el("span", { class: "tag danger" }, "risky") : null));
  }
  const delBox = $("#delegate-checks");
  delBox.innerHTML = "";
  const others = S.agents.filter((x) => x.id !== id);
  for (const o of others) {
    delBox.append(el("label", { class: "check" },
      el("input", { type: "checkbox", "data-delegate": o.id, checked: a.delegates.includes(o.id) }), `${o.emoji} ${o.name}`));
  }
  if (!others.length) delBox.append(el("div", { class: "empty-note" }, "No other agents yet."));
  $("#template-row").hidden = !!id;
  const tpl = $("#agent-template");
  tpl.innerHTML = "";
  tpl.append(el("option", { value: "" }, "Blank agent"), ...AGENT_TEMPLATES.map((t, i) => el("option", { value: i }, `${t.emoji} ${t.name}`)));
  tpl.onchange = () => {
    const t = AGENT_TEMPLATES[tpl.value];
    if (!t) return;
    f("emoji").value = t.emoji;
    f("name").value = t.name;
    f("color").value = t.color;
    f("purpose").value = t.purpose;
    f("system_prompt").value = t.system_prompt;
    for (const box of form.querySelectorAll("input[data-tool]")) box.checked = t.tools.includes(box.dataset.tool);
    syncAgentForm();
  };
  syncAgentForm();
  $("#agent-delete").hidden = !id;
  $("#agent-dialog").showModal();
}

function syncAgentForm() {
  const form = $("#agent-form");
  const has = (name) => $(`#tool-checks input[data-tool="${name}"]`)?.checked;
  const all = form.elements.namedItem("delegate_all").checked;
  $("#delegates-fieldset").disabled = !has("ask_agent");
  form.elements.namedItem("confirm_shell").disabled = !has("run_shell");
  for (const box of form.querySelectorAll("input[data-delegate]")) {
    box.disabled = all;
    if (all) box.checked = true;
  }
}
$("#tool-checks").addEventListener("change", syncAgentForm);
$("#agent-form").elements.namedItem("delegate_all").addEventListener("change", (e) => {
  if (!e.target.checked) for (const box of $("#agent-form").querySelectorAll("input[data-delegate]")) box.checked = false;
  syncAgentForm();
});

$("#agent-form").addEventListener("submit", async (e) => {
  if (e.submitter?.value !== "save") return;
  e.preventDefault();
  const form = e.target;
  const f = (n) => form.elements.namedItem(n);
  const temp = f("temperature").value.trim();
  const body = {
    name: f("name").value.trim(),
    emoji: f("emoji").value.trim() || "🤖",
    color: f("color").value,
    purpose: f("purpose").value.trim(),
    system_prompt: f("system_prompt").value,
    tools: [...form.querySelectorAll("input[data-tool]:checked")].map((x) => x.dataset.tool),
    delegate_all: f("delegate_all").checked,
    delegates: f("delegate_all").checked ? []
      : [...form.querySelectorAll("input[data-delegate]:checked")].map((x) => x.dataset.delegate),
    model: f("model").value,
    temperature: temp === "" ? null : Number(temp),
    workspace: f("workspace").value.trim(),
    confirm_shell: f("confirm_shell").checked,
    memory: f("memory").checked,
  };
  try {
    const saved = editingAgentId
      ? await api("PUT", `/api/agents/${editingAgentId}`, body)
      : await api("POST", "/api/agents", body);
    $("#agent-dialog").close();
    await reloadState();
    toast(`Saved ${saved.name}`);
  } catch (err) {
    toast(err.message, true);
  }
});

$("#agent-delete").onclick = async () => {
  const a = S.agents.find((x) => x.id === editingAgentId);
  if (!a || !confirm(`Delete the agent “${a.name}”? Existing chats with it stay readable but can't continue.`)) return;
  try {
    await api("DELETE", `/api/agents/${a.id}`);
    $("#agent-dialog").close();
    await reloadState();
  } catch (e) {
    toast(e.message, true);
  }
};

function openSettings() {
  const form = $("#settings-form");
  const f = (n) => form.elements.namedItem(n);
  f("base_url").value = S.settings.base_url;
  f("api_key").value = S.settings.api_key;
  f("max_steps").value = S.settings.max_steps ?? 30;
  f("shell_timeout").value = S.settings.shell_timeout ?? 180;
  f("auto_compact").checked = S.settings.auto_compact !== false;
  f("auto_memory").checked = S.settings.auto_memory !== false;
  f("bypass_approvals").checked = !!S.settings.bypass_approvals;
  f("compact_at").value = S.settings.compact_at ?? 70;
  f("context_size").value = S.settings.context_size ?? 0;
  f("search_url").value = S.settings.search_url ?? "";
  f("browser_executable").value = S.settings.browser_executable ?? "/usr/bin/brave";
  f("browser_headless").checked = S.settings.browser_headless !== false;
  fillModelSelect(f("model"), S.settings.model, "Server default / first model");
  $("#settings-test").textContent = "";
  $("#email-test").textContent = "";
  $("#calendar-test").textContent = "";
  loadEmailSettings();
  loadCalendars();
  $("#settings-dialog").showModal();
}

$("#settings-test-btn").onclick = async () => {
  const form = $("#settings-form");
  const f = (n) => form.elements.namedItem(n);
  const out = $("#settings-test");
  out.textContent = "Testing…";
  const qs = new URLSearchParams({ base_url: f("base_url").value.trim(), api_key: f("api_key").value });
  try {
    const info = await api("GET", `/api/server?${qs}`);
    out.textContent = info.ok
      ? `✓ Connected. Models: ${info.models.join(", ") || "none listed"}${info.n_ctx ? ` · context ${info.n_ctx}` : ""}`
      : `✗ ${info.error}`;
    if (info.ok) {
      const cur = f("model").value;
      S.server.models = info.models;
      fillModelSelect(f("model"), cur, "Server default / first model");
    }
  } catch (e) {
    out.textContent = `✗ ${e.message}`;
  }
};

const EMAIL_PRESETS = {
  gmail: { imap_host: "imap.gmail.com", imap_port: 993, imap_security: "ssl", smtp_host: "smtp.gmail.com", smtp_port: 465, smtp_security: "ssl" },
  icloud: { imap_host: "imap.mail.me.com", imap_port: 993, imap_security: "ssl", smtp_host: "smtp.mail.me.com", smtp_port: 587, smtp_security: "starttls" },
  yahoo: { imap_host: "imap.mail.yahoo.com", imap_port: 993, imap_security: "ssl", smtp_host: "smtp.mail.yahoo.com", smtp_port: 465, smtp_security: "ssl" },
  fastmail: { imap_host: "imap.fastmail.com", imap_port: 993, imap_security: "ssl", smtp_host: "smtp.fastmail.com", smtp_port: 465, smtp_security: "ssl" },
};
const EMAIL_FIELDS = ["imap_host", "imap_port", "imap_security", "smtp_host", "smtp_port", "smtp_security", "username", "from_name"];

async function loadEmailSettings() {
  const form = $("#settings-form");
  try {
    const cfg = await api("GET", "/api/email");
    for (const k of EMAIL_FIELDS) form.elements.namedItem("email_" + k).value = cfg[k] ?? "";
    form.elements.namedItem("email_password").value = "";
    form.elements.namedItem("email_password").placeholder = cfg.has_password ? "saved (leave empty to keep)" : "";
    $("#email-state").textContent = cfg.configured ? `· ${cfg.username}` : "· not set up";
  } catch {}
}

async function saveEmailSettings() {
  const form = $("#settings-form");
  const body = {};
  for (const k of EMAIL_FIELDS) {
    const v = form.elements.namedItem("email_" + k).value.trim();
    body[k] = k.endsWith("_port") ? Number(v) || 0 : v;
  }
  body.password = form.elements.namedItem("email_password").value;
  if (!body.from_address) body.from_address = body.username;
  const cfg = await api("PUT", "/api/email", body);
  $("#email-state").textContent = cfg.configured ? `· ${cfg.username}` : "· not set up";
  return cfg;
}

async function loadCalendars() {
  try {
    const cals = await api("GET", "/api/calendars");
    $("#calendar-state").textContent = cals.length ? `· ${cals.length} connected` : "· none yet";
    const list = $("#calendar-list");
    list.innerHTML = "";
    for (const c of cals) {
      list.append(el("div", { class: "cal-item" }, el("b", {}, c.name), el("span", { class: "muted small grow" }, c.link),
        el("button", { type: "button", class: "icon-btn danger", title: "Disconnect", onclick: async () => {
          try { await api("DELETE", `/api/calendars/${c.id}`); loadCalendars(); } catch (e) { toast(e.message, true); }
        } }, svgIcon("trash"))));
    }
  } catch {}
}

$("#calendar-add").onclick = async () => {
  try {
    await api("POST", "/api/calendars", { name: $("#calendar-name").value, url: $("#calendar-url").value });
    $("#calendar-name").value = "";
    $("#calendar-url").value = "";
    loadCalendars();
  } catch (e) {
    toast(e.message, true);
  }
};
$("#calendar-url").addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); $("#calendar-add").click(); } });
$("#calendar-test-btn").onclick = async () => {
  $("#calendar-test").textContent = "Reading calendars…";
  try { $("#calendar-test").textContent = (await api("POST", "/api/calendars/test")).message; }
  catch (e) { $("#calendar-test").textContent = "✗ " + e.message; }
};

$("#settings-form").elements.namedItem("bypass_approvals").addEventListener("change", (e) => {
  if (e.target.checked && !confirm(BYPASS_WARNING)) e.target.checked = false;
});

$("#email-preset").addEventListener("change", (e) => {
  const preset = EMAIL_PRESETS[e.target.value];
  if (!preset) return;
  for (const [k, v] of Object.entries(preset)) $("#settings-form").elements.namedItem("email_" + k).value = v;
});
$("#email-test-btn").onclick = async () => {
  const out = $("#email-test");
  out.textContent = "Testing…";
  try {
    await saveEmailSettings();
    const r = await api("POST", "/api/email/test");
    out.textContent = (r.ok ? "✓ " : "✗ ") + r.message;
  } catch (e) {
    out.textContent = "✗ " + e.message;
  }
};

const wholeOrKeep = (v) => (String(v).trim() === "" ? undefined : Math.max(0, Math.floor(Number(v)) || 0));

$("#settings-form").addEventListener("submit", async (e) => {
  if (e.submitter?.value !== "save") return;
  e.preventDefault();
  const form = e.target;
  const f = (n) => form.elements.namedItem(n);
  try {
    if ($("#email-settings").open || f("email_password").value) await saveEmailSettings();
    S.settings = await api("PUT", "/api/settings", {
      base_url: f("base_url").value.trim(),
      api_key: f("api_key").value,
      model: f("model").value,
      max_steps: wholeOrKeep(f("max_steps").value),        // empty = leave as is; 0 = no limit
      shell_timeout: wholeOrKeep(f("shell_timeout").value),
      auto_compact: f("auto_compact").checked,
      auto_memory: f("auto_memory").checked,
      bypass_approvals: f("bypass_approvals").checked,
      compact_at: Math.min(95, Math.max(30, Number(f("compact_at").value) || 70)),
      context_size: Math.max(0, Number(f("context_size").value) || 0),
      search_url: f("search_url").value.trim(),
      browser_executable: f("browser_executable").value.trim() || "/usr/bin/brave",
      browser_headless: f("browser_headless").checked,
    });
    $("#settings-dialog").close();
    renderMode();
    pollServer();
    toast("Settings saved");
  } catch (err) {
    toast(err.message, true);
  }
});

// ---------------------------------------------------- notifications + files

function notify(title, body, chatId) {
  if (!("Notification" in window) || Notification.permission !== "granted" || !document.hidden) return;
  const n = new Notification(title, { body: (body || "").slice(0, 180), tag: chatId || "agent-chat" });
  n.onclick = () => { window.focus(); if (chatId) openChat(chatId); n.close(); };
}

function notifyStatus(chat, before, now) {
  const a = agentById(chat.agent_id);
  if (now === "waiting" && before !== "waiting") notify(`${a.emoji} ${a.name} needs your approval`, chat.title, chat.id);
  else if (now === "idle" && before && before !== "idle" && !chat.parent) notify(`${a.emoji} ${a.name} replied`, chat.title, chat.id);
}

function renderAttachments() {
  const box = $("#attachments");
  box.innerHTML = "";
  box.hidden = !S.attachments.length;
  S.attachments.forEach((f, i) => {
    box.append(el("span", { class: "attach-chip", title: f.path },
      f.name, el("span", { class: "muted" }, ` ${fmtSize(f.size)}`),
      el("button", { type: "button", class: "icon-btn", title: "Remove", onclick: () => { S.attachments.splice(i, 1); renderAttachments(); } }, "✕")));
  });
}

const fmtSize = (n) => (n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(1)} KB` : `${(n / 1048576).toFixed(1)} MB`);

function attachmentText() {
  return S.attachments.map((f) => {
    if (f.text == null) return `\n\n[Attached file: ${f.name} (${fmtSize(f.size)}), saved in the workspace at ${f.path}]`;
    const fence = f.text.includes("```") ? "````" : "```";
    return `\n\n[Attached file: ${f.name}, saved at ${f.path}]\n${fence}\n${f.text}\n${fence}`;
  }).join("");
}

async function uploadFiles(files) {
  if (!S.chatId || S.chat?.parent) return toast("Open a chat first to attach files.");
  for (const file of files) {
    if (file.size > 20 * 1024 * 1024) { toast(`${file.name} is larger than 20 MB`, true); continue; }
    try {
      const res = await fetch(`/api/chats/${S.chatId}/upload?name=${encodeURIComponent(file.name || "pasted")}`, { method: "POST", body: file, headers: { "X-Agent-Chat": "1" } });
      const info = await res.json();
      if (!res.ok) throw new Error(info.detail || res.statusText);
      S.attachments.push(info);
      renderAttachments();
    } catch (e) {
      toast(`Couldn't attach ${file.name}: ${e.message}`, true);
    }
  }
  $("#input").focus();
}

$("#attach-btn").onclick = () => $("#file-input").click();
$("#file-input").addEventListener("change", (e) => { uploadFiles([...e.target.files]); e.target.value = ""; });
$("#input").addEventListener("paste", (e) => {
  const files = [...(e.clipboardData?.files || [])];
  if (files.length) { e.preventDefault(); uploadFiles(files); }
});
let dragDepth = 0;
window.addEventListener("dragenter", (e) => {
  if (![...(e.dataTransfer?.types || [])].includes("Files") || !S.chatId || S.chat?.parent) return;
  dragDepth++;
  $("#drop-overlay").hidden = false;
});
window.addEventListener("dragleave", () => { if (--dragDepth <= 0) { dragDepth = 0; $("#drop-overlay").hidden = true; } });
window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("drop", (e) => {
  e.preventDefault();
  dragDepth = 0;
  $("#drop-overlay").hidden = true;
  if (e.dataTransfer?.files?.length) uploadFiles([...e.dataTransfer.files]);
});

// --------------------------------------------------------------- routines

const DAY_NAMES = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]; // Python weekday() order
const ROUTINE_TEMPLATES = [
  { name: "Morning briefing", agent_id: "planner", schedule: { type: "daily", time: "07:30", days: [0, 1, 2, 3, 4] },
    prompt: "Give me a short briefing for today: my calendar (times, conflicts, anything to prepare), and ask the Mail agent whether anything urgent came in. End with the 3 things I should focus on today." },
  { name: "Morning email briefing", agent_id: "mail", schedule: { type: "daily", time: "08:00", days: [0, 1, 2, 3, 4] },
    prompt: "Check my unread email. Group it into: needs a reply, worth knowing, and low priority. For each one that needs a reply, suggest a short reply in my voice. Don't send anything." },
  { name: "Plan tomorrow", agent_id: "assistant", schedule: { type: "daily", time: "20:00", days: [0, 1, 2, 3, 4, 5, 6] },
    prompt: "Help me plan tomorrow. Remind me of open tasks, deadlines and commitments you know about from memory, suggest a realistic order for them, and ask what I want to focus on." },
  { name: "News digest", agent_id: "researcher", schedule: { type: "daily", time: "09:00", days: [0, 1, 2, 3, 4, 5, 6] },
    prompt: "Find 5 notable news items from the last 24 hours about topics I care about (use what you remember about my interests and projects). One-line summary and a link for each." },
  { name: "Weekly memory check", agent_id: "assistant", schedule: { type: "daily", time: "18:00", days: [6] },
    prompt: "Look through what you remember about me (use recall_memory with a few broad searches). Point out anything that seems outdated or contradictory and ask me about it, and forget anything I confirm is wrong." },
];

function scheduleText(s) {
  if (s.type === "interval") return s.minutes % 60 === 0 ? `Every ${s.minutes / 60} h` : `Every ${s.minutes} min`;
  const d = s.days.join(",");
  const days = d === "0,1,2,3,4,5,6" ? "Every day" : d === "0,1,2,3,4" ? "Weekdays" : d === "5,6" ? "Weekends"
    : s.days.map((i) => DAY_NAMES[i]).join(", ");
  return `${days} at ${s.time}`;
}

function whenText(ts) {
  if (!ts) return "—";
  const s = ts - Date.now() / 1000;
  const date = new Date(ts * 1000);
  const clock = date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  if (s < 60) return "now";
  if (s < 3600) return `in ${Math.round(s / 60)} min`;
  if (s < 86400) return `in ${Math.floor(s / 3600)} h ${Math.round((s % 3600) / 60)} min (${clock})`;
  return `${DAY_NAMES[(date.getDay() + 6) % 7]} ${clock}`;
}

let editingRoutineId = null;

async function loadRoutines() {
  const items = await api("GET", "/api/routines");
  const list = $("#routine-list");
  list.innerHTML = "";
  if (!items.length) list.append(el("div", { class: "empty-note" }, "No routines yet. Create one below; the templates are a good start."));
  for (const r of items) {
    const a = agentById(r.agent_id);
    const toggle = el("input", { type: "checkbox", checked: r.enabled, title: r.enabled ? "On" : "Off" });
    toggle.onchange = async () => { try { await api("PUT", `/api/routines/${r.id}`, { enabled: toggle.checked }); loadRoutines(); } catch (e) { toast(e.message, true); } };
    const runNow = el("button", { type: "button", class: "btn sm" }, svgIcon("play"), "Run now");
    runNow.onclick = async () => {
      try {
        const res = await api("POST", `/api/routines/${r.id}/run`);
        if (res.status !== "started") toast(`Not started: ${res.status}`);
        else { $("#routines-dialog").close(); await refreshChats(); openChat(res.chat_id); }
      } catch (e) { toast(e.message, true); }
    };
    const open = r.chat_id ? el("button", { type: "button", class: "btn sm ghost", onclick: () => { $("#routines-dialog").close(); openChat(r.chat_id); } }, "Open chat") : null;
    const edit = el("button", { type: "button", class: "icon-btn", title: "Edit", onclick: () => openRoutineEditor(r) }, svgIcon("pencil"));
    const del = el("button", { type: "button", class: "icon-btn danger", title: "Delete (its chat is kept)" }, svgIcon("trash"));
    del.onclick = async () => {
      if (!confirm(`Delete the routine “${r.name}”? Its chat is kept.`)) return;
      try { await api("DELETE", `/api/routines/${r.id}`); loadRoutines(); } catch (e) { toast(e.message, true); }
    };
    const last = r.last_run ? `last ${timeAgo(r.last_run)}${r.last_status && r.last_status !== "started" ? ` (${r.last_status})` : ""}` : "never run";
    list.append(el("div", { class: "routine-item" + (r.enabled ? "" : " off") },
      toggle,
      el("div", { class: "mem-main" },
        el("div", { class: "routine-name" }, avatar(a, true), r.name),
        el("div", { class: "mem-meta" }, `${scheduleText(r.schedule)} · ${a.name} · next ${r.enabled ? whenText(r.next_run) : "— (off)"} · ${last}`)),
      runNow, open, edit, del));
  }
}

function openRoutineEditor(r = null) {
  editingRoutineId = r?.id || null;
  const form = $("#routines-form");
  const f = (n) => form.elements.namedItem(n);
  $("#routine-editor-title").textContent = r ? `Edit “${r.name}”` : "New routine";
  $("#routine-template-row").hidden = !!r;
  const agentSel = f("r_agent");
  agentSel.innerHTML = "";
  agentSel.append(...S.agents.map((a) => el("option", { value: a.id }, `${a.emoji} ${a.name}`)));
  const fill = (x) => {
    f("r_name").value = x.name || "";
    f("r_prompt").value = x.prompt || "";
    agentSel.value = S.agents.some((a) => a.id === x.agent_id) ? x.agent_id : S.agents[0]?.id;
    const s = x.schedule || { type: "daily", time: "08:00", days: [0, 1, 2, 3, 4, 5, 6] };
    f("r_type").value = s.type;
    f("r_time").value = s.time || "08:00";
    f("r_minutes").value = s.minutes || 60;
    $("#r-days").innerHTML = "";
    DAY_NAMES.forEach((name, i) => {
      $("#r-days").append(el("label", { class: "day" }, el("input", { type: "checkbox", value: i, checked: (s.days || [0, 1, 2, 3, 4, 5, 6]).includes(i) }), name));
    });
    syncRoutineType();
  };
  fill(r || {});
  const tpl = $("#routine-template");
  tpl.innerHTML = "";
  tpl.append(el("option", { value: "" }, "Blank"), ...ROUTINE_TEMPLATES.map((t, i) => el("option", { value: i }, t.name)));
  tpl.onchange = () => ROUTINE_TEMPLATES[tpl.value] && fill(ROUTINE_TEMPLATES[tpl.value]);
  $("#routine-editor").open = true;
  f("r_name").focus();
}

function syncRoutineType() {
  const daily = $("#routines-form").elements.namedItem("r_type").value === "daily";
  for (const n of document.querySelectorAll(".r-daily")) n.hidden = !daily;
  for (const n of document.querySelectorAll(".r-interval")) n.hidden = daily;
}

async function saveRoutine() {
  const form = $("#routines-form");
  const f = (n) => form.elements.namedItem(n);
  const type = f("r_type").value;
  const body = {
    name: f("r_name").value.trim(), agent_id: f("r_agent").value, prompt: f("r_prompt").value.trim(),
    schedule: type === "daily"
      ? { type, time: f("r_time").value, days: [...$("#r-days").querySelectorAll("input:checked")].map((x) => Number(x.value)) }
      : { type, minutes: Number(f("r_minutes").value) },
  };
  try {
    if (editingRoutineId) await api("PUT", `/api/routines/${editingRoutineId}`, body);
    else await api("POST", "/api/routines", body);
    $("#routine-editor").open = false;
    editingRoutineId = null;
    loadRoutines();
    toast("Routine saved");
  } catch (e) {
    toast(e.message, true);
  }
}

$("#routines-form").elements.namedItem("r_type").addEventListener("change", syncRoutineType);
$("#routine-save").onclick = saveRoutine;
$("#routine-cancel").onclick = () => { $("#routine-editor").open = false; editingRoutineId = null; };
$("#routine-editor").addEventListener("toggle", (e) => { if (e.target.open && !$("#routines-form").elements.namedItem("r_name").value && !editingRoutineId) openRoutineEditor(null); });
$("#routines-btn").onclick = async () => {
  try { await loadRoutines(); } catch (e) { return toast(e.message, true); }
  $("#routine-editor").open = false;
  $("#routines-dialog").showModal();
};

// ----------------------------------------------------------------- memory

async function refreshMemoryCount() {
  try {
    const data = await api("GET", "/api/memory");
    $("#memory-count").textContent = data.total ? (data.total > 99 ? "99+" : data.total) : "";
    $("#onboarding").hidden = data.total > 0;
  } catch {}
}

$("#onboard-memory").onclick = () => $("#memory-btn").click();
$("#onboard-chat").onclick = async () => {
  await newChat(S.agents.find((a) => a.id === "assistant" && a.memory)?.id || S.agents.find((a) => a.memory)?.id || S.agents[0]?.id);
  $("#input").value = "Hi! A bit about me so you can help me better: my name is …, I work on …, and I like answers that are …";
  autoGrow();
  $("#input").focus();
};

async function loadMemory() {
  const cat = $("#memory-category");
  const params = new URLSearchParams({ q: $("#memory-search").value.trim(), category: cat.value });
  const data = await api("GET", `/api/memory?${params}`);
  if (cat.options.length <= 1) {
    cat.innerHTML = "";
    cat.append(el("option", { value: "" }, "All kinds"), ...data.categories.map((c) => el("option", { value: c }, c)));
  }
  $("#memory-total").textContent = `· ${data.total} remembered`;
  const list = $("#memory-list");
  list.innerHTML = "";
  if (!data.memories.length) {
    list.append(el("div", { class: "empty-note" }, data.total ? "Nothing matches." : "Nothing yet. Chat with your agents, or add something above."));
  }
  for (const m of data.memories) list.append(memoryRow(m, data.categories));
}

function memoryRow(m, categories) {
  const save = async (fields) => {
    try { await api("PUT", `/api/memory/${m.id}`, fields); } catch (e) { toast(e.message, true); loadMemory(); }
  };
  const fact = el("div", { class: "mem-fact", contenteditable: "true", spellcheck: "false" }, m.fact);
  fact.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); fact.blur(); } });
  fact.addEventListener("blur", () => { const t = fact.textContent.trim(); if (t && t !== m.fact) { m.fact = t; save({ fact: t }); } });
  const cat = el("select", { class: "mem-select" }, categories.map((c) => el("option", { value: c, selected: c === m.category }, c)));
  cat.onchange = () => save({ category: cat.value });
  const imp = el("input", { type: "number", min: 1, max: 10, value: m.importance, class: "mem-imp", title: "Importance 1–10" });
  imp.onchange = () => save({ importance: Number(imp.value) });
  const pin = el("button", { type: "button", class: "icon-btn pin" + (m.pinned ? " on" : ""), title: m.pinned ? "Pinned: always given to agents" : "Pin: always give this to agents" }, svgIcon("pin"));
  pin.onclick = async () => { await save({ pinned: !m.pinned }); loadMemory(); };
  const del = el("button", { type: "button", class: "icon-btn danger", title: "Forget this" }, svgIcon("trash"));
  del.onclick = async () => {
    try { await api("DELETE", `/api/memory/${m.id}`); loadMemory(); } catch (e) { toast(e.message, true); }
  };
  const src = m.source === "user" ? "added by you" : m.source.startsWith("auto") ? "learned automatically"
    : `saved by ${agentById(m.source.replace(/^agent:/, "")).name}`;
  return el("div", { class: "mem-item" + (m.pinned ? " pinned" : "") }, pin,
    el("div", { class: "mem-main" }, fact,
      el("div", { class: "mem-meta" }, cat, el("span", {}, "importance"), imp,
        el("span", {}, `· ${src} · used ${m.uses}× · ${timeAgo(m.updated)}`))),
    del);
}

async function addMemory() {
  const input = $("#memory-new");
  const fact = input.value.trim();
  if (!fact) return;
  try {
    const m = await api("POST", "/api/memory", { fact, importance: 7 });
    input.value = "";
    if (m.action === "merged") toast("Updated a similar memory instead of adding a duplicate");
    loadMemory();
  } catch (e) {
    toast(e.message, true);
  }
}

let memSearchTimer;
$("#memory-search").addEventListener("input", () => { clearTimeout(memSearchTimer); memSearchTimer = setTimeout(loadMemory, 200); });
$("#memory-search").addEventListener("keydown", (e) => { if (e.key === "Enter") e.preventDefault(); });
$("#memory-category").addEventListener("change", loadMemory);
$("#memory-add-btn").onclick = addMemory;
$("#memory-tidy-btn").onclick = async () => {
  const btn = $("#memory-tidy-btn");
  btn.disabled = true;
  btn.textContent = "Tidying…";
  try {
    const r = await api("POST", "/api/memory/tidy");
    toast(r.message);
    loadMemory();
  } catch (e) {
    toast(e.message, true);
  } finally {
    btn.disabled = false;
    btn.textContent = "Tidy up";
  }
};
$("#memory-new").addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); addMemory(); } });
$("#memory-btn").onclick = async () => {
  try { await loadMemory(); } catch (e) { return toast(e.message, true); }
  $("#memory-dialog").showModal();
};

// ------------------------------------------------------- expanded view (Ctrl+O)

function applyVerbose(announce) {
  document.body.classList.toggle("verbose", S.verbose);
  const b = $("#hdr-verbose");
  b.setAttribute("aria-pressed", String(S.verbose));
  b.classList.toggle("on", S.verbose);
  b.title = S.verbose ? "Collapse thinking and tool details (Ctrl+O)" : "Show all thinking and tool details (Ctrl+O)";
  for (const d of document.querySelectorAll("#messages details.thinking, #messages details.tool-card:not(.draft):not(.approval)")) {
    d.open = S.verbose;
  }
  try { localStorage.setItem("verbose", S.verbose ? "1" : "0"); } catch {}
  if (announce) toast(S.verbose ? "Showing all thinking and tool details. Ctrl+O collapses them." : "Details collapsed. Ctrl+O shows them again.");
}

function toggleVerbose() {
  S.verbose = !S.verbose;
  applyVerbose(true);
}
$("#hdr-verbose").onclick = toggleVerbose;

// ------------------------------------------------------- bypass permissions

const BYPASS_WARNING = "Bypass permissions?\n\nAgents will run shell commands and send email without asking you first. " +
  "Anything they read, like web pages or emails, could try to trick them into running a command or sending mail.\n\n" +
  "You can switch this off again at any time.";

function renderMode() {
  const on = !!S.settings.bypass_approvals;
  const b = $("#mode-btn");
  b.classList.toggle("bypass", on);
  b.querySelector("span").textContent = on ? "Bypassing permissions" : "Ask before acting";
  b.title = on ? "Agents run shell commands and send email without asking. Click to make them ask again."
    : "Agents ask before running shell commands or sending email. Click to bypass.";
  document.body.classList.toggle("bypass", on);
}

async function setBypass(on) {
  if (on && !confirm(BYPASS_WARNING)) return false;
  try {
    S.settings = await api("PUT", "/api/settings", { bypass_approvals: on });
    renderMode();
    toast(on ? "Permissions bypassed: agents act without asking" : "Agents will ask before acting again");
    return true;
  } catch (e) {
    toast(e.message, true);
    return false;
  }
}
$("#mode-btn").onclick = () => setBypass(!S.settings.bypass_approvals);

// ------------------------------------------------------------------- theme

const THEMES = ["auto", "light", "dark"];
function applyTheme(t) {
  if (t === "auto") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = t;
  try { if (t === "auto") localStorage.removeItem("theme"); else localStorage.setItem("theme", t); } catch {}
  $("#theme-btn").replaceChildren(svgIcon(t === "light" ? "sun" : t === "dark" ? "moon" : "auto"),
    el("span", { class: "strip" }, t === "auto" ? "Auto" : t === "light" ? "Light" : "Dark"));
  $("#theme-btn").title = t === "auto" ? "Theme: follows your system (click to change)" : `Theme: ${t} (click to change)`;
}
$("#theme-btn").onclick = () => {
  const now = document.documentElement.dataset.theme || "auto";
  applyTheme(THEMES[(THEMES.indexOf(now) + 1) % THEMES.length]);
};
applyTheme(document.documentElement.dataset.theme || "auto");
applyVerbose(false);

// ------------------------------------------------------------------- boot

$("#new-chat-btn").onclick = () => {
  if (S.filter) return newChat(S.filter);
  agentCards($("#pick-agents"), (a) => { $("#pick-dialog").close(); newChat(a.id); });
  $("#pick-dialog").showModal();
};
$("#new-agent-btn").onclick = () => openAgentDialog(null);
$("#settings-btn").onclick = openSettings;
let searchTimer;
$("#chat-search").addEventListener("input", () => {
  renderSidebar();
  clearTimeout(searchTimer);
  const q = $("#chat-search").value.trim();
  if (q.length < 2) { S.hits = new Map(); return renderSidebar(); }
  searchTimer = setTimeout(async () => {  // search inside messages too
    try {
      const hits = await api("GET", `/api/search?q=${encodeURIComponent(q)}`);
      if ($("#chat-search").value.trim() !== q) return;
      S.hits = new Map(hits.map((h) => [h.chat_id, h]));
      renderSidebar();
    } catch {}
  }, 250);
});
document.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "o") {
    e.preventDefault();  // instead of the browser's "open file"
    toggleVerbose();
    return;
  }
  const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName) || document.activeElement?.isContentEditable;
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
    e.preventDefault();
    document.body.classList.add("side-open");
    $("#chat-search").focus();
    $("#chat-search").select();
  } else if (e.key === "/" && !typing && !document.querySelector("dialog[open]") && !$("#composer").hidden) {
    e.preventDefault();
    $("#input").focus();
  }
});
document.addEventListener("visibilitychange", () => {
  if (!document.hidden && S.chat) document.title = `${S.chat.title} · Agent Chat`;
});
window.addEventListener("hashchange", () => {
  const id = location.hash.match(/^#\/chat\/([\w-]+)/)?.[1] || null;
  if (id !== S.chatId) openChat(id);
});
setInterval(renderSidebar, 60_000); // keep "5m ago" labels fresh

(async function boot() {
  try {
    await reloadState();
  } catch (e) {
    toast("Can't reach the Agent Chat backend: " + e.message, true);
    return;
  }
  connectGlobalEvents();
  pollServer();
  setInterval(pollServer, 10_000);
  const id = location.hash.match(/^#\/chat\/([\w-]+)/)?.[1];
  openChat(id && S.chats.some((c) => c.id === id) ? id : null);
})();
