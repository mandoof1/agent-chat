"""HTTP API + static UI. Run with: uv run uvicorn app.main:app --port 8765"""

import asyncio
import json
import os
import time
from contextlib import asynccontextmanager, suppress
from datetime import datetime
from pathlib import Path

from fastapi import FastAPI, HTTPException, Request
from fastapi.middleware.trustedhost import TrustedHostMiddleware
from fastapi.responses import FileResponse, JSONResponse, PlainTextResponse, StreamingResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

from . import agenda, browser, llm, mail, memory, routines, runner, store, tools

STATIC = Path(__file__).resolve().parent.parent / "static"


@asynccontextmanager
async def lifespan(app: FastAPI):
    store.seed_defaults()
    memory.prune()
    scheduler = asyncio.create_task(routines.loop(run_routine))
    yield
    scheduler.cancel()
    for run in list(runner.runs.values()):
        run.task.cancel()
    await browser.shutdown()
    await runner.client.aclose()


app = FastAPI(title="Agent Chat", lifespan=lifespan)
# Agents can run shell commands, so refuse requests addressed to any other host name
# (stops DNS-rebinding tricks from web pages). Add hosts with AGENT_CHAT_HOSTS=a,b to use it over a LAN.
app.add_middleware(TrustedHostMiddleware, allowed_hosts=["127.0.0.1", "localhost", "::1"] + [
    h.strip() for h in os.environ.get("AGENT_CHAT_HOSTS", "").split(",") if h.strip()])
app.mount("/static", StaticFiles(directory=STATIC), name="static")


@app.middleware("http")
async def require_app_header(request: Request, call_next):
    """Every state-changing API call must carry X-Agent-Chat. Other websites can't add custom
    headers to requests to this server (that needs a CORS preflight, which this app never
    grants), so they can't trigger uploads, approvals or deletes behind your back."""
    if request.url.path.startswith("/api/") and request.method not in ("GET", "HEAD", "OPTIONS") \
            and request.headers.get("x-agent-chat") != "1":
        return JSONResponse({"detail": "missing X-Agent-Chat header"}, status_code=403)
    return await call_next(request)


@app.get("/")
async def index():
    return FileResponse(STATIC / "index.html", headers={"Cache-Control": "no-cache"})


def _sse(event: dict) -> str:
    return f"data: {json.dumps(event, ensure_ascii=False)}\n\n"


async def _sse_stream(queue: asyncio.Queue, first: list[dict], cleanup):
    try:
        for event in first:
            yield _sse(event)
        while True:
            try:
                event = await asyncio.wait_for(queue.get(), timeout=15)
            except asyncio.TimeoutError:
                yield ": ping\n\n"
                continue
            yield _sse(event)
    finally:
        cleanup()


def _need_chat(chat_id: str) -> dict:
    chat = store.get_chat(chat_id)
    if not chat:
        raise HTTPException(404, "chat not found")
    return chat


def _need_agent(agent_id: str) -> dict:
    agent = store.get_agent(agent_id)
    if not agent:
        raise HTTPException(404, "agent not found")
    return agent


# ------------------------------------------------------------------- state

@app.get("/api/state")
async def state():
    return {"agents": store.list_agents(), "chats": await chats(), "settings": store.get_settings(),
            "tools": tools.TOOL_INFO, "default_workspace": str(store.DEFAULT_WORKSPACE)}


@app.get("/api/chats")
async def chats():
    chats = store.list_chats()
    for c in chats:
        c["status"] = runner.chat_status(c["id"])
    return chats


@app.get("/api/search")
async def search(q: str = ""):
    return await asyncio.to_thread(store.search_chats, q)


@app.get("/api/server")
async def server(base_url: str | None = None, api_key: str | None = None):
    """Model server status. Query params let Settings test a URL before saving it."""
    settings = store.get_settings()
    if base_url:
        settings |= {"base_url": base_url, "api_key": api_key or ""}
    return await llm.server_info(runner.client, settings)


@app.get("/api/events")
async def events():
    queue: asyncio.Queue = asyncio.Queue()
    runner.global_subs.add(queue)
    return StreamingResponse(_sse_stream(queue, [{"type": "hello"}], lambda: runner.global_subs.discard(queue)),
                             media_type="text/event-stream", headers={"Cache-Control": "no-cache"})


class SettingsIn(BaseModel):
    base_url: str | None = None
    api_key: str | None = None
    model: str | None = None
    max_steps: int | None = None
    shell_timeout: int | None = None
    auto_compact: bool | None = None
    compact_at: int | None = None
    context_size: int | None = None
    auto_memory: bool | None = None
    bypass_approvals: bool | None = None
    search_url: str | None = None
    browser_headless: bool | None = None
    browser_executable: str | None = None


@app.put("/api/settings")
async def put_settings(body: SettingsIn):
    return store.save_settings(body.model_dump(exclude_none=True))


# ------------------------------------------------------------------ agents

class AgentIn(BaseModel):
    name: str
    emoji: str = "🤖"
    color: str = "#7c6cff"
    purpose: str = ""
    system_prompt: str = ""
    tools: list[str] = ["ask_agent"]
    delegate_all: bool = True
    delegates: list[str] = []
    model: str = ""
    temperature: float | None = None
    workspace: str = ""
    confirm_shell: bool = True
    memory: bool = True


@app.post("/api/agents")
async def create_agent(body: AgentIn):
    return store.save_agent(body.model_dump() | {"id": store.new_id()})


@app.put("/api/agents/{agent_id}")
async def update_agent(agent_id: str, body: AgentIn):
    _need_agent(agent_id)
    return store.save_agent(body.model_dump() | {"id": agent_id})


@app.delete("/api/agents/{agent_id}")
async def delete_agent(agent_id: str):
    _need_agent(agent_id)
    store.delete_agent(agent_id)
    return {"ok": True}


# ------------------------------------------------------------------- chats

class ChatIn(BaseModel):
    agent_id: str


class ChatPatch(BaseModel):
    title: str | None = None
    agent_id: str | None = None


class RunIn(BaseModel):
    content: str | None = None
    from_index: int | None = None  # truncate history here first (edit / regenerate)


@app.post("/api/chats")
async def create_chat(body: ChatIn):
    _need_agent(body.agent_id)
    chat = store.create_chat(body.agent_id)
    runner.broadcast({"type": "chats_changed"})
    return chat


@app.patch("/api/chats/{chat_id}")
async def patch_chat(chat_id: str, body: ChatPatch):
    chat = _need_chat(chat_id)
    if chat_id in runner.views:
        raise HTTPException(409, "chat is busy")
    if body.title is not None:
        chat["title"] = body.title.strip()[:120] or "Untitled"
    if body.agent_id is not None:
        _need_agent(body.agent_id)
        chat["agent_id"] = body.agent_id
    store.save_chat(chat)
    runner.broadcast({"type": "chats_changed"})
    return store.chat_summary(chat)


@app.delete("/api/chats/{chat_id}")
async def delete_chat(chat_id: str):
    view = runner.views.get(chat_id)
    if view:  # stop the run writing this chat (for a sub-chat: the run of the chat that started it)
        view.run.task.cancel()
        with suppress(asyncio.CancelledError):
            await view.run.task
    chat = store.get_chat(chat_id)
    for sub_id in (chat or {}).get("subchats") or []:
        store.delete_chat(sub_id)
    store.delete_chat(chat_id)
    runner.broadcast({"type": "chats_changed"})
    return {"ok": True}


def _startable(chat_id: str) -> tuple[dict, dict]:
    if chat_id in runner.views:
        raise HTTPException(409, "this chat is already running")
    chat = _need_chat(chat_id)
    if chat.get("parent"):
        raise HTTPException(400, "this chat shows one agent working for another; reply in the chat that started it")
    agent = store.get_agent(chat["agent_id"])
    if not agent:
        raise HTTPException(400, "this chat's agent was deleted; start a new chat")
    return chat, agent


@app.post("/api/chats/{chat_id}/run")
async def run_chat(chat_id: str, body: RunIn):
    """Send a message. While the chat is running it is queued for the agent's next step instead."""
    run = runner.runs.get(chat_id)
    content = (body.content or "").strip()
    if run and content and body.from_index is None:
        return {"ok": True, "queued": run.enqueue(content)}
    chat, agent = _startable(chat_id)
    if body.from_index is not None:
        if not 0 <= body.from_index <= len(chat["messages"]):
            raise HTTPException(400, "from_index out of range")
        runner.truncate(chat, body.from_index)
    runner.add_user_message(chat, agent, content)
    if not chat["messages"] or chat["messages"][-1]["role"] not in ("user", "tool"):
        raise HTTPException(400, "nothing to answer: send a message first")
    store.save_chat(chat)
    runner.start(chat, agent)
    runner.broadcast({"type": "chats_changed"})
    return {"ok": True}


@app.delete("/api/chats/{chat_id}/queue")
async def unqueue_all(chat_id: str):
    """Take back every queued message (to edit them). Returns them, oldest first."""
    run = runner.runs.get(chat_id)
    return {"items": run.dequeue() if run else []}


@app.delete("/api/chats/{chat_id}/queue/{item_id}")
async def unqueue(chat_id: str, item_id: str):
    run = runner.runs.get(chat_id)
    taken = run.dequeue(item_id) if run else []
    if not taken:
        raise HTTPException(404, "that message was already sent to the agent")
    return taken[0]


@app.post("/api/chats/{chat_id}/compact")
async def compact_chat(chat_id: str):
    chat, agent = _startable(chat_id)
    runner.start(chat, agent, mode="compact")
    return {"ok": True}


@app.post("/api/chats/{chat_id}/stop")
async def stop_chat(chat_id: str):
    view = runner.views.get(chat_id)
    if view:
        view.run.task.cancel()
    return {"ok": True}


class ApprovalIn(BaseModel):
    approve: bool


@app.post("/api/chats/{chat_id}/approvals/{approval_id}")
async def approve(chat_id: str, approval_id: str, body: ApprovalIn):
    view = runner.views.get(chat_id)
    fut = view.run.approvals.get(approval_id) if view else None
    if not fut or fut.done():
        raise HTTPException(404, "no pending approval with that id")
    fut.set_result(body.approve)
    return {"ok": True}


# ------------------------------------------------------- attachments + export

MAX_UPLOAD = 20 * 1024 * 1024
INLINE_TEXT = 40_000


@app.post("/api/chats/{chat_id}/upload")
async def upload(chat_id: str, request: Request, name: str):
    """Save a dropped/pasted file into the chat agent's workspace (uploads/). Text files also
    come back as text so the UI can put them straight into the message."""
    chat = _need_chat(chat_id)
    agent = store.get_agent(chat["agent_id"]) or {}
    data = await request.body()
    if len(data) > MAX_UPLOAD:
        raise HTTPException(413, "file is larger than 20 MB")
    safe = "".join(c if c.isalnum() or c in "._- " else "_" for c in Path(name).name).strip() or "file"
    folder = tools.workspace_for(agent) / "uploads"
    folder.mkdir(parents=True, exist_ok=True)
    target, n = folder / safe, 1
    while target.exists():
        target = folder / f"{Path(safe).stem}-{n}{Path(safe).suffix}"
        n += 1
    target.write_bytes(data)
    text = None
    try:
        decoded = data.decode("utf-8")
        if "\x00" not in decoded and len(decoded) <= INLINE_TEXT:
            text = decoded
    except UnicodeDecodeError:
        pass
    return {"name": target.name, "path": f"uploads/{target.name}", "size": len(data), "text": text}


@app.get("/api/chats/{chat_id}/export.md")
async def export_chat(chat_id: str):
    chat = _need_chat(chat_id)
    agent = store.get_agent(chat["agent_id"]) or {"name": "Deleted agent"}
    lines = [f"# {chat['title']}", "", f"Agent: {agent['name']}", ""]
    for m in chat["messages"]:
        if m["role"] == "user":
            heading = "## Request" if chat.get("parent") else "## You (while it worked)" if m.get("_queued") else "## You"
            lines += [heading, "", m["content"], ""]
        elif m["role"] == "assistant":
            if m.get("_error"):
                lines += [f"> Error: {m['_error']}", ""]
                continue
            if m.get("content"):
                lines += [f"## {agent['name']}", "", m["content"], ""]
            for tc in m.get("tool_calls") or []:
                lines += [f"*Used `{tc['function']['name']}`*", ""]
    filename = "".join(c if c.isalnum() else "-" for c in chat["title"])[:60].strip("-") or "chat"
    return PlainTextResponse("\n".join(lines), media_type="text/markdown",
                             headers={"Content-Disposition": f'attachment; filename="{filename}.md"'})


# ------------------------------------------------------------------- email

class EmailIn(BaseModel):
    imap_host: str | None = None
    imap_port: int | None = None
    imap_security: str | None = None
    smtp_host: str | None = None
    smtp_port: int | None = None
    smtp_security: str | None = None
    username: str | None = None
    password: str | None = None  # empty keeps the saved one
    from_name: str | None = None
    from_address: str | None = None


@app.get("/api/email")
async def get_email():
    return mail.public_config()


@app.put("/api/email")
async def put_email(body: EmailIn):
    for field in ("imap_security", "smtp_security"):
        if getattr(body, field) not in (None, "ssl", "starttls", "none"):
            raise HTTPException(400, f"{field} must be ssl, starttls or none")
    return mail.save_config(body.model_dump(exclude_none=True))


@app.post("/api/email/test")
async def test_email():
    try:
        return {"ok": True, "message": await asyncio.to_thread(mail.test_connection)}
    except Exception as e:
        return {"ok": False, "message": str(e)}


# --------------------------------------------------------------- calendars

class CalendarIn(BaseModel):
    name: str
    url: str


@app.get("/api/calendars")
async def list_calendars():
    return agenda.public()


@app.post("/api/calendars")
async def add_calendar(body: CalendarIn):
    try:
        return agenda.add(body.name, body.url)
    except ValueError as e:
        raise HTTPException(400, str(e))


@app.delete("/api/calendars/{calendar_id}")
async def remove_calendar(calendar_id: str):
    if not agenda.remove(calendar_id):
        raise HTTPException(404, "calendar not found")
    return {"ok": True}


@app.post("/api/calendars/test")
async def test_calendars():
    return {"message": await agenda.test(runner.client)}


# ---------------------------------------------------------------- routines

async def run_routine(routine: dict) -> str:
    """Start one run of a routine in its own chat. Returns a short status for the UI."""
    agent = store.get_agent(routine["agent_id"])
    if not agent:
        return "failed: its agent was deleted"
    chat = store.get_chat(routine["chat_id"]) if routine.get("chat_id") else None
    if chat is None:
        chat = store.create_chat(agent["id"])
        chat["title"] = f"Routine · {routine['name']}"
        chat["routine_id"] = routine["id"]
        routines.record(routine["id"], chat_id=chat["id"])
    if chat["id"] in runner.views:
        return "skipped (its previous run was still going)"
    if runner.chat_status(chat["id"]) != "idle":
        return "skipped (busy)"
    chat["agent_id"] = agent["id"]
    when = datetime.now().strftime("%A %Y-%m-%d %H:%M")
    runner.add_user_message(chat, agent, f"{routine['prompt']}\n\n(Scheduled routine “{routine['name']}”, {when}.)")
    store.save_chat(chat)
    runner.start(chat, agent)
    runner.broadcast({"type": "chats_changed"})
    runner.broadcast({"type": "routines_changed"})
    return "started"


class RoutineIn(BaseModel):
    name: str | None = None
    agent_id: str | None = None
    prompt: str | None = None
    schedule: dict | None = None
    enabled: bool | None = None


@app.get("/api/routines")
async def list_routines():
    return routines.load()


@app.post("/api/routines")
async def create_routine(body: RoutineIn):
    try:
        r = routines.create(body.model_dump(exclude_none=True))
    except (ValueError, TypeError) as e:
        raise HTTPException(400, str(e))
    runner.broadcast({"type": "routines_changed"})
    return r


@app.put("/api/routines/{routine_id}")
async def update_routine(routine_id: str, body: RoutineIn):
    try:
        r = routines.update(routine_id, body.model_dump(exclude_none=True))
    except KeyError:
        raise HTTPException(404, "routine not found")
    except (ValueError, TypeError) as e:
        raise HTTPException(400, str(e))
    runner.broadcast({"type": "routines_changed"})
    return r


@app.delete("/api/routines/{routine_id}")
async def delete_routine(routine_id: str):
    if not routines.delete(routine_id):
        raise HTTPException(404, "routine not found")
    runner.broadcast({"type": "routines_changed"})
    return {"ok": True}


@app.post("/api/routines/{routine_id}/run")
async def run_routine_now(routine_id: str):
    r = routines.get(routine_id)
    if not r:
        raise HTTPException(404, "routine not found")
    status = await run_routine(r)
    routines.record(routine_id, last_status=status, **({"last_run": time.time()} if status == "started" else {}))
    return {"status": status, "chat_id": routines.get(routine_id)["chat_id"]}


# ------------------------------------------------------------------ memory

class MemoryIn(BaseModel):
    fact: str | None = None
    category: str | None = None
    importance: int | None = None
    pinned: bool | None = None


@app.get("/api/memory")
async def list_memory(q: str = "", category: str = ""):
    return {"memories": memory.listing(q, category), "categories": memory.CATEGORIES, "total": memory.count()}


@app.post("/api/memory/tidy")
async def tidy_memory():
    try:
        return {"message": await runner.tidy_memory()}
    except llm.LLMError as e:
        raise HTTPException(502, str(e))


@app.post("/api/memory")
async def add_memory(body: MemoryIn):
    try:
        m, action = memory.add(body.fact or "", body.category or "general", body.importance or 5,
                               source="user", pinned=bool(body.pinned))
    except ValueError as e:
        raise HTTPException(400, str(e))
    runner.broadcast({"type": "memory_changed", "by": "you"})
    return m | {"action": action}


@app.put("/api/memory/{memory_id}")
async def edit_memory(memory_id: int, body: MemoryIn):
    if not memory.get(memory_id):
        raise HTTPException(404, "memory not found")
    try:
        m = memory.update(memory_id, **body.model_dump(exclude_none=True))
    except ValueError as e:
        raise HTTPException(400, str(e))
    runner.broadcast({"type": "memory_changed", "by": "you"})
    return m


@app.delete("/api/memory/{memory_id}")
async def delete_memory(memory_id: int):
    if not memory.delete(memory_id):
        raise HTTPException(404, "memory not found")
    runner.broadcast({"type": "memory_changed", "by": "you"})
    return {"ok": True}


@app.get("/api/chats/{chat_id}")
async def get_chat(chat_id: str):
    view = runner.views.get(chat_id)
    return view.chat if view else _need_chat(chat_id)


@app.get("/api/chats/{chat_id}/stream")
async def chat_stream(chat_id: str):
    """SSE: a snapshot of the chat, the current run's events so far, then live events."""
    chat = _need_chat(chat_id)
    view = runner.views.get(chat_id)
    queue: asyncio.Queue = asyncio.Queue()
    # No await between building `first` and subscribing, so no event can slip through.
    if view:
        run = runner.runs.get(chat_id)  # only a chat the user writes in (not a sub-chat) has a queue
        first = [{"type": "snapshot", "chat": view.chat, "running": True, "run_base": view.base,
                  "queue": run.queue if run else []}] + list(view.events)
    else:
        first = [{"type": "snapshot", "chat": chat, "running": False, "run_base": len(chat["messages"])}]
    runner.chat_subs.setdefault(chat_id, set()).add(queue)

    def cleanup():
        subs = runner.chat_subs.get(chat_id)
        if subs is not None:
            subs.discard(queue)
            if not subs:
                runner.chat_subs.pop(chat_id, None)

    return StreamingResponse(_sse_stream(queue, first, cleanup), media_type="text/event-stream",
                             headers={"Cache-Control": "no-cache", "X-Accel-Buffering": "no"})
