// Boot, global events (server-sent), keyboard shortcuts, theme.

import { S, prefs } from "./state.js";
import { $, $$, el, svgIcon, api, fmtK, isTyping } from "./util.js";
import { toast, closeMenu } from "./ui.js";
import { renderSidebar, refreshChats, agentCards, renderRecent, setCollapsed, markSeen, drawCords } from "./sidebar.js";
import { openChat, newChat, renderHeader, renderStats } from "./chat.js";
import { renderMode, renderOffline, returnToInput } from "./composer.js";
import { openAgentDialog } from "./agents.js";
import { openSettings } from "./settings.js";
import { refreshMemoryCount, loadMemory } from "./memory.js";
import { loadRoutines } from "./routines.js";
import { togglePalette } from "./palette.js";
import { toggleFiles } from "./files.js";
import { notifyStatus, updateBadge } from "./notify.js";

// ------------------------------------------------------------------- state

export async function reloadState() {
  const st = await api("GET", "/api/state");
  Object.assign(S, { agents: st.agents, chats: st.chats, settings: st.settings, tools: st.tools, defaultWorkspace: st.default_workspace, version: st.version });
  if (S.seen == null) {  // first visit on this browser: nothing is "unread" yet
    S.seen = Object.fromEntries(S.chats.map((c) => [c.id, c.updated || 0]));
    prefs.set("seen", JSON.stringify(S.seen));
  }
  renderSidebar();
  renderRecent();
  renderMode();
  agentCards($("#welcome-agents"), (a) => newChat(a.id));
  if (S.chat) renderHeader();
}

export async function pollServer() {
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
    ? `Local · ${model} · ${S.agents.length} agents${S.server.n_ctx ? ` · ${fmtK(S.server.n_ctx)} context` : ""}`
    : `Model server offline · ${S.agents.length} agents`;
  const where = S.server.base_url || S.settings.base_url;
  box.title = S.server.ok
    ? `Connected to ${where}\nModels: ${S.server.models.join(", ") || "?"}${S.server.n_ctx ? `\nContext: ${S.server.n_ctx} tokens` : ""}`
    : `Can't reach ${where}\n${S.server.error || ""}`;
  renderOffline();
  if (S.chat) renderStats();
}
$("#offline-retry").onclick = async () => { await pollServer(); toast(S.server.ok ? "Connected" : "Still not reachable", S.server.ok ? "ok" : "err"); };

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
        updateBadge();
        notifyStatus(c, before, ev.status);
      }
    } else if (ev.type === "chats_changed" || ev.type === "hello") {
      refreshChats();
      if (ev.type === "hello") refreshMemoryCount();
    } else if (ev.type === "chat_title") {
      const c = S.chats.find((x) => x.id === ev.chat_id);
      if (c) c.title = ev.title;
      if (S.chat?.id === ev.chat_id) { S.chat.title = ev.title; renderHeader(); }
      renderSidebar();
    } else if (ev.type === "queue_returned") {
      returnToInput(ev.chat_id, ev.text);
      if (ev.chat_id === S.chatId) toast("The run ended before the agent read your queued message, so it's back in the message box.", "info");
    } else if (ev.type === "routines_changed") {
      if ($("#routines-dialog").open) loadRoutines();
    } else if (ev.type === "memory_status") {
      $("#memory-btn").classList.toggle("is-running", ev.state === "working");
      $("#memory-btn").title = ev.state === "working" ? "Memory: reviewing the last reply for things to remember…" : "Memory: what your agents know about you";
    } else if (ev.type === "memory_changed") {
      refreshMemoryCount();
      if ($("#memory-dialog").open) loadMemory();
      const learned = [...(ev.added || []), ...(ev.updated || [])];
      if (ev.by === "auto" && learned.length) toast(`Remembered: ${learned[0]}${learned.length > 1 ? ` (+${learned.length - 1} more)` : ""}`, "ok",
        { action: { label: "Memory", onClick: () => $("#memory-btn").click() } });
      else if (ev.by && ev.by !== "you" && ev.by !== "auto" && ev.by !== "tidy" && ev.summary) toast(`${ev.by}: ${ev.summary}`);
    }
  };
  es.onerror = () => { $("#server-status").className = "server-status down"; };
  es.onopen = () => pollServer();
}

// ------------------------------------------------------- expanded view (Ctrl+O)

export function applyVerbose(announce) {
  document.body.classList.toggle("verbose", S.verbose);
  const b = $("#hdr-verbose");
  b.setAttribute("aria-pressed", String(S.verbose));
  b.title = S.verbose ? "Collapse thinking and tool details (Ctrl+O)" : "Show all thinking and tool details (Ctrl+O)";
  for (const d of document.querySelectorAll("#messages details.thinking, #messages details.tool-card:not(.draft):not(.approval)")) {
    d.open = S.verbose;
  }
  prefs.set("verbose", S.verbose ? "1" : "0");
  if (announce) toast(S.verbose ? "Showing all thinking and tool details. Ctrl+O collapses them." : "Details collapsed. Ctrl+O shows them again.", "info");
}

export function toggleVerbose() {
  S.verbose = !S.verbose;
  applyVerbose(true);
}
$("#hdr-verbose").onclick = toggleVerbose;

// ------------------------------------------------------------------- theme

const THEMES = ["auto", "light", "dark"];
export function applyTheme(t) {
  if (t === "auto") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = t;
  prefs.set("theme", t === "auto" ? null : t);
  $("#theme-btn").replaceChildren(svgIcon(t === "light" ? "sun" : t === "dark" ? "moon" : "auto"),
    el("span", { class: "strip" }, t === "auto" ? "Auto" : t === "light" ? "Light" : "Dark"));
  $("#theme-btn").title = t === "auto" ? "Theme: follows your system (click to change)" : `Theme: ${t} (click to change)`;
}
export function cycleTheme() {
  const now = document.documentElement.dataset.theme || "auto";
  applyTheme(THEMES[(THEMES.indexOf(now) + 1) % THEMES.length]);
}
$("#theme-btn").onclick = cycleTheme;

// --------------------------------------------------------------- shortcuts

const SHORTCUTS = [
  [["Ctrl", "K"], "Search chats and commands"],
  [["Ctrl", "O"], "Show or hide all thinking and tool details"],
  [["Ctrl", "B"], "Show or hide the chat list"],
  [["Ctrl", "."], "Show or hide workspace files"],
  [["/"], "Focus the message box"],
  [["n"], "New chat"],
  [["?"], "This list"],
  [["Esc"], "Close a dialog or menu"],
  [["Enter"], "Send (or queue, while the agent works)"],
  [["Shift", "Enter"], "New line"],
  [["↑"], "In an empty message box: take queued messages back to edit"],
];

export function openShortcuts() {
  const box = $("#shortcuts-list");
  box.innerHTML = "";
  for (const [keys, what] of SHORTCUTS) {
    box.append(el("div", { class: "shortcut" }, el("span", {}, what), el("span", { class: "keys" }, keys.map((k) => el("kbd", {}, k)))));
  }
  $("#shortcuts-dialog").showModal();
}
$("#hint-shortcuts").onclick = openShortcuts;

document.addEventListener("keydown", (e) => {
  const mod = e.ctrlKey || e.metaKey;
  const key = e.key.toLowerCase();
  if (mod && !e.shiftKey && !e.altKey && key === "k") { e.preventDefault(); togglePalette(); return; }
  if (mod && !e.shiftKey && !e.altKey && key === "o") { e.preventDefault(); toggleVerbose(); return; }
  if (mod && !e.shiftKey && !e.altKey && key === "b") { e.preventDefault(); setCollapsed(!S.sideCollapsed); return; }
  if (mod && !e.shiftKey && !e.altKey && e.key === ".") { e.preventDefault(); toggleFiles(); return; }
  if (mod || e.altKey) return;
  const dialogOpen = !!document.querySelector("dialog[open]");
  if (dialogOpen || isTyping()) return;
  if (e.key === "/" && !$("#composer").hidden) { e.preventDefault(); $("#input").focus(); }
  else if (e.key === "?") { e.preventDefault(); openShortcuts(); }
  else if (key === "n") { e.preventDefault(); $("#new-chat-btn").click(); }
});

// ------------------------------------------------------------------- boot

$("#new-chat-btn").onclick = () => {
  if (S.filter) return newChat(S.filter);
  agentCards($("#pick-agents"), (a) => { $("#pick-dialog").close(); newChat(a.id); });
  $("#pick-dialog").showModal();
};
$("#new-agent-btn").onclick = () => openAgentDialog(null);
$("#settings-btn").onclick = () => openSettings();
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
$("#chat-search").addEventListener("keydown", (e) => {
  if (e.key === "Escape") { e.target.value = ""; S.hits = new Map(); renderSidebar(); e.target.blur(); }
  if (e.key === "Enter") { const first = $("#chat-list .chat-item"); if (first) first.click(); }
});
document.addEventListener("visibilitychange", () => {
  if (!document.hidden && S.chat) markSeen(S.chatId, S.chat.updated);
});
window.addEventListener("hashchange", () => {
  const id = location.hash.match(/^#\/chat\/([\w-]+)/)?.[1] || null;
  if (id !== S.chatId) openChat(id);
});
window.addEventListener("blur", closeMenu);
setInterval(renderSidebar, 60_000); // keep "5 min ago" labels fresh

applyTheme(document.documentElement.dataset.theme || "auto");
applyVerbose(false);
if (S.sideCollapsed) document.body.classList.add("side-collapsed");
$("#hdr-files").setAttribute("aria-pressed", String(S.filesOpen));
$("#files").hidden = !S.filesOpen;

(async function boot() {
  try {
    await reloadState();
  } catch (e) {
    toast("Can't reach the Agent Chat backend: " + e.message, "err", { timeout: 60_000 });
    return;
  }
  connectGlobalEvents();
  await pollServer();
  setInterval(pollServer, 10_000);
  const id = location.hash.match(/^#\/chat\/([\w-]+)/)?.[1];
  openChat(id && S.chats.some((c) => c.id === id) ? id : null);
  requestAnimationFrame(drawCords);
})();
