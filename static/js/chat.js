// A chat: opening it, its header and stats, the rendered history, and the live view of a run.

import { S, agentById, agentByName, APPROVAL_TEXT } from "./state.js";
import { $, el, svgIcon, avatar, iconBtn, api, fmtK, fmtSpan, oneLine, pkey, clock, renderMd, langFor, setCode, codeBlock, partialArgs, copyText } from "./util.js";
import { toast, confirm, contextMenu, openImage } from "./ui.js";
import { renderSidebar, markSeen, renameChat, togglePin } from "./sidebar.js";
import { setRunning, renderQueue, applyChatMode, autoGrow, renderAttachments, fileUrl } from "./composer.js";
import { refreshFiles, filesChanged, openWorkspaceFile } from "./files.js";
import { openAgentDialog } from "./agents.js";
import { openSettings } from "./settings.js";
import { setTitle } from "./notify.js";
import { syncMenu } from "./commands.js";

export const messagesEl = $("#messages");

export function scrollBottom() {
  messagesEl.scrollTop = messagesEl.scrollHeight;
}
messagesEl.addEventListener("scroll", () => {
  const atBottom = messagesEl.scrollHeight - messagesEl.scrollTop - messagesEl.clientHeight < 80;
  if (atBottom !== S.follow) {
    S.follow = atBottom;
    if (atBottom) S.newSince = 0;
    renderJump();
  }
});

function renderJump() {
  const b = $("#jump-btn");
  const show = !S.follow && !$("#messages").hidden && messagesEl.scrollHeight > messagesEl.clientHeight + 200;
  b.hidden = !show;
  b.classList.toggle("fresh", S.newSince > 0);
  b.querySelector("span").textContent = S.newSince > 0 ? `${S.newSince} new` : "Newest";
}
$("#jump-btn").onclick = () => { S.follow = true; S.newSince = 0; scrollBottom(); renderJump(); };

// ------------------------------------------------------------ navigation

export function showView(view) {
  const chat = view === "chat";
  $("#welcome").hidden = chat;
  $("#chat-header").hidden = !chat;
  $("#messages").hidden = !chat;
  $("#composer").hidden = !chat;
  if (!chat) { $("#sub-banner").hidden = true; $("#jump-btn").hidden = true; }
}

export function openChat(id) {
  if (S.chatId) S.drafts[S.chatId] = $("#input").value;
  if (S.es) { S.es.close(); S.es = null; }
  Object.assign(S, { chatId: id, chat: null, running: false, live: null, follow: true, newSince: 0 });
  history.replaceState(null, "", id ? `#/chat/${id}` : location.pathname);
  document.body.classList.remove("side-open");
  showView(id ? "chat" : "welcome");
  if (id) applyChatMode(S.chats.find((c) => c.id === id)?.parent);
  renderSidebar();
  $("#history").innerHTML = "";
  $("#live").innerHTML = "";
  renderFoot();
  $("#cmd-out").replaceChildren();  // a slash command's readout belongs to the chat it ran in
  if (!id) { setTitle(); refreshFiles(true); return; }

  $("#input").value = S.drafts[id] || "";
  syncMenu();  // a menu open over the last chat's text must not run its command here
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
      toast("That chat no longer exists.", "err");
      openChat(null);
    }
  };
  if (matchMedia("(min-width: 821px)").matches) $("#input").focus();
}

export async function newChat(agentId) {
  try {
    const chat = await api("POST", "/api/chats", { agent_id: agentId });
    S.chats.unshift({ ...chat, preview: "", status: "idle", messages: 0 });
    if (S.filter && S.filter !== agentId) S.filter = agentId;
    openChat(chat.id);
  } catch (e) {
    toast(e.message, "err");
  }
}

export async function runChat(body) {
  if (S.running) return toast("Wait for the current reply or stop it first.", "info");
  setRunning(true);
  try {
    return await api("POST", `/api/chats/${S.chatId}/run`, body);
  } catch (e) {
    setRunning(false);
    toast(e.message, "err");
    return false;
  }
}

export async function forkChat(upto) {
  if (!S.chat) return;
  try {
    const fork = await api("POST", `/api/chats/${S.chatId}/fork`, { upto });
    S.chats.unshift({ ...fork, status: "idle" });
    toast("Branched. This copy keeps the conversation up to here; the original is unchanged.", "info");
    openChat(fork.id);
  } catch (e) {
    toast(e.message, "err");
  }
}

// ------------------------------------------------------------------ header

export function renderHeader() {
  const a = agentById(S.chat.agent_id);
  const av = $("#hdr-avatar");
  av.textContent = a.emoji;
  av.style.setProperty("--c", a.color || "#7c6cff");
  if ($("#hdr-title").contentEditable !== "true") $("#hdr-title").textContent = S.chat.title;
  const parent = S.chat.parent;
  $("#hdr-sub").textContent = parent ? `${a.name} · working for ${agentById(parent.caller_id).name}`
    : a.purpose ? `${a.name} · ${a.purpose}` : a.name;
  $("#hdr-compact").hidden = !!parent;
  applyChatMode(parent);
  setTitle();
  renderStats();
}

export function renderStats() {
  const box = $("#hdr-stats");
  box.innerHTML = "";
  const st = S.chat?.stats || {};
  if (st.context_tokens) {
    const n = S.server.n_ctx;
    const pct = n ? Math.min(100, (st.context_tokens / n) * 100) : 0;
    const meter = el("div", { class: "ctx-meter", title: n
      ? `Context window: ${st.context_tokens.toLocaleString()} of ${n.toLocaleString()} tokens (${Math.round(pct)}%). Updates live while the agent works; older messages are summarized at ${S.settings.compact_at || 70}%.`
      : `Prompt size: about ${st.context_tokens.toLocaleString()} tokens. Set the context size in Settings to see how full the window is.` });
    if (n) {
      const fill = el("i", { class: pct >= 90 ? "full" : pct >= 70 ? "hi" : "", style: `width:${Math.max(2, pct)}%` });
      meter.append(el("div", { class: "bar" }, fill));
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
    toast(err.message, "err");
  }
});
$("#hdr-compact").onclick = async () => {
  if (!S.chat || S.running) return toast("Wait for the current reply or stop it first.", "info");
  try {
    await api("POST", `/api/chats/${S.chatId}/compact`);
  } catch (e) {
    toast(e.message, "err");
  }
};
$("#hdr-export").onclick = (e) => S.chat && contextMenu([
  { label: "Markdown (.md)", icon: "download", onClick: () => (location.href = `/api/chats/${S.chatId}/export.md`) },
  { label: "JSON, with agent-to-agent work (.json)", icon: "download", onClick: () => (location.href = `/api/chats/${S.chatId}/export.json`) },
], e.currentTarget);
$("#hdr-more").onclick = (e) => {
  if (!S.chat) return;
  const c = S.chats.find((x) => x.id === S.chatId) || { ...S.chat, pinned: !!S.chat.pinned };
  const a = agentById(S.chat.agent_id);
  contextMenu([
    !c.parent && { label: "Rename", icon: "pencil", onClick: () => renameChat(c) },
    !c.parent && { label: c.pinned ? "Unpin" : "Pin to top", icon: "pin", onClick: () => togglePin(c) },
    !c.parent && S.chat.messages.length > 0 && { label: "Branch from the end", icon: "branch", onClick: () => forkChat(S.chat.messages.length) },
    S.agents.some((x) => x.id === a.id) && { label: `Edit ${a.name}`, icon: "sliders", onClick: () => openAgentDialog(a.id) },
    "-",
    { label: "Delete chat", icon: "trash", danger: true, onClick: () => import("./sidebar.js").then((m) => m.deleteChat(c)) },
  ].filter(Boolean), e.currentTarget);
};
$("#menu-btn").onclick = () => document.body.classList.add("side-open");

// ---------------------------------------------------------- message pieces

function thinkingEl(text, live, seconds = null) {
  const label = el("span", { class: "label" }, live ? "Thinking" : seconds != null ? `Thought for ${fmtSecs(seconds)}` : "Thoughts");
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
      label.textContent = seconds != null ? `Thought for ${fmtSecs(seconds)}` : "Thoughts";
    },
  };
}

const fmtSecs = (s) => (s >= 60 ? `${Math.floor(s / 60)} min ${Math.round(s % 60)} s` : `${Math.max(1, Math.round(s))} s`);

function compactDivider(c) {
  const why = { auto: "automatically", manual: "on request", overflow: "because the context was full", cutoff: "because a reply filled the context" }[c.reason] || "";
  const size = c.before_tokens && c.after_tokens ? ` · ${fmtK(c.before_tokens)} → ${fmtK(c.after_tokens)} tokens` : "";
  const keep = c.instructions ? ` · keeping ${oneLine(c.instructions, 60)}` : "";
  const body = el("div", { class: "md" });
  renderMd(body, c.summary, true);
  return el("details", { class: "compact-divider", "data-at": c.at },
    el("summary", { title: c.instructions ? `Asked to keep: ${c.instructions}` : null }, `Earlier messages summarized ${why}${size}${keep}`), body);
}

// An error with a way out: retry the turn, or fix the server address.
function errorBox(text, index) {
  const connection = /reach the model server|Connection to the model server|HTTP 404/.test(text);
  const row = el("div", { class: "row" });
  if (index != null && !S.chat?.parent) {
    row.append(el("button", { type: "button", class: "btn sm", onclick: () => retryFrom(index) }, svgIcon("redo"), "Try again"));
  }
  if (connection) row.append(el("button", { type: "button", class: "btn sm ghost", onclick: () => openSettings("model") }, "Open settings"));
  return el("div", { class: "error-box" }, el("div", { class: "e-title" }, connection ? "Couldn't reach the model" : "The agent hit an error"),
    el("div", { class: "e-text" }, text), row.children.length ? row : null);
}

// Re-run the turn that produced the error at message `index` (the error message itself is dropped).
function retryFrom(index) {
  if (S.running) return toast("Wait for the current reply or stop it first.", "info");
  const msgs = S.chat.messages;
  // find the start of the assistant turn containing the error: back to the last user message
  let i = index;
  while (i > 0 && msgs[i - 1].role !== "user") i--;
  runChat({ from_index: i });
}

const noticeEl = (text) => el("div", { class: "notice" }, text);

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
    case "web_fetch": case "browser_navigate": return a.url;
    case "browser_click": return a.text || a.selector;
    case "browser_type": return `${a.label || a.selector || ""}: ${oneLine(a.text, 60)}`;
    case "browser_screenshot": return a.path || "screenshot.png";
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

function openFileBtn(path) {
  return el("button", { type: "button", class: "btn sm ghost", onclick: (e) => { e.preventDefault(); openWorkspaceFile(S.chat?.agent_id, path); } }, svgIcon("eye"), "Open");
}

function argDetails(name, a) {
  const label = (t, ...extra) => el("div", { class: "t-label" }, t, ...extra);
  const pre = (t) => el("pre", {}, t ?? "");
  if (a._raw !== undefined) return [label("Raw arguments"), pre(a._raw)];
  if (name === "run_shell") return [label("Command"), codeBlock(a.command, "bash")];
  if (name === "email_send" || name === "email_draft") {
    const head = [`To: ${a.to}`, a.cc && `Cc: ${a.cc}`, `Subject: ${a.subject}`, a.reply_to_uid && `In reply to email #${a.reply_to_uid}`];
    return [label(name === "email_send" ? "Email to send" : "Draft"), pre(`${head.filter(Boolean).join("\n")}\n\n${a.body ?? ""}`)];
  }
  if (name === "write_file") return [label(`Content → ${a.path}`, openFileBtn(a.path)), codeBlock(a.content, langFor(a.path))];
  if (name === "edit_file") return [label(`Replace in ${a.path}`, openFileBtn(a.path)), codeBlock(a.old_text, langFor(a.path)), label("With"), codeBlock(a.new_text, langFor(a.path))];
  if (["read_file", "list_dir", "web_search", "web_fetch", "remember", "recall_memory", "forget_memory", "browser_navigate",
       "browser_read", "browser_links", "browser_click", "browser_type", "browser_screenshot", "browser_close",
       "email_list", "email_search", "email_read", "calendar_events"].includes(name)) return [];
  return [label("Arguments"), pre(JSON.stringify(a, null, 2))];
}

function resultView(name, args, text) {
  // a saved screenshot: show the picture itself
  if (name === "browser_screenshot" && /^Saved a screenshot/.test(text)) {
    const path = args.path || "screenshot.png";
    const src = fileUrl(S.chat?.agent_id, path) + `&t=${Date.now()}`;
    return [el("div", { class: "t-label" }, text), el("img", { class: "t-img", src, alt: path, onclick: () => openImage(src, path) })];
  }
  if (name === "read_file" && !/^Error/.test(text)) {
    return [el("div", { class: "t-label" }, "Result", openFileBtn(args.path)), el("pre", {}, text)];
  }
  return [el("div", { class: "t-label" }, "Result"), el("pre", {}, text)];
}

function toolCard(name, args, live) {
  const isAsk = name === "ask_agent";
  const state = el("span", { class: "t-state" + (live ? " run" : "") }, live ? "running" : "");
  const summary = el("summary");
  const body = el("div", { class: "t-body" });
  const card = el("details", { class: "tool-card" + (isAsk ? " sub" : "") }, summary, body);
  let subBody = null;
  let approvalBox = null;
  let liveOut = null;

  if (isAsk) {
    const target = agentByName(args.agent);
    const text = args.message ?? args.task ?? "";
    card.style.setProperty("--c", target.color || "#7c6cff");
    summary.append(el("span", { class: "patch" }, svgIcon("handoff"), "handoff"), avatar(target, "xs"), el("span", { class: "t-name" }, target.name),
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
    el: card, subBody, name,
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
    progress(text) {  // a shell command's output so far
      if (!liveOut) {
        liveOut = el("pre", { class: "live-out" });
        body.append(el("div", { class: "t-label live-label" }, "Output so far"), liveOut);
        card.open = true;
      }
      const follow = liveOut.scrollHeight - liveOut.scrollTop - liveOut.clientHeight < 30;
      liveOut.textContent = text;
      if (follow) liveOut.scrollTop = liveOut.scrollHeight;
    },
    setResult(text) {
      text = String(text ?? "");
      liveOut?.previousSibling?.remove();
      liveOut?.remove();
      liveOut = null;
      const failed = /^Error\b/.test(text) || text.startsWith("The user denied") || text.startsWith("The user did not approve") || text.startsWith("[cancelled");
      let label = !failed ? "done"
        : text.startsWith("[cancelled") ? "cancelled"
        : text.startsWith("The user") ? "denied" : "failed";
      let cls = failed ? "err" : "ok";
      const exit = name === "run_shell" && /^exit code (-?\d+)/.exec(text);
      if (exit) { label = `exit ${exit[1]}`; cls = exit[1] === "0" ? "ok" : "err"; }
      if (name === "run_shell" && /^Command timed out/.test(text)) { label = "timed out"; cls = "err"; }
      state.className = "t-state " + cls;
      state.textContent = label;
      if (isAsk && !failed) return; // the sub-agent's final reply is already in its transcript
      body.append(...resultView(name, args, text));
    },
    askApproval(approvalId) {
      card.classList.add("approval");
      card.open = true;
      state.className = "t-state";
      state.textContent = "needs approval";
      const yes = el("button", { class: "btn ok sm", type: "button" }, "Approve");
      const all = el("button", { class: "btn sm", type: "button", title: "Approve this and everything else the agent asks for until this run ends" }, "Approve all for this run");
      const no = el("button", { class: "btn danger sm", type: "button" }, "Deny");
      const answer = async (approve, everything = false) => {
        yes.disabled = no.disabled = all.disabled = true;
        try {
          await api("POST", `/api/chats/${S.chatId}/approvals/${approvalId}`, { approve, all: everything });
        } catch (e) {
          toast(e.message, "err");
          yes.disabled = no.disabled = all.disabled = false;
        }
      };
      yes.onclick = () => answer(true);
      all.onclick = () => answer(true, true);
      no.onclick = () => answer(false);
      approvalBox = el("div", { class: "approval-box" },
        el("div", { class: "q" }, APPROVAL_TEXT[name] || "Allow this?"), el("div", { class: "row" }, yes, all, no));
      body.append(approvalBox);
      requestAnimationFrame(() => yes.focus({ preventScroll: true }));
    },
    markBypassed(reason) {
      if (!summary.querySelector(".t-auto")) state.before(el("span", { class: "t-auto",
        title: reason === "run" ? "Ran without asking: you approved everything for this run" : "Ran without asking: permissions are bypassed" }, "auto-approved"));
    },
    approvalDone(approved, everything) {
      approvalBox?.remove();
      approvalBox = null;
      card.classList.remove("approval");
      state.className = "t-state " + (approved ? "run" : "err");
      state.textContent = approved ? "running" : "denied";
      if (everything) toast("Approved for the rest of this run. New runs ask again.", "info");
    },
  };
}

function statLine(stats) {
  const bits = [];
  if (stats?.tok_per_s) bits.push(`${stats.tok_per_s.toFixed(1)} tok/s`);
  if (stats?.prompt_tokens) bits.push(`${fmtK(stats.prompt_tokens)} in`);
  if (stats?.completion_tokens) bits.push(`${fmtK(stats.completion_tokens)} out`);
  return bits;
}

function makeTurn(agent, ts) {
  const stat = el("span", { class: "stat" });
  const when = el("span", { class: "when" }, clock(ts));
  const content = el("div", { class: "trace" });
  const actions = el("div", { class: "turn-actions" });
  const node = el("div", { class: "turn" },
    el("div", { class: "turn-head" }, avatar(agent, "sm"), el("span", { class: "call" }, agent.name), stat, when),
    el("div", { class: "turn-body" }, content, actions));
  node.style.setProperty("--c", agent.color || "#7c6cff");
  return {
    el: node, body: content, actions, texts: [],
    setStat(stats) {
      const bits = statLine(stats);
      if (bits.length) { stat.replaceChildren(...bits.map((b) => el("span", {}, b))); stat.title = "Speed of this reply · prompt tokens in · tokens generated"; }
    },
    setTime(ts) { when.textContent = clock(ts); },
  };
}

// ------------------------------------------------------------ history view

function appendAssistant(container, m, results, index) {
  if (m._error) container.append(errorBox(m._error, index));
  if (m.reasoning_content) container.append(thinkingEl(m.reasoning_content, false, m._stats?.think_s ?? null).el);
  if (m.content) {
    const md = el("div", { class: "md segment" });
    renderMd(md, m.content, true);
    container.append(md);
  }
  if (m._stopped) container.append(el("div", { class: "stopped-tag" }, "Stopped here"));
  if (m._truncated) container.append(noticeEl("This reply filled the context window before it finished. The agent continues from where it stopped in the next reply."));
  for (const tc of m.tool_calls || []) {
    let args;
    try { args = JSON.parse(tc.function.arguments || "{}"); } catch { args = { _raw: tc.function.arguments }; }
    const card = toolCard(tc.function.name, args, false);
    const res = results.get(tc.id);
    if (res?._bypassed) card.markBypassed(res._bypass_reason);
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

export function renderHistory(upTo) {
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
      h.append(userBubble(m, i, caller));
    } else if (m.role === "assistant") {
      if (!turn) {
        turn = makeTurn(agent, m._ts);
        turn.start = i;
        turns.push(turn);
        h.append(turn.el);
      }
      appendAssistant(turn.body, m, results, i);
      if (m.content) turn.texts.push(m.content);
      if (m._stats) turn.setStat(m._stats);
      turn.end = i + 1;
    }
  });
  turns.forEach((t, idx) => {
    const last = idx === turns.length - 1;
    if (t.texts.length) t.actions.append(iconBtn("copy", "Copy reply", () => copyText(t.texts.join("\n\n"))));
    if (!caller) {
      t.actions.append(iconBtn("redo", "Regenerate", async () => {
        if (!last && !(await confirm({ title: "Regenerate this reply?", text: "Every message after it will be deleted.", ok: "Regenerate", danger: true }))) return;
        runChat({ from_index: t.start });
      }));
      t.actions.append(iconBtn("branch", "Branch from here: a new chat that continues from this reply", () => forkChat(t.end)));
    }
    if (last) t.actions.classList.add("show");
  });
  if (!msgs.length && !S.running && !caller) {
    const a = agentById(S.chat.agent_id);
    const t = makeTurn(a, null);
    t.body.append(el("div", { class: "md muted" }, a.purpose ? `Ready. My job: ${a.purpose}.` : "Ready when you are."));
    h.append(t.el);
  }
  renderFoot();
}

// ------------------------------------------------------------------ footer
// One quiet line after the last turn (and under a running one): how often the chat was compacted
// and how long it has been going. /usage and /context say the same with the start time.

const startedAt = (chat) => chat.created || chat.messages[0]?._ts || chat.updated;
const compacted = (chat) => (chat.compactions?.length ? `compacted ${chat.compactions.length}×` : "not compacted yet");
// A running time that keeps counting: the ticker below updates every element with data-since.
const since = (ts) => el("span", { "data-since": ts }, fmtSpan(Date.now() / 1000 - ts));
export const chatAge = (chat) => [`${compacted(chat)} · started ${clock(startedAt(chat))} (going for `, since(startedAt(chat)), ")"];

export function renderFoot() {
  const foot = $("#chat-foot");
  foot.hidden = !S.chat?.messages.length;
  if (foot.hidden) return foot.replaceChildren();
  foot.replaceChildren(`${compacted(S.chat)} · going for `, since(startedAt(S.chat)));
  foot.title = `Started ${new Date(startedAt(S.chat) * 1000).toLocaleString()}`;
}
setInterval(() => {
  const now = Date.now() / 1000;
  for (const n of document.querySelectorAll("[data-since]")) {
    const t = fmtSpan(now - n.dataset.since);
    if (n.textContent !== t) n.textContent = t;
  }
}, 1000);

// A compaction mid-run: take the count from the chat as the server has it now (an edit or regenerate
// can also shorten the list). A newer copy of the chat (run_start, done, a snapshot) wins over a late answer.
let compSeq = 0;
async function refreshCompactions() {
  const chat = S.chat, seq = ++compSeq;
  if (!chat) return;
  try {
    const fresh = await api("GET", `/api/chats/${chat.id}`);
    if (S.chat !== chat || seq !== compSeq) return;
    chat.compactions = fresh.compactions || [];
    renderFoot();
  } catch {}  // the chat is gone: its stream says so
}

export function userBubble(m, index, from = null) {
  const text = m.content || "";
  const wrap = el("div", { class: "msg-user" + (from ? " from-agent" : "") });
  if (from) wrap.style.setProperty("--c", from.color || "#7c6cff");
  const show = () => {
    wrap.innerHTML = "";
    const actions = el("div", { class: "actions" }, iconBtn("copy", "Copy", () => copyText(text)));
    if (index >= 0 && !from) {
      actions.prepend(iconBtn("pencil", "Edit and resend", edit));
      actions.append(iconBtn("branch", "Branch from here: a new chat with the conversation up to this message", () => forkChat(index + 1)));
    }
    const who = el("div", { class: "who" },
      from ? el("span", { class: "from" }, avatar(from, "xs"), `${from.name} asked`) : "You",
      m._ts ? el("span", { class: "when" }, clock(m._ts)) : null,
      m._queued ? el("span", { class: "queued-tag", title: "You sent this while the agent was working; it read it at its next step" }, "· sent while it worked") : null,
      actions);
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
    if (m._images?.length) {
      col.append(el("div", { class: "msg-images" }, m._images.map((p) => {
        const src = fileUrl(S.chat?.agent_id, p);
        return el("img", { src, alt: p, title: p, onclick: () => openImage(src, p) });
      })));
    }
    col.append(bubble);
    const meta = el("div", { class: "row", style: "gap:10px" });
    if (m._recall) {
      const lines = m._recall.split("\n").filter(Boolean);
      const list = el("div", { class: "recall-list", hidden: true }, lines.map((l) => el("div", {}, l.replace(/^- \[\d+\] /, "• "))));
      meta.append(el("button", {
        class: "recall-chip", type: "button", title: "Memories the agent was reminded of with this message",
        onclick: () => (list.hidden = !list.hidden),
      }, svgIcon("memory"), `${lines.length} ${lines.length === 1 ? "memory" : "memories"} recalled`));
      col.append(meta, list);
    } else if (meta.children.length) col.append(meta);
    wrap.append(who, col);
  };
  const edit = () => {
    if (S.running) return toast("Wait for the current reply or stop it first.", "info");
    const ta = el("textarea", {}, text);
    const send = el("button", { class: "btn primary sm", type: "button" }, "Send");
    const cancel = el("button", { class: "btn ghost sm", type: "button", onclick: show }, "Cancel");
    send.onclick = () => {
      const content = ta.value.trim();
      if (content) runChat({ content, from_index: index, images: m._images || [] });
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
  const turn = makeTurn(agentById(agentId), Date.now() / 1000);
  $("#live").append(turn.el);
  S.live = { turn, containers: new Map([["", { body: turn.body, seg: null }]]), cards: new Map() };
}

const liveContainer = (path) => S.live?.containers.get(pkey(path));

const MD_EVERY = 60;  // ms between Markdown re-renders of a streaming reply (parsing a long reply 60×/s is wasteful)

function newSegment(container) {
  const seg = {
    text: "", reasoning: "", args: 0, t0: Date.now(), thinkT0: null, thinkT1: null, mdAt: 0,
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
      const now = performance.now();
      if (now - seg.mdAt >= MD_EVERY || seg.text.length < 2000) {
        seg.mdAt = now;
        renderMd(seg.md, seg.text, false);
      } else markDirty(seg);  // render the newest text on a later frame
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
    S.live.turn.setStat(m._stats);
    if (m._stats.prompt_tokens) {
      S.chat.stats = { ...S.chat.stats, context_tokens: m._stats.prompt_tokens + (m._stats.completion_tokens || 0), tok_per_s: m._stats.tok_per_s };
      renderStats();
    }
  }
  c.seg = null;
  if (!S.follow) { S.newSince++; renderJump(); }
}

function liveUserMessage(ev) {
  if (!S.live) return;
  const m = ev.message;
  $("#live").append(userBubble(m, ev.index, null));
  const turn = makeTurn(agentById(S.chat.agent_id), Date.now() / 1000);  // the agent's output after it continues here
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

const FILE_TOOLS = new Set(["write_file", "edit_file", "run_shell", "browser_screenshot"]);

export function handleEvent(ev) {
  switch (ev.type) {
    case "snapshot":
      S.chat = ev.chat;
      syncMenu();  // a menu opened before the chat arrived lists only what needs no chat
      renderHeader();
      renderHistory(ev.running ? ev.run_base : null);
      resetLive();
      setRunning(ev.running);
      S.queue = ev.queue || [];
      renderQueue();
      refreshFiles(true);
      markSeen(S.chatId, S.chat.updated);
      requestAnimationFrame(() => { scrollBottom(); renderJump(); });
      break;
    case "run_start":
      S.chat = ev.chat;
      renderHeader();
      renderHistory(ev.run_base);
      resetLive();
      startLive(ev.agent_id);
      setRunning(true);
      S.follow = true;
      S.newSince = 0;
      renderJump();
      break;
    case "assistant_start": liveAssistantStart(ev); break;
    case "delta": liveDelta(ev); return; // scroll handled by the frame flush
    case "assistant_end": liveAssistantEnd(ev); break;
    case "tool_start": liveToolStart(ev); break;
    case "tool_progress": S.live?.cards.get(ev.call_id)?.progress(ev.content); break;
    case "user_message": liveUserMessage(ev); break;
    case "queue": S.queue = ev.items; renderQueue(); return;
    case "ctx":  // live context size while the agent works (the exact count still arrives with assistant_end)
      if (!ev.path?.length && S.chat) { S.chat.stats = { ...S.chat.stats, context_tokens: ev.tokens }; renderStats(); }
      return;
    case "tool_result": {
      const card = S.live?.cards.get(ev.call_id);
      card?.setResult(ev.content);
      if (card && FILE_TOOLS.has(card.name)) filesChanged();
      break;
    }
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
      if (!ev.path?.length) {
        // Joining mid-run, the snapshot's history already shows this one, and the replay just drew it again.
        for (const d of $("#history").querySelectorAll(".compact-divider")) if (d.dataset.at === String(ev.compaction.at)) d.remove();
        refreshCompactions();
      }
      break;
    }
    case "approval": S.live?.cards.get(ev.call_id)?.askApproval(ev.approval_id); break;
    case "approval_done": S.live?.cards.get(ev.call_id)?.approvalDone(ev.approved, ev.all); break;
    case "approval_bypassed": S.live?.cards.get(ev.call_id)?.markBypassed(ev.reason); break;
    case "notice": liveContainer(ev.path)?.body.append(noticeEl(ev.text)); break;
    case "error": S.live?.turn.body.append(errorBox(ev.message, S.chat?.messages.length)); break;
    case "stopped": break;
    case "done": {
      const keepScroll = !S.follow && messagesEl.scrollTop;
      S.chat = ev.chat;
      renderHeader();
      renderHistory(null);
      resetLive();
      setRunning(false);
      S.queue = [];  // delivered, or handed back to the message box (queue_returned)
      renderQueue();
      filesChanged();
      markSeen(S.chatId, S.chat.updated);
      if (keepScroll) messagesEl.scrollTop = keepScroll;
      break;
    }
  }
  if (S.follow) scrollBottom();
}
