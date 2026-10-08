// Reusable UI pieces: toasts, confirm/prompt dialogs, context menus, the file viewer.

import { $, el, svgIcon, setCode, langFor, isImagePath, fmtSize, copyText } from "./util.js";

// ------------------------------------------------------------------- toasts

export function toast(msg, kind = "ok", { action, timeout } = {}) {
  const box = $("#toasts");
  const node = el("div", { class: `toast ${kind === "err" ? "err" : kind === "info" ? "info" : ""}`.trim() }, el("span", {}, msg));
  if (action) {
    node.append(el("button", { type: "button", class: "btn sm", onclick: () => { action.onClick(); node.remove(); } }, action.label));
  }
  box.append(node);
  while (box.children.length > 4) box.firstChild.remove();
  setTimeout(() => node.remove(), timeout ?? (kind === "err" ? 7000 : action ? 6000 : 2600));
  return node;
}

// ------------------------------------------------------------ confirm/prompt

let confirmResolve = null;
const confirmDialog = $("#confirm-dialog");
confirmDialog.addEventListener("close", () => {
  const ok = confirmDialog.returnValue === "ok";
  const resolve = confirmResolve;
  confirmResolve = null;
  if (!resolve) return;
  const input = $("#confirm-input");
  resolve(input.hidden ? ok : ok ? input.value : null);
});

function openConfirm({ title, text, ok = "OK", cancel = "Cancel", danger = false, value = null, placeholder = "" }) {
  $("#confirm-title").textContent = title;
  $("#confirm-text").textContent = text || "";
  $("#confirm-text").hidden = !text;
  const okBtn = $("#confirm-ok");
  okBtn.textContent = ok;
  okBtn.className = "btn " + (danger ? "danger" : "primary");
  $("#confirm-cancel").textContent = cancel;
  const input = $("#confirm-input");
  input.hidden = value == null;
  input.value = value ?? "";
  input.placeholder = placeholder;
  return new Promise((resolve) => {
    confirmResolve = resolve;
    confirmDialog.returnValue = "cancel";
    confirmDialog.showModal();
    if (value != null) { input.focus(); input.select(); } else okBtn.focus();
  });
}

export const confirm = (opts) => openConfirm(opts);
export const prompt = (opts) => openConfirm({ ...opts, value: opts.value ?? "" });
$("#confirm-input").addEventListener("keydown", (e) => {
  if (e.key === "Enter") { e.preventDefault(); $("#confirm-ok").click(); }
});

// ------------------------------------------------------------- context menu

const menu = $("#context-menu");
let menuCleanup = null;

export function closeMenu() {
  if (menu.hidden) return;
  menu.hidden = true;
  menu.innerHTML = "";
  menuCleanup?.();
  menuCleanup = null;
}

// items: [{label, icon, onClick, danger}] or "-" for a separator. `at` is {x, y} or an element.
export function contextMenu(items, at, { onClose } = {}) {
  closeMenu();
  for (const item of items) {
    if (item === "-") { menu.append(el("hr")); continue; }
    if (!item) continue;
    const b = el("button", { type: "button", class: item.danger ? "danger" : "", role: "menuitem",
      onclick: () => { closeMenu(); item.onClick(); } }, item.icon ? svgIcon(item.icon) : null, item.label);
    menu.append(b);
  }
  menu.hidden = false;
  let x, y;
  if (at instanceof Element) {
    const r = at.getBoundingClientRect();
    x = r.right - menu.offsetWidth; y = r.bottom + 4;
    if (x < 8) x = r.left;
  } else { x = at.x; y = at.y; }
  x = Math.min(x, innerWidth - menu.offsetWidth - 8);
  y = Math.min(y, innerHeight - menu.offsetHeight - 8);
  menu.style.left = `${Math.max(8, x)}px`;
  menu.style.top = `${Math.max(8, y)}px`;
  menu.querySelector("button")?.focus();
  const onDoc = (e) => { if (!menu.contains(e.target)) closeMenu(); };
  const onKey = (e) => {
    if (e.key === "Escape") { closeMenu(); return; }
    const buttons = [...menu.querySelectorAll("button")];
    const i = buttons.indexOf(document.activeElement);
    if (e.key === "ArrowDown") { e.preventDefault(); buttons[(i + 1) % buttons.length]?.focus(); }
    if (e.key === "ArrowUp") { e.preventDefault(); buttons[(i - 1 + buttons.length) % buttons.length]?.focus(); }
  };
  setTimeout(() => {
    document.addEventListener("pointerdown", onDoc, true);
    document.addEventListener("keydown", onKey, true);
    window.addEventListener("resize", closeMenu, { once: true });
  });
  menuCleanup = () => {
    document.removeEventListener("pointerdown", onDoc, true);
    document.removeEventListener("keydown", onKey, true);
    onClose?.();
  };
}

// --------------------------------------------------------------- file viewer

const viewer = $("#viewer-dialog");
let viewerText = "";
$("#viewer-close").onclick = () => viewer.close();
$("#viewer-copy").onclick = () => viewerText && copyText(viewerText);
viewer.addEventListener("click", (e) => { if (e.target === viewer) viewer.close(); });

// Show a workspace file (or any url) in the viewer: image, text (highlighted) or a download note.
export async function openViewer({ title, url, download, size, mtime }) {
  $("#viewer-title").textContent = title;
  $("#viewer-meta").textContent = [size != null && fmtSize(size), mtime && new Date(mtime * 1000).toLocaleString()].filter(Boolean).join(" · ");
  const dl = $("#viewer-download");
  dl.href = download || url;
  dl.hidden = !(download || url);
  const body = $("#viewer-body");
  body.innerHTML = "";
  viewerText = "";
  $("#viewer-copy").hidden = true;
  if (!viewer.open) viewer.showModal();
  if (isImagePath(title)) {
    body.append(el("img", { src: url, alt: title }));
    return;
  }
  if (size != null && size > 3_000_000) {
    body.append(el("div", { class: "viewer-note" }, `This file is ${fmtSize(size)}. Download it to open it.`,
      el("a", { class: "btn primary sm", href: download || url, download: "" }, "Download")));
    return;
  }
  body.append(el("div", { class: "viewer-note" }, "Loading…"));
  try {
    const res = await fetch(url, { headers: { "X-Agent-Chat": "1" } });
    if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
    const type = res.headers.get("content-type") || "";
    const buf = await res.arrayBuffer();
    const bytes = new Uint8Array(buf);
    const binary = bytes.subarray(0, 4096).some((b) => b === 0);
    if (binary && !type.startsWith("text/")) {
      body.innerHTML = "";
      body.append(el("div", { class: "viewer-note" }, `This is a binary file (${type || "unknown type"}).`,
        el("a", { class: "btn primary sm", href: download || url, download: "" }, "Download")));
      return;
    }
    viewerText = new TextDecoder().decode(buf);
    const code = el("code");
    setCode(code, viewerText, langFor(title));
    body.innerHTML = "";
    body.append(el("pre", {}, code));
    $("#viewer-copy").hidden = false;
  } catch (e) {
    body.innerHTML = "";
    body.append(el("div", { class: "viewer-note" }, `Couldn't load the file: ${e.message}`));
  }
}

// A lightbox for an image already on the page.
export function openImage(src, title = "image") {
  $("#viewer-title").textContent = title;
  $("#viewer-meta").textContent = "";
  $("#viewer-download").href = src;
  $("#viewer-download").hidden = false;
  $("#viewer-copy").hidden = true;
  const body = $("#viewer-body");
  body.innerHTML = "";
  body.append(el("img", { src, alt: title }));
  viewerText = "";
  if (!viewer.open) viewer.showModal();
}
