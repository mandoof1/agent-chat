// The sidebar: the agent list (with what each one is doing) and the chat list.

import { S, prefs, agentById, STATUS_TEXT } from "./state.js";
import { $, el, svgIcon, avatar, api, timeAgo, dayGroup, highlight, iconBtn, downloadBlob } from "./util.js";
import { toast, confirm, prompt, contextMenu } from "./ui.js";
import { openChat, newChat } from "./chat.js";
import { openAgentDialog } from "./agents.js";

// ------------------------------------------------------------------ status

// The most urgent state among an agent's chats (or all chats when agentId is empty).
export function busyState(agentId) {
  const st = new Set(S.chats.filter((c) => !agentId || c.agent_id === agentId).map((c) => c.status));
  return ["waiting", "running", "delegated"].find((s) => st.has(s)) || null;
}

// Clicking a busy agent shows what it is doing right now.
export function openActiveChat(agentId) {
  const mine = S.chats.filter((c) => c.agent_id === agentId);
  const target = mine.find((c) => c.status === "running" || c.status === "waiting") || mine.find((c) => c.status === "delegated");
  if (target && target.id !== S.chatId) openChat(target.id);
}

// Unread: a reply arrived in a chat you weren't looking at.
export function markSeen(chatId, updated) {
  if (!chatId) return;
  S.seen[chatId] = Math.max(S.seen[chatId] || 0, updated || Date.now() / 1000);
  prefs.set("seen", JSON.stringify(S.seen));
}

export function isUnread(c) {
  if (c.parent || c.id === S.chatId || c.last_role !== "assistant" || (c.status && c.status !== "idle")) return false;
  return (c.updated || 0) > (S.seen[c.id] || 0) + 1;
}

export function toolTags(agent) {
  const groups = new Map();
  for (const t of agent.tools) {
    const info = S.tools.find((x) => x.name === t);
    if (!info) continue;
    const label = info.name === "run_shell" ? "Shell" : info.group;
    groups.set(label, groups.get(label) || info.danger);
  }
  if (!groups.size) return [el("span", { class: "tag" }, "Chat only")];
  return [...groups].map(([label, danger]) => el("span", { class: "tag" + (danger ? " danger" : "") }, label));
}

export function setFilter(agentId) {
  S.filter = agentId || "";
  prefs.set("agentFilter", S.filter);
  renderSidebar();
}

export async function refreshChats() {
  try {
    const chats = await api("GET", "/api/chats");
    // Status events arrive in order and for every change, so one that came in while this
    // request was in flight is fresher than what the response says.
    for (const c of chats) if (S.statuses[c.id]) c.status = S.statuses[c.id];
    S.chats = chats;
    renderSidebar();
    renderRecent();
  } catch {}
}

// ------------------------------------------------------------- agent list

let dragId = null;

function stateWord(agent) {
  const b = busyState(agent?.id);
  if (!b) return "";
  if (b === "waiting") return "needs you";
  if (b === "running") return "working";
  const waitingOn = S.chats.find((c) => c.parent && c.parent.caller_id === agent?.id && c.status && c.status !== "idle");
  return waitingOn ? `→ ${agentById(waitingOn.agent_id).name}` : "waiting";
}

function agentRow(agent, title) {
  const b = busyState(agent?.id);
  const hint = { running: " (working)", waiting: " (needs your approval)", delegated: " (waiting on another agent)" }[b] || "";
  const item = el("button", {
    type: "button", "data-agent": agent?.id || "",
    class: "agent-row" + (agent ? "" : " all") + ((agent?.id || "") === S.filter ? " active" : "") + (b ? ` is-${b}` : ""),
    title: title + hint + (agent ? "\nRight-click for options · drag to reorder" : ""),
    draggable: !!agent,
    onclick: () => { setFilter(agent?.id); if (agent) openActiveChat(agent.id); },
    oncontextmenu: (e) => { if (!agent) return; e.preventDefault(); agentMenu(agent, { x: e.clientX, y: e.clientY }); },
  }, agent ? avatar(agent, "sm") : el("span", { class: "mark sm" }, svgIcon("grid")),
    el("span", { class: "name" }, agent ? agent.name : "All chats"),
    el("span", { class: "state" }, agent ? stateWord(agent) : ""));
  if (!agent) return item;
  item.addEventListener("dragstart", (e) => {
    dragId = agent.id;
    item.classList.add("dragging");
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", agent.id);
  });
  item.addEventListener("dragend", () => { dragId = null; item.classList.remove("dragging"); clearDrop(); });
  item.addEventListener("dragover", (e) => {
    if (!dragId || dragId === agent.id) return;
    e.preventDefault();
    const r = item.getBoundingClientRect();
    clearDrop();
    item.classList.add(e.clientY < r.top + r.height / 2 ? "drop-before" : "drop-after");
  });
  item.addEventListener("dragleave", () => item.classList.remove("drop-before", "drop-after"));
  item.addEventListener("drop", async (e) => {
    e.preventDefault();
    const before = item.classList.contains("drop-before");
    clearDrop();
    if (!dragId || dragId === agent.id) return;
    const ids = S.agents.map((a) => a.id).filter((id) => id !== dragId);
    const at = ids.indexOf(agent.id) + (before ? 0 : 1);
    ids.splice(at, 0, dragId);
    try {
      S.agents = await api("PUT", "/api/agents/order", { ids });
      renderSidebar();
      agentCards($("#welcome-agents"), (a) => newChat(a.id));
    } catch (err) { toast(err.message, "err"); }
  });
  return item;
}

const clearDrop = () => $("#rail-agents").querySelectorAll(".drop-before, .drop-after").forEach((n) => n.classList.remove("drop-before", "drop-after"));

function agentMenu(agent, at) {
  contextMenu([
    { label: `New ${agent.name} chat`, icon: "plus", onClick: () => newChat(agent.id) },
    { label: "Edit agent", icon: "pencil", onClick: () => openAgentDialog(agent.id) },
    { label: "Show its chats", icon: "grid", onClick: () => setFilter(agent.id) },
  ], at);
}

export function drawCords() {}  // the trace inside each reply shows delegation now

// -------------------------------------------------------------- chat list

export function chatMenu(c, at, onClose) {
  const a = agentById(c.agent_id);
  contextMenu([
    { label: "Open", icon: "arrow", onClick: () => openChat(c.id) },
    !c.parent && { label: "Rename", icon: "pencil", onClick: () => renameChat(c) },
    !c.parent && { label: c.pinned ? "Unpin" : "Pin to top", icon: "pin", onClick: () => togglePin(c) },
    "-",
    { label: "Export as Markdown", icon: "download", onClick: () => (location.href = `/api/chats/${c.id}/export.md`) },
    { label: "Export as JSON", icon: "download", onClick: () => (location.href = `/api/chats/${c.id}/export.json`) },
    "-",
    { label: "Delete", icon: "trash", danger: true, onClick: () => deleteChat(c) },
  ].filter(Boolean), at, { onClose });
}

export async function renameChat(c) {
  const title = await prompt({ title: "Rename chat", value: c.title, ok: "Rename" });
  if (title == null || !title.trim() || title.trim() === c.title) return;
  try {
    await api("PATCH", `/api/chats/${c.id}`, { title: title.trim() });
    if (S.chat?.id === c.id) S.chat.title = title.trim();
    await refreshChats();
    if (S.chat?.id === c.id) (await import("./chat.js")).renderHeader();
  } catch (e) { toast(e.message, "err"); }
}

export async function togglePin(c) {
  try {
    await api("PATCH", `/api/chats/${c.id}`, { pinned: !c.pinned });
    await refreshChats();
  } catch (e) { toast(e.message, "err"); }
}

export async function deleteChat(c) {
  const ok = await confirm({ title: `Delete “${c.title}”?`, text: c.parent ? "This agent-to-agent conversation will be removed." :
    "Its messages, and any work other agents did for it, will be removed. This can't be undone.", ok: "Delete", danger: true });
  if (!ok) return;
  try {
    await api("DELETE", `/api/chats/${c.id}`);
    S.chats = S.chats.filter((x) => x.id !== c.id);
    delete S.drafts[c.id];
    if (S.chatId === c.id) openChat(null);
    else renderSidebar();
    toast("Chat deleted");
  } catch (e) { toast(e.message, "err"); }
}

export async function deleteAllIdle(agentId) {
  const victims = S.chats.filter((c) => (!agentId ? !c.parent : c.agent_id === agentId) && (!c.status || c.status === "idle") && !c.pinned && !c.routine_id);
  if (!victims.length) return toast("Nothing to delete: every chat here is pinned, running, or belongs to a routine.", "info");
  const who = agentId ? agentById(agentId).name : "every agent";
  const ok = await confirm({ title: `Delete ${victims.length} chat${victims.length > 1 ? "s" : ""}?`,
    text: `Every idle, unpinned chat ${agentId ? "with " + who : "you started"} will be removed. Pinned chats and routine chats stay.`, ok: "Delete all", danger: true });
  if (!ok) return;
  try {
    await api("POST", "/api/chats/delete", { ids: victims.map((c) => c.id) });
    if (victims.some((c) => c.id === S.chatId)) openChat(null);
    await refreshChats();
    toast(`Deleted ${victims.length} chat${victims.length > 1 ? "s" : ""}`);
  } catch (e) { toast(e.message, "err"); }
}

function chatRow(c, q) {
  const a = agentById(c.agent_id);
  const status = c.status && c.status !== "idle" ? el("span", { class: `status ${c.status}`, title: STATUS_TEXT[c.status] })
    : isUnread(c) ? el("span", { class: "status unread", title: "New reply since you last looked" }) : el("span");
  const forWho = c.parent ? `for ${agentById(c.parent.caller_id).name}` : "";
  const hit = q ? S.hits.get(c.id) : null;
  const sub = hit ? el("div", { class: "sub" }, `${a.name}: `, ...highlight(hit.snippet, q))
    : el("div", { class: "sub" }, [S.filter && !q ? "" : a.name, forWho, c.routine_id && "routine", timeAgo(c.updated)].filter(Boolean).join(" · "));
  const row = el("div", {
    class: "chat-item" + (c.id === S.chatId ? " active" : "") + (c.parent ? " sub-chat" : ""),
    title: c.preview || "", tabindex: 0, role: "button",
    onclick: () => openChat(c.id),
    onkeydown: (e) => { if (e.key === "Enter") openChat(c.id); },
    oncontextmenu: (e) => { e.preventDefault(); row.classList.add("menu-open"); chatMenu(c, { x: e.clientX, y: e.clientY }, () => row.classList.remove("menu-open")); },
  }, avatar(a, "sm"),
    el("div", { class: "meta" },
      el("div", { class: "title" }, c.pinned ? svgIcon("pin") : null, el("span", { class: "grow" }, ...(q ? highlight(c.title, q) : [c.title]))),
      sub),
    el("div", { class: "side" }, status,
      iconBtn("more", "Options", (e) => { e.stopPropagation(); row.classList.add("menu-open"); chatMenu(c, e.currentTarget, () => row.classList.remove("menu-open")); }, "sm more")));
  return row;
}

export function renderSidebar() {
  if (S.filter && !S.agents.some((a) => a.id === S.filter)) S.filter = "";

  // agents: "all chats" + one row per agent, with what it is doing right now
  const rail = $("#rail-agents");
  rail.innerHTML = "";
  rail.append(agentRow(null, "All chats"));
  for (const a of S.agents) rail.append(agentRow(a, `${a.name}${a.purpose ? ": " + a.purpose : ""}`));

  // the chats section's label says whose chats we are looking at
  const head = $("#side-head");
  head.innerHTML = "";
  const fa = S.filter ? agentById(S.filter) : null;
  if (fa) {
    head.append(el("div", { class: "meta", title: fa.purpose }, avatar(fa, "xs"), el("span", { class: "name" }, `${fa.name} chats`)),
      iconBtn("pencil", "Edit agent", () => openAgentDialog(fa.id), "xs"),
      iconBtn("more", "Options", (e) => headMenu(fa, e.currentTarget), "xs"));
    $("#new-chat-btn").replaceChildren(svgIcon("plus"), `New ${fa.name} chat`);
  } else {
    head.append(el("div", { class: "meta" }, el("span", {}, "Chats")),
      iconBtn("more", "Options", (e) => headMenu(null, e.currentTarget), "xs"));
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
  if (!chats.length) {
    list.append(el("div", { class: "empty-note" }, q ? "No chats match." : fa
      ? el("span", {}, el("b", {}, `No ${fa.name} chats yet. `), "Start one above, or ask another agent to patch it in.")
      : el("span", {}, el("b", {}, "No chats yet. "), "Start one above, or pick an agent on the right.")));
    return;
  }
  let group = null;
  for (const c of chats) {
    const g = q ? "Matches" : c.pinned && !c.parent ? "Pinned" : dayGroup(c.updated);
    if (g !== group) {
      group = g;
      list.append(el("div", { class: "chat-group" }, g));
    }
    list.append(chatRow(c, q));
  }
}

function headMenu(agent, at) {
  contextMenu([
    agent && { label: `Edit ${agent.name}`, icon: "pencil", onClick: () => openAgentDialog(agent.id) },
    { label: "Mark all as read", icon: "check", onClick: () => { for (const c of S.chats) markSeen(c.id, c.updated); renderSidebar(); } },
    "-",
    { label: agent ? `Delete ${agent.name}'s idle chats` : "Delete all idle chats", icon: "trash", danger: true, onClick: () => deleteAllIdle(agent?.id) },
  ].filter(Boolean), at);
}

// ------------------------------------------------------------ agent cards

export function agentCards(container, onPick) {
  container.innerHTML = "";
  const card = (cls, onPickIt, ...children) => el("div", {
    class: cls, tabindex: 0, role: "button", onclick: onPickIt,
    onkeydown: (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onPickIt(); } },
  }, ...children);
  for (const a of S.agents) {
    const busy = busyState(a.id);
    container.append(colored(card("agent-card" + (busy ? " busy" : ""), () => onPick(a),
      avatar(a, "lg"),
      el("div", { class: "body" },
        el("div", { class: "call" }, a.name, el("span", { class: "lampdot", title: busy ? STATUS_TEXT[busy] : "" })),
        el("div", { class: "purpose" }, a.purpose || "No job description yet."),
        el("div", { class: "tags" }, toolTags(a))),
      svgIcon("right", "ic go")), a.color));
  }
  container.append(card("agent-card new", () => { $("#pick-dialog").close(); openAgentDialog(null); },
    el("span", { class: "mark lg" }, svgIcon("plus")),
    el("div", { class: "body" }, el("div", { class: "call" }, "New agent"),
      el("div", { class: "purpose" }, "Give it a job, instructions and tools. Start from a template, a blank sheet, or a file.")),
    svgIcon("right", "ic go")));
}

function colored(node, color) { node.style.setProperty("--c", color || "#7c6cff"); return node; }

// The welcome page's "pick up where you left off" strip.
export function renderRecent() {
  const box = $("#welcome-recent");
  const list = $("#recent-list");
  const recent = S.chats.filter((c) => !c.parent).slice(0, 4);
  box.hidden = !recent.length;
  list.innerHTML = "";
  for (const c of recent) {
    const a = agentById(c.agent_id);
    list.append(el("button", { type: "button", class: "recent-item", onclick: () => openChat(c.id) }, avatar(a, "sm"),
      el("div", { class: "meta" }, el("div", { class: "title" }, c.title),
        el("div", { class: "sub" }, [a.name, c.status && c.status !== "idle" ? STATUS_TEXT[c.status] : timeAgo(c.updated)].join(" · ")))));
  }
}

// Sidebar collapse (desktop)
export function setCollapsed(on) {
  S.sideCollapsed = on;
  prefs.set("sideCollapsed", on ? "1" : "0");
  document.body.classList.toggle("side-collapsed", on);
}
$("#collapse-btn").onclick = () => setCollapsed(true);
$("#expand-btn").onclick = () => setCollapsed(false);

export function exportAgentJson(agent) {
  const { id, order, ...rest } = agent;
  downloadBlob(`${agent.name.replace(/[^\w-]+/g, "-").toLowerCase() || "agent"}.agent.json`, JSON.stringify(rest, null, 2));
}
