// The memory panel: what the agents know about you.

import { S, agentById } from "./state.js";
import { $, el, svgIcon, api, timeAgo } from "./util.js";
import { toast, confirm } from "./ui.js";
import { newChat } from "./chat.js";
import { autoGrow } from "./composer.js";

export async function refreshMemoryCount() {
  try {
    const data = await api("GET", "/api/memory");
    $("#memory-count").textContent = data.total ? (data.total > 99 ? "99+" : data.total) : "";
    $("#onboarding").hidden = data.total > 0;
  } catch {}
}

$("#onboard-memory").onclick = () => openMemory();
$("#onboard-chat").onclick = async () => {
  await newChat(S.agents.find((a) => a.id === "assistant" && a.memory)?.id || S.agents.find((a) => a.memory)?.id || S.agents[0]?.id);
  $("#input").value = "Hi! A bit about me so you can help me better: my name is …, I work on …, and I like answers that are …";
  autoGrow();
  $("#input").focus();
};

export async function loadMemory() {
  const cat = $("#memory-category");
  const params = new URLSearchParams({ q: $("#memory-search").value.trim(), category: cat.value });
  const data = await api("GET", `/api/memory?${params}`);
  if (cat.options.length <= 1) {
    cat.innerHTML = "";
    cat.append(el("option", { value: "" }, "All kinds"), ...data.categories.map((c) => el("option", { value: c }, c)));
  }
  $("#memory-total").textContent = `${data.total} remembered`;
  const list = $("#memory-list");
  list.innerHTML = "";
  if (!data.memories.length) {
    list.append(el("div", { class: "empty-note" }, data.total ? "Nothing matches." : "Nothing yet. Chat with your agents, or add something above."));
  }
  for (const m of data.memories) list.append(memoryRow(m, data.categories));
}

function memoryRow(m, categories) {
  const save = async (fields) => {
    try { await api("PUT", `/api/memory/${m.id}`, fields); } catch (e) { toast(e.message, "err"); loadMemory(); }
  };
  const fact = el("div", { class: "mem-fact", contenteditable: "true", spellcheck: "false", title: "Click to edit" }, m.fact);
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
    try { await api("DELETE", `/api/memory/${m.id}`); loadMemory(); refreshMemoryCount(); } catch (e) { toast(e.message, "err"); }
  };
  const src = m.source === "user" ? "added by you" : m.source.startsWith("auto") ? "learned automatically"
    : m.source.startsWith("tidy") ? "merged by tidy-up" : `saved by ${agentById(m.source.replace(/^agent:/, "")).name}`;
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
    if (m.action === "merged") toast("Updated a similar memory instead of adding a duplicate", "info");
    loadMemory();
    refreshMemoryCount();
  } catch (e) {
    toast(e.message, "err");
  }
}

let memSearchTimer;
$("#memory-search").addEventListener("input", () => { clearTimeout(memSearchTimer); memSearchTimer = setTimeout(loadMemory, 200); });
$("#memory-search").addEventListener("keydown", (e) => { if (e.key === "Enter") e.preventDefault(); });
$("#memory-category").addEventListener("change", loadMemory);
$("#memory-add-btn").onclick = addMemory;
$("#memory-tidy-btn").onclick = async () => {
  const btn = $("#memory-tidy-btn");
  if (!(await confirm({ title: "Tidy up memory?", text: "The model merges duplicates, resolves contradictions (newest wins) and drops trivia. Facts you typed in yourself are never changed.", ok: "Tidy up" }))) return;
  btn.disabled = true;
  btn.textContent = "Tidying…";
  try {
    const r = await api("POST", "/api/memory/tidy");
    toast(r.message);
    loadMemory();
    refreshMemoryCount();
  } catch (e) {
    toast(e.message, "err");
  } finally {
    btn.disabled = false;
    btn.textContent = "Tidy up";
  }
};
$("#memory-new").addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); addMemory(); } });

export async function openMemory() {
  try { await loadMemory(); } catch (e) { return toast(e.message, "err"); }
  $("#memory-dialog").showModal();
}
$("#memory-btn").onclick = openMemory;
