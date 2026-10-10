// Slash commands, like Claude Code: "/" at the start of the message box opens a menu of commands
// above it. The terminal client keeps the same registry (tui/src/commands.rs); a test checks that
// both list the same names and aliases, so keep one entry per line in this shape.

import { S } from "./state.js";
import { $, el, iconBtn, api, fmtK, copyText } from "./util.js";
import { toast } from "./ui.js";
import { newChat, forkChat, runChat, renderHeader, scrollBottom, chatAge } from "./chat.js";
import { setBypass, autoGrow } from "./composer.js";
import { renameChat, togglePin, deleteChat, refreshChats } from "./sidebar.js";
import { openAgentDialog } from "./agents.js";
import { openSettings } from "./settings.js";
import { openMemory } from "./memory.js";
import { openRoutines } from "./routines.js";
import { toggleFiles } from "./files.js";
import { toggleVerbose, cycleTheme, applyTheme, openShortcuts, pollServer, reloadState } from "./main.js";
import { openPalette } from "./palette.js";

const input = $("#input");
const menu = $("#slash-menu");
const list = $("#slash-list");
const card = $("#cmd-out");

// chat: only in a chat you write in. idle: refused while the agent works (it would change what it is working on,
// or the server refuses it mid-run: pinning gets "chat is busy").
export const COMMANDS = [
  { name: "help", desc: "List every command and the keyboard shortcuts", run: help },
  { name: "new", aliases: ["clear"], args: "[agent]", desc: "New chat with this chat's agent, or the one you name", run: startChat },
  { name: "resume", aliases: ["chats"], args: "[search]", desc: "Find an earlier chat", run: resume },
  { name: "rename", args: "[title]", chat: true, idle: true, desc: "Rename this chat", run: rename },
  { name: "pin", chat: true, idle: true, desc: "Pin or unpin this chat", run: pin },
  { name: "branch", aliases: ["fork"], chat: true, desc: "Branch this chat from the end into a new chat", run: branch },
  { name: "retry", aliases: ["regenerate"], chat: true, idle: true, desc: "Regenerate the last reply", run: retry },
  { name: "edit", chat: true, idle: true, desc: "Edit your last message and send it again", run: editLast },
  { name: "stop", chat: true, desc: "Stop the running reply", run: stop },
  { name: "compact", args: "[instructions]", chat: true, idle: true, desc: "Summarize older messages now; say what the summary must keep", run: compact },
  { name: "export", args: "[md|json]", chat: true, desc: "Download this chat (Markdown unless you say json)", run: exportChat },
  { name: "copy", chat: true, desc: "Copy the last reply", run: copyLast },
  { name: "delete", chat: true, idle: true, desc: "Delete this chat (asks first)", run: () => deleteChat(thisChat()) },
  { name: "agents", desc: "Edit this chat's agent", run: () => openAgentDialog(chatAgent()?.id || null) },
  { name: "model", args: "[name]", desc: "Show the models, or set this chat's agent's model (default clears it)", run: model },
  { name: "memory", desc: "Open the memory panel", run: () => openMemory() },
  { name: "remember", args: "<fact>", desc: "Save a fact to the memory every agent shares", run: remember },
  { name: "permissions", args: "[ask|bypass]", desc: "Show or switch whether agents ask before acting", run: permissions },
  { name: "settings", aliases: ["config"], desc: "Open settings", run: () => openSettings() },
  { name: "routines", desc: "Open routines", run: () => openRoutines() },
  { name: "files", desc: "Show or hide the workspace files", run: () => toggleFiles() },
  { name: "verbose", desc: "Show or collapse every thinking block and tool detail", run: () => toggleVerbose() },
  { name: "theme", args: "[auto|light|dark]", desc: "Switch the theme (no name: the next one)", run: theme },
  { name: "context", chat: true, desc: "How much of the context window this chat uses", run: context },
  { name: "usage", aliases: ["cost", "stats"], chat: true, desc: "Tokens in and out for this chat, replies, speed", run: usage },
  { name: "status", desc: "Model server, models, context size, app version", run: status },
];

const names = (c) => [c.name, ...(c.aliases || [])];
const inChat = () => !!S.chat && !S.chat.parent;
const thisChat = () => S.chats.find((c) => c.id === S.chatId) || { ...S.chat, pinned: !!S.chat.pinned };
const chatAgent = () => S.chat && S.agents.find((a) => a.id === S.chat.agent_id);

export const findCommand = (word) => COMMANDS.find((c) => names(c).includes(word.toLowerCase()));

// The message box's text as typed: {cmd, args} to run, {unknown} for a /word that is no command, or
// {text} to send. "//text" sends "/text"; a first word with another "/" in it (a path) is a message.
export function parseSlash(text) {
  if (!text.startsWith("/")) return { text };
  if (text.startsWith("//")) return { text: text.slice(1) };
  const [, word, args] = /^\/(\S*)\s*([\s\S]*)$/.exec(text);
  if (word.includes("/")) return { text };
  const cmd = findCommand(word);
  return cmd ? { cmd, args: args.trim() } : { unknown: word };
}

// Commands whose name or an alias starts with q: an exact name or alias first, then those whose own name
// starts with it ("/stat" is /status before /usage's alias /stats); then those that only contain it.
export function matchCommands(q, chat = inChat()) {
  q = q.toLowerCase();
  const rank = (c) => names(c).includes(q) ? 0 : c.name.startsWith(q) ? 1 : names(c).some((n) => n.startsWith(q)) ? 2
    : names(c).some((n) => n.includes(q)) ? 3 : null;
  return COMMANDS.filter((c) => (chat || !c.chat) && rank(c) != null).sort((a, b) => rank(a) - rank(b));  // stable: registry order otherwise
}

// What a refused command leaves in the box to run later: the text as typed, spelled out if it was picked
// from the menu half-typed.
const spelled = (cmd, args) => findCommand(/^\s*\/(\S*)/.exec(input.value)?.[1] ?? "") === cmd ? input.value : `/${cmd.name}${args ? ` ${args}` : ""}`;

// A command's run() returns this when it can't do what its arguments ask: it said why, and the text stays.
const REFUSED = Symbol("refused");
const refuse = (text) => { toast(text, "info"); return REFUSED; };

// Run a command typed or picked in the message box. It never reaches the agent, even mid-run.
export async function runCommand(cmd, args = "") {
  closeMenu();
  args = args.trim();
  const refused = cmd.chat && !inChat() ? "Open a chat first"
    : cmd.idle && S.running ? `The agent is working. Wait for it, or /stop it first, then /${cmd.name}.` : null;
  if (refused) {
    input.value = spelled(cmd, args);
    autoGrow();
    return toast(refused, "info");
  }
  const typed = spelled(cmd, args), chatId = S.chatId;
  input.value = "";
  autoGrow();
  delete S.drafts[S.chatId];
  let failed;
  try {
    failed = (await cmd.run(args)) === REFUSED;
  } catch (e) {
    toast(e.message, "err");
    failed = true;
  }
  if (failed && S.chatId === chatId && !input.value) {  // bad arguments or a server error: keep the line to fix it
    input.value = typed;
    autoGrow();
  }
}

// --------------------------------------------------------------------- the menu

let shown = [];         // the commands listed right now
let sel = 0;
let built = null;       // what the list was built for
let dismissed = null;   // the box's text when Esc closed the menu: stays closed until it changes

// The command word being typed (without the slash), while the cursor is still in it; otherwise null.
function typedWord() {
  const v = input.value;
  if (!v.startsWith("/") || v.startsWith("//") || document.activeElement !== input) return null;
  const word = /^\/\S*/.exec(v)[0];
  if (input.selectionEnd > word.length || word.includes("/", 1)) return null;
  return word.slice(1);
}

export function syncMenu() {
  if (dismissed !== null && input.value !== dismissed) dismissed = null;
  const q = dismissed === null ? typedWord() : null;
  if (q == null) return closeMenu();
  const low = q.toLowerCase();
  const key = `${low}\n${inChat()}`;  // the list depends on the chat too: a snapshot arriving refreshes it
  if (key === built && !menu.hidden) return;
  built = key;
  shown = matchCommands(low);
  sel = 0;
  if (!shown.length) return closeMenu();
  list.replaceChildren(...shown.map((c, i) => {
    const alias = c.name.includes(low) ? null : c.aliases?.find((a) => a.startsWith(low)) || c.aliases?.find((a) => a.includes(low));  // what matched, if not the name
    return el("div", {
      class: "sc-row", id: `slash-opt-${i}`, role: "option", "aria-selected": "false",
      onmousemove: () => { if (sel !== i) { sel = i; mark(); } },
      onmousedown: (e) => e.preventDefault(),  // keep the focus (and the cursor) in the message box
      onclick: () => pick(i, true),
    }, el("span", { class: "sc-call" }, el("span", { class: "sc-name" }, `/${c.name}`),
      alias ? el("span", { class: "sc-alias" }, ` /${alias}`) : null, c.args ? el("span", { class: "sc-args" }, ` ${c.args}`) : null),
    el("span", { class: "sc-desc" }, c.desc));
  }));
  menu.hidden = false;
  mark();
}

function closeMenu() {
  built = null;
  if (menu.hidden) return;
  menu.hidden = true;
  list.replaceChildren();
  shown = [];
  input.removeAttribute("aria-activedescendant");
}

function mark() {
  [...list.children].forEach((r, i) => { r.classList.toggle("selected", i === sel); r.setAttribute("aria-selected", String(i === sel)); });
  const row = list.children[sel];
  if (!row) return;
  input.setAttribute("aria-activedescendant", row.id);
  if (row.offsetTop < list.scrollTop) list.scrollTop = row.offsetTop - 4;
  else if (row.offsetTop + row.offsetHeight > list.scrollTop + list.clientHeight) list.scrollTop = row.offsetTop + row.offsetHeight - list.clientHeight + 4;
}

// Tab: the command's full name in the box, plus a space when it takes arguments.
function complete(i) {
  const c = shown[i];
  if (!c) return;
  const rest = input.value.replace(/^\/\S*\s*/, "");
  const head = `/${c.name}${c.args || rest ? " " : ""}`;
  input.value = head + rest;
  input.setSelectionRange(head.length, head.length);
  autoGrow();
  syncMenu();
}

// Enter or a click runs it with whatever follows; a click on one that needs an argument completes it instead.
function pick(i, click = false) {
  const c = shown[i];
  if (!c) return;
  const rest = input.value.replace(/^\/\S*\s*/, "");
  if (click && c.args?.startsWith("<") && !rest.trim()) return complete(i);
  runCommand(c, rest);
}

// Called first by the message box's keydown handler; true when the menu used the key. Ctrl+N reaches the
// page only on a Mac (Chrome and Firefox elsewhere keep it for a new window); Ctrl+P and the arrows work everywhere.
export function menuKeydown(e) {
  if (menu.hidden || e.isComposing || e.altKey || e.metaKey) return false;
  syncMenu();  // the app may have changed the text under the menu (a queued message handed back): never act on a stale one
  if (menu.hidden) return false;
  const ctrl = e.ctrlKey && !e.shiftKey;
  const k = e.key;
  if (e.ctrlKey && !ctrl) return false;
  if (k === "ArrowDown" || (ctrl && k === "n")) { sel = (sel + 1) % shown.length; mark(); }
  else if (k === "ArrowUp" || (ctrl && k === "p")) { sel = (sel - 1 + shown.length) % shown.length; mark(); }
  else if (e.ctrlKey) return false;
  else if (k === "Tab" && !e.shiftKey) complete(sel);
  else if (k === "Enter" && !e.shiftKey) pick(sel);
  else if (k === "Escape") { dismissed = input.value; closeMenu(); }
  else return false;
  e.preventDefault();
  e.stopPropagation();
  return true;
}

input.addEventListener("blur", closeMenu);
input.addEventListener("click", syncMenu);
input.addEventListener("keyup", (e) => { if (/^(Arrow(Left|Right)|Home|End)$/.test(e.key)) syncMenu(); });

// ------------------------------------------------------- the readout a command leaves

// A card under the conversation, for this browser only: it is not a message and the agent never sees it.
function showCard(name, ...body) {
  card.dataset.cmd = name;
  card.replaceChildren(el("section", { class: "cmd-card", "aria-label": `/${name}` },
    el("div", { class: "cmd-head" }, el("span", { class: "cmd-name" }, `/${name}`), el("span", { class: "cmd-only" }, "only you see this"),
      el("span", { class: "grow" }), iconBtn("x", "Close", closeCard, "xs")),
    el("div", { class: "cmd-body" }, ...body)));
  S.follow = true;
  requestAnimationFrame(scrollBottom);
}

export function closeCard() {
  if (!card.firstChild) return false;
  card.replaceChildren();
  delete card.dataset.cmd;
  return true;
}

const kv = (rows) => el("dl", { class: "cmd-kv" }, rows.filter(Boolean).flatMap(([k, v]) => [el("dt", {}, k), el("dd", {}, v)]));
const plural = (n, one, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

function help() {
  showCard("help",
    el("dl", { class: "cmd-kv cmd-help" }, COMMANDS.flatMap((c) => [
      el("dt", {}, `/${c.name}`, c.args ? el("span", { class: "sc-args" }, ` ${c.args}`) : null),
      el("dd", {}, c.desc, c.aliases ? el("span", { class: "muted" }, ` · also ${c.aliases.map((a) => `/${a}`).join(", ")}`) : null),
    ])),
    el("div", { class: "hint" }, "Start a message with // to send one that begins with /. ",
      el("button", { type: "button", class: "linkish cmd-link", onclick: openShortcuts }, "Keyboard shortcuts ", el("kbd", {}, "?"))));
}

async function context() {
  if (!S.server.n_ctx && !S.settings.context_size) await pollServer();  // the window size may not be known yet
  const used = S.chat.stats?.context_tokens || 0;
  const n = S.server.n_ctx || S.settings.context_size || 0;
  const at = S.settings.compact_at || 70;
  const auto = S.settings.auto_compact !== false ? `auto-compacts at ${at}%` : "auto-compact is off";
  const pct = n ? Math.round((used / n) * 100) : 0;
  showCard("context",
    el("div", { class: "cmd-line" }, !used ? `ctx 0${n ? ` / ${fmtK(n)}` : ""} · nothing sent yet`
      : n ? `ctx ${fmtK(used)} / ${fmtK(n)} (${pct}%) · ${auto}` : `ctx ~${fmtK(used)} · window size unknown (set it in Settings)`),
    n ? el("div", { class: "cmd-bar", title: `${used.toLocaleString()} of ${n.toLocaleString()} tokens` },
      el("i", { class: pct >= 90 ? "full" : pct >= at ? "hi" : "", style: `width:${Math.min(100, Math.max(1, pct))}%` }),
      S.settings.auto_compact !== false ? el("b", { style: `left:${at}%` }) : null) : null,
    el("div", { class: "cmd-age" }, chatAge(S.chat)),
    el("div", { class: "hint" }, "/compact summarizes older messages now."));
}

function usage() {
  const msgs = S.chat.messages;
  const calls = msgs.filter((m) => m.role === "assistant" && m._stats);
  const sum = (k) => calls.reduce((t, m) => t + (m._stats[k] || 0), 0);
  const replies = msgs.filter((m, i) => m.role === "assistant" && msgs[i - 1]?.role !== "assistant" && msgs[i - 1]?.role !== "tool").length;
  const speed = calls.findLast((m) => m._stats.tok_per_s)?._stats.tok_per_s;
  showCard("usage",
    el("div", { class: "cmd-line" }, `${fmtK(sum("prompt_tokens"))} in · ${fmtK(sum("completion_tokens"))} out · ${plural(replies, "reply", "replies")}`),
    el("div", { class: "cmd-age" }, chatAge(S.chat)),
    kv([
      ["tokens in", `${sum("prompt_tokens").toLocaleString()} (prompts summed over ${plural(calls.length, "model call")})`],
      ["tokens out", sum("completion_tokens").toLocaleString()],
      ["replies", String(replies)],
      ["last speed", speed ? `${speed.toFixed(1)} tok/s` : "not measured yet"],
    ]),
    S.chat.subchats?.length ? el("div", { class: "hint" }, "Work other agents did for this chat is counted in their own chats.") : null);
}

async function status() {
  await pollServer();
  const s = S.server;
  const n = s.n_ctx || S.settings.context_size || 0;
  showCard("status", kv([
    ["server", s.ok ? el("span", { class: "cmd-ok" }, "reachable") : el("span", { class: "cmd-bad" }, `not reachable${s.error ? `: ${s.error}` : ""}`)],
    ["address", s.base_url || S.settings.base_url || "not set"],
    s.ok && ["kind", s.kind === "llama" ? "llama-server" : "OpenAI-compatible server"],
    ["models", s.models.join(", ") || "none listed"],
    ["default", S.settings.model || (s.models[0] ? `${s.models[0]} (the server's first)` : "the server's first")],
    ["context", n ? `${n.toLocaleString()} tokens${s.n_ctx ? "" : " (from Settings)"}` : "unknown"],
    ["app", `Agent Chat ${S.version}`],
  ]));
}

// /model: list the models (pick one), or set this chat's agent's model by (part of) its name.
async function model(name) {
  if (!S.server.models.length) await pollServer();
  const a = chatAgent();
  if (!name) return showModels(a);
  if (!a) return refuse(S.chat ? "This chat's agent was deleted." : "Open a chat first");
  const models = S.server.models;
  const low = name.toLowerCase();
  const base = (m) => m.split("/").pop().replace(/\.gguf$/i, "").toLowerCase();
  const hits = models.filter((m) => m.toLowerCase().includes(low));
  let want = low === "default" ? "" : models.find((m) => m.toLowerCase() === low || base(m) === low) || (hits.length === 1 ? hits[0] : null);
  if (want == null && hits.length > 1) return refuse(`Which one? ${hits.join(", ")}`);
  want ??= name;
  await api("PUT", `/api/agents/${a.id}`, { ...a, model: want });
  await reloadState();
  const listed = !want || models.includes(want);
  toast(want ? `${a.name} now uses ${want}${listed ? "" : " (the server doesn't list it)"}` : `${a.name} uses the default model again`, listed ? "ok" : "info");
  if (card.dataset.cmd === "model") showModels(chatAgent());
}

function showModels(a) {
  const fallback = S.settings.model || S.server.models[0] || "the server's first model";
  const current = a?.model || "";
  const models = [...S.server.models];
  if (current && !models.includes(current)) models.push(current);
  const opt = (m) => el("button", { type: "button", class: "cmd-opt" + (m === current ? " on" : ""), "aria-pressed": String(m === current),
    onclick: () => model(m || "default").catch((e) => toast(e.message, "err")) }, m || `default · ${fallback}`);
  showCard("model",
    el("div", { class: "cmd-line" }, a ? `${a.name} uses ${current || `the default: ${fallback}`}` : `Default: ${fallback}`),
    a ? el("div", { class: "cmd-opts", role: "group", "aria-label": "Models" }, opt(""), ...models.map(opt)) : null,
    el("div", { class: "hint" }, !S.server.ok ? "The model server isn't reachable, so no models are listed."
      : a ? "Pick one, or type /model <name>. It changes the agent, so its other chats use it too." : "Open a chat to set its agent's model."));
}

// ------------------------------------------------------------------ the rest

function startChat(name) {
  if (!name) return S.chat ? newChat(S.chat.agent_id) : $("#new-chat-btn").click();
  const low = name.toLowerCase();
  const a = S.agents.find((x) => x.id === name || x.name.toLowerCase() === low) || S.agents.find((x) => x.name.toLowerCase().startsWith(low));
  return a ? newChat(a.id) : refuse(`No agent called “${name}”. /agents makes one.`);
}

function resume(q) {
  openPalette(q);
  if (q) $("#palette-q").dispatchEvent(new Event("input"));  // search inside messages too
}

async function rename(title) {
  if (!title) return renameChat(thisChat());
  const c = await api("PATCH", `/api/chats/${S.chatId}`, { title });
  S.chat.title = c.title;
  renderHeader();
  await refreshChats();
  toast(`Renamed to “${c.title}”`);
}

async function pin() {
  const c = thisChat();
  await togglePin(c);
  const now = !!S.chats.find((x) => x.id === c.id)?.pinned;
  if (now !== !!c.pinned) toast(now ? "Pinned to the top of the list" : "Unpinned");
}

function branch() {
  const n = S.chat.messages.length;
  return n ? forkChat(n) : toast("Nothing to branch yet", "info");
}

function retry() {  // the last turn's Regenerate button
  const u = S.chat.messages.findLastIndex((m) => m.role === "user");
  return u < 0 ? toast("Nothing to regenerate yet", "info") : runChat({ from_index: u + 1 });
}

function editLast() {  // the last message's Edit button
  const mine = [...document.querySelectorAll("#history .msg-user:not(.from-agent)")].at(-1);
  const btn = mine?.querySelector(".actions .icon-btn[title='Edit and resend']");
  if (!btn) return toast("No message of yours to edit yet", "info");
  btn.click();
  mine.scrollIntoView({ block: "nearest" });
}

function stop() {
  if (!S.running) return toast("Nothing is running", "info");
  return api("POST", `/api/chats/${S.chatId}/stop`);
}

function compact(instructions) {
  return api("POST", `/api/chats/${S.chatId}/compact`, instructions ? { instructions } : undefined);
}

function exportChat(fmt) {
  fmt = (fmt || "md").toLowerCase().replace(/^\./, "").replace(/^markdown$/, "md");
  if (fmt !== "md" && fmt !== "json") return refuse("Export as md or json, like /export json");
  const a = el("a", { href: `/api/chats/${S.chatId}/export.${fmt}`, download: "" });  // the server names the file
  document.body.append(a);
  a.click();
  a.remove();
}

function copyLast() {  // the newest reply that has text, as its Copy button copies it
  const texts = [];
  for (const m of [...S.chat.messages].reverse()) {
    if (m.role === "user") { if (texts.length) break; continue; }
    if (m.role === "assistant" && m.content) texts.unshift(m.content);
  }
  return texts.length ? copyText(texts.join("\n\n")) : toast("No reply to copy yet", "info");
}

async function remember(fact) {
  if (!fact) return refuse("Say what to remember, like /remember I prefer short answers");
  const m = await api("POST", "/api/memory", { fact });
  toast(`${m.action === "merged" ? "Updated a memory" : "Remembered"}: ${m.fact}`, "ok", { action: { label: "Memory", onClick: () => openMemory() } });
}

function permissions(arg) {
  const on = !!S.settings.bypass_approvals;
  arg = arg.toLowerCase();
  if (arg === "ask") return on ? setBypass(false) : toast("Agents already ask before running commands or sending email.", "info");
  if (arg === "bypass") return on ? toast("Permissions are already bypassed.", "info") : setBypass(true);
  if (arg) return refuse("Use /permissions ask or /permissions bypass");
  toast(on ? "Bypassing permissions: agents run commands and send email without asking. /permissions ask turns that off."
    : "Agents ask before running shell commands or sending email. /permissions bypass stops the asking.", "info", { timeout: 6000 });
}

function theme(name) {
  name = name.toLowerCase();
  if (!name) cycleTheme();
  else if (["auto", "light", "dark"].includes(name)) applyTheme(name);
  else return refuse("Themes: auto, light, dark");
  toast(`Theme: ${document.documentElement.dataset.theme || "auto"}`);
}
