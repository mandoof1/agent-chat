// The jack panel (agents, lamps, patch cords) and the call log (chat list).

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

// ------------------------------------------------------------------- rail

let dragId = null;

function railItem(agent, title) {
  const b = busyState(agent?.id);
  const hint = { running: " (working)", waiting: " (needs your approval)", delegated: " (waiting on another agent)" }[b] || "";
  const item = el("button", {
    type: "button", "data-agent": agent?.id || "",
    class: "jack-item" + (agent ? "" : " all") + ((agent?.id || "") === S.filter ? " active" : "") + (b ? ` is-${b}` : ""),
    title: title + hint + (agent ? "\nRight-click for options · drag to reorder" : ""),
    draggable: !!agent,
    onclick: () => { setFilter(agent?.id); if (agent) openActiveChat(agent.id); },
    oncontextmenu: (e) => { if (!agent) return; e.preventDefault(); agentMenu(agent, { x: e.clientX, y: e.clientY }); },
  }, el("span", { class: "lamp" }),
    agent ? avatar(agent) : el("span", { class: "jack" }, svgIcon("grid")),
    el("span", { class: "strip" }, agent ? agent.name : "All"));
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

const clearDrop = () => $("#rail").querySelectorAll(".drop-before, .drop-after").forEach((n) => n.classList.remove("drop-before", "drop-after"));

function agentMenu(agent, at) {
  contextMenu([
    { label: `New ${agent.name} chat`, icon: "plus", onClick: () => newChat(agent.id) },
    { label: "Edit agent", icon: "pencil", onClick: () => openAgentDialog(agent.id) },
    { label: "Show its chats", icon: "grid", onClick: () => setFilter(agent.id) },
  ], at);
}

// Patch cords: while an agent waits on another, draw a cord between their jacks
// (from caller to callee, with a slow flow so you can see which way the call goes).
export function drawCords() {
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

  // jack panel: "all chats" + one jack per agent; the lamp shows what its chats are doing
  const rail = $("#rail-agents");
  rail.innerHTML = "";
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
      iconBtn("pencil", "Edit agent", () => openAgentDialog(fa.id)),
      iconBtn("more", "Options", (e) => headMenu(fa, e.currentTarget)));
    $("#new-chat-btn").replaceChildren(svgIcon("plus"), `New ${fa.name} chat`);
  } else {
    head.append(el("div", { class: "meta" }, el("div", { class: "name" }, "All chats"),
      el("div", { class: "purpose" }, "Every chat you started. Pick an agent on the left to see its own work.")),
      iconBtn("more", "Options", (e) => headMenu(null, e.currentTarget)));
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
      el("span", { class: "lampdot", title: busy ? STATUS_TEXT[busy] : "" }),
      el("div", { class: "top" }, avatar(a), el("span", { class: "call" }, a.name)),
      el("div", { class: "purpose" }, a.purpose || "No job description yet."),
      el("div", { class: "tags" }, toolTags(a))), a.color));
  }
  container.append(card("agent-card new", () => { $("#pick-dialog").close(); openAgentDialog(null); },
    el("div", { class: "top" }, el("span", { class: "jack" }, svgIcon("plus")), el("span", { class: "call" }, "New agent")),
    el("div", { class: "purpose" }, "Give it a job, instructions and tools. Start from a template, a blank sheet, or a file.")));
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
  requestAnimationFrame(drawCords);
}
$("#collapse-btn").onclick = () => setCollapsed(true);
$("#expand-btn").onclick = () => setCollapsed(false);

export function exportAgentJson(agent) {
  const { id, order, ...rest } = agent;
  downloadBlob(`${agent.name.replace(/[^\w-]+/g, "-").toLowerCase() || "agent"}.agent.json`, JSON.stringify(rest, null, 2));
}
