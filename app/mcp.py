"""Tools from MCP servers (the Model Context Protocol), over its stdio transport.

Settings → MCP holds the servers, in the JSON shape most MCP READMEs give for Claude Desktop:
  {"mcpServers": {"rea": {"command": "npx", "args": ["-y", "rea-agents@6.4.0", "mcp"], "env": {}}}}
Optional keys per server: "confirm" says when to ask the user before a call ("destructive", the
default, asks unless the tool declares itself read-only or harmless; "always"; "never"),
"timeout" is the seconds one call may take (default 600, 0 = no limit) and "disabled" turns a
server off without deleting it.

An agent gets a server by having "mcp:<name>" among its tools. The server starts when an agent
that has it starts a turn and keeps running, one process shared by every agent, until its
settings change or the app stops; if it dies, the next call starts it again. A small server's
tools go to the model directly, named <server>__<tool>. A big one's would crowd a local model's
context (REA's 170 tools are about 70K tokens), so the agent gets find_tools and call_tool
instead: it searches the catalog for what it needs and calls that.
"""

import asyncio
import base64
import collections
import itertools
import json
import logging
import mimetypes
import os
import re
import signal
import time
from pathlib import Path

from . import store

PROTOCOL = "2025-06-18"
INLINE_BUDGET = 16_000     # characters of tool definitions a server may hand the model directly
START_TIMEOUT = 180        # seconds to start and answer `initialize` (npx may download the package first)
CALL_TIMEOUT = 600         # default seconds per tool call; a server's "timeout" overrides it
MAX_RESULT = 200_000       # characters of one result kept (history.fit_output cuts it to the context)
LINE_LIMIT = 256 << 20     # one JSON-RPC message per line, and a big catalog is one line (REA's is 2 MB)
MAX_FOUND = 8              # tools one find_tools search returns
FULL_FOUND = 3             # ...of which the best ones show their arguments (the rest: look them up by name)
SCHEMA_BUDGET = 12_000     # characters of argument schemas one search shows (the best match's always)
PROGRESS_LINES = 30
client_version = "dev"     # main.py sets the app version
CONFIRM = ("destructive", "always", "never")
NAME = re.compile(r"[A-Za-z0-9_-]{1,32}")
STOP_WORDS = {"a", "an", "the", "to", "of", "in", "on", "for", "and", "or", "with", "from", "by", "at", "is",
              "it", "this", "that", "tool", "tools", "find", "get", "show", "me", "my", "how", "what", "which"}

log = logging.getLogger("agent_chat.mcp")


class MCPError(Exception):
    pass


def _fn(name: str, description: str, properties: dict, required: list[str]) -> dict:
    return {"type": "function", "function": {
        "name": name, "description": description,
        "parameters": {"type": "object", "properties": properties, "required": required},
    }}


# ------------------------------------------------------------------ settings

def parse_config(raw) -> dict:
    """Settings → MCP checked and cleaned: {"mcpServers": {...}}, or just the inner object, or
    its JSON text. Raises ValueError with a message for the user."""
    if isinstance(raw, str):
        try:
            raw = json.loads(raw) if raw.strip() else {}
        except ValueError as e:
            raise ValueError(f"MCP servers: not valid JSON ({e})") from None
    if not isinstance(raw, dict):
        raise ValueError("MCP servers must be a JSON object")
    if "mcpServers" in raw:
        raw = raw["mcpServers"]
        if not isinstance(raw, dict):
            raise ValueError("mcpServers must be a JSON object")
    servers = {}
    for name, cfg in raw.items():
        where = f"MCP server {name!r}"
        if not NAME.fullmatch(name):
            raise ValueError(f"{where}: a name may only have letters, digits, _ and - (at most 32)")
        if not isinstance(cfg, dict):
            raise ValueError(f"{where}: must be an object with a command")
        if not cfg.get("command"):
            if cfg.get("url") or cfg.get("type") in ("http", "sse", "streamable-http"):
                raise ValueError(f"{where}: only local servers that run as a command (stdio) are supported")
            raise ValueError(f"{where}: needs a command")
        if not isinstance(cfg["command"], str):
            raise ValueError(f"{where}: command must be a string")
        args = cfg.get("args", [])
        if not isinstance(args, list) or not all(isinstance(a, (str, int, float)) for a in args):
            raise ValueError(f"{where}: args must be a list of strings")
        env = cfg.get("env", {})
        if not isinstance(env, dict) or not all(isinstance(v, (str, int, float, bool)) for v in env.values()):
            raise ValueError(f"{where}: env must be an object of strings")
        confirm = cfg.get("confirm", "destructive")
        if confirm not in CONFIRM:
            raise ValueError(f"{where}: confirm must be one of {', '.join(CONFIRM)}")
        timeout = cfg.get("timeout", CALL_TIMEOUT)
        if isinstance(timeout, bool) or not isinstance(timeout, (int, float)) or timeout < 0:
            raise ValueError(f"{where}: timeout must be a number of seconds (0 = no limit)")
        clean = {"command": cfg["command"], "args": [str(a) for a in args],
                 "env": {str(k): str(v).lower() if isinstance(v, bool) else str(v) for k, v in env.items()}}
        if confirm != "destructive":
            clean["confirm"] = confirm
        if timeout != CALL_TIMEOUT:
            clean["timeout"] = timeout
        if cfg.get("disabled"):
            clean["disabled"] = True
        servers[name] = clean
    return servers


def configs() -> dict:
    try:
        return parse_config(store.get_settings().get("mcp_servers") or {})
    except ValueError as e:  # hand-edited settings.json: run nothing rather than guess
        log.warning("%s", e)
        return {}


def tool_info() -> list[dict]:
    """The agent editor's entries for the configured servers (next to tools.TOOL_INFO)."""
    return [{"name": f"mcp:{name}", "label": f"{name} (MCP server)" + (" · off" if cfg.get("disabled") else ""),
             "group": "MCP servers", "danger": cfg.get("confirm") == "never"}
            for name, cfg in configs().items()]


# ------------------------------------------------------------------ one server

class Server:
    def __init__(self, name: str, cfg: dict):
        self.name, self.cfg = name, cfg
        self.proc: asyncio.subprocess.Process | None = None
        self.tasks: list[asyncio.Task] = []
        self.ids = itertools.count(1)
        self.pending: dict[int, asyncio.Future] = {}
        self.progress: dict[str, callable] = {}
        self.ready = False
        self.stale = False            # the server said its tool list changed
        self.tools: list[dict] = []   # kept when the process dies: the next call restarts it
        self.listed = False
        self.instructions = ""
        self.info: dict = {}
        self.error = ""
        self.stderr: collections.deque[str] = collections.deque(maxlen=40)
        self.lock = asyncio.Lock()
        self._inline: dict[str, str] | None = None  # model-facing name -> tool name, when small enough

    @property
    def running(self) -> bool:
        return self.proc is not None and self.proc.returncode is None

    def label(self) -> str:
        return " ".join([self.cfg["command"], *self.cfg["args"]])

    async def ensure(self) -> bool:
        """Start the server if it isn't running. True if this call started it."""
        async with self.lock:
            if self.running and self.ready:
                if self.stale:
                    await self._list_tools()
                return False
            await self._start()
            return True

    async def _start(self) -> None:
        await self._stop()
        self.error, self.ready = "", False
        self.stderr.clear()
        store.DEFAULT_WORKSPACE.mkdir(parents=True, exist_ok=True)
        try:
            self.proc = await asyncio.create_subprocess_exec(
                self.cfg["command"], *self.cfg["args"], env={**os.environ, **self.cfg["env"]},
                cwd=store.DEFAULT_WORKSPACE, stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE, limit=LINE_LIMIT, start_new_session=True)
        except OSError as e:
            self.error = f"can't run {self.cfg['command']}: {e.strerror or e}"
            raise MCPError(self.error) from None
        self.tasks = [asyncio.create_task(self._read(self.proc)), asyncio.create_task(self._read_stderr(self.proc))]
        try:
            init = await self.request("initialize", {
                "protocolVersion": PROTOCOL, "capabilities": {},
                "clientInfo": {"name": "agent-chat", "version": client_version}}, timeout=START_TIMEOUT)
            await self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})
            self.info = init.get("serverInfo") or {}
            self.instructions = str(init.get("instructions") or "").strip()
            if "tools" not in (init.get("capabilities") or {}):
                self.tools, self.listed = [], True
            else:
                await self._list_tools()
            self.ready = True
            log.info("MCP server %s started: %s %s, %d tools", self.name, self.info.get("name", "?"),
                     self.info.get("version", ""), len(self.tools))
        except MCPError as e:
            self.error = str(e)
            await self._stop()
            raise

    async def _list_tools(self) -> None:
        self.stale = False
        found, cursor = [], None
        for _ in range(100):
            page = await self.request("tools/list", {"cursor": cursor} if cursor else {}, timeout=START_TIMEOUT)
            found += [t for t in page.get("tools") or [] if isinstance(t, dict) and isinstance(t.get("name"), str)]
            cursor = page.get("nextCursor")
            if not cursor:
                break
        self.tools, self.listed, self._inline = found, True, None

    async def stop(self) -> None:
        async with self.lock:
            await self._stop()

    async def _stop(self) -> None:
        proc, self.proc, self.ready = self.proc, None, False
        if proc and proc.returncode is None:
            try:
                proc.stdin.close()
                os.killpg(proc.pid, signal.SIGTERM)  # its own session: npx's node child goes too
            except (ProcessLookupError, PermissionError, OSError):
                pass
            try:
                await asyncio.wait_for(proc.wait(), 3)
            except TimeoutError:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass
                await proc.wait()
        for t in self.tasks:
            t.cancel()
        self.tasks = []
        self._fail_pending("the server was stopped")

    def _fail_pending(self, why: str) -> None:
        for fut in self.pending.values():
            if not fut.done():
                fut.set_exception(MCPError(why))
        self.pending.clear()

    def _exit_message(self, proc) -> str:
        tail = "\n".join(self.stderr).strip()[-1500:]
        code = proc.returncode
        return (f"the server exited (code {code})" if code is not None else "the server stopped answering") + \
               (f":\n{tail}" if tail else "")

    async def _read(self, proc) -> None:
        try:
            while line := await proc.stdout.readline():
                try:
                    msg = json.loads(line)
                except ValueError:
                    self.stderr.append(line.decode(errors="replace").rstrip()[:500])  # stray print on stdout
                    continue
                for m in msg if isinstance(msg, list) else [msg]:
                    if isinstance(m, dict):
                        await self._handle(m)
        except ValueError as e:  # a line over LINE_LIMIT
            self.stderr.append(f"(agent-chat: {e})")
        except asyncio.CancelledError:
            return
        if proc is self.proc:  # it ended on its own (or broke its output): fail what waits, restart on next use
            try:
                await asyncio.wait_for(proc.wait(), 2)
            except TimeoutError:
                pass
            await asyncio.sleep(0.05)  # let the last stderr lines in
            self.error = self._exit_message(proc)
            log.warning("MCP server %s: %s", self.name, self.error)
            self.ready = False
            self._fail_pending(self.error)
            if proc.returncode is None:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass

    async def _read_stderr(self, proc) -> None:
        try:
            while line := await proc.stderr.readline():
                self.stderr.append(line.decode(errors="replace").rstrip()[:500])
        except (ValueError, asyncio.CancelledError):
            pass

    async def _handle(self, msg: dict) -> None:
        method = msg.get("method")
        if method is None:  # a reply to one of ours
            fut = self.pending.pop(msg.get("id"), None)
            if fut and not fut.done():
                if "error" in msg:
                    err = msg["error"] if isinstance(msg["error"], dict) else {"message": str(msg["error"])}
                    fut.set_exception(MCPError(str(err.get("message") or err)))
                else:
                    fut.set_result(msg.get("result") or {})
        elif "id" in msg:  # the server asks us something: we offer no client features but ping
            reply = {"result": {}} if method == "ping" else {"error": {"code": -32601, "message": f"{method} is not supported"}}
            await self._send({"jsonrpc": "2.0", "id": msg["id"], **reply})
        elif method == "notifications/progress":
            p = msg.get("params") or {}
            if cb := self.progress.get(str(p.get("progressToken"))):
                await cb(p)
        elif method == "notifications/tools/list_changed":
            self.stale = True

    async def _send(self, msg: dict) -> None:
        if not self.running:
            raise MCPError(self.error or "the server is not running")
        try:
            self.proc.stdin.write(json.dumps(msg, ensure_ascii=False).encode() + b"\n")
            await self.proc.stdin.drain()
        except (BrokenPipeError, ConnectionResetError) as e:
            raise MCPError(f"the server closed its input: {e}") from None

    async def request(self, method: str, params: dict, *, timeout: float | None = None, on_progress=None) -> dict:
        id_ = next(self.ids)
        token = f"agent-chat-{id_}"
        fut = asyncio.get_running_loop().create_future()
        self.pending[id_] = fut
        if on_progress:
            params = {**params, "_meta": {"progressToken": token}}
            self.progress[token] = on_progress
        try:
            await self._send({"jsonrpc": "2.0", "id": id_, "method": method, "params": params})
            return await asyncio.wait_for(fut, timeout or None)
        except TimeoutError:
            asyncio.create_task(self._cancel(id_, "timed out"))
            raise MCPError(f"no answer within {timeout:g} s (the call was cancelled)") from None
        except asyncio.CancelledError:  # the user pressed Stop: tell the server to drop the work
            asyncio.create_task(self._cancel(id_, "stopped by the user"))
            raise
        finally:
            self.pending.pop(id_, None)
            self.progress.pop(token, None)

    async def _cancel(self, id_: int, reason: str) -> None:
        try:
            await self._send({"jsonrpc": "2.0", "method": "notifications/cancelled",
                              "params": {"requestId": id_, "reason": reason}})
        except MCPError:
            pass

    # ------------------------------------------------- how the model sees it

    def tool(self, name: str) -> dict | None:
        return next((t for t in self.tools if t["name"] == name), None)

    def inline_names(self) -> dict[str, str]:
        """Model-facing name -> tool name when the whole catalog fits INLINE_BUDGET, else {}."""
        if self._inline is None:
            names = {_fn_name(self.name, t["name"]): t["name"] for t in self.tools}
            size = sum(len(json.dumps(function(self.name, t))) for t in self.tools)
            self._inline = names if size <= INLINE_BUDGET and len(names) == len(self.tools) else {}
        return self._inline

    def inline(self) -> bool:
        return bool(self.inline_names()) or not self.tools

    def needs_approval(self, tool: dict) -> bool:
        mode = self.cfg.get("confirm", "destructive")
        if mode != "destructive":
            return mode == "always"
        notes = tool.get("annotations") or {}
        # MCP's defaults: a tool that doesn't say it is read-only may be destructive
        return not notes.get("readOnlyHint") and notes.get("destructiveHint", True) is not False

    def status(self) -> dict:
        return {"name": self.name, "command": self.label(), "running": self.running and self.ready,
                "error": self.error, "server": self.info, "tools": len(self.tools), "listed": self.listed,
                "mode": "direct" if self.inline() else "search",
                "tokens": sum(len(json.dumps(function(self.name, t))) for t in self.tools) // 4,
                "tool_names": [t["name"] for t in self.tools]}


def _fn_name(server: str, tool: str) -> str:
    return re.sub(r"[^A-Za-z0-9_-]", "_", f"{server}__{tool}")[:64]


def _schema(tool: dict) -> dict:
    schema = tool.get("inputSchema")
    schema = dict(schema) if isinstance(schema, dict) else {}
    schema.pop("$schema", None)
    schema["type"] = "object"
    schema.setdefault("properties", {})
    return schema


def function(server: str, tool: dict) -> dict:
    """An MCP tool as an OpenAI-style function definition."""
    return {"type": "function", "function": {"name": _fn_name(server, tool["name"]),
                                             "description": str(tool.get("description") or "")[:1500],
                                             "parameters": _schema(tool)}}


# --------------------------------------------------------------- all servers

_servers: dict[str, Server] = {}


def server(name: str) -> Server | None:
    cfg = configs().get(name)
    if not cfg or cfg.get("disabled"):
        return None
    s = _servers.get(name)
    if s is None or s.cfg != cfg:
        if s:  # settings changed under it (reconfigure normally got there first)
            try:
                asyncio.get_running_loop().create_task(s.stop())
            except RuntimeError:
                pass
        s = _servers[name] = Server(name, cfg)
    return s


def agent_servers(agent: dict) -> list[Server]:
    names = [t[4:] for t in agent.get("tools") or [] if t.startswith("mcp:")]
    return [s for s in map(server, dict.fromkeys(names)) if s]


async def reconfigure() -> None:
    """Settings changed: stop the servers whose settings changed or that were removed."""
    cfgs = configs()
    for name, s in list(_servers.items()):
        if cfgs.get(name) != s.cfg or s.cfg.get("disabled"):
            del _servers[name]
            await s.stop()


async def shutdown() -> None:
    for s in list(_servers.values()):
        await s.stop()
    _servers.clear()


async def prepare(agent: dict, notice) -> None:
    """Start the agent's servers before its turn, so its tool list is known. `notice(text)` tells
    the user about a slow start or a failure; the turn goes on without that server."""
    for s in agent_servers(agent):
        if s.running and s.ready and not s.stale:
            continue
        if not s.running:
            notice(f"Starting MCP server {s.name}…")
        try:
            await s.ensure()
        except MCPError as e:
            notice(f"MCP server {s.name} didn't start: {e}" + ("" if not s.tools else
                   " Its tools are still offered; a call tries to start it again."))


async def test(name: str) -> dict:
    """Settings → MCP → Test: (re)start one server and report what it offers."""
    s = server(name)
    if not s:
        return {"name": name, "running": False, "error": "not configured, or turned off"}
    try:
        await s.stop()
        await s.ensure()
    except MCPError:
        pass
    return s.status()


def status() -> list[dict]:
    out = []
    for name, cfg in configs().items():
        s = _servers.get(name)
        if s and s.cfg == cfg:
            out.append(s.status())
        else:
            out.append({"name": name, "command": " ".join([cfg["command"], *cfg["args"]]), "running": False,
                        "error": "", "tools": 0, "listed": False, "disabled": bool(cfg.get("disabled"))})
    return out


# ------------------------------------------------------- what the agent gets

FIND, CALL = "find_tools", "call_tool"


def schemas(agent: dict) -> list[dict]:
    out, searched = [], []
    for s in agent_servers(agent):
        if not s.tools:
            continue
        if s.inline():
            out += [function(s.name, t) for t in s.tools]
        else:
            searched.append(s.name)
    if searched:
        props = {"query": {"type": "string", "description": "What you want to do, in a few words (e.g. \"decompile a "
                           "function\"), or a tool's exact name to see its arguments. Empty lists every tool."}}
        if len(searched) > 1:
            props["server"] = {"type": "string", "enum": searched, "description": "Search only this server"}
        out.append(_fn(FIND, f"Search the tools of your MCP server{'s' * (len(searched) > 1)} ({', '.join(searched)}). "
                       "Returns the matching tools with what they do and their arguments (a JSON Schema). "
                       f"Run one with {CALL}.", props, ["query"]))
        out.append(_fn(CALL, f"Run a tool from your MCP server{'s' * (len(searched) > 1)} ({', '.join(searched)}). "
                       f"Look it up with {FIND} first, so you know its arguments.",
                       {"tool": {"type": "string", "description": f"The tool's name as {FIND} shows it, e.g. "
                                 f"{searched[0]}/{_example(searched[0])}"},
                        "arguments": {"type": "object", "description": "The tool's arguments, as its schema says"}},
                       ["tool", "arguments"]))
    return out


def _example(name: str) -> str:
    s = _servers.get(name)
    return s.tools[0]["name"] if s and s.tools else "tool_name"


def prompt_note(agent: dict) -> str:
    """The system-prompt paragraph about the agent's MCP servers ("" if it has none)."""
    lines = []
    for s in agent_servers(agent):
        if not s.tools:
            continue
        how = "its tools are in your tool list" if s.inline() else \
              f"find its tools with {FIND} and run them with {CALL}"
        line = f"- {s.name} ({len(s.tools)} tools; {how})"
        if s.instructions:
            line += ": " + " ".join(s.instructions.split())[:800]
        lines.append(line)
    if not lines:
        return ""
    return ("MCP servers (programs on this computer that give you extra tools; pass them absolute file paths):\n" +
            "\n".join(lines))


def _words(text: str) -> list[str]:
    return [w for w in re.findall(r"[a-z0-9]+", text.lower()) if len(w) > 1 and w not in STOP_WORDS]


def find(servers: list[Server], query: str) -> str:
    entries = [(s, t) for s in servers for t in s.tools]
    if not entries:
        return "No MCP tools are available right now."
    q = query.strip()
    if not q:
        return "\n\n".join(f"{s.name} ({len(s.tools)} tools): " + ", ".join(t["name"] for t in s.tools)
                           for s in servers if s.tools) + \
               f"\n\nSearch with {FIND} to see what a tool does and its arguments."
    exact = [(s, t) for s, t in entries if q in (t["name"], f"{s.name}/{t['name']}", _fn_name(s.name, t["name"]))]
    if exact:
        hits = exact
    else:
        words = _words(q)
        scored = []
        for s, t in entries:
            name, desc = t["name"].lower(), str(t.get("description") or "").lower()
            score = 0
            for w in words:
                stem = w[:5] if len(w) > 5 else w
                score += 4 if w in name else 2 if stem in name else 1 if stem in desc else 0
            if score:
                scored.append((-score, len(name), s, t))
        scored.sort(key=lambda x: x[:2])
        hits = [(s, t) for *_, s, t in scored]
        if not hits:
            return (f"No tool matches {q!r}. Try other words, or an empty query to list every tool's name.")
    shown, budget, out = hits[:MAX_FOUND], SCHEMA_BUDGET, []
    for i, (s, t) in enumerate(shown):
        schema = json.dumps(_schema(t), ensure_ascii=False, separators=(",", ":"))
        desc = " ".join(str(t.get("description") or "").split())
        if i == 0 or (i < FULL_FOUND and len(schema) <= budget):
            part = f"## {s.name}/{t['name']}\n{desc[:1200]}\nArguments: {schema}"
            budget -= len(schema)
        else:
            part = f"## {s.name}/{t['name']}\n{_clip(desc, 200)}\n(Arguments: {FIND} with its exact name.)"
        if s.needs_approval(t):
            part += "\n(Asks the user before it runs.)"
        out.append(part)
    more = f"\n\n{len(hits) - len(shown)} more match; use more specific words to narrow it down." \
        if len(hits) > len(shown) else ""
    return "\n\n".join(out) + more + f"\n\nRun one with {CALL}(tool, arguments)."


def _clip(text: str, n: int) -> str:
    return text if len(text) <= n else text[:n].rsplit(" ", 1)[0] + "…"


def _resolve(servers: list[Server], name: str) -> tuple[Server, dict] | None:
    for s in servers:
        for t in s.tools:
            if name in (f"{s.name}/{t['name']}", _fn_name(s.name, t["name"]), f"{s.name}.{t['name']}"):
                return s, t
    bare = [(s, t) for s in servers for t in s.tools if t["name"] == name]
    return bare[0] if len(bare) == 1 else None


def is_mcp(agent: dict, name: str) -> bool:
    if name in (FIND, CALL):
        return True
    return any(name in s.inline_names() for s in agent_servers(agent))


async def call(agent: dict, name: str, args: dict, workspace: Path, *, approve, progress) -> str:
    """Run find_tools, call_tool or a directly offered tool. `approve(name, args)` asks the user;
    `progress(text)` shows the server's progress messages on the tool card."""
    servers = [s for s in agent_servers(agent) if s.tools]
    if name == FIND:
        if args.get("server"):
            servers = [s for s in servers if s.name == args["server"]] or servers
        return find(servers, str(args.get("query") or ""))
    if name == CALL:
        target = _resolve(servers, str(args.get("tool") or "").strip())
        if not target:
            return f"Error: no MCP tool {args.get('tool')!r}. Look it up with {FIND}; use the name it shows."
        arguments = args.get("arguments") or {}
        if isinstance(arguments, str):  # some models send the object as JSON text
            try:
                arguments = json.loads(arguments) if arguments.strip() else {}
            except ValueError:
                return "Error: arguments must be a JSON object."
        if not isinstance(arguments, dict):
            return "Error: arguments must be a JSON object."
    else:
        target = next(((s, s.tool(s.inline_names()[name])) for s in servers if name in s.inline_names()), None)
        if not target:
            return f"Error: tool '{name}' is not available to you."
        arguments = args
    s, tool = target
    if s.needs_approval(tool) and not await approve(f"{s.name}/{tool['name']}", arguments):
        return "The user did not allow this tool call. Ask them, or try another approach."

    lines: collections.deque[str] = collections.deque(maxlen=PROGRESS_LINES)

    async def on_progress(p: dict) -> None:
        done, total = p.get("progress"), p.get("total")
        lines.append(" ".join(str(x) for x in (f"{done:g}/{total:g}" if isinstance(done, (int, float)) and isinstance(total, (int, float))
                                                else done if isinstance(done, (int, float)) else None,
                                                p.get("message")) if x is not None))
        await progress("\n".join(lines))

    try:
        await s.ensure()
        tool = s.tool(tool["name"]) or tool
        result = await s.request("tools/call", {"name": tool["name"], "arguments": arguments},
                                 timeout=s.cfg.get("timeout", CALL_TIMEOUT), on_progress=on_progress)
    except MCPError as e:
        return f"Error from MCP server {s.name}: {e}"
    return result_text(s.name, result, workspace)


def result_text(server_name: str, result: dict, workspace: Path) -> str:
    parts = []
    for item in result.get("content") or []:
        if not isinstance(item, dict):
            continue
        kind = item.get("type")
        if kind == "text":
            parts.append(str(item.get("text") or ""))
        elif kind in ("image", "audio"):
            parts.append(_save(server_name, item.get("data"), item.get("mimeType"), workspace))
        elif kind == "resource":
            res = item.get("resource") or {}
            parts.append(str(res["text"]) if "text" in res else
                         _save(server_name, res.get("blob"), res.get("mimeType"), workspace) + f" ({res.get('uri', '')})")
        elif kind == "resource_link":
            parts.append(f"[resource {item.get('name') or ''}: {item.get('uri', '')}]".replace(" :", ":"))
    if not any(p.strip() for p in parts) and result.get("structuredContent") is not None:
        parts = [json.dumps(result["structuredContent"], ensure_ascii=False, indent=1)]
    text = "\n\n".join(p for p in parts if p) or "(the tool returned nothing)"
    if len(text) > MAX_RESULT:
        text = text[:MAX_RESULT] + f"\n\n... [{len(text) - MAX_RESULT:,} more characters cut]"
    return ("Error: " + text) if result.get("isError") else text


def _save(server_name: str, data, mime: str | None, workspace: Path) -> str:
    """An image or other binary the tool returned, saved in the workspace for the agent to use."""
    try:
        raw = base64.b64decode(data or "", validate=True)
    except ValueError:
        return f"[{mime or 'binary'} data that wasn't valid base64]"
    ext = mimetypes.guess_extension(mime or "") or ".bin"
    folder = workspace / "mcp"
    folder.mkdir(parents=True, exist_ok=True)
    path = folder / f"{server_name}-{time.strftime('%Y%m%d-%H%M%S')}-{os.urandom(2).hex()}{ext}"
    path.write_bytes(raw)
    return f"[{mime or 'file'} saved to {path} ({len(raw):,} bytes)]"
