// Desktop notifications, the tab title and the favicon badge.

import { S, agentById } from "./state.js";
import { $ } from "./util.js";

const FAVICON = $("#favicon").href;
const BADGED = FAVICON.replace("</svg>", "<circle cx='26' cy='6' r='5' fill='%23ff6b4f' stroke='%231b2427' stroke-width='1.5'/></svg>");

export function notify(title, body, chatId) {
  if (!("Notification" in window) || Notification.permission !== "granted" || !document.hidden) return;
  const n = new Notification(title, { body: (body || "").slice(0, 180), tag: chatId || "agent-chat" });
  n.onclick = async () => { window.focus(); if (chatId) (await import("./chat.js")).openChat(chatId); n.close(); };
}

export function notifyStatus(chat, before, now) {
  const a = agentById(chat.agent_id);
  if (now === "waiting" && before !== "waiting") notify(`${a.emoji} ${a.name} needs your approval`, chat.title, chat.id);
  else if (now === "idle" && before && before !== "idle" && !chat.parent) notify(`${a.emoji} ${a.name} replied`, chat.title, chat.id);
}

// The favicon gets a red dot while any agent waits for an approval; the title says so too.
export function updateBadge() {
  const waiting = S.chats.some((c) => c.status === "waiting");
  $("#favicon").href = waiting ? BADGED : FAVICON;
  setTitle();
}

export function setTitle() {
  const waiting = S.chats.filter((c) => c.status === "waiting").length;
  const base = S.chat ? `${S.chat.title} · Agent Chat` : "Agent Chat";
  document.title = waiting ? `(${waiting}) ${base}` : base;
}
