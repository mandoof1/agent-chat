// Shared UI state and small persisted preferences.

export const S = {
  agents: [], chats: [], settings: {}, tools: [], defaultWorkspace: "", version: "",
  server: { ok: null, models: [], n_ctx: null, kind: "other" },
  chatId: null, chat: null, es: null, running: false, live: null, follow: true,
  drafts: {}, filter: "", attachments: [], statuses: {}, hits: new Map(), verbose: false,
  queue: [],          // messages sent while the agent works, not delivered to it yet (server-side, per run)
  seen: {},           // chat id -> `updated` of the chat when you last looked at it (unread dots)
  filesOpen: false,   // workspace drawer
  sideCollapsed: false,
  newSince: 0,        // messages that arrived below the fold while you were scrolled up
};

export const prefs = {
  get(key, fallback = null) {
    try { const v = localStorage.getItem(key); return v == null ? fallback : v; } catch { return fallback; }
  },
  set(key, value) {
    try { value == null ? localStorage.removeItem(key) : localStorage.setItem(key, value); } catch {}
  },
  json(key, fallback) {
    try { return JSON.parse(localStorage.getItem(key)) ?? fallback; } catch { return fallback; }
  },
};

S.filter = prefs.get("agentFilter", "");
S.verbose = prefs.get("verbose") === "1";
S.filesOpen = prefs.get("filesOpen") === "1";
S.sideCollapsed = prefs.get("sideCollapsed") === "1";
S.seen = prefs.json("seen", null);

export const MISSING_AGENT = { id: "", name: "Deleted agent", emoji: "❔", color: "#888888", purpose: "", tools: [], delegates: [] };
export const agentById = (id) => S.agents.find((a) => a.id === id) || { ...MISSING_AGENT, id };
export const agentByName = (name) =>
  S.agents.find((a) => a.name.toLowerCase() === String(name || "").trim().toLowerCase()) ||
  { ...MISSING_AGENT, name: name || "Unknown agent" };

export const STATUS_TEXT = {
  running: "Working…", waiting: "Waiting for your approval", delegated: "Waiting on another agent",
};

export const APPROVAL_TEXT = {
  run_shell: "Let the agent run this command on your machine?",
  email_send: "Send this email?",
};
