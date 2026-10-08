// Settings: model server, agent behaviour, web and browser, email, calendars.

import { S } from "./state.js";
import { $, $$, el, svgIcon, api } from "./util.js";
import { toast, confirm } from "./ui.js";
import { fillModelSelect } from "./agents.js";
import { renderMode, BYPASS_WARNING } from "./composer.js";
import { pollServer } from "./main.js";

const form = $("#settings-form");
const f = (n) => form.elements.namedItem(n);

export function showTab(name) {
  for (const t of $$("#settings-tabs .tab")) t.classList.toggle("active", t.dataset.tab === name);
  for (const p of $$("#settings-form .tab-pane")) p.classList.toggle("active", p.dataset.pane === name);
}
$("#settings-tabs").addEventListener("click", (e) => {
  const tab = e.target.closest(".tab");
  if (tab) showTab(tab.dataset.tab);
});

export function openSettings(tab = "model") {
  f("base_url").value = S.settings.base_url;
  f("api_key").value = S.settings.api_key;
  f("max_steps").value = S.settings.max_steps ?? 30;
  f("shell_timeout").value = S.settings.shell_timeout ?? 180;
  f("auto_compact").checked = S.settings.auto_compact !== false;
  f("auto_memory").checked = S.settings.auto_memory !== false;
  f("auto_title").checked = S.settings.auto_title !== false;
  f("bypass_approvals").checked = !!S.settings.bypass_approvals;
  f("compact_at").value = S.settings.compact_at ?? 70;
  f("context_size").value = S.settings.context_size ?? 0;
  f("search_url").value = S.settings.search_url ?? "";
  f("browser_executable").value = S.settings.browser_executable ?? "/usr/bin/brave";
  f("browser_headless").checked = S.settings.browser_headless !== false;
  fillModelSelect(f("model"), S.settings.model, "Server default / first model");
  $("#settings-test").textContent = S.server.ok ? `Connected · ${S.server.models.length} model${S.server.models.length === 1 ? "" : "s"}${S.server.n_ctx ? ` · context ${S.server.n_ctx.toLocaleString()}` : ""}` : S.server.ok === false ? `Not reachable: ${S.server.error || ""}` : "";
  $("#email-test").textContent = "";
  $("#calendar-test").textContent = "";
  $("#settings-version").textContent = S.version ? `Agent Chat ${S.version}` : "";
  loadEmailSettings();
  loadCalendars();
  showTab(tab);
  $("#settings-dialog").showModal();
}

$("#settings-test-btn").onclick = async () => {
  const out = $("#settings-test");
  out.textContent = "Testing…";
  const qs = new URLSearchParams({ base_url: f("base_url").value.trim(), api_key: f("api_key").value });
  try {
    const info = await api("GET", `/api/server?${qs}`);
    out.textContent = info.ok
      ? `Connected (${info.kind === "llama" ? "llama-server" : "OpenAI-compatible"}). Models: ${info.models.join(", ") || "none listed"}${info.n_ctx ? ` · context ${info.n_ctx.toLocaleString()}` : ""}`
      : `Not reachable: ${info.error}`;
    if (info.ok) {
      const cur = f("model").value;
      S.server.models = info.models;
      fillModelSelect(f("model"), cur, "Server default / first model");
    }
  } catch (e) {
    out.textContent = `Not reachable: ${e.message}`;
  }
};

const EMAIL_PRESETS = {
  gmail: { imap_host: "imap.gmail.com", imap_port: 993, imap_security: "ssl", smtp_host: "smtp.gmail.com", smtp_port: 465, smtp_security: "ssl" },
  icloud: { imap_host: "imap.mail.me.com", imap_port: 993, imap_security: "ssl", smtp_host: "smtp.mail.me.com", smtp_port: 587, smtp_security: "starttls" },
  yahoo: { imap_host: "imap.mail.yahoo.com", imap_port: 993, imap_security: "ssl", smtp_host: "smtp.mail.yahoo.com", smtp_port: 465, smtp_security: "ssl" },
  fastmail: { imap_host: "imap.fastmail.com", imap_port: 993, imap_security: "ssl", smtp_host: "smtp.fastmail.com", smtp_port: 465, smtp_security: "ssl" },
};
const EMAIL_FIELDS = ["imap_host", "imap_port", "imap_security", "smtp_host", "smtp_port", "smtp_security", "username", "from_name"];

async function loadEmailSettings() {
  try {
    const cfg = await api("GET", "/api/email");
    for (const k of EMAIL_FIELDS) f("email_" + k).value = cfg[k] ?? "";
    f("email_password").value = "";
    f("email_password").placeholder = cfg.has_password ? "saved (leave empty to keep)" : "";
    $("#email-state").textContent = cfg.configured ? cfg.username : "not set up";
  } catch {}
}

async function saveEmailSettings() {
  const body = {};
  for (const k of EMAIL_FIELDS) {
    const v = f("email_" + k).value.trim();
    body[k] = k.endsWith("_port") ? Number(v) || 0 : v;
  }
  body.password = f("email_password").value;
  if (!body.from_address) body.from_address = body.username;
  const cfg = await api("PUT", "/api/email", body);
  $("#email-state").textContent = cfg.configured ? cfg.username : "not set up";
  return cfg;
}

async function loadCalendars() {
  try {
    const cals = await api("GET", "/api/calendars");
    $("#calendar-state").textContent = cals.length ? `${cals.length} connected` : "none";
    const list = $("#calendar-list");
    list.innerHTML = "";
    for (const c of cals) {
      list.append(el("div", { class: "cal-item" }, el("b", {}, c.name), el("span", { class: "muted small grow" }, c.link),
        el("button", { type: "button", class: "icon-btn danger", title: "Disconnect", onclick: async () => {
          if (!(await confirm({ title: `Disconnect ${c.name}?`, ok: "Disconnect", danger: true }))) return;
          try { await api("DELETE", `/api/calendars/${c.id}`); loadCalendars(); } catch (e) { toast(e.message, "err"); }
        } }, svgIcon("trash"))));
    }
  } catch {}
}

$("#calendar-add").onclick = async () => {
  try {
    await api("POST", "/api/calendars", { name: $("#calendar-name").value, url: $("#calendar-url").value });
    $("#calendar-name").value = "";
    $("#calendar-url").value = "";
    loadCalendars();
    toast("Calendar connected");
  } catch (e) {
    toast(e.message, "err");
  }
};
$("#calendar-url").addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); $("#calendar-add").click(); } });
$("#calendar-test-btn").onclick = async () => {
  $("#calendar-test").textContent = "Reading calendars…";
  try { $("#calendar-test").textContent = (await api("POST", "/api/calendars/test")).message; }
  catch (e) { $("#calendar-test").textContent = "Failed: " + e.message; }
};

f("bypass_approvals").addEventListener("change", async (e) => {
  if (e.target.checked && !(await confirm({ title: "Bypass permissions?", text: BYPASS_WARNING, ok: "Bypass", danger: true }))) e.target.checked = false;
});

$("#email-preset").addEventListener("change", (e) => {
  const preset = EMAIL_PRESETS[e.target.value];
  if (!preset) return;
  for (const [k, v] of Object.entries(preset)) f("email_" + k).value = v;
});
$("#email-test-btn").onclick = async () => {
  const out = $("#email-test");
  out.textContent = "Testing…";
  try {
    await saveEmailSettings();
    const r = await api("POST", "/api/email/test");
    out.textContent = (r.ok ? "" : "Failed: ") + r.message;
  } catch (e) {
    out.textContent = "Failed: " + e.message;
  }
};

const wholeOrKeep = (v) => (String(v).trim() === "" ? undefined : Math.max(0, Math.floor(Number(v)) || 0));

form.addEventListener("submit", async (e) => {
  if (e.submitter?.value !== "save") return;
  e.preventDefault();
  try {
    if (f("email_username").value.trim() || f("email_password").value) await saveEmailSettings();
    S.settings = await api("PUT", "/api/settings", {
      base_url: f("base_url").value.trim(),
      api_key: f("api_key").value,
      model: f("model").value,
      max_steps: wholeOrKeep(f("max_steps").value),        // empty = leave as is; 0 = no limit
      shell_timeout: wholeOrKeep(f("shell_timeout").value),
      auto_compact: f("auto_compact").checked,
      auto_memory: f("auto_memory").checked,
      auto_title: f("auto_title").checked,
      bypass_approvals: f("bypass_approvals").checked,
      compact_at: Math.min(95, Math.max(30, Number(f("compact_at").value) || 70)),
      context_size: Math.max(0, Number(f("context_size").value) || 0),
      search_url: f("search_url").value.trim(),
      browser_executable: f("browser_executable").value.trim() || "/usr/bin/brave",
      browser_headless: f("browser_headless").checked,
    });
    $("#settings-dialog").close();
    renderMode();
    pollServer();
    toast("Settings saved");
  } catch (err) {
    toast(err.message, "err");
  }
});
