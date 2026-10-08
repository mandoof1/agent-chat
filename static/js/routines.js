// Routines: agents that run on a schedule.

import { S, agentById } from "./state.js";
import { $, el, svgIcon, avatar, api, timeAgo } from "./util.js";
import { toast, confirm } from "./ui.js";
import { openChat } from "./chat.js";
import { refreshChats } from "./sidebar.js";

const DAY_NAMES = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]; // Python weekday() order
export const ROUTINE_TEMPLATES = [
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

export function scheduleText(s) {
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
  const clockText = date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  if (s < 60) return "now";
  if (s < 3600) return `in ${Math.round(s / 60)} min`;
  if (s < 86400) return `in ${Math.floor(s / 3600)} h ${Math.round((s % 3600) / 60)} min (${clockText})`;
  return `${DAY_NAMES[(date.getDay() + 6) % 7]} ${clockText}`;
}

let editingRoutineId = null;

export async function loadRoutines() {
  const items = await api("GET", "/api/routines");
  const list = $("#routine-list");
  list.innerHTML = "";
  if (!items.length) list.append(el("div", { class: "empty-note" }, el("b", {}, "No routines yet. "), "Create one below; the templates are a good start."));
  for (const r of items) {
    const a = agentById(r.agent_id);
    const toggle = el("input", { type: "checkbox", checked: r.enabled, title: r.enabled ? "On" : "Off" });
    toggle.onchange = async () => { try { await api("PUT", `/api/routines/${r.id}`, { enabled: toggle.checked }); loadRoutines(); } catch (e) { toast(e.message, "err"); } };
    const runNow = el("button", { type: "button", class: "btn sm" }, svgIcon("play"), "Run now");
    runNow.onclick = async () => {
      try {
        const res = await api("POST", `/api/routines/${r.id}/run`);
        if (res.status !== "started") toast(`Not started: ${res.status}`, "info");
        else { $("#routines-dialog").close(); await refreshChats(); openChat(res.chat_id); }
      } catch (e) { toast(e.message, "err"); }
    };
    const open = r.chat_id ? el("button", { type: "button", class: "btn sm ghost", onclick: () => { $("#routines-dialog").close(); openChat(r.chat_id); } }, "Open chat") : null;
    const edit = el("button", { type: "button", class: "icon-btn", title: "Edit", onclick: () => openRoutineEditor(r) }, svgIcon("pencil"));
    const del = el("button", { type: "button", class: "icon-btn danger", title: "Delete (its chat is kept)" }, svgIcon("trash"));
    del.onclick = async () => {
      if (!(await confirm({ title: `Delete the routine “${r.name}”?`, text: "Its chat is kept.", ok: "Delete", danger: true }))) return;
      try { await api("DELETE", `/api/routines/${r.id}`); loadRoutines(); } catch (e) { toast(e.message, "err"); }
    };
    const last = r.last_run ? `last run ${timeAgo(r.last_run)}${r.last_status && r.last_status !== "started" ? ` (${r.last_status})` : ""}` : "never run";
    list.append(el("div", { class: "routine-item" + (r.enabled ? "" : " off") },
      toggle,
      el("div", { class: "mem-main" },
        el("div", { class: "routine-name" }, avatar(a, "sm"), r.name),
        el("div", { class: "mem-meta" }, `${scheduleText(r.schedule)} · ${a.name} · next ${r.enabled ? whenText(r.next_run) : "— (off)"} · ${last}`)),
      runNow, open, edit, del));
  }
}

export function openRoutineEditor(r = null) {
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
    toast(e.message, "err");
  }
}

$("#routines-form").elements.namedItem("r_type").addEventListener("change", syncRoutineType);
$("#routine-save").onclick = saveRoutine;
$("#routine-cancel").onclick = () => { $("#routine-editor").open = false; editingRoutineId = null; };
$("#routine-editor").addEventListener("toggle", (e) => { if (e.target.open && !$("#routines-form").elements.namedItem("r_name").value && !editingRoutineId) openRoutineEditor(null); });

export async function openRoutines() {
  try { await loadRoutines(); } catch (e) { return toast(e.message, "err"); }
  $("#routine-editor").open = false;
  $("#routines-dialog").showModal();
}
$("#routines-btn").onclick = openRoutines;
