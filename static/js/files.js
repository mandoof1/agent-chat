// The workspace drawer: what the current chat's agent can see with its file and shell tools.

import { S, prefs, agentById } from "./state.js";
import { $, el, svgIcon, api, fmtSize, timeAgo, isImagePath } from "./util.js";
import { openViewer } from "./ui.js";
import { fileUrl } from "./composer.js";

let path = ".";
let agentId = null;
let timer = null;

export function filesAgent() {
  return S.chat ? S.chat.agent_id : S.filter || S.agents[0]?.id || null;
}

export function toggleFiles(open = !S.filesOpen) {
  S.filesOpen = open;
  prefs.set("filesOpen", open ? "1" : "0");
  $("#files").hidden = !open;
  $("#hdr-files").setAttribute("aria-pressed", String(open));
  if (open) refreshFiles(true);
}
$("#hdr-files").onclick = () => toggleFiles();
$("#files-close").onclick = () => toggleFiles(false);
$("#files-refresh").onclick = () => refreshFiles();

// Called when a chat opens or a tool changed files. `reset` goes back to the folder root for a new agent.
export async function refreshFiles(reset = false) {
  if (!S.filesOpen) return;
  const a = filesAgent();
  if (!a) return;
  if (a !== agentId || reset) path = ".";  // another agent's workspace, or a fresh look: start at its root
  agentId = a;
  const list = $("#files-list");
  try {
    const data = await api("GET", `/api/workspace?${new URLSearchParams({ agent_id: agentId, path })}`);
    renderPath(data.path);
    $("#files-foot").textContent = data.workspace;
    $("#files-foot").title = data.workspace;
    list.innerHTML = "";
    if (!data.entries.length) {
      list.append(el("div", { class: "empty-note" }, el("b", {}, "Empty folder. "),
        "Files the agent writes, and files you attach, show up here."));
    }
    for (const e of data.entries) list.append(fileRow(e));
  } catch (e) {
    if (/not a folder|outside/.test(e.message) && path !== ".") { path = "."; return refreshFiles(); }
    list.innerHTML = "";
    list.append(el("div", { class: "empty-note" }, `Couldn't list the workspace: ${e.message}`));
  }
}

function renderPath(current) {
  const box = $("#files-path");
  box.innerHTML = "";
  const a = agentById(agentId);
  box.append(el("button", { type: "button", title: `${a.name}'s workspace`, onclick: () => { path = "."; refreshFiles(); } }, `${a.emoji} ${a.name}`));
  if (current === "." || !current) return;
  const parts = current.split("/");
  parts.forEach((p, i) => {
    box.append(el("span", { class: "sep" }, "/"));
    const target = parts.slice(0, i + 1).join("/");
    box.append(el("button", { type: "button", onclick: () => { path = target; refreshFiles(); } }, p));
  });
}

function fileRow(e) {
  const hidden = e.name.startsWith(".");
  const row = el("div", { class: "file-row" + (e.dir ? " dir" : "") + (hidden ? " hidden-file" : ""), title: e.path, tabindex: 0, role: "button" },
    svgIcon(e.dir ? "folder" : isImagePath(e.name) ? "image" : "file"),
    el("span", { class: "name" }, e.name),
    el("span", { class: "meta" }, e.dir ? "" : `${fmtSize(e.size)} · ${timeAgo(e.mtime)}`));
  const open = () => {
    if (e.dir) { path = e.path; refreshFiles(); return; }
    openWorkspaceFile(agentId, e.path, e);
  };
  row.onclick = open;
  row.onkeydown = (ev) => { if (ev.key === "Enter") open(); };
  return row;
}

export function openWorkspaceFile(agent, filePath, meta = {}) {
  openViewer({ title: filePath, url: fileUrl(agent, filePath), download: fileUrl(agent, filePath, true), size: meta.size, mtime: meta.mtime });
}

// Tools that may have changed the workspace: refresh soon, coalescing bursts.
export function filesChanged() {
  if (!S.filesOpen) return;
  clearTimeout(timer);
  timer = setTimeout(() => refreshFiles(), 400);
}
