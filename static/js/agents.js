// The agent editor: templates, tools, who it may talk to, prompt preview, duplicate, import/export.

import { S } from "./state.js";
import { $, el, api, copyText, downloadBlob } from "./util.js";
import { toast, confirm } from "./ui.js";
import { reloadState } from "./main.js";

export const AGENT_TEMPLATES = [
  { name: "Personal Assistant", emoji: "🗓️", color: "#4fb3a9", purpose: "Keeps track of your plans, tasks and the people in your life",
    tools: ["ask_agent", "read_file", "write_file", "calendar_events"],
    system_prompt: "You are the user's personal assistant. Help them plan their day, keep track of tasks and commitments, think through decisions, and remember what matters to them (people, routines, goals, deadlines). Keep answers practical and short. Keep a running to-do list in todo.md in the workspace when it helps. For email, ask the Mail agent; for research, ask the Researcher." },
  { name: "Tutor", emoji: "🎓", color: "#6c8cff", purpose: "Teaches any topic step by step",
    tools: ["ask_agent", "web_search", "web_fetch"],
    system_prompt: "You are a patient tutor. Find out what the user already knows, explain step by step with small concrete examples, and check understanding with a quick question before moving on. Adapt to how they learn best and remember it. Don't just hand over homework answers; explain the reasoning." },
  { name: "Translator", emoji: "🌐", color: "#2bb3d9", purpose: "Translates text naturally between languages", tools: [],
    system_prompt: "Translate the user's text faithfully and naturally, keeping formatting, names and tone. If the target language isn't stated, translate into English, or from English into the language the user used most recently. Briefly point out idioms or phrases that don't translate directly." },
  { name: "Brainstormer", emoji: "💡", color: "#f2c14e", purpose: "Generates and sharpens ideas with you", tools: ["ask_agent"],
    system_prompt: "You are a creative partner. Generate many varied ideas quickly, then help the user pick and refine the best ones. Build on their ideas, offer unexpected angles, and keep momentum. Use what you know about the user's interests." },
  { name: "Data Analyst", emoji: "📊", color: "#3fb27f", purpose: "Explores data files in the workspace with Python and reports what they show",
    tools: ["list_dir", "read_file", "write_file", "run_shell", "ask_agent"],
    system_prompt: "You are a data analyst working in the workspace folder. Look at the data first (list_dir, read_file, or a quick Python script via run_shell) before drawing conclusions. Write small, readable Python scripts into the workspace and run them; print the numbers you base claims on. Report findings plainly: what the data shows, what it doesn't, and what you'd check next. Save charts and cleaned data into the workspace and say where they are." },
];

const BLANK = {
  name: "", emoji: "🤖", color: "#7c6cff", purpose: "", system_prompt: "", tools: ["ask_agent"], delegates: [],
  delegate_all: true, model: "", temperature: null, workspace: "", confirm_shell: true, memory: true,
};
const FIELDS = ["name", "emoji", "color", "purpose", "system_prompt", "tools", "delegates", "delegate_all", "model", "temperature", "workspace", "confirm_shell", "memory"];

let editingAgentId = null;

export function fillModelSelect(select, current, defaultLabel) {
  select.innerHTML = "";
  const models = [...S.server.models];
  if (current && !models.includes(current)) models.push(current);
  select.append(el("option", { value: "" }, defaultLabel));
  for (const m of models) select.append(el("option", { value: m, title: m }, m.split("/").pop()));
  select.value = current || "";
}

function fillForm(a) {
  const form = $("#agent-form");
  const f = (n) => form.elements.namedItem(n);
  f("emoji").value = a.emoji || "🤖";
  f("name").value = a.name || "";
  f("color").value = /^#[0-9a-f]{6}$/i.test(a.color) ? a.color : "#7c6cff";
  f("purpose").value = a.purpose || "";
  f("system_prompt").value = a.system_prompt || "";
  f("temperature").value = a.temperature ?? "";
  f("workspace").value = a.workspace || "";
  f("confirm_shell").checked = a.confirm_shell !== false;
  f("delegate_all").checked = a.delegate_all !== false;
  f("memory").checked = a.memory !== false;
  fillModelSelect(f("model"), a.model, "Default (from Settings)");
  for (const box of form.querySelectorAll("input[data-tool]")) box.checked = (a.tools || []).includes(box.dataset.tool);
  for (const box of form.querySelectorAll("input[data-delegate]")) box.checked = (a.delegates || []).includes(box.dataset.delegate);
  syncAgentForm();
}

export function openAgentDialog(id, preset = null) {
  editingAgentId = id;
  const a = id ? S.agents.find((x) => x.id === id) : { ...BLANK, ...(preset || {}) };
  if (!a) return;
  const form = $("#agent-form");
  $("#agent-dialog-title").textContent = id ? `Edit ${a.name}` : preset?.name ? `New agent from ${preset.name}` : "New agent";
  $("#workspace-input").placeholder = S.defaultWorkspace + " (shared default)";

  const toolBox = $("#tool-checks");
  toolBox.innerHTML = "";
  for (const t of S.tools) {
    toolBox.append(el("label", { class: "check" },
      el("input", { type: "checkbox", "data-tool": t.name }),
      t.label,
      t.danger ? el("span", { class: "tag danger" }, "risky") : null));
  }
  const delBox = $("#delegate-checks");
  delBox.innerHTML = "";
  const others = S.agents.filter((x) => x.id !== id);
  for (const o of others) {
    delBox.append(el("label", { class: "check" }, el("input", { type: "checkbox", "data-delegate": o.id }), `${o.emoji} ${o.name}`));
  }
  if (!others.length) delBox.append(el("div", { class: "empty-note" }, "No other agents yet."));
  $("#template-row").hidden = !!id;
  const tpl = $("#agent-template");
  tpl.innerHTML = "";
  tpl.append(el("option", { value: "" }, "Blank agent"), ...AGENT_TEMPLATES.map((t, i) => el("option", { value: i }, `${t.emoji} ${t.name}`)));
  tpl.onchange = () => {
    const t = AGENT_TEMPLATES[tpl.value];
    if (!t) return;
    fillForm({ ...BLANK, ...t });
  };
  fillForm(a);
  $("#agent-delete").hidden = !id;
  $("#agent-preview").hidden = !id;
  $("#agent-duplicate").hidden = !id;
  $("#agent-import").hidden = !!id;
  $("#agent-dialog").showModal();
}

export function syncAgentForm() {
  const form = $("#agent-form");
  const has = (name) => $(`#tool-checks input[data-tool="${name}"]`)?.checked;
  const all = form.elements.namedItem("delegate_all").checked;
  $("#delegates-fieldset").disabled = !has("ask_agent");
  form.elements.namedItem("confirm_shell").disabled = !has("run_shell");
  for (const box of form.querySelectorAll("input[data-delegate]")) {
    box.disabled = all;
    if (all) box.checked = true;
  }
}
$("#tool-checks").addEventListener("change", syncAgentForm);
$("#agent-form").elements.namedItem("delegate_all").addEventListener("change", (e) => {
  if (!e.target.checked) for (const box of $("#agent-form").querySelectorAll("input[data-delegate]")) box.checked = false;
  syncAgentForm();
});

function readForm() {
  const form = $("#agent-form");
  const f = (n) => form.elements.namedItem(n);
  const temp = f("temperature").value.trim();
  return {
    name: f("name").value.trim(),
    emoji: f("emoji").value.trim() || "🤖",
    color: f("color").value,
    purpose: f("purpose").value.trim(),
    system_prompt: f("system_prompt").value,
    tools: [...form.querySelectorAll("input[data-tool]:checked")].map((x) => x.dataset.tool),
    delegate_all: f("delegate_all").checked,
    delegates: f("delegate_all").checked ? [] : [...form.querySelectorAll("input[data-delegate]:checked")].map((x) => x.dataset.delegate),
    model: f("model").value,
    temperature: temp === "" ? null : Number(temp),
    workspace: f("workspace").value.trim(),
    confirm_shell: f("confirm_shell").checked,
    memory: f("memory").checked,
  };
}

$("#agent-form").addEventListener("submit", async (e) => {
  if (e.submitter?.value !== "save") return;
  e.preventDefault();
  const body = readForm();
  try {
    const saved = editingAgentId
      ? await api("PUT", `/api/agents/${editingAgentId}`, body)
      : await api("POST", "/api/agents", body);
    $("#agent-dialog").close();
    await reloadState();
    toast(`Saved ${saved.name}`);
  } catch (err) {
    toast(err.message, "err");
  }
});

$("#agent-delete").onclick = async () => {
  const a = S.agents.find((x) => x.id === editingAgentId);
  if (!a) return;
  const ok = await confirm({ title: `Delete ${a.name}?`, text: "Existing chats with it stay readable but can't continue. Routines that use it will fail.", ok: "Delete agent", danger: true });
  if (!ok) return;
  try {
    await api("DELETE", `/api/agents/${a.id}`);
    $("#agent-dialog").close();
    await reloadState();
    toast(`Deleted ${a.name}`);
  } catch (e) {
    toast(e.message, "err");
  }
};

$("#agent-duplicate").onclick = async () => {
  const body = readForm();
  body.name = `${body.name} copy`.slice(0, 40);
  try {
    const saved = await api("POST", "/api/agents", body);
    await reloadState();
    toast(`Created ${saved.name}`);
    openAgentDialog(saved.id);
  } catch (e) {
    toast(e.message, "err");
  }
};

$("#agent-export").onclick = () => {
  const body = readForm();
  downloadBlob(`${(body.name || "agent").replace(/[^\w-]+/g, "-").toLowerCase()}.agent.json`, JSON.stringify(body, null, 2));
};

$("#agent-import").onclick = () => $("#agent-import-file").click();
$("#agent-import-file").addEventListener("change", async (e) => {
  const file = e.target.files[0];
  e.target.value = "";
  if (!file) return;
  try {
    const data = JSON.parse(await file.text());
    const src = data.agent && typeof data.agent === "object" ? data.agent : data;  // a chat export carries its agent too
    if (!src || typeof src.name !== "string") throw new Error("That file doesn't look like an exported agent.");
    const clean = {};
    for (const k of FIELDS) if (k in src) clean[k] = src[k];
    if (!Array.isArray(clean.tools)) clean.tools = [];
    clean.tools = clean.tools.filter((t) => S.tools.some((x) => x.name === t));
    clean.delegates = Array.isArray(clean.delegates) ? clean.delegates.filter((d) => S.agents.some((a) => a.id === d)) : [];
    fillForm({ ...BLANK, ...clean });
    $("#agent-dialog-title").textContent = `New agent from ${file.name}`;
    toast(`Loaded ${clean.name}. Review it, then save.`, "info");
  } catch (err) {
    toast(`Couldn't import: ${err.message}`, "err");
  }
});

$("#agent-preview").onclick = async () => {
  if (!editingAgentId) return;
  try {
    const p = await api("GET", `/api/agents/${editingAgentId}/prompt`);
    $("#prompt-dialog-sub").textContent = [
      p.tools.length ? `Tools: ${p.tools.join(", ")}.` : "No tools.",
      p.contacts.length ? `Can ask: ${p.contacts.join(", ")}.` : "Works alone.",
      "Unsaved edits aren't included; memory and the team are as they are right now.",
    ].join("\n");
    $("#prompt-preview").textContent = p.system_prompt;
    $("#prompt-copy").onclick = () => copyText(p.system_prompt);
    $("#prompt-dialog").showModal();
  } catch (e) {
    toast(e.message, "err");
  }
};
