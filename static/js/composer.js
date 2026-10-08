// The message box: sending, queueing while the agent works, attachments, the permission chip.

import { S, agentById } from "./state.js";
import { $, el, svgIcon, iconBtn, api, fmtSize } from "./util.js";
import { toast, confirm, openImage } from "./ui.js";
import { runChat, userBubble, scrollBottom, openChat } from "./chat.js";
import { openSettings } from "./settings.js";

const input = $("#input");

export function setRunning(running) {
  S.running = running;
  const send = $("#send-btn");
  send.querySelector("span").textContent = running ? "Queue" : "Send";
  send.title = running ? "Queue (Enter): the agent reads it at its next step" : "Send (Enter)";
  input.placeholder = running ? "Queue a message: the agent reads it at its next step" : "Message";
  $("#stop-btn").hidden = !running;
  $("#sub-banner-stop").hidden = !running;
  $("#hdr-compact").disabled = running;
}

// A sub-chat shows an agent working for another agent: read-only, replies go through the chat that started it.
export function applyChatMode(parent) {
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
$("#sub-banner-stop").onclick = () => S.chatId && api("POST", `/api/chats/${S.chatId}/stop`).catch((e) => toast(e.message, "err"));

// ------------------------------------------------------------ message queue
// Like Claude Code: messages sent while the agent works wait here, and the server hands them to
// the agent between steps. Edit or ✕ takes one back; ↑ in an empty box takes them all back.

export function renderQueue() {
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
      iconBtn("x", "Remove from the queue", () => unqueue(item.id, true))));
  }
}

// Put text back in a chat's message box, ahead of anything typed there since.
export function returnToInput(chatId, text) {
  if (!text) return;
  if (chatId !== S.chatId) {
    S.drafts[chatId] = S.drafts[chatId] ? `${text}\n\n${S.drafts[chatId]}` : text;
    return;
  }
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
    toast(e.message, "err");
  }
}

export function autoGrow() {
  input.style.height = "auto";
  input.style.height = Math.min(input.scrollHeight, 260) + "px";
}

input.addEventListener("input", autoGrow);
input.addEventListener("keydown", (e) => {
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
  const typed = input.value.trim();
  const content = (typed + attachmentText()).trim();
  if (!content || !S.chatId) return;
  if ("Notification" in window && Notification.permission === "default") Notification.requestPermission();
  const images = S.attachments.filter((f) => f.image).map((f) => f.path);
  const attachments = S.attachments;
  input.value = "";
  S.attachments = [];
  renderAttachments();
  autoGrow();
  delete S.drafts[S.chatId];
  const restore = () => { input.value = typed; S.attachments = attachments; renderAttachments(); autoGrow(); };
  if (S.running) {  // the server queues it (or starts a run with it, if the run just ended)
    try {
      const res = await api("POST", `/api/chats/${S.chatId}/run`, { content, images });
      if (!res.queued) setRunning(true);
    } catch (err) {
      restore();
      toast(err.message, "err");
    }
    return;
  }
  const bubble = userBubble({ content, _images: images, _ts: Date.now() / 1000 }, -1);
  $("#history").append(bubble);
  S.follow = true;
  scrollBottom();
  const res = await runChat({ content, images });
  if (!res) {
    restore();
    bubble.remove();
  } else if (res.queued) bubble.remove();  // a run had started elsewhere (another tab, a routine)
});
$("#stop-btn").onclick = () => S.chatId && api("POST", `/api/chats/${S.chatId}/stop`).catch((e) => toast(e.message, "err"));

// ------------------------------------------------------------- attachments

export function renderAttachments() {
  const box = $("#attachments");
  box.innerHTML = "";
  box.hidden = !S.attachments.length;
  S.attachments.forEach((f, i) => {
    const chip = el("span", { class: "attach-chip", title: f.path });
    if (f.image) {
      const src = fileUrl(f.agent_id, f.path);
      chip.append(el("img", { src, alt: f.name, onclick: () => openImage(src, f.name) }));
    }
    chip.append(f.name, el("span", { class: "muted" }, ` ${fmtSize(f.size)}`),
      iconBtn("x", "Remove", () => { S.attachments.splice(i, 1); renderAttachments(); }));
    box.append(chip);
  });
}

export const fileUrl = (agentId, path, download = false) =>
  `/api/workspace/file?${new URLSearchParams({ agent_id: agentId || S.chat?.agent_id || "", path, ...(download ? { download: "true" } : {}) })}`;

export function attachmentText() {
  return S.attachments.map((f) => {
    if (f.image) return "";  // images travel as image parts (see runChat's `images`), the note is enough
    if (f.text == null) return `\n\n[Attached file: ${f.name} (${fmtSize(f.size)}), saved in the workspace at ${f.path}]`;
    const fence = f.text.includes("```") ? "````" : "```";
    return `\n\n[Attached file: ${f.name}, saved at ${f.path}]\n${fence}\n${f.text}\n${fence}`;
  }).join("");
}

export async function uploadFiles(files) {
  if (!S.chatId || S.chat?.parent) return toast("Open a chat first to attach files.", "info");
  for (const file of files) {
    if (file.size > 20 * 1024 * 1024) { toast(`${file.name} is larger than 20 MB`, "err"); continue; }
    try {
      const res = await fetch(`/api/chats/${S.chatId}/upload?name=${encodeURIComponent(file.name || (file.type.startsWith("image/") ? "pasted.png" : "pasted"))}`,
        { method: "POST", body: file, headers: { "X-Agent-Chat": "1" } });
      const info = await res.json();
      if (!res.ok) throw new Error(info.detail || res.statusText);
      S.attachments.push(info);
      renderAttachments();
    } catch (e) {
      toast(`Couldn't attach ${file.name}: ${e.message}`, "err");
    }
  }
  input.focus();
}

$("#attach-btn").onclick = () => $("#file-input").click();
$("#file-input").addEventListener("change", (e) => { uploadFiles([...e.target.files]); e.target.value = ""; });
input.addEventListener("paste", (e) => {
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
window.addEventListener("dragover", (e) => { if ([...(e.dataTransfer?.types || [])].includes("Files")) e.preventDefault(); });
window.addEventListener("drop", (e) => {
  if (![...(e.dataTransfer?.types || [])].includes("Files")) return;
  e.preventDefault();
  dragDepth = 0;
  $("#drop-overlay").hidden = true;
  if (e.dataTransfer?.files?.length) uploadFiles([...e.dataTransfer.files]);
});

// ------------------------------------------------------- bypass permissions

export const BYPASS_WARNING = "Agents will run shell commands and send email without asking you first. " +
  "Anything they read, like web pages or emails, could try to trick them into running a command or sending mail. " +
  "You can switch this off again at any time.";

export function renderMode() {
  const on = !!S.settings.bypass_approvals;
  const b = $("#mode-btn");
  b.classList.toggle("bypass", on);
  b.querySelector("span").textContent = on ? "Bypassing permissions" : "Asks before acting";
  b.title = on ? "Agents run shell commands and send email without asking. Click to make them ask again."
    : "Agents ask before running shell commands or sending email. Click to bypass.";
  document.body.classList.toggle("bypass", on);
}

export async function setBypass(on) {
  if (on && !(await confirm({ title: "Bypass permissions?", text: BYPASS_WARNING, ok: "Bypass", danger: true }))) return false;
  try {
    S.settings = await api("PUT", "/api/settings", { bypass_approvals: on });
    renderMode();
    toast(on ? "Permissions bypassed: agents act without asking" : "Agents will ask before acting again", on ? "info" : "ok");
    return true;
  } catch (e) {
    toast(e.message, "err");
    return false;
  }
}
$("#mode-btn").onclick = () => setBypass(!S.settings.bypass_approvals);

// ---------------------------------------------------------------- offline

export function renderOffline() {
  const down = S.server.ok === false;
  $("#offline-bar").hidden = !down;
  $("#offline-note").hidden = !down;
  if (down) {
    const where = S.server.base_url || S.settings.base_url || "the model server";
    $("#offline-bar-text").textContent = `The model server at ${where} isn't reachable. Messages you send will fail until it's back.`;
    $("#offline-text").textContent = `Nothing answers at ${where}. Start llama-server (or whichever server you use), or point Settings at a different address.`;
  }
}
$("#offline-bar-settings").onclick = () => openSettings("model");
$("#offline-settings").onclick = () => openSettings("model");
