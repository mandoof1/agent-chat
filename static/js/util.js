// DOM, formatting and API helpers shared by every module.

export const $ = (sel, root = document) => root.querySelector(sel);
export const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

export function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k === "class") node.className = v;
    else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
    else node.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children.flat()) {
    if (c != null && c !== false) node.append(c instanceof Node ? c : String(c));
  }
  return node;
}

// Drawn icons from the sprite in index.html (agents keep their own emoji).
export function svgIcon(name, cls = "ic") {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", cls);
  svg.setAttribute("aria-hidden", "true");
  const use = document.createElementNS("http://www.w3.org/2000/svg", "use");
  use.setAttribute("href", `#i-${name}`);
  svg.append(use);
  return svg;
}

export function colored(node, color) {
  node.style.setProperty("--c", color || "#7c6cff");
  return node;
}

// An agent's mark: a rounded tile in its color with its own icon.
export function avatar(agent, size = "") {
  return colored(el("div", { class: "mark" + (size ? ` ${size}` : "") }, agent.emoji || "🤖"), agent.color);
}

export function iconBtn(icon, title, onclick, cls = "") {
  return el("button", { class: `icon-btn ${cls}`.trim(), type: "button", title, "aria-label": title, onclick }, svgIcon(icon));
}

export async function api(method, url, body) {
  const res = await fetch(url, {
    method,
    headers: body ? { "Content-Type": "application/json", "X-Agent-Chat": "1" } : { "X-Agent-Chat": "1" },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) {
    let msg = `${res.status} ${res.statusText}`;
    try {
      const j = await res.json();
      msg = typeof j.detail === "string" ? j.detail : JSON.stringify(j.detail ?? j);
    } catch {}
    throw new Error(msg);
  }
  return res.json();
}

export const fmtK = (n) => (n >= 1000 ? (n / 1000).toFixed(1).replace(/\.0$/, "") + "k" : String(n));
export const fmtSize = (n) => (n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(1)} KB` : `${(n / 1048576).toFixed(1)} MB`);
export const oneLine = (s, n = 140) => String(s ?? "").replace(/\s+/g, " ").trim().slice(0, n);
export const pkey = (path) => (path || []).join("/");

export function timeAgo(ts) {
  if (!ts) return "";
  const s = Date.now() / 1000 - ts;
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  if (s < 7 * 86400) return `${Math.floor(s / 86400)} d ago`;
  return new Date(ts * 1000).toLocaleDateString();
}

export function clock(ts) {
  if (!ts) return "";
  const d = new Date(ts * 1000);
  const today = new Date();
  const time = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  if (d.toDateString() === today.toDateString()) return time;
  return `${d.toLocaleDateString([], { month: "short", day: "numeric" })}, ${time}`;
}

// How long something has been going: 45s, 12m, 3h 12m, 2d 4h (app/main.py's _span says it the same way).
export function fmtSpan(s) {
  s = Math.max(0, Math.floor(s));
  if (s < 3600) return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m`;
  return s < 86400 ? `${Math.floor(s / 3600)}h ${Math.floor(s / 60) % 60}m` : `${Math.floor(s / 86400)}d ${Math.floor(s / 3600) % 24}h`;
}

// Which list section a chat belongs in, by when it was last touched.
export function dayGroup(ts) {
  if (!ts) return "Older";
  const d = new Date(ts * 1000), now = new Date();
  const start = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((start(now) - start(d)) / 86400000);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 7) return "This week";
  if (days < 30) return "This month";
  return "Older";
}

export async function copyText(text, btn) {
  try {
    await navigator.clipboard.writeText(text);
    if (btn) {
      const old = btn.textContent;
      btn.textContent = "Copied";
      setTimeout(() => (btn.textContent = old), 1200);
      return true;
    }
    const { toast } = await import("./ui.js");
    toast("Copied");
    return true;
  } catch {
    const { toast } = await import("./ui.js");
    toast("Copy failed", "err");
    return false;
  }
}

export function downloadBlob(name, text, type = "application/json") {
  const url = URL.createObjectURL(new Blob([text], { type }));
  const a = el("a", { href: url, download: name });
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

// Text with the query highlighted (safe: builds nodes, not HTML).
export function highlight(text, q) {
  text = String(text ?? "");
  if (!q) return [text];
  const parts = text.split(new RegExp(`(${escapeRe(q)})`, "ig"));
  return parts.map((p, i) => (i % 2 ? el("mark", {}, p) : p));
}

// ---------------------------------------------------------------- markdown + code

marked.use({ gfm: true });
hljs.configure({ ignoreUnescapedHTML: true });

export function renderMd(node, text, final) {
  node.innerHTML = DOMPurify.sanitize(marked.parse(text || ""));
  for (const a of node.querySelectorAll("a")) {
    a.target = "_blank";
    a.rel = "noopener noreferrer";
  }
  if (!final) return;
  for (const code of node.querySelectorAll("pre code")) {
    try { hljs.highlightElement(code); } catch {}
    const btn = el("button", { class: "copy-code", type: "button" }, "Copy");
    btn.onclick = () => copyText(code.innerText, btn);
    code.parentElement.append(btn);
  }
}

const LANG_BY_EXT = {
  py: "python", js: "javascript", mjs: "javascript", cjs: "javascript", jsx: "javascript", ts: "typescript", tsx: "typescript",
  html: "xml", htm: "xml", xml: "xml", svg: "xml", vue: "xml", css: "css", scss: "scss", json: "json", sh: "bash", bash: "bash",
  zsh: "bash", fish: "bash", md: "markdown", rs: "rust", go: "go", c: "c", h: "c", cpp: "cpp", cc: "cpp", hpp: "cpp",
  java: "java", kt: "kotlin", rb: "ruby", php: "php", sql: "sql", yaml: "yaml", yml: "yaml", toml: "ini", ini: "ini",
  lua: "lua", swift: "swift", cs: "csharp", dockerfile: "dockerfile", makefile: "makefile", txt: "plaintext", log: "plaintext",
};

export function langFor(path) {
  const base = String(path || "").split("/").pop().toLowerCase();
  return LANG_BY_EXT[base] || LANG_BY_EXT[base.split(".").pop()] || null;
}

export const isImagePath = (path) => /\.(png|jpe?g|gif|webp|bmp)$/i.test(String(path || ""));

// Fill a <code> element, highlighted when the language is known (hljs escapes the text).
export function setCode(code, text, lang) {
  if (lang && lang !== "plaintext" && hljs.getLanguage(lang) && text.length < 200_000) {
    code.innerHTML = hljs.highlight(text, { language: lang, ignoreIllegals: true }).value;
    code.className = `hljs language-${lang}`;
  } else {
    code.textContent = text;
  }
}

export function codeBlock(text, lang) {
  const code = el("code");
  setCode(code, text ?? "", lang);
  return el("pre", { class: "code" }, code);
}

// Read the fields of a tool call's JSON arguments while they are still being written
// (unterminated strings and objects are fine). Returns what is known so far.
export function partialArgs(raw) {
  const out = {};
  let i = raw.indexOf("{");
  if (i < 0) return out;
  i++;
  const ws = () => { while (i < raw.length && /[\s,]/.test(raw[i])) i++; };
  const str = () => {  // at an opening quote; returns [value, complete]
    let v = "";
    i++;
    while (i < raw.length) {
      const ch = raw[i];
      if (ch === '"') { i++; return [v, true]; }
      if (ch === "\\") {
        const n = raw[i + 1];
        if (n === undefined) return [v, false];
        if (n === "u") {
          const hex = raw.slice(i + 2, i + 6);
          if (hex.length < 4) return [v, false];
          v += String.fromCharCode(parseInt(hex, 16));
          i += 6;
          continue;
        }
        v += { n: "\n", t: "\t", r: "\r", b: "\b", f: "\f" }[n] ?? n;
        i += 2;
        continue;
      }
      v += ch;
      i++;
    }
    return [v, false];
  };
  while (i < raw.length) {
    ws();
    if (raw[i] !== '"') break;
    const [key, keyDone] = str();
    if (!keyDone) break;
    ws();
    if (raw[i] !== ":") break;
    i++;
    ws();
    if (raw[i] === '"') {
      const [val, done] = str();
      out[key] = val;
      if (!done) break;
    } else {
      const m = /^[^,}]*/.exec(raw.slice(i));
      const token = m[0].trim();
      i += m[0].length;
      if (token) { try { out[key] = JSON.parse(token); } catch { out[key] = token; } }
    }
  }
  return out;
}

export const isTyping = () =>
  /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName) || !!document.activeElement?.isContentEditable;
