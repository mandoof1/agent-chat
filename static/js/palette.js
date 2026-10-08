// The command palette (Ctrl+K): find a chat, start one with an agent, or run a command.

import { S, agentById, STATUS_TEXT } from "./state.js";
import { $, el, svgIcon, avatar, api, timeAgo, highlight } from "./util.js";
import { openChat, newChat, forkChat } from "./chat.js";
import { openAgentDialog } from "./agents.js";
import { openSettings } from "./settings.js";
import { openMemory } from "./memory.js";
import { openRoutines } from "./routines.js";
import { toggleFiles } from "./files.js";
import { setCollapsed } from "./sidebar.js";
import { toggleVerbose, cycleTheme, openShortcuts } from "./main.js";
import { setBypass } from "./composer.js";

const dialog = $("#palette");
const input = $("#palette-q");
const list = $("#palette-list");
let items = [];
let selected = 0;
let searchTimer = null;
let hits = [];

function commands() {
  const inChat = !!S.chat && !S.chat.parent;
  return [
    { group: "Commands", title: "New chat", sub: "Pick an agent", icon: "plus", keys: ["n"], run: () => $("#new-chat-btn").click() },
    { group: "Commands", title: "Settings", sub: "Model server, agents, email, calendars", icon: "sliders", run: () => openSettings() },
    { group: "Commands", title: "Memory", sub: "What your agents know about you", icon: "memory", run: () => openMemory() },
    { group: "Commands", title: "Routines", sub: "Agents that run on a schedule", icon: "clock", run: () => openRoutines() },
    { group: "Commands", title: "New agent", sub: "From a template, a blank sheet, or a file", icon: "plus", run: () => openAgentDialog(null) },
    { group: "Commands", title: S.filesOpen ? "Hide workspace files" : "Show workspace files", sub: "What the agent's file tools can see", icon: "folder", keys: ["Ctrl", "."], run: () => toggleFiles() },
    { group: "Commands", title: S.verbose ? "Collapse thinking and tool details" : "Show all thinking and tool details", icon: "expand", keys: ["Ctrl", "O"], run: () => toggleVerbose() },
    { group: "Commands", title: S.sideCollapsed ? "Show the chat list" : "Hide the chat list", icon: "sidebar", keys: ["Ctrl", "B"], run: () => setCollapsed(!S.sideCollapsed) },
    { group: "Commands", title: "Switch theme", sub: "Auto, light, dark", icon: "sun", run: () => cycleTheme() },
    { group: "Commands", title: S.settings.bypass_approvals ? "Ask before acting again" : "Bypass permissions", sub: S.settings.bypass_approvals ? "Agents currently act without asking" : "Let agents run commands and send email without asking", icon: "shield", run: () => setBypass(!S.settings.bypass_approvals) },
    { group: "Commands", title: "Keyboard shortcuts", icon: "keyboard", keys: ["?"], run: () => openShortcuts() },
    inChat && { group: "This chat", title: "Export as Markdown", icon: "download", run: () => (location.href = `/api/chats/${S.chatId}/export.md`) },
    inChat && { group: "This chat", title: "Export as JSON", icon: "download", run: () => (location.href = `/api/chats/${S.chatId}/export.json`) },
    inChat && S.chat.messages.length > 0 && { group: "This chat", title: "Branch from the end", sub: "A new chat that continues from here", icon: "branch", run: () => forkChat(S.chat.messages.length) },
    inChat && !S.running && { group: "This chat", title: "Compact older messages", sub: "Summarize to free up context", icon: "compress", run: () => $("#hdr-compact").click() },
    inChat && { group: "This chat", title: `Edit ${agentById(S.chat.agent_id).name}`, icon: "pencil", run: () => openAgentDialog(S.chat.agent_id) },
  ].filter(Boolean);
}

function build(q) {
  const ql = q.toLowerCase();
  const match = (s) => !ql || String(s || "").toLowerCase().includes(ql);
  const out = [];
  for (const a of S.agents) {
    if (match(a.name) || match(a.purpose)) out.push({ group: "Agents", title: `New ${a.name} chat`, sub: a.purpose, avatar: a, run: () => newChat(a.id) });
  }
  const hitMap = new Map(hits.map((h) => [h.chat_id, h]));
  const chats = S.chats.filter((c) => !ql ? !c.parent : match(c.title) || hitMap.has(c.id));
  for (const c of chats.slice(0, ql ? 30 : 8)) {
    const a = agentById(c.agent_id);
    const hit = hitMap.get(c.id);
    out.push({ group: ql ? "Chats" : "Recent chats", title: c.title, titleNodes: highlight(c.title, q),
      subNodes: hit ? [`${a.name}: `, ...highlight(hit.snippet, q)] : [[a.name, c.parent && `for ${agentById(c.parent.caller_id).name}`, c.status && c.status !== "idle" ? STATUS_TEXT[c.status] : timeAgo(c.updated)].filter(Boolean).join(" · ")],
      avatar: a, run: () => openChat(c.id) });
  }
  for (const c of commands()) if (match(c.title) || match(c.sub) || match(c.group)) out.push(c);
  // with a query, rank commands whose title starts with it first
  if (ql) out.sort((x, y) => (y.title.toLowerCase().startsWith(ql)) - (x.title.toLowerCase().startsWith(ql)));
  return out;
}

function render() {
  const q = input.value.trim();
  items = build(q);
  selected = Math.min(selected, Math.max(0, items.length - 1));
  list.innerHTML = "";
  if (!items.length) { list.append(el("div", { class: "palette-empty" }, "Nothing matches.")); return; }
  let group = null;
  items.forEach((it, i) => {
    if (it.group !== group) { group = it.group; list.append(el("div", { class: "palette-group" }, group)); }
    const node = el("div", { class: "palette-item" + (i === selected ? " selected" : ""), role: "option", "aria-selected": i === selected,
      onmousemove: () => { if (selected !== i) { selected = i; mark(); } }, onclick: () => pick(i) },
      it.avatar ? avatar(it.avatar, "xs") : svgIcon(it.icon || "arrow"),
      el("div", { class: "p-text" }, el("div", { class: "p-title" }, ...(it.titleNodes || [it.title])),
        (it.subNodes || it.sub) ? el("div", { class: "p-sub" }, ...(it.subNodes || [it.sub])) : null),
      it.keys ? el("div", { class: "p-key" }, it.keys.map((k) => el("kbd", {}, k))) : el("span"));
    node.dataset.index = i;
    list.append(node);
  });
}

function mark() {
  list.querySelectorAll(".palette-item").forEach((n) => n.classList.toggle("selected", Number(n.dataset.index) === selected));
  list.querySelector(".palette-item.selected")?.scrollIntoView({ block: "nearest" });
}

function pick(i) {
  const it = items[i];
  if (!it) return;
  closePalette();
  it.run();
}

export function openPalette(prefill = "") {
  if (dialog.open) return;
  input.value = prefill;
  hits = [];
  selected = 0;
  render();
  dialog.showModal();
  input.focus();
  input.select();
}

export function closePalette() { if (dialog.open) dialog.close(); }
export function togglePalette() { dialog.open ? closePalette() : openPalette(); }

input.addEventListener("input", () => {
  selected = 0;
  render();
  clearTimeout(searchTimer);
  const q = input.value.trim();
  if (q.length < 2) { hits = []; return; }
  searchTimer = setTimeout(async () => {  // search inside messages too
    try {
      const found = await api("GET", `/api/search?q=${encodeURIComponent(q)}`);
      if (input.value.trim() !== q) return;
      hits = found;
      render();
    } catch {}
  }, 200);
});
input.addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown") { e.preventDefault(); selected = (selected + 1) % Math.max(1, items.length); mark(); }
  else if (e.key === "ArrowUp") { e.preventDefault(); selected = (selected - 1 + items.length) % Math.max(1, items.length); mark(); }
  else if (e.key === "Enter") { e.preventDefault(); pick(selected); }
  else if (e.key === "Escape") { e.preventDefault(); closePalette(); }
});
dialog.addEventListener("click", (e) => { if (e.target === dialog) closePalette(); });
$("#palette-btn").onclick = () => openPalette();
