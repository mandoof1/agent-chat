"""Long-term memory about the user, shared by all agents.

Modelled on amnis (github.com/mandoof1/amnis): facts with a category, importance (1-10) and
confidence live in SQLite, searched with FTS5 (bm25) and ranked by relevance x importance x
confidence. Near-duplicates are merged instead of stored twice.

How agents get memories:
  * profile  - the most important facts, snapshotted into a chat's system prompt when the chat
               starts (kept fixed afterwards so llama-server's prompt cache stays valid);
  * recall   - facts relevant to each new user message, attached to that message once;
  * tools    - remember / recall_memory / forget_memory, for the agent to manage memory itself;
  * extract  - after a reply, the model reviews the turn and adds/updates/deletes facts
               (runs in the background and yields to any new request).
"""

import json
import re
import sqlite3
import threading
import time

from . import store

CATEGORIES = ["preference", "personal", "project", "person", "setup", "goal", "routine", "general"]
PROFILE_MAX_ITEMS = 30
PROFILE_MAX_CHARS = 2500
STOPWORDS = set("""a an and are as at be but by for from has have i i'm im in is it its me my of on or our so that the
their them they this to was we were what when where which who why will with you your user users about can do does
just like want need please also into than then there these those how would could should""".split())

_lock = threading.Lock()
_db: sqlite3.Connection | None = None


def db() -> sqlite3.Connection:
    global _db
    if _db is None:
        store.DATA.mkdir(parents=True, exist_ok=True)
        _db = sqlite3.connect(store.DATA / "memory.db", check_same_thread=False)
        _db.row_factory = sqlite3.Row
        _db.executescript("""
            CREATE TABLE IF NOT EXISTS memories (
                id INTEGER PRIMARY KEY,
                fact TEXT NOT NULL,
                category TEXT NOT NULL DEFAULT 'general',
                importance INTEGER NOT NULL DEFAULT 5,
                confidence REAL NOT NULL DEFAULT 1.0,
                source TEXT NOT NULL DEFAULT 'user',
                pinned INTEGER NOT NULL DEFAULT 0,
                created REAL NOT NULL,
                updated REAL NOT NULL,
                last_used REAL,
                uses INTEGER NOT NULL DEFAULT 0
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts
                USING fts5(fact, content='memories', content_rowid='id', tokenize='porter unicode61');
            CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, fact) VALUES (new.id, new.fact);
            END;
            CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, fact) VALUES ('delete', old.id, old.fact);
            END;
            CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE OF fact ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, fact) VALUES ('delete', old.id, old.fact);
                INSERT INTO memories_fts(rowid, fact) VALUES (new.id, new.fact);
            END;
        """)
    return _db


def _row(r: sqlite3.Row) -> dict:
    return dict(r)


def _words(text: str) -> list[str]:
    return [w for w in re.findall(r"[a-z0-9][a-z0-9'+#.-]*", text.lower()) if len(w) > 2 and w not in STOPWORDS]


def _similarity(a: str, b: str) -> float:
    wa, wb = set(_words(a)), set(_words(b))
    return len(wa & wb) / len(wa | wb) if wa and wb else 0.0


# --------------------------------------------------------------------- CRUD

def get(memory_id: int) -> dict | None:
    r = db().execute("SELECT * FROM memories WHERE id = ?", (memory_id,)).fetchone()
    return _row(r) if r else None


def add(fact: str, category: str = "general", importance: int = 5, source: str = "user",
        confidence: float = 1.0, pinned: bool = False) -> tuple[dict, str]:
    """Store a fact, or merge it into a near-duplicate. Returns (memory, 'added'|'merged')."""
    fact = " ".join(str(fact).split())[:500]
    if not fact:
        raise ValueError("empty fact")
    category = category if category in CATEGORIES else "general"
    importance = max(1, min(10, int(importance or 5)))
    now = time.time()
    with _lock:
        dup = max(search(fact, limit=5, touch=False), key=lambda m: _similarity(fact, m["fact"]), default=None)
        if dup and _similarity(fact, dup["fact"]) >= 0.6:
            db().execute("UPDATE memories SET fact = ?, importance = MAX(importance, ?), confidence = MIN(1.0, confidence + 0.1), "
                         "updated = ? WHERE id = ?", (fact, importance, now, dup["id"]))
            db().commit()
            return get(dup["id"]), "merged"
        cur = db().execute("INSERT INTO memories (fact, category, importance, confidence, source, pinned, created, updated) "
                           "VALUES (?, ?, ?, ?, ?, ?, ?, ?)", (fact, category, importance, confidence, source, int(pinned), now, now))
        db().commit()
        return get(cur.lastrowid), "added"


def update(memory_id: int, **fields) -> dict | None:
    allowed = {k: v for k, v in fields.items() if k in ("fact", "category", "importance", "pinned", "confidence") and v is not None}
    if "fact" in allowed:
        allowed["fact"] = " ".join(str(allowed["fact"]).split())[:500]
    if "importance" in allowed:
        allowed["importance"] = max(1, min(10, int(allowed["importance"])))
    if "category" in allowed and allowed["category"] not in CATEGORIES:
        allowed["category"] = "general"
    if not allowed:
        return get(memory_id)
    sets = ", ".join(f"{k} = ?" for k in allowed)
    with _lock:
        db().execute(f"UPDATE memories SET {sets}, updated = ? WHERE id = ?", (*allowed.values(), time.time(), memory_id))
        db().commit()
    return get(memory_id)


def delete(memory_id: int) -> bool:
    with _lock:
        cur = db().execute("DELETE FROM memories WHERE id = ?", (memory_id,))
        db().commit()
    return cur.rowcount > 0


def listing(query: str = "", category: str = "") -> list[dict]:
    if query.strip():
        rows = search(query, limit=200, touch=False)
    else:
        rows = [_row(r) for r in db().execute("SELECT * FROM memories ORDER BY pinned DESC, importance DESC, updated DESC")]
    return [r for r in rows if not category or r["category"] == category]


def count() -> int:
    return db().execute("SELECT COUNT(*) FROM memories").fetchone()[0]


# ------------------------------------------------------------------ retrieval

def search(query: str, limit: int = 8, exclude: set[int] = frozenset(), touch: bool = True) -> list[dict]:
    words = list(dict.fromkeys(_words(query)))[:24]
    if not words:
        return []
    match = " OR ".join(f'"{w}"' for w in words)
    rows = db().execute(
        "SELECT m.*, bm25(memories_fts) AS rank FROM memories_fts JOIN memories m ON m.id = memories_fts.rowid "
        "WHERE memories_fts MATCH ? ORDER BY rank LIMIT 60", (match,)).fetchall()
    scored = []
    for r in rows:
        if r["id"] in exclude:
            continue
        relevance = -r["rank"]  # bm25: lower is better
        scored.append((relevance * (0.5 + r["importance"] / 10) * r["confidence"], _row(r)))
    scored.sort(key=lambda x: x[0], reverse=True)
    found = [m for _, m in scored[:limit]]
    for m in found:
        m.pop("rank", None)
    if touch and found:
        _touch([m["id"] for m in found])
    return found


def _touch(ids: list[int]) -> None:
    with _lock:
        db().executemany("UPDATE memories SET uses = uses + 1, last_used = ? WHERE id = ?", [(time.time(), i) for i in ids])
        db().commit()


def profile() -> dict:
    """Snapshot of the most important facts, for a chat's system prompt."""
    rows = db().execute("SELECT * FROM memories ORDER BY pinned DESC, importance DESC, confidence DESC, updated DESC "
                        "LIMIT ?", (PROFILE_MAX_ITEMS * 2,)).fetchall()
    lines, ids, size = [], [], 0
    for r in rows:
        if not r["pinned"] and r["importance"] < 4:
            continue
        line = f"- {r['fact']}"
        if size + len(line) > PROFILE_MAX_CHARS or len(lines) >= PROFILE_MAX_ITEMS:
            break
        lines.append(line)
        ids.append(r["id"])
        size += len(line)
    return {"text": "\n".join(lines), "ids": ids}


def recall_block(text: str, exclude: list[int] | None = None, limit: int = 6) -> str:
    """Memories relevant to a new message, formatted to attach to it ('' if none)."""
    found = search(text, limit=limit, exclude=set(exclude or []))
    return "\n".join(f"- [{m['id']}] {m['fact']}" for m in found)


def prune() -> int:
    """Forget weak automatic memories nobody has used for 60 days (manual ones are never pruned)."""
    cutoff = time.time() - 60 * 86400
    with _lock:
        cur = db().execute("DELETE FROM memories WHERE source LIKE 'auto%' AND pinned = 0 AND importance < 3 "
                           "AND COALESCE(last_used, updated) < ?", (cutoff,))
        db().commit()
    return cur.rowcount


# ---------------------------------------------------------------- agent tools

SCHEMAS = {
    "remember": {"type": "function", "function": {
        "name": "remember",
        "description": "Save a durable fact about the user to long-term memory (preferences, personal details they "
                       "shared, projects, setup, people, goals, routines). Write it as a short third-person statement, "
                       "e.g. 'The user prefers Python over JavaScript'.",
        "parameters": {"type": "object", "properties": {
            "fact": {"type": "string"},
            "category": {"type": "string", "enum": CATEGORIES},
            "importance": {"type": "integer", "description": "1 (trivia) to 10 (core identity or strong preference)"},
        }, "required": ["fact"]}}},
    "recall_memory": {"type": "function", "function": {
        "name": "recall_memory",
        "description": "Search long-term memory about the user. Use it when personal context would help and it isn't "
                       "already in your instructions.",
        "parameters": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}}},
    "forget_memory": {"type": "function", "function": {
        "name": "forget_memory",
        "description": "Delete a memory by its id (shown in [brackets]) when it is wrong or out of date. To change a "
                       "fact, forget the old one and remember the new one.",
        "parameters": {"type": "object", "properties": {"id": {"type": "integer"}}, "required": ["id"]}}},
}


def call(name: str, args: dict, source: str) -> str:
    if name == "remember":
        try:
            m, action = add(args.get("fact", ""), args.get("category") or "general", args.get("importance") or 5, source=source)
        except ValueError as e:
            return f"Error: {e}"
        return f"{'Saved' if action == 'added' else 'Updated existing'} memory [{m['id']}]: {m['fact']}"
    if name == "recall_memory":
        found = search(str(args.get("query", "")), limit=10)
        return "\n".join(f"[{m['id']}] ({m['category']}, importance {m['importance']}) {m['fact']}" for m in found) \
            or "No matching memories."
    if name == "forget_memory":
        try:
            mid = int(args.get("id"))
        except (TypeError, ValueError):
            return "Error: 'id' must be a memory id number."
        m = get(mid)
        return f"Forgot [{mid}]: {m['fact']}" if m and delete(mid) else f"Error: no memory with id {mid}."
    return f"Error: unknown memory tool {name}"


# ---------------------------------------------------------------- extraction

EXTRACT_PROMPT = """You maintain long-term memory about the user of an AI assistant app.

From the conversation excerpt, find durable facts about the USER that will still be useful in future conversations: preferences and dislikes, personal details they chose to share, their projects, hardware and software setup, goals, routines, and people or relationships they mention, plus anything they explicitly ask to be remembered.

Do NOT store: details of the one-off task itself, facts about the assistant, guesses, or passwords and keys that only appeared in passing (store them when the user asks you to remember one).

Compare with the existing memories listed. Don't repeat them. If the user corrected or changed something, update that memory (or delete it if it is no longer true).

Reply with JSON only, no other text:
{"add": [{"fact": "The user ...", "category": "preference|personal|project|person|setup|goal|routine|general", "importance": 1-10}], "update": [{"id": 12, "fact": "..."}], "delete": [34]}
Write each fact as one short third-person statement. Use importance 8-10 only for core facts (name, job, strong preferences). If nothing is worth remembering, reply {"add": [], "update": [], "delete": []}."""


def extraction_request(turn_text: str) -> list[dict]:
    related = search(turn_text, limit=12, touch=False)
    existing = "\n".join(f"[{m['id']}] {m['fact']}" for m in related) or "(none yet)"
    return [{"role": "system", "content": EXTRACT_PROMPT},
            {"role": "user", "content": f"Existing related memories:\n{existing}\n\nConversation excerpt:\n{turn_text}"}]


TIDY_PROMPT = """You maintain long-term memory about the user of an AI assistant app. Below is everything currently remembered, one fact per line with its id, category, importance and age.

Tidy it up:
- merge facts that say the same thing (update one to the best combined wording, delete the others);
- where facts contradict each other, keep the newer one and delete the older;
- delete trivia that will never matter again;
- fix facts that are vague or badly worded.
Do not invent new facts. Facts marked (yours) were typed in by the user: don't delete or reword those.

Reply with JSON only: {"update": [{"id": 12, "fact": "..."}], "delete": [34], "add": []}
If everything is fine, reply {"update": [], "delete": [], "add": []}."""


def tidy_request() -> list[dict] | None:
    rows = db().execute("SELECT * FROM memories ORDER BY category, updated").fetchall()
    if len(rows) < 2:
        return None
    now = time.time()
    lines = [f"[{r['id']}] ({r['category']}, importance {r['importance']}, {int((now - r['updated']) / 86400)} days old"
             f"{', yours' if r['source'] == 'user' else ''}) {r['fact']}" for r in rows]
    return [{"role": "system", "content": TIDY_PROMPT}, {"role": "user", "content": "\n".join(lines)}]


def apply_extraction(reply: str, source: str) -> dict:
    """Apply the extractor's JSON. Returns what changed (for the UI)."""
    match = re.search(r"\{.*\}", reply, re.S)
    changes = {"added": [], "updated": [], "deleted": []}
    if not match:
        return changes
    try:
        data = json.loads(match.group(0))
    except json.JSONDecodeError:
        return changes
    for item in data.get("add") or []:
        if isinstance(item, dict) and item.get("fact"):
            try:
                m, action = add(item["fact"], item.get("category") or "general", item.get("importance") or 5,
                                source=source, confidence=0.8)
                changes["added" if action == "added" else "updated"].append(m["fact"])
            except ValueError:
                pass
    for item in data.get("update") or []:
        current = get(int(item.get("id") or 0)) if isinstance(item, dict) and str(item.get("id", "")).isdigit() else None
        if current and current["source"] == "user" and source.startswith("tidy"):
            continue  # tidying never rewrites what the user typed in
        if current and item.get("fact"):
            try:
                m = update(int(item["id"]), fact=item["fact"])
                changes["updated"].append(m["fact"])
            except (ValueError, TypeError):
                pass
    for mid in data.get("delete") or []:
        try:
            m = get(int(mid))
        except (TypeError, ValueError):
            continue
        if m and m["source"] != "user" and not m["pinned"] and delete(m["id"]):  # never things you typed in yourself
            changes["deleted"].append(m["fact"])
    return changes
