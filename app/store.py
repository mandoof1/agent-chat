"""JSON-file storage for agents, chats and settings.

Everything lives under data/ so it is easy to back up or hand-edit:
  data/settings.json
  data/agents/<id>.json
  data/chats/<id>.json
"""

import copy
import json
import os
import re
import time
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DATA = Path(os.environ.get("AGENT_CHAT_DATA", ROOT / "data")).expanduser()
AGENTS = DATA / "agents"
CHATS = DATA / "chats"
SETTINGS = DATA / "settings.json"
SECRETS = DATA / "secrets.json"   # email password, private calendar links: owner-only file
DEFAULT_WORKSPACE = Path(os.environ.get("AGENT_CHAT_WORKSPACE", ROOT / "workspace")).expanduser()

DEFAULT_SETTINGS = {
    "base_url": "http://127.0.0.1:8080/v1",
    "api_key": "llama",
    "model": "",
    "max_steps": 30,       # rounds of tool use per message; 0 = no limit
    "shell_timeout": 180,  # seconds a shell command may run; 0 = no limit
    "auto_compact": True,
    "compact_at": 70,      # % of the context window at which older messages get summarized
    "context_size": 0,     # 0 = ask the server (llama-server reports it); set it for other servers
    "auto_memory": True,   # after each reply, let the model save new facts about the user
    "auto_title": True,    # after the first reply, let the model name the chat
    "bypass_approvals": False,  # run shell commands and send email without asking
    "search_url": "",      # SearXNG base URL for web_search (JSON API); empty = scrape DuckDuckGo
    "browser_headless": True,              # Browser agent: run Brave headless (True) or visible (False)
    "browser_executable": "/usr/bin/brave",  # path to the Brave (or other Chromium) binary to drive
}

AGENT_FIELDS = {
    "name": "Agent",
    "emoji": "🤖",
    "color": "#7c6cff",
    "purpose": "",
    "system_prompt": "",
    "tools": ["ask_agent"],
    "delegate_all": True,  # may contact every other agent, including ones added later
    "delegates": [],       # used only when delegate_all is off
    "model": "",
    "temperature": None,
    "workspace": "",
    "confirm_shell": True,
    "memory": True,        # sees the user profile + relevant memories, and can remember/forget
    "order": None,         # position in the rail (None = after the starter agents, by name)
}
STARTER_ORDER = ["assistant", "mail", "planner", "orchestrator", "coder", "researcher", "browser", "writer", "reviewer"]


def new_id() -> str:
    return uuid.uuid4().hex[:12]


def _safe_id(id_: str) -> str:
    if not re.fullmatch(r"[A-Za-z0-9_-]{1,64}", id_ or ""):
        raise KeyError(id_)
    return id_


def _read(path: Path, default=None):
    try:
        return json.loads(path.read_text())
    except FileNotFoundError:
        return default


def _write(path: Path, obj) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(obj, indent=2, ensure_ascii=False))
    os.replace(tmp, path)


# ----------------------------------------------------------------- secrets

def read_secrets() -> dict:
    return _read(SECRETS, {}) or {}


def write_secrets(data: dict) -> None:
    SECRETS.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(SECRETS, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        json.dump(data, f, indent=2)
    os.chmod(SECRETS, 0o600)


# ---------------------------------------------------------------- settings

def get_settings() -> dict:
    return {**DEFAULT_SETTINGS, **(_read(SETTINGS, {}) or {})}


def save_settings(values: dict) -> dict:
    settings = get_settings()
    settings.update({k: v for k, v in values.items() if k in DEFAULT_SETTINGS})
    _write(SETTINGS, settings)
    return settings


# ------------------------------------------------------------------ agents

def normalize_agent(data: dict) -> dict:
    agent = {k: data.get(k, default) for k, default in AGENT_FIELDS.items()}
    agent["id"] = data.get("id") or new_id()
    agent["name"] = (agent["name"] or "Agent").strip()
    agent["tools"] = [t for t in agent["tools"] if isinstance(t, str)]
    agent["delegates"] = [d for d in agent["delegates"] if isinstance(d, str) and d != agent["id"]]
    agent["order"] = int(agent["order"]) if isinstance(agent["order"], (int, float)) else None
    return agent


def list_agents() -> list[dict]:
    agents = [_read(p) for p in sorted(AGENTS.glob("*.json"))]
    agents = [normalize_agent(a) for a in agents if a]
    default = {a: i for i, a in enumerate(STARTER_ORDER)}
    # agents the user has arranged by hand come first, in that order; the rest keep the starter order
    return sorted(agents, key=lambda a: (a["order"] is None, a["order"] or 0, default.get(a["id"], 99), a["name"].lower()))


def reorder_agents(ids: list[str]) -> list[dict]:
    """Give every agent a position: the listed ids in that order, then the rest as they were."""
    current = list_agents()
    rank = {aid: i for i, aid in enumerate(ids)}
    rest = [a["id"] for a in current if a["id"] not in rank]
    for i, aid in enumerate(list(rank) + rest):
        agent = get_agent(aid)
        if agent and agent["order"] != i:
            save_agent(agent | {"order": i})
    return list_agents()


def get_agent(agent_id: str) -> dict | None:
    try:
        data = _read(AGENTS / f"{_safe_id(agent_id)}.json")
    except KeyError:
        return None
    return normalize_agent(data) if data else None


def save_agent(data: dict) -> dict:
    agent = normalize_agent(data)
    _write(AGENTS / f"{_safe_id(agent['id'])}.json", agent)
    return agent


def delete_agent(agent_id: str) -> None:
    (AGENTS / f"{_safe_id(agent_id)}.json").unlink(missing_ok=True)
    for other in list_agents():
        if agent_id in other["delegates"]:
            other["delegates"].remove(agent_id)
            save_agent(other)


# ------------------------------------------------------------------- chats

def create_chat(agent_id: str) -> dict:
    now = time.time()
    chat = {"id": new_id(), "title": "New chat", "agent_id": agent_id,
            "created": now, "updated": now, "messages": [], "stats": {}}
    _write(CHATS / f"{chat['id']}.json", chat)
    return chat


def get_chat(chat_id: str) -> dict | None:
    try:
        return _read(CHATS / f"{_safe_id(chat_id)}.json")
    except KeyError:
        return None


def save_chat(chat: dict) -> None:
    chat["updated"] = time.time()
    _write(CHATS / f"{_safe_id(chat['id'])}.json", chat)


def delete_chat(chat_id: str) -> None:
    (CHATS / f"{_safe_id(chat_id)}.json").unlink(missing_ok=True)


def chat_summary(chat: dict) -> dict:
    preview = ""
    for m in reversed(chat.get("messages", [])):
        if m.get("role") in ("user", "assistant") and m.get("content"):
            preview = m["content"][:120]
            break
    msgs = chat.get("messages", [])
    return ({k: chat.get(k) for k in ("id", "title", "agent_id", "created", "updated", "parent", "routine_id")}
            | {"preview": preview, "pinned": bool(chat.get("pinned")), "messages": len(msgs),
               "last_role": msgs[-1]["role"] if msgs else None,
               "last_ts": msgs[-1].get("_ts") if msgs else None})


_summary_cache: dict[str, tuple[int, float, dict]] = {}  # path -> (size, mtime, summary)


def list_chats() -> list[dict]:
    """Summaries of every chat, newest first. Reads only the files that changed since last time."""
    chats, seen = [], set()
    for p in CHATS.glob("*.json"):
        try:
            st = p.stat()
        except OSError:
            continue
        key = str(p)
        seen.add(key)
        hit = _summary_cache.get(key)
        if hit and hit[0] == st.st_size and hit[1] == st.st_mtime:
            chats.append(hit[2])
            continue
        chat = _read(p)
        if chat:
            summary = chat_summary(chat)
            _summary_cache[key] = (st.st_size, st.st_mtime, summary)
            chats.append(summary)
    for key in list(_summary_cache):
        if key not in seen:
            del _summary_cache[key]
    return sorted(chats, key=lambda c: (c["pinned"], c["updated"] or 0), reverse=True)


def fork_chat(chat: dict, upto: int) -> dict:
    """A new chat with the first `upto` messages of `chat` (agent-to-agent threads are not copied)."""
    now = time.time()
    fork = copy.deepcopy(chat)
    fork.update({"id": new_id(), "created": now, "updated": now, "messages": fork["messages"][:upto],
                 "title": f"{chat['title']} (branch)"[:120], "pinned": False})
    for key in ("parent", "routine_id", "subchats", "threads", "memory_upto", "auto_title"):
        fork.pop(key, None)
    comps = fork.get("compactions") or []
    while comps and comps[-1]["upto"] > upto:
        comps.pop()
    for m in fork["messages"]:
        m.pop("_sub", None)
    _write(CHATS / f"{fork['id']}.json", fork)
    return fork


def search_chats(query: str, limit: int = 40) -> list[dict]:
    """Case-insensitive search through every chat's messages. Returns chat ids with a snippet."""
    q = query.strip().lower()
    if len(q) < 2:
        return []
    hits = []
    for p in CHATS.glob("*.json"):
        chat = _read(p)
        if not chat:
            continue
        count, snippet = 0, ""
        for m in chat.get("messages", []):
            text = m.get("content") or ""
            if not isinstance(text, str) or m.get("role") == "tool":
                continue
            pos = text.lower().find(q)
            if pos < 0:
                continue
            count += text.lower().count(q)
            if not snippet:
                start = max(0, pos - 40)
                snippet = ("…" if start else "") + " ".join(text[start:pos + len(q) + 60].split())
        if count:
            hits.append({"chat_id": chat["id"], "count": count, "snippet": snippet, "updated": chat.get("updated", 0)})
    hits.sort(key=lambda h: (h["count"], h["updated"]), reverse=True)
    return hits[:limit]


def seed_defaults() -> None:
    """Install starter agents: all of them on first run, and later only ones added in newer
    versions. Agents the user deleted are not brought back (settings["seeded_agents"])."""
    from .defaults import DEFAULT_AGENTS
    raw = _read(SETTINGS, {}) or {}
    existing = {p.stem for p in AGENTS.glob("*.json")} if AGENTS.exists() else set()
    seeded = raw.get("seeded_agents")
    if seeded is None:  # first run, or an install from before this list existed
        original = {"assistant", "orchestrator", "coder", "researcher", "writer", "reviewer"}
        seeded = sorted(existing | original) if existing else []
    for agent in DEFAULT_AGENTS:
        if agent["id"] not in seeded:
            if agent["id"] not in existing:
                save_agent(agent)
            seeded.append(agent["id"])
    _write(SETTINGS, raw | {"seeded_agents": seeded})
