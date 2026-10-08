"""Tools agents can call. File and shell tools are rooted in the agent's workspace folder."""

import asyncio
import itertools
import os
import re
import signal
import time
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlparse

import httpx

from .store import DEFAULT_WORKSPACE, get_settings

MAX_READ = 60_000
MAX_OUTPUT = 20_000
MAX_FETCH = 2_000_000  # bytes of a web page web_fetch will download
PROGRESS_EVERY = 0.5   # seconds between live-output updates while a shell command runs
SHELL_TIMEOUT = 180  # default; Settings → "Shell command timeout" (0 = no limit)
UA = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0"

_bg_counter = itertools.count()


class ToolError(Exception):
    pass


def _fn(name: str, description: str, properties: dict, required: list[str]) -> dict:
    return {"type": "function", "function": {
        "name": name, "description": description,
        "parameters": {"type": "object", "properties": properties, "required": required},
    }}


SCHEMAS = {
    "list_dir": _fn("list_dir", "List files and folders in a workspace directory.",
                    {"path": {"type": "string", "description": "Directory relative to the workspace. Default '.'"}}, []),
    "read_file": _fn("read_file", "Read a text file from the workspace. Lines are numbered.",
                     {"path": {"type": "string", "description": "File path relative to the workspace"},
                      "start_line": {"type": "integer", "description": "First line to read (1-based, optional)"},
                      "end_line": {"type": "integer", "description": "Last line to read (inclusive, optional)"}},
                     ["path"]),
    "write_file": _fn("write_file", "Create or overwrite a file in the workspace with the given content.",
                      {"path": {"type": "string", "description": "File path relative to the workspace"},
                       "content": {"type": "string", "description": "Full file content"}},
                      ["path", "content"]),
    "edit_file": _fn("edit_file", "Replace an exact snippet in a workspace file. old_text must appear exactly "
                     "once; include enough surrounding lines to make it unique.",
                     {"path": {"type": "string"},
                      "old_text": {"type": "string", "description": "Exact text to replace"},
                      "new_text": {"type": "string", "description": "Replacement text"}},
                     ["path", "old_text", "new_text"]),
    "run_shell": _fn("run_shell", "Run a shell command in the workspace directory. "
                     "Returns the exit code and combined stdout/stderr. "
                     "Set background=true for a long-running process (a server, a daemon, a watch): "
                     "it starts detached and returns at once with its pid and a log-file path, so you "
                     "can keep working and read the log later instead of waiting for it to finish.",
                     {"command": {"type": "string"},
                      "background": {"type": "boolean",
                                     "description": "Start the command detached and return immediately. "
                                                    "Default false."}},
                     ["command"]),
    "web_search": _fn("web_search", "Search the web. Returns titles, URLs and snippets.",
                      {"query": {"type": "string"}}, ["query"]),
    "web_fetch": _fn("web_fetch", "Fetch a web page and return its readable text.",
                     {"url": {"type": "string", "description": "http(s) URL"}}, ["url"]),
}

# Shown in the agent editor.
TOOL_INFO = [
    {"name": "list_dir", "label": "List folders", "group": "Files", "danger": False},
    {"name": "read_file", "label": "Read files", "group": "Files", "danger": False},
    {"name": "write_file", "label": "Write files", "group": "Files", "danger": False},
    {"name": "edit_file", "label": "Edit files", "group": "Files", "danger": False},
    {"name": "run_shell", "label": "Shell commands", "group": "System", "danger": True},
    {"name": "web_search", "label": "Web search", "group": "Web", "danger": False},
    {"name": "web_fetch", "label": "Fetch web pages", "group": "Web", "danger": False},
    {"name": "browser_navigate", "label": "Open pages in Brave", "group": "Browser", "danger": False},
    {"name": "browser_read", "label": "Read the open page", "group": "Browser", "danger": False},
    {"name": "browser_links", "label": "List links on the page", "group": "Browser", "danger": False},
    {"name": "browser_click", "label": "Click on the page", "group": "Browser", "danger": False},
    {"name": "browser_type", "label": "Type into the page", "group": "Browser", "danger": False},
    {"name": "browser_screenshot", "label": "Screenshot the page", "group": "Browser", "danger": False},
    {"name": "browser_close", "label": "Close the browser", "group": "Browser", "danger": False},
    {"name": "ask_agent", "label": "Talk to other agents", "group": "Agents", "danger": False},
    {"name": "email_list", "label": "List emails", "group": "Email", "danger": False},
    {"name": "email_search", "label": "Search emails", "group": "Email", "danger": False},
    {"name": "email_read", "label": "Read emails", "group": "Email", "danger": False},
    {"name": "email_draft", "label": "Save email drafts", "group": "Email", "danger": False},
    {"name": "email_send", "label": "Send email (asks you first)", "group": "Email", "danger": True},
    {"name": "calendar_events", "label": "Read your calendars", "group": "Calendar", "danger": False},
]


def ask_agent_schema(names: list[str]) -> dict:
    return _fn(
        "ask_agent",
        "Send a task or a question to another agent on your team and get its reply. It cannot see your "
        "conversation, so include all the context it needs. Your conversation with each agent is remembered: "
        "if it replies with a question, call ask_agent again with your answer and it continues where it left off.",
        {"agent": {"type": "string", "enum": names},
         "message": {"type": "string", "description": "The task or question, with all the context needed"},
         "new_conversation": {"type": "boolean",
                              "description": "Start fresh instead of continuing your earlier conversation "
                                             "with this agent. Default false."}},
        ["agent", "message"],
    )


# ---------------------------------------------------------------- helpers

def workspace_for(agent: dict) -> Path:
    ws = Path(agent.get("workspace") or DEFAULT_WORKSPACE).expanduser()
    ws.mkdir(parents=True, exist_ok=True)
    return ws.resolve()


def _resolve(ws: Path, path: str) -> Path:
    target = (ws / (path or ".")).resolve()
    if not target.is_relative_to(ws):
        raise ToolError(f"'{path}' is outside the workspace ({ws}). Use paths inside the workspace.")
    return target


def _clip(text: str, limit: int = MAX_OUTPUT) -> str:
    if len(text) <= limit:
        return text
    half = limit // 2
    return f"{text[:half]}\n\n... [{len(text) - limit} characters omitted] ...\n\n{text[-half:]}"


class _TextExtractor(HTMLParser):
    SKIP = {"script", "style", "noscript", "svg", "template", "iframe"}
    BLOCK = {"p", "div", "br", "li", "tr", "h1", "h2", "h3", "h4", "h5", "h6", "section", "article",
             "header", "footer", "pre", "blockquote", "table", "ul", "ol", "dd", "dt"}

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.parts, self.skip, self.title, self._in_title = [], 0, "", False

    def handle_starttag(self, tag, attrs):
        if tag in self.SKIP:
            self.skip += 1
        elif tag == "title":
            self._in_title = True
        elif tag in self.BLOCK:
            self.parts.append("\n")
        if tag in ("h1", "h2", "h3"):
            self.parts.append("#" * int(tag[1]) + " ")
        elif tag == "li":
            self.parts.append("- ")

    def handle_endtag(self, tag):
        if tag in self.SKIP:
            self.skip = max(0, self.skip - 1)
        elif tag == "title":
            self._in_title = False
        elif tag in self.BLOCK:
            self.parts.append("\n")

    def handle_data(self, data):
        if self._in_title:
            self.title += data
        elif not self.skip:
            self.parts.append(data)

    def text(self) -> str:
        text = re.sub(r"[ \t\r\f\v]+", " ", "".join(self.parts))
        return re.sub(r"\n\s*\n+", "\n\n", text).strip()


# ------------------------------------------------------------------ tools

def list_dir(ws: Path, path: str = ".") -> str:
    target = _resolve(ws, path)
    if not target.is_dir():
        raise ToolError(f"'{path}' is not a directory")
    entries = sorted(target.iterdir(), key=lambda p: (not p.is_dir(), p.name.lower()))
    lines = []
    for p in entries[:500]:
        lines.append(f"{p.name}/" if p.is_dir() else f"{p.name}  ({p.stat().st_size} bytes)")
    if len(entries) > 500:
        lines.append(f"... and {len(entries) - 500} more")
    return f"{target.relative_to(ws) or '.'}:\n" + ("\n".join(lines) or "(empty)")


def read_file(ws: Path, path: str, start_line: int | None = None, end_line: int | None = None) -> str:
    target = _resolve(ws, path)
    if not target.is_file():
        raise ToolError(f"'{path}' does not exist or is not a file")
    lines = target.read_text(errors="replace").splitlines()
    start = max(1, start_line or 1)
    end = min(len(lines), end_line or len(lines))
    out, size = [], 0
    for n in range(start, end + 1):
        row = f"{n:>5}  {lines[n - 1]}"
        size += len(row) + 1
        if size > MAX_READ:
            out.append(f"... [stopped at line {n - 1} of {len(lines)}; use start_line to read more]")
            break
        out.append(row)
    return "\n".join(out) if out else "(empty file)"


def write_file(ws: Path, path: str, content: str) -> str:
    target = _resolve(ws, path)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(content)
    return f"Wrote {len(content.encode())} bytes to {target.relative_to(ws)}"


def edit_file(ws: Path, path: str, old_text: str, new_text: str) -> str:
    target = _resolve(ws, path)
    if not target.is_file():
        raise ToolError(f"'{path}' does not exist")
    text = target.read_text()
    count = text.count(old_text) if old_text else 0
    if count != 1:
        raise ToolError(f"old_text must match exactly once, but it matched {count} times. "
                        "Read the file again and copy the exact text, with more context if needed.")
    target.write_text(text.replace(old_text, new_text, 1))
    return f"Edited {target.relative_to(ws)}"


async def run_shell(ws: Path, command: str, timeout: float | None = SHELL_TIMEOUT,
                    background: bool = False, on_progress=None) -> str:
    """Run `command`. `on_progress(text)` (async) is called every PROGRESS_EVERY seconds with the
    output so far, so the UI can show a long command's output while it runs."""
    # Output goes to a file, not a PIPE. communicate() on a PIPE waits for the write end to close,
    # but a backgrounded child inherits that fd and holds it open, so communicate() would hang
    # forever (especially with the timeout set to 0). Waiting on the process itself and reading a
    # file sidesteps that: a leaked child can keep the file open without blocking us.
    if background:
        log = ws / f".shell-bg-{os.getpid()}-{next(_bg_counter)}.log"
        with open(log, "wb") as fout:
            proc = await asyncio.create_subprocess_shell(
                command, cwd=ws, stdin=asyncio.subprocess.DEVNULL, stdout=fout,
                stderr=asyncio.subprocess.STDOUT, start_new_session=True,
            )
        rel = log.relative_to(ws)
        return (f"Started in the background (pid {proc.pid}). It keeps running after this call. "
                f"Output is being written to {rel} — read that file to check on it.")

    out_file = ws / f".shell-{os.getpid()}-{next(_bg_counter)}.out"
    try:
        with open(out_file, "wb") as fout:
            proc = await asyncio.create_subprocess_shell(
                command, cwd=ws, stdin=asyncio.subprocess.DEVNULL, stdout=fout,
                stderr=asyncio.subprocess.STDOUT, start_new_session=True,
            )
        try:
            waiter = asyncio.ensure_future(proc.wait())
            deadline = time.monotonic() + timeout if timeout else None  # 0/None: wait as long as it takes
            while True:
                left = None if deadline is None else deadline - time.monotonic()
                if left is not None and left <= 0:
                    waiter.cancel()
                    raise asyncio.TimeoutError
                wait = PROGRESS_EVERY if on_progress else left
                if left is not None and wait is not None:
                    wait = min(wait, left)
                done, _ = await asyncio.wait({waiter}, timeout=wait)
                if done:
                    break
                if on_progress:
                    await on_progress(_clip(out_file.read_bytes().decode(errors="replace")))
            out = out_file.read_bytes().decode(errors="replace")
            return f"exit code {proc.returncode}\n{_clip(out)}"
        except asyncio.TimeoutError:
            _kill(proc)
            out = out_file.read_bytes().decode(errors="replace")
            return (f"Command timed out after {timeout}s and was killed. (The limit is set in "
                    f"Settings → Shell command timeout. Use background=true for long-running "
                    f"commands.)\n{_clip(out)}")
        except asyncio.CancelledError:
            _kill(proc)
            raise
    finally:
        try:
            out_file.unlink()
        except OSError:
            pass


def _kill(proc) -> None:
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def _format_results(items: list[tuple[str, str, str]]) -> str:
    """items: (title, url, snippet). Returns a numbered, trimmed list."""
    clean = lambda s: re.sub(r"\s+", " ", re.sub(r"<[^>]+>", "", s or "")).strip()
    lines = []
    for title, url, snippet in items[:8]:
        lines.append(f"{len(lines) + 1}. {clean(title) or url}\n   {url}\n   {clean(snippet)}")
    return "\n".join(lines)


async def _searxng(client: httpx.AsyncClient, base: str, query: str) -> str:
    """Query a SearXNG instance's JSON API. Raises ToolError with a clear message on failure."""
    url = base.rstrip("/") + "/search"
    try:
        r = await client.get(url, params={"q": query, "format": "json"},
                             headers={"User-Agent": UA, "Accept": "application/json"},
                             timeout=20, follow_redirects=True)
    except httpx.HTTPError as e:
        raise ToolError(f"can't reach SearXNG at {base} ({type(e).__name__}). Is it running? "
                        "Check Settings → Search engine URL.")
    if r.status_code == 403:
        raise ToolError(f"SearXNG at {base} returned 403. Enable the JSON format in its settings.yml "
                        "(search.formats must include 'json') and restart it.")
    if r.status_code != 200:
        raise ToolError(f"SearXNG at {base} returned HTTP {r.status_code}.")
    try:
        data = r.json()
    except ValueError:
        raise ToolError(f"SearXNG at {base} did not return JSON. Add 'json' to search.formats in its "
                        "settings.yml and restart it.")
    items = [(it.get("title", ""), it.get("url", ""), it.get("content", ""))
             for it in (data.get("results") or []) if it.get("url")]
    return _format_results(items) or "No results for that query. Try different or broader terms."


async def _ddg_scrape(client: httpx.AsyncClient, query: str) -> str:
    """Fallback: scrape DuckDuckGo's HTML endpoint. Fragile — DDG bot-blocks with HTTP 202."""
    r = await client.post("https://html.duckduckgo.com/html/", data={"q": query},
                          headers={"User-Agent": UA}, timeout=20, follow_redirects=True)
    if r.status_code != 200 or "anomaly" in r.text.lower() or 'class="result' not in r.text:
        raise ToolError("DuckDuckGo is blocking automated searches from this network (its bot check "
                        "tripped). Set up a SearXNG instance and put its URL in Settings → Search "
                        "engine URL for reliable, keyless search.")
    items = []
    for block in re.split(r'<div class="result results_links', r.text)[1:]:
        link = re.search(r'class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>', block, re.S)
        if not link:
            continue
        url = link.group(1)
        if "duckduckgo.com/l/" in url:
            url = unquote(parse_qs(urlparse(url).query).get("uddg", [url])[0])
        snippet = re.search(r'class="result__snippet"[^>]*>(.*?)</a>', block, re.S)
        items.append((link.group(2), url, snippet.group(1) if snippet else ""))
    return _format_results(items) or "No results for that query. Try different or broader terms."


async def web_search(client: httpx.AsyncClient, query: str) -> str:
    base = (get_settings().get("search_url") or "").strip()
    if base:
        return await _searxng(client, base, query)
    return await _ddg_scrape(client, query)


async def web_fetch(client: httpx.AsyncClient, url: str) -> str:
    if urlparse(url).scheme not in ("http", "https"):
        raise ToolError("Only http(s) URLs are supported")
    async with client.stream("GET", url, headers={"User-Agent": UA}, timeout=30, follow_redirects=True) as r:
        ctype = r.headers.get("content-type", "")
        if r.status_code >= 400:
            return f"HTTP {r.status_code} fetching {url}"
        if not ("html" in ctype or ctype.startswith("text/") or "json" in ctype or "xml" in ctype):
            return f"Fetched {url} but it is not text ({ctype or 'unknown type'})."
        chunks, size, truncated = [], 0, False
        async for chunk in r.aiter_bytes():
            chunks.append(chunk)
            size += len(chunk)
            if size > MAX_FETCH:  # don't read a huge file into memory; the clip below drops the rest anyway
                truncated = True
                break
        raw = b"".join(chunks)
        final_url = r.url
    text = raw.decode(r.encoding or "utf-8", errors="replace")
    if "html" in ctype:
        parser = _TextExtractor()
        parser.feed(text)
        body = f"# {parser.title.strip()}\n\n{parser.text()}" if parser.title.strip() else parser.text()
    else:
        body = text
    note = f"\n\n[page truncated at {MAX_FETCH // 1_000_000} MB]" if truncated else ""
    return f"URL: {final_url}\n\n{_clip(body, 30_000)}{note}"


async def call(name: str, args: dict, ws: Path, client: httpx.AsyncClient, shell_timeout: float | None = SHELL_TIMEOUT,
               on_progress=None) -> str:
    try:
        if name == "list_dir":
            return list_dir(ws, args.get("path") or ".")
        if name == "read_file":
            return read_file(ws, args["path"], args.get("start_line"), args.get("end_line"))
        if name == "write_file":
            return write_file(ws, args["path"], args["content"])
        if name == "edit_file":
            return edit_file(ws, args["path"], args["old_text"], args["new_text"])
        if name == "run_shell":
            return await run_shell(ws, args["command"], shell_timeout, bool(args.get("background")), on_progress)
        if name == "web_search":
            return await web_search(client, args["query"])
        if name == "web_fetch":
            return await web_fetch(client, args["url"])
    except KeyError as e:
        raise ToolError(f"missing required argument {e}")
    except httpx.HTTPError as e:
        raise ToolError(f"network error: {type(e).__name__}: {e}")
    raise ToolError(f"unknown tool '{name}'")
