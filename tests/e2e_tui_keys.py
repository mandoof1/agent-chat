"""Every input of the Rust TUI, driven in a pseudo-terminal against the app on AC_URL wired to
tests/mock_llm.py (MOCK_URL): the keys of every focus and dialog, the mouse, every form's save,
cancel and checks, both answers of every question, state.json across restarts, the command-line
flags, and the app going away and coming back (or stalling). Needs the binary (TUI_BIN, default
tui/target/release/agent-chat-tui) and pyte: `uv run --with pyte python tests/e2e_tui_keys.py`.

It shares the app with the other suites (their chats, agents and memories are there; settings may
have been changed), so it makes its own chats and agents with this run's stamp in their names and
puts back the settings it changes. What needs an app to itself (stopping it, a dead model server,
deleting every idle chat, the email account) runs against a scratch app of its own on a free port
(KEYS_SCRATCH_PORT picks one).
Each section runs on its own: a failing one prints its screen and the rest still run.
KEYS_ONLY=s_cli,s_files runs some of them.
"""
import base64
import json
import os
import re
import signal
import socket
import subprocess
import sys
import tempfile
import time
import traceback

import httpx

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tui_driver import Tui  # noqa: E402

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")
MOCK = os.environ.get("MOCK_URL", "http://127.0.0.1:8766/v1")
BIN = os.environ.get("TUI_BIN") or next((p for p in ("tui/target/release/agent-chat-tui", "tui/target/debug/agent-chat-tui") if os.path.exists(p)), None)
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
H = {"X-Agent-Chat": "1"}
STAMP = f"{int(time.time() * 10) % 10_000_000:07d}"  # in every name this run makes
WORK = tempfile.mkdtemp(prefix="agent-chat-tui-keys-")  # state.json folders, exports, files, the scratch app
# the dark theme's colors as pyte reports them
ACCENT, AMBER, SURFACE3, DANGER = "4d7cff", "ffb020", "1e2330", "ff5c5c"
SETTINGS = ("base_url", "bypass_approvals", "auto_title", "auto_memory", "max_steps", "auto_compact", "compact_at")


def call(base, method, path, body=None):
    r = httpx.request(method, base + path, json=body, headers=H, timeout=30)
    r.raise_for_status()
    return r.json()


def api(method, path, body=None):
    return call(B, method, path, body)


def section(name):
    print(f"== {name}", flush=True)


def expect(t, needle, timeout=15, what=None):
    if not t.wait_for(needle, timeout):
        raise AssertionError(f"expected {what or needle!r} on screen")


def absent(t, needle, what=None):
    if t.find(needle):
        raise AssertionError(f"did not expect {what or needle!r} on screen")


def gone(t, needle, timeout=10, what=None):
    """Wait for `needle` to leave the screen."""
    end = time.time() + timeout
    while time.time() < end:
        t.pump(0.2)
        if not t.find(needle):
            return
    raise AssertionError(f"expected {what or needle!r} to go from the screen")


def until(pred, timeout=15, t=None, what="the condition"):
    """Poll `pred` (reading the TUI's screen meanwhile, so it never blocks on output)."""
    end = time.time() + timeout
    while time.time() < end:
        v = pred()
        if v:
            return v
        if t:
            t.pump(0.2)
        else:
            time.sleep(0.2)
    raise AssertionError(f"timed out waiting for {what}")


SOFT = []  # checks that failed without stopping their section (it goes on to test the rest)


def soft(ok, what):
    """Check `ok`, and on failure note it and carry on: the section fails at its end."""
    if not ok:
        print(f"   FAILED CHECK: {what}", flush=True)
        SOFT.append(what)
    return ok


# ------------------------------------------------------------- the app's data

def chats(base=B):
    return call(base, "GET", "/api/chats")


def summary(cid, base=B):
    return next((c for c in chats(base) if c["id"] == cid), None)


def chat(cid, base=B):
    return call(base, "GET", f"/api/chats/{cid}")


def mine(cid, base=B):
    return [m["content"] for m in chat(cid, base)["messages"] if m["role"] == "user"]


def settle(cid, more_than=0, t=None, base=B, timeout=40):
    """Until the chat has more than `more_than` messages and nothing runs in it."""
    def ready():
        c = summary(cid, base)
        return c and c["status"] == "idle" and c["messages"] > more_than
    until(ready, timeout, t, f"chat {cid} to finish")
    if t:
        t.pump(0.6)


def new_chat(title, agent="assistant", base=B):
    """A chat with a title of our own (so the model never renames it)."""
    cid = call(base, "POST", "/api/chats", {"agent_id": agent})["id"]
    call(base, "PATCH", f"/api/chats/{cid}", {"title": title})
    return cid


def seed(title, messages=("hello",), agent="assistant", base=B):
    """A chat with these messages sent and answered."""
    cid = new_chat(title, agent, base)
    for m in messages:
        n = summary(cid, base)["messages"]
        call(base, "POST", f"/api/chats/{cid}/run", {"content": m})
        settle(cid, n, base=base)
    return cid


def agent_index(aid, base=B):
    return [a["id"] for a in call(base, "GET", "/api/state")["agents"]].index(aid)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Scratch:
    """An app of our own on a free port with its own data, to stop and start again, or to change
    what no other suite should see changed (the model server, every idle chat, the email
    account). It talks to the same mock model."""

    def __init__(self):
        self.port = int(os.environ.get("KEYS_SCRATCH_PORT") or free_port())
        self.url = f"http://127.0.0.1:{self.port}"
        self.dir = tempfile.mkdtemp(prefix="scratch-app-", dir=WORK)
        self.proc = None

    def start(self):
        env = os.environ | {"AGENT_CHAT_DATA": os.path.join(self.dir, "data"), "AGENT_CHAT_WORKSPACE": os.path.join(self.dir, "ws")}
        log = open(os.path.join(self.dir, "app.log"), "ab")
        self.proc = subprocess.Popen([sys.executable, "-m", "uvicorn", "app.main:app", "--port", str(self.port), "--log-level", "warning"],
                                     cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)

        def up():
            try:
                return httpx.get(self.url + "/api/health", timeout=2).status_code == 200
            except httpx.HTTPError:
                return False
        until(up, 40, what="the scratch app to start")
        self.api("PUT", "/api/settings", {"base_url": MOCK, "auto_memory": False, "auto_title": False, "bypass_approvals": False})

    def kill(self):
        """Gone at once, as in a crash (a polite stop would wait for the open streams)."""
        if self.proc and self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait(10)

    def api(self, method, path, body=None):
        return call(self.url, method, path, body)


SCRATCH = None


def scratch():
    global SCRATCH
    if SCRATCH is None:
        SCRATCH = Scratch()
    if SCRATCH.proc is None or SCRATCH.proc.poll() is not None:
        SCRATCH.start()
    return SCRATCH


# --------------------------------------------------------------- the terminal

OPEN = []  # the TUIs a section started, closed when it ends


def new_xdg():
    return tempfile.mkdtemp(prefix="xdg-", dir=WORK)


def state_of(xdg):
    try:
        with open(os.path.join(xdg, "agent-chat-tui", "state.json")) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def launch(url=None, cols=120, rows=40, extra=(), argv=None, env=None, xdg=None, cwd=None, side=True):
    """A TUI with a state.json of its own (unless `xdg` names one to share). A chat opened with
    --chat starts with its message box focused; with `side` (the sections were written for it) the
    sidebar takes the focus once it is up, on the open chat's row."""
    xdg = xdg or new_xdg()
    t = Tui(BIN, url or B, cols=cols, rows=rows, extra=extra, cwd=cwd or WORK, env={"XDG_CONFIG_HOME": xdg, **(env or {})}, argv=argv)
    t.xdg = xdg
    OPEN.append(t)
    if side and "--chat" in (*extra, *(argv or ())):
        expect(t, "Agent Chat", 15, "the first frame")
        to_side(t)
    return t


def to_side(t):
    """Shift+Tab until the sidebar has the focus (box -> trace -> sidebar)."""
    for _ in range(4):
        if side_selected(t):
            return
        t.key("backtab", 0.25)
    assert side_selected(t), "the sidebar never took the focus"


def close(t):
    if t.pid:
        t.close()
        t.pid = None


def quit_ok(t, key="ctrl-q"):
    t.key(key, 0.3)
    status = t.wait_exit(10)
    assert status is not None, "the TUI didn't quit"
    assert os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0, f"exit status {status}"


def alive(t):
    return t.pid is not None and os.waitpid(t.pid, os.WNOHANG) == (0, 0)


def bg(t, x, y):
    return t.screen.buffer[y][x].bg


def fg(t, x, y):
    return t.screen.buffer[y][x].fg


def row_of(t, needle):
    return next((y for y, l in enumerate(t.lines()) if needle in l), None)


def line_with(t, needle):
    return next((l for l in t.lines() if needle in l), None)


def side_line(t, needle):
    """The sidebar's part of the first row whose sidebar part holds `needle`."""
    return next((l[:29] for l in t.display() if needle in l[:29]), None)


def side_selected(t):
    """The sidebar row drawn as selected (it is only while the sidebar has the focus), or None."""
    return next((l[:29] for y, l in enumerate(t.display()) if 1 < y < t.rows - 1 and l[29:30] == "│" and bg(t, 0, y) == SURFACE3), None)


def box_focused(t, x=30):
    """The message box's border is drawn in the accent color while it has the focus."""
    d = t.display()
    bottom = max(y for y in range(t.rows) if d[y][x] == "└")
    top = max(y for y in range(bottom) if d[y][x] == "┌")
    return fg(t, x, top) == ACCENT


def trace_focused(t, x=30):
    """A thin accent bar runs down the trace's left edge while it has the focus."""
    return t.display()[3][x] == "▏"


def to_box(t):
    """Click the trace: the focus goes to the message box of a chat you write in."""
    t.click(70, 8, 0.4)


def say(t, text, wait=0.5):
    """Type a message (one paste-like write) and send it."""
    t.send(text, 0.3)
    t.key("enter", wait)


def osc52(t, since=0):
    m = re.findall(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", t.log[since:])
    return base64.b64decode(m[-1]).decode() if m else None


# ------------------------------------------------- agents, sidebar, drawer, box

def keep_agent(name, **fields):
    """An agent of ours, made once and kept for later runs (so the agent list doesn't grow every
    run); chats left with it by an earlier run are removed first."""
    aid = next((a["id"] for a in api("GET", "/api/state")["agents"] if a["name"] == name), None)
    if aid is None:
        aid = api("POST", "/api/agents", {"name": name, **fields})["id"]
    for c in chats():
        if c["agent_id"] == aid:
            api("DELETE", f"/api/chats/{c['id']}")
    return aid


def to_agent_row(t, aid):
    """Select an agent's row in the sidebar (which has the focus): Home, then down past All chats."""
    t.key("home")
    for _ in range(agent_index(aid) + 1):
        t.key("j", 0.1)


def list_top(t):
    """The first row of the sidebar's chat list."""
    return row_of(t, " n  new chat") + 2


def files_selected(t):
    """The drawer row drawn as selected (only while the drawer has the focus), or None."""
    x = t.cols - 31
    return next((l[x:] for y, l in enumerate(t.display()) if 1 < y < t.rows - 1 and l[x - 1:x] == "│" and bg(t, x, y) == SURFACE3), None)


def box_text(t):
    """The message box's text rows (between its borders, without the footer row)."""
    d = t.display()
    bottom = max(y for y in range(t.rows) if "└" in d[y][28:])
    x = d[bottom].index("└", 28)
    top = max(y for y in range(bottom) if d[y][x] == "┌")
    rows = [d[y][x + 2:].split("│")[0].rstrip() for y in range(top + 1, bottom - 1)]
    return [""] if rows[0].strip().startswith("Message  (Enter sends") else rows  # (the empty box's placeholder)


def chip(t, icon, name, shown=True, timeout=3):
    """An attachment chip above the box (pyte puts a blank after a wide icon)."""
    pat = re.compile(rf"{icon}\s+{re.escape(name)} ")
    if shown:
        until(lambda: pat.search(t.text()), timeout, t, f"the {name} chip")
    else:
        t.pump(0.3)
        assert not pat.search(t.text()), f"the {name} chip is still there"


# ------------------------------------------------------- toasts and dialogs

def toasts(t):
    """The text in every box drawn over the trace (toasts, and whatever else is boxed there),
    each one's rows joined: a long toast wraps."""
    d, out = t.display(), []
    for y, l in enumerate(d):
        for x0 in [i for i, c in enumerate(l) if c == "┌" and i >= 29]:
            x1 = l.find("┐", x0)
            if x1 < 0 or set(l[x0 + 1:x1]) - {"─"}:
                continue
            rows, yy = [], y + 1
            while yy < t.rows and d[yy][x0] == "│":
                rows.append(d[yy][x0 + 1:x1].strip())
                yy += 1
            out.append(" ".join(rows))
    return out


def toasted(t, text, timeout=10):
    """Wait for a toast saying `text` (compared without spaces: a wrapped row may split a word)."""
    want = text.replace(" ", "")
    until(lambda: any(want in x.replace(" ", "") for x in toasts(t)), timeout, t, f"a toast saying {text!r}")


def palette_selected(t):
    """The palette's selected row (its ▸ sits under the query's ⌕)."""
    lines = t.lines()
    x = next((l.index("│ ⌕") + 2 for l in lines if "│ ⌕" in l), None)
    return next((l for l in lines if x is not None and l[x:x + 1] == "▸"), None)


def palette_pick(t, query, title, sub=None):
    """Ctrl+K, type `query`, and move the ▸ to the item called `title` exactly, with a detail
    starting `sub` ("" for any; None for none). Other items may come first: a chat named like a
    command, or a longer title starting the same."""
    t.key("ctrl-k", 0.5)
    t.send(query, 0.8)
    pat = re.compile(rf"▸ {re.escape(title)}\s{{2,}}" + ("│" if sub is None else re.escape(sub)))
    for _ in range(60):
        if pat.search(palette_selected(t) or ""):
            return
        t.key("down", 0.1)
    raise AssertionError(f"{title!r} not found in the palette for {query!r}")


def open_by(t, title):
    """Open a chat by its exact title through the palette."""
    palette_pick(t, title, title, "")
    t.key("enter", 1.0)
    assert title in t.lines()[0], f"opening {title!r}: {t.lines()[0]!r}"


def focused_label(t, label):
    """A form field's label is drawn in the accent color while the field has the focus."""
    y = row_of(t, label)
    return y is not None and fg(t, t.lines()[y].index(label), y) == ACCENT


def field_value(t, label):
    """The row under a text field's label (its value), trimmed."""
    y = row_of(t, label)
    x = t.lines()[y].index(label)
    return t.lines()[y + 1][x:].split("│")[0].strip()


def focus_shown(t):
    """The focused form field is on screen: its label (or button) is drawn bold in the accent."""
    return any(c.bold and ACCENT in (c.fg, c.bg) for y in range(t.rows) for c in t.screen.buffer[y].values())


# ------------------------------------------------------ memory and routines

def memories(q=""):
    return api("GET", f"/api/memory?q={q}")["memories"]


def routines():
    return api("GET", "/api/routines")


def select_routine(t, rid):
    """Move the Routines list's selection (it opens on the first) to this routine."""
    for _ in range([r["id"] for r in routines()].index(rid)):
        t.key("j", 0.12)


SECTIONS = []


def sect(fn):
    SECTIONS.append(fn)
    return fn


# =================================================================== sections

@sect
def s_cli():
    section("command line: --version, --help, an unreachable or bad --url exits 1 before the alternate screen, AGENT_CHAT_URL with a trailing slash, --chat opens a chat")
    v = subprocess.run([BIN, "--version"], capture_output=True, text=True, timeout=10)
    assert v.returncode == 0 and v.stdout.startswith("agent-chat-tui "), v
    h = subprocess.run([BIN, "--help"], capture_output=True, text=True, timeout=10)
    assert h.returncode == 0, h
    for flag in ("--url", "--chat", "--light", "--plain", "--quiet", "AGENT_CHAT_URL"):
        assert flag in h.stdout, f"{flag} not in --help:\n{h.stdout}"
    for url in ("http://127.0.0.1:1", "bad"):
        t = launch(argv=["--url", url, "--quiet"])
        status = t.wait_exit(15)
        assert status is not None and os.WIFEXITED(status) and os.WEXITSTATUS(status) == 1, f"--url {url}: exit status {status}"
        assert f"Can't reach Agent Chat at {url}".encode() in t.log and b"--url" in t.log, t.log[-300:]
        assert b"\x1b[?1049h" not in t.log, "it entered the alternate screen before giving up"
    # no --url: the environment's, its trailing slash trimmed (requests to //api would fail)
    first = api("GET", "/api/state")["agents"][0]["name"]
    before = {c["id"] for c in chats()}
    t = launch(argv=["--quiet"], env={"AGENT_CHAT_URL": B + "/"})
    expect(t, "Pick an agent.")
    t.type("1")
    expect(t, f"{first} is ready", 10, "a chat with the first agent, made through AGENT_CHAT_URL")
    assert any(c["id"] not in before for c in chats()), "no chat was made"
    close(t)
    # --chat opens that chat with no key pressed
    title = f"Flag chat {STAMP}"
    cid = seed(title)
    t = launch(extra=("--chat", cid), side=False)
    expect(t, "Done.", 10, "the chat's saved reply")
    assert title in t.lines()[0], t.lines()[0]
    # ...with its message box focused: what is typed first is a message, not sidebar shortcuts
    # (e opened the agent editor, and the rest of the words became the agent's name)
    names = {a["id"]: a["name"] for a in api("GET", "/api/state")["agents"]}
    assert box_focused(t), "the message box of a chat opened with --chat has no focus"
    t.type("hello there", gap=0.05)
    t.key("enter", 0.5)
    until(lambda: mine(cid) == ["hello", "hello there"], 10, t, "the typed message sent")
    absent(t, "Esc  cancel", "the agent editor (a form)")
    assert {a["id"]: a["name"] for a in api("GET", "/api/state")["agents"]} == names, "an agent was renamed by the typing"
    settle(cid, 2, t)


@sect
def s_themes():
    section("--light and --plain palettes; /theme is kept in state.json, and a flag wins over it")
    t = launch(extra=("--light",))
    expect(t, "Pick an agent.")
    assert (bg(t, 60, 20), bg(t, 5, 20)) == ("f2f3f7", "ffffff"), (bg(t, 60, 20), bg(t, 5, 20))
    close(t)
    t = launch(extra=("--plain",))
    expect(t, "Pick an agent.")
    fills = {t.screen.buffer[y][x].bg for y in range(t.rows) for x in range(t.cols)}
    dark = {"0b0d12", "11141b", "171b24", "1e2330", "f2f3f7", "ffffff"}
    assert bg(t, 60, 20) == "default" and not fills & dark, f"--plain painted backgrounds: {sorted(fills)}"
    close(t)
    title = f"Theme chat {STAMP}"
    cid = new_chat(title)
    t = launch(extra=("--chat", cid))
    expect(t, title)
    to_box(t)
    say(t, "/theme light")
    expect(t, "Theme: light", 5)
    until(lambda: (state_of(t.xdg) or {}).get("theme") == "light", 5, t, "state.json to keep the theme")
    quit_ok(t)
    t2 = launch(xdg=t.xdg)
    expect(t2, "Pick an agent.")
    assert bg(t2, 60, 20) == "f2f3f7", f"the saved theme wasn't used: {bg(t2, 60, 20)}"
    close(t2)
    t3 = launch(xdg=t.xdg, extra=("--plain",))
    expect(t3, "Pick an agent.")
    assert bg(t3, 60, 20) == "default", f"--plain should win over the saved theme: {bg(t3, 60, 20)}"


@sect
def s_bell():
    section("the bell: one BEL when an approval comes in without --quiet, none with it")
    rang = {}
    for quiet in (False, True):
        title = f"Bell {'quiet' if quiet else 'loud'} {STAMP}"
        cid = new_chat(title, "coder")
        t = launch(argv=["--url", B, "--chat", cid] + (["--quiet"] if quiet else []))
        expect(t, title)
        t.pump(1.0)
        mark = len(t.log)
        api("POST", f"/api/chats/{cid}/run", {"content": "shell: echo bell"})
        expect(t, "needs approval", 25)
        t.pump(1.5)
        bells = re.sub(rb"\x1b\][^\x07]*\x07", b"", t.log[mark:]).count(b"\x07")  # (OSC sequences end in BEL too)
        t.key("alt-n", 0.5)
        settle(cid, 1, t)
        rang[quiet] = bells
        close(t)
    assert rang[True] == 0, f"{rang[True]} bells with --quiet"
    assert rang[False] == 1, f"{rang[False]} bells for one approval (without --quiet): the approval event and the chat turning 'waiting' both ring"


@sect
def s_persist():
    section("state.json: made with its folder, written on every toggle (verbose, sidebar, drawer, filter, seen), restored on the next start; a broken one falls back to defaults")
    title = f"Persist {STAMP}"
    seed(title, ["hello there"])
    xdg = os.path.join(new_xdg(), "not", "made", "yet")
    t = launch(xdg=xdg)
    expect(t, "Pick an agent.")
    until(lambda: state_of(xdg) is not None, 5, t, "state.json to be written at the first start")
    assert state_of(xdg)["seen"], "the first start marks every chat seen"
    t.key("alt-o")
    until(lambda: state_of(xdg)["verbose"] is True, 3, t, "verbose in state.json")
    t.key("alt-f", 0.8)
    until(lambda: state_of(xdg)["files"] is True, 3, t, "the drawer in state.json")
    t.key("esc")  # (the drawer took the focus; Esc hands it back, to the sidebar with no chat open)
    t.key("home")
    for _ in range(agent_index("coder") + 1):
        t.key("j", 0.15)
    t.key("enter", 0.6)
    expect(t, "Coder chats", 3)
    until(lambda: state_of(xdg)["filter"] == "coder", 3, t, "the filter in state.json")
    t.key("alt-b")
    until(lambda: state_of(xdg)["sidebar"] is False, 3, t, "the hidden sidebar in state.json")
    absent(t, "Agent Chat")
    quit_ok(t)

    t = launch(xdg=xdg)
    expect(t, "Workspace", 8, "the drawer, open as it was left")
    expect(t, "Coder", 3, "the drawer listing the filter's agent")
    absent(t, "Agent Chat", "the sidebar (hidden last time)")
    t.key("ctrl-k", 0.5)
    t.send(title, 1.0)
    t.key("enter", 1.5)
    expect(t, "The user said: hello there", 8, "the thinking text (verbose kept from last time)")
    t.key("alt-b", 0.5)
    expect(t, "Coder chats", 3, "the filter kept from last time")
    quit_ok(t)

    with open(os.path.join(xdg, "agent-chat-tui", "state.json"), "w") as f:
        f.write("{not json")
    t = launch(xdg=xdg)
    expect(t, "Pick an agent.", 8, "a start with a broken state.json")
    expect(t, "Agent Chat", 2, "the sidebar shown (the default)")
    absent(t, "Workspace")
    t.key("alt-o")
    until(lambda: (state_of(xdg) or {}).get("verbose") is True, 3, t, "a readable state.json again")


@sect
def s_welcome():
    section("welcome page: numbered agents with what they can use ('Shell', 'Chat only'), the five newest chats, the memory hint only while memory is empty")
    name = f"Plain{STAMP}"
    aid = api("POST", "/api/agents", {"name": name, "emoji": "🗨", "purpose": "talks", "tools": []})["id"]
    try:
        title = f"Recent {STAMP}"
        new_chat(title)
        agents = api("GET", "/api/state")["agents"]
        t = launch(rows=70)
        expect(t, name)
        assert "Chat only" in line_with(t, f"{name}  talks"), line_with(t, f"{name}  talks")
        coder = next(a for a in agents if a["id"] == "coder")
        assert "Shell" in line_with(t, f"{coder['name']}  {coder['purpose'][:20]}"), line_with(t, coder["purpose"][:20])
        assert re.search(rf"│\s+1 \S+\s+{re.escape(agents[0]['name'])}  {re.escape(agents[0]['purpose'][:20])}", t.text()), "the first agent numbered 1 with its purpose"
        expect(t, "Pick up where you left off")
        assert line_with(t, f"{title}  Assistant · just now"), "the new chat listed, 'just now'"
        top = row_of(t, "Pick up where you left off")
        listed = [l for l in t.lines()[top + 1:top + 8] if " · " in l and ("ago" in l or "just now" in l)]
        assert 1 <= len(listed) <= 5, listed
        assert not any("↳" in l for l in listed)
        if api("GET", "/api/memory")["total"] == 0:
            expect(t, "They don't know you yet", 2)
        else:
            absent(t, "They don't know you yet")
    finally:
        api("DELETE", f"/api/agents/{aid}")


@sect
def s_offline():
    section("the model server unreachable: the welcome page, the sidebar and Settings say so; they recover on their own once it is back")
    s = scratch()
    s.api("PUT", "/api/settings", {"base_url": "http://127.0.0.1:9/v1"})
    try:
        t = launch(url=s.url)
        expect(t, "Model server offline", 10)
        expect(t, "isn't reachable at http://127.0.0.1:9", 3)
        expect(t, "model offline", 3, "the sidebar's status line")
        assert fg(t, 1, t.rows - 1) == DANGER, "the status dot in red"
        t.key("alt-s", 1.0)
        expect(t, "Server: not reachable", 3)
        t.key("esc", 0.4)
    finally:
        s.api("PUT", "/api/settings", {"base_url": MOCK})
    expect(t, "Local · mock-model", 15, "the welcome page back (the server is checked every 10 s)")
    expect(t, "● mock-model", 3)
    absent(t, "model offline")


@sect
def s_restart():
    section("the app stops and starts again: the TUI keeps running, says so, keeps unsent text, reconnects both streams; a reply cut off by the stop doesn't stay 'running'")
    s = scratch()
    title = f"Restart {STAMP}"
    cid = seed(title, ["hello"], base=s.url)
    t = launch(url=s.url, extra=("--chat", cid))
    expect(t, "Done.", 10)
    to_box(t)
    s.kill()
    expect(t, "Lost the chat stream", 10)
    expect(t, "model offline", 15, "the status line once the app is gone")
    t.type("after the restart")
    t.key("enter", 1.0)
    expect(t, "request failed", 5, "a toast for the message that couldn't be sent")
    expect(t, "│ after the restart", 2, "the unsent text kept in the box")
    assert alive(t), "the TUI quit when the app went away"
    s.start()
    expect(t, "● mock-model", 15, "the status line once the app is back")
    s.api("PATCH", f"/api/chats/{cid}", {"title": f"Restarted {STAMP}"})
    expect(t, f"Restarted {STAMP}", 10, "a change made after the restart (the event stream reconnected)")
    t.key("enter", 0.5)
    settle(cid, 2, t, s.url)
    assert mine(cid, s.url)[-1] == "after the restart", mine(cid, s.url)
    expect(t, "after the restart", 3)
    # stopped mid-reply
    say(t, "slow please")
    expect(t, "● running", 10)
    expect(t, "word3", 10)
    s.kill()
    expect(t, "Lost the chat stream", 10)
    s.start()
    gone(t, "● running", 20, "the running mark once the app came back without the run")
    expect(t, "slow please", 3)
    assert alive(t)


@sect
def s_stall():
    section("the app stalls (there but not answering): the TUI keeps taking keys, and Ctrl+Q still quits")
    s = scratch()
    t = launch(url=s.url)
    expect(t, "Pick an agent.")
    os.kill(s.proc.pid, signal.SIGSTOP)
    try:
        t.pump(12)  # (past the next 10 s check of the model server)
        t.key("ctrl-k", 0.8)
        soft(t.find("Search and commands"), "with the app stalled the TUI stops taking keys (Ctrl+K opens nothing): its requests have no timeout and are awaited on the loop that reads the keys")
        t.key("esc", 0.3)
        t.key("ctrl-q", 0.3)
        soft(t.wait_exit(5) is not None, "with the app stalled Ctrl+Q doesn't quit (it does once the app answers again)")
    finally:
        os.kill(s.proc.pid, signal.SIGCONT)
    if t.pid:
        t.wait_exit(15)  # (the keys it held are handled once the app answers)


@sect
def s_rejoin():
    section("opening a chat while its reply streams: the saved part, then the live rest, each word once")
    a = new_chat(f"Rejoin {STAMP}")
    b = new_chat(f"Rejoin other {STAMP}")
    t = launch(extra=("--chat", b))
    expect(t, f"Rejoin other {STAMP}")
    api("POST", f"/api/chats/{a}/run", {"content": "slow please"})
    until(lambda: summary(a)["status"] == "running", 10, t, "the reply to start")
    t.pump(1.5)
    open_by(t, f"Rejoin {STAMP}")
    expect(t, "● running", 5)
    expect(t, "word3 ", 10)
    garbled, end = [], time.time() + 3
    while time.time() < end:
        t.pump(0.15)
        nums = [int(x) for x in re.findall(r"word(\d+)", t.text())][:-1]  # (the last may be half streamed)
        garbled += [(x, y) for x, y in zip(nums, nums[1:]) if y != x + 1]
    assert not garbled, f"words out of order after joining mid-reply: {garbled[:5]}"
    t.key("ctrl-s", 0.5)
    settle(a, 1, t)


@sect
def s_events():
    section("other clients' changes show with no key pressed: a new chat, a retitle and a deletion in the sidebar; a rename of the open chat in its header")
    tok = f"ev{STAMP}"
    t = launch()
    expect(t, "Pick an agent.")
    t.key("/")
    t.send(tok, 0.6)
    t.key("enter", 0.4)
    expect(t, "No chats match.", 3)
    cid = new_chat(f"{tok} made elsewhere")
    expect(t, f"{tok} made elsewhere", 5)
    api("PATCH", f"/api/chats/{cid}", {"title": f"{tok} retitled elsewhere"})
    expect(t, f"{tok} retitled elsewhere", 5)
    api("DELETE", f"/api/chats/{cid}")
    gone(t, f"{tok} retitled", 5)
    expect(t, "No chats match.", 3)
    close(t)
    title = f"Header {STAMP}"
    cid = seed(title)
    t = launch(extra=("--chat", cid))
    expect(t, "Done.", 10)
    api("PATCH", f"/api/chats/{cid}", {"title": f"Header renamed {STAMP}"})
    expect(t, f"Header renamed {STAMP}", 5, "the new title somewhere (the sidebar)")
    t.pump(1.0)
    assert f"Header renamed {STAMP}" in t.lines()[0], f"the header kept the old title: {t.lines()[0]!r}"


@sect
def s_sidebar_keys():
    section("sidebar keys: PgDn/PgUp move 8 rows, Home/End, j/k step over the group headers; an agent row filters (with 'No chats yet' when it has none), n starts a chat with it, All chats clears; Enter opens a chat and gives the box the focus")
    aid = keep_agent("KeysNav", emoji="🧪", purpose="sidebar tests", tools=[])
    agents = api("GET", "/api/state")["agents"]
    at = [a["id"] for a in agents].index(aid)
    tok = f"nv{STAMP}"
    t = launch()
    expect(t, "Pick an agent.")
    assert "All chats" in side_selected(t), side_selected(t)
    t.key("pgdn")
    assert agents[7]["name"][:8] in side_selected(t), (agents[7]["name"], side_selected(t))
    t.key("pgup")
    assert "All chats" in side_selected(t)
    for _ in range(at + 1):
        t.key("j", 0.15)
    assert "KeysNav" in side_selected(t)
    t.key("enter", 0.6)
    expect(t, "KeysNav chats", 3)
    expect(t, "No chats yet. Press n.", 3)
    ids = [new_chat(f"{tok} n{i:02d}", aid) for i in range(24)]  # n00 the oldest, listed last
    api("PATCH", f"/api/chats/{ids[5]}", {"pinned": True})
    try:
        expect(t, f"{tok} n23", 5, "chats made elsewhere, listed under the filter")
        expect(t, " Pinned", 3)
        t.key("end")
        assert f"{tok} n00" in (side_selected(t) or ""), side_selected(t)
        t.key("k")
        assert f"{tok} n01" in side_selected(t), side_selected(t)
        t.key("home")
        assert "All chats" in side_selected(t)
        for _ in range(len(agents)):
            t.key("j", 0.12)
        assert agents[-1]["name"][:8] in side_selected(t), side_selected(t)
        t.key("j")
        assert f"⚲{tok} n05" in side_selected(t), f"j from the last agent should skip the 'Pinned' header: {side_selected(t)!r}"
        t.key("j")
        assert f"{tok} n23" in side_selected(t), f"and the 'Today' header: {side_selected(t)!r}"
        t.key("k")
        assert f"{tok} n05" in side_selected(t), side_selected(t)

        section("the wheel scrolls the sidebar's list, and back; clicks open a chat or filter by an agent")
        top = list_top(t)
        first = t.lines()[top][:29]
        for _ in range(2):
            t.send(f"\x1b[<65;5;{top + 3}M", 0.3)  # wheel down over the list
        assert t.lines()[top][:29] != first and tok in t.lines()[top], f"the wheel didn't scroll the list: {t.lines()[top]!r}"
        for _ in range(4):
            t.send(f"\x1b[<64;5;{top + 3}M", 0.3)
        assert t.lines()[top][:29].strip() == "Pinned", f"the wheel up didn't scroll back to the top: {t.lines()[top]!r}"
        t.click(8, row_of(t, f"{tok} n22"), 1.0)
        expect(t, "KeysNav is ready", 5)
        assert f"{tok} n22" in t.lines()[0], t.lines()[0]
        assert box_focused(t), "a click on a chat hands the message box the focus"
        t.type("z")
        expect(t, "│ z ", 2)
        t.key("ctrl-c")
        t.click(8, row_of(t, "  Coder  "), 0.8)
        expect(t, "Coder chats", 3, "a click on an agent filters by it")
        t.click(8, row_of(t, "All chats"), 0.8)
        expect(t, " Chats ", 3)
        absent(t, "Coder chats")

        section("Enter on a chat opens it, focus to the box; n on an agent row starts a chat with it, on a chat row asks which agent; the status line")
        t.key("home")
        for _ in range(at + 1):
            t.key("j", 0.12)
        t.key("enter", 0.6)  # filter: KeysNav
        expect(t, "KeysNav chats", 3)
        while f"{tok} n23" not in (side_selected(t) or ""):
            t.key("j", 0.12)
        t.key("enter", 1.0)
        assert f"{tok} n23" in t.lines()[0], t.lines()[0]
        assert box_focused(t)
        n = len([c for c in chats() if c["agent_id"] == aid])
        t.key("esc")  # box -> trace
        t.key("esc")  # trace -> sidebar
        t.key("home")
        for _ in range(at + 1):
            t.key("j", 0.12)
        t.key("n", 1.0)
        absent(t, "New chat with")
        until(lambda: len([c for c in chats() if c["agent_id"] == aid]) == n + 1, 5, t, "a KeysNav chat from n")
        expect(t, "KeysNav is ready", 5)
        t.key("esc")
        t.key("esc")
        t.key("home")
        t.key("enter", 0.6)  # All chats
        expect(t, " Chats ", 3)
        while (side_selected(t) or "")[:1] != "▎":  # the open chat's row
            t.key("j", 0.12)
        t.key("n", 0.6)
        expect(t, "New chat with", 3, "the agent picker (n on a chat row, no filter)")
        t.key("esc", 0.4)
        total = api("GET", "/api/memory")["total"]
        status = t.lines()[t.rows - 1][:29]
        assert "● mock-model" in status and (f"◉{total}" in status if total else "◉" not in status), status
    finally:
        api("PATCH", f"/api/chats/{ids[5]}", {"pinned": False})


@sect
def s_sidebar_search():
    section("sidebar filter: / starts it, typing narrows the list (message text too, from the server), Backspace edits, Enter leaves on the first match, Esc clears; a paste goes in; odd characters are fine; Ctrl+C clears it")
    tok = f"sr{STAMP}"
    seed(f"{tok} alpha")
    seed(f"{tok} beta")
    word = f"qz{STAMP}xv"
    seed(f"Gamma {STAMP}", [f"{word} in the text", "hello"])  # only its first message has it
    t = launch()
    expect(t, "Pick an agent.")
    t.key("/")
    expect(t, "/▏", 2)
    t.send(tok, 0.8)
    expect(t, f"/{tok}▏", 3)
    expect(t, " Matches", 3)
    top = list_top(t)
    rows = [l[:29].strip() for l in t.lines()[top:t.rows - 1] if l[:29].strip()]
    assert rows == ["Matches", f"💬  {tok} beta", f"💬  {tok} alpha"], rows
    t.key("backspace")
    expect(t, f"/{tok[:-1]}▏", 2)
    t.type(tok[-1])
    t.key("enter", 0.4)
    absent(t, "▏", "the filter's cursor (Enter leaves typing)")
    assert f"{tok} beta" in side_selected(t), side_selected(t)
    t.key("enter", 1.0)
    assert f"{tok} beta" in t.lines()[0], t.lines()[0]
    assert box_focused(t)
    t.key("esc")  # box -> trace
    t.key("esc")  # trace -> sidebar: the filter is still on
    expect(t, f"/{tok}", 2)
    t.key("esc", 0.4)
    expect(t, "/ filter", 2, "Esc clears the filter")
    absent(t, " Matches")
    # message text found by the server
    t.key("/")
    t.send(word, 1.0)
    expect(t, f"Gamma {STAMP}", 3, "a chat whose message text matches")
    t.key("esc", 0.4)
    t.key("/")
    t.send("zzqq", 0.8)
    expect(t, "No chats match.", 3)
    for _ in range(4):
        t.key("backspace", 0.15)
    absent(t, "No chats match.")
    expect(t, "/▏", 2)
    t.send("a&b#?/", 0.8)
    expect(t, "No chats match.", 3, "a query with &, #, ? and / (encoded for the server)")
    absent(t, "422")
    t.key("esc", 0.4)
    t.key("/")
    t.paste(tok)
    expect(t, f"/{tok}▏", 3, "a paste into the filter")
    expect(t, f"{tok} alpha", 2)
    t.key("ctrl-c", 0.4)
    expect(t, "/ filter", 2, "Ctrl+C clears the filter")
    assert alive(t), "Ctrl+C with a filter to clear quit the app"


@sect
def s_sidebar_rows():
    section("sidebar row keys act on the highlighted chat (r rename, p pin, x and J export, e edit its agent, Alt+D/Alt+P/F2/Alt+X too); outside the sidebar the same shortcuts mean the open chat")
    tok = f"rw{STAMP}"
    a = seed(f"{tok} rowA")
    b = seed(f"{tok} rowB")
    cwd = tempfile.mkdtemp(prefix="exports-", dir=WORK)
    t = launch(extra=("--chat", a), cwd=cwd)
    expect(t, f"{tok} rowA")
    t.key("/")
    t.send(tok, 0.8)
    t.key("enter", 0.4)
    assert f"{tok} rowB" in side_selected(t), side_selected(t)

    def prompt_value():
        y = row_of(t, "Rename chat")
        return t.lines()[y + 1] if y is not None else None

    t.key("r", 0.5)
    expect(t, "Rename chat", 3)
    assert f" {tok} rowB" in prompt_value(), prompt_value()
    t.key("esc", 0.4)
    t.key("p", 0.8)
    expect(t, "Pinned to the top", 3)
    until(lambda: summary(b)["pinned"] and not summary(a)["pinned"], 5, t, "rowB pinned, rowA not")
    expect(t, f"⚲{tok} rowB", 3)
    t.key("p", 0.8)
    expect(t, "Unpinned", 3)
    until(lambda: not summary(b)["pinned"], 5, t, "rowB unpinned")
    t.key("x", 1.0)
    expect(t, "Saved", 3)
    md = os.path.join(cwd, f"{tok}-rowB.md")
    assert os.path.exists(md), os.listdir(cwd)
    text = open(md).read()
    assert text.startswith(f"# {tok} rowB\n") and "hello" in text, text[:200]
    t.key("x", 1.0)
    assert sorted(os.listdir(cwd)) == [f"{tok}-rowB.md"], os.listdir(cwd)
    t.key("J", 1.0)
    data = json.load(open(os.path.join(cwd, f"{tok}-rowB.json")))
    assert data["chat"]["title"] == f"{tok} rowB", data.get("chat", {}).get("title")
    t.key("e", 0.6)
    expect(t, "Edit Assistant", 3)
    t.key("esc", 0.4)
    t.key("alt-d", 0.5)
    expect(t, f"Delete “{tok} rowB”?", 3)
    t.key("n", 0.5)
    absent(t, "Delete “")
    assert summary(b), "n kept the chat"
    t.key("f2", 0.5)
    assert f" {tok} rowB" in prompt_value(), prompt_value()
    t.key("esc", 0.4)
    t.key("alt-p", 0.8)
    until(lambda: summary(b)["pinned"], 5, t, "Alt+P in the sidebar pins the highlighted chat")
    t.key("alt-p", 0.8)
    until(lambda: not summary(b)["pinned"], 5, t, "and unpins it")
    os.remove(md)
    t.key("alt-x", 1.0)
    until(lambda: os.path.exists(md), 5, t, "Alt+X in the sidebar exports the highlighted chat")
    # outside the sidebar: the open chat
    t.key("tab", 0.3)
    assert trace_focused(t)
    t.key("alt-d", 0.5)
    expect(t, f"Delete “{tok} rowA”?", 3, "Alt+D on the open chat (the trace has the focus)")
    t.key("esc", 0.4)
    t.key("f2", 0.5)
    assert f" {tok} rowA" in prompt_value(), prompt_value()
    t.key("esc", 0.4)
    t.key("alt-2", 0.5)
    assert f" {tok} rowA" in prompt_value(), "Alt+2 renames too"
    t.key("esc", 0.4)
    t.key("alt-p", 0.8)
    until(lambda: summary(a)["pinned"] and not summary(b)["pinned"], 5, t, "Alt+P outside the sidebar pins the open chat")
    t.key("alt-p", 0.8)
    until(lambda: not summary(a)["pinned"], 5, t, "and unpins it")
    t.key("alt-x", 1.0)
    until(lambda: os.path.exists(os.path.join(cwd, f"{tok}-rowA.md")), 5, t, "Alt+X outside the sidebar exports the open chat")
    t.key("alt-a", 0.6)
    expect(t, "Edit Assistant", 3)
    t.key("esc", 0.4)
    # an agent row: e and Alt+A edit that agent
    t.key("esc", 0.3)  # trace -> sidebar
    t.key("home")
    for _ in range(agent_index("orchestrator") + 1):
        t.key("j", 0.12)
    t.key("e", 0.6)
    expect(t, "Edit Orchestrator", 3)
    t.key("esc", 0.4)
    t.key("alt-a", 0.6)
    expect(t, "Edit Orchestrator", 3)
    t.key("esc", 0.4)


@sect
def s_sidebar_delete():
    section("deleting from the sidebar: d on a chat asks (n keeps it, y deletes it), on an agent row asks about its idle chats (Esc keeps them, Enter deletes all but pinned and routine ones), on All chats asks about every idle chat")
    aid = keep_agent("KeysBulk", emoji="🧹", purpose="bulk delete tests", tools=[])
    at = agent_index(aid)
    tok = f"dl{STAMP}"
    idle = [new_chat(f"{tok} idle{i}", aid) for i in range(2)]
    pinned = new_chat(f"{tok} pinned", aid)
    api("PATCH", f"/api/chats/{pinned}", {"pinned": True})
    rid = api("POST", "/api/routines", {"name": f"{tok} routine", "agent_id": aid, "prompt": "hello", "schedule": {"type": "interval", "minutes": 600}, "enabled": False})["id"]
    try:
        routine_chat = api("POST", f"/api/routines/{rid}/run")["chat_id"]
        settle(routine_chat, 1)
        t = launch(extra=("--chat", idle[0]))
        expect(t, f"{tok} idle0")
        t.key("home")
        for _ in range(at + 1):
            t.key("j", 0.12)
        assert "KeysBulk" in side_selected(t)
        t.key("d", 0.5)
        expect(t, "Delete KeysBulk's idle chats?", 3)
        t.key("esc", 0.5)
        absent(t, "idle chats?")
        assert all(summary(c) for c in idle), "Esc deleted chats"
        t.key("enter", 0.6)  # filter by it, to see its chats
        expect(t, "KeysBulk chats", 3)
        while f"{tok} idle1" not in (side_selected(t) or ""):
            t.key("j", 0.12)
        t.key("d", 0.5)
        expect(t, f"Delete “{tok} idle1”?", 3)
        t.key("n", 0.5)
        assert summary(idle[1]), "n deleted the chat"
        t.key("d", 0.5)
        t.key("y", 1.0)
        expect(t, "Chat deleted", 3)
        until(lambda: summary(idle[1]) is None, 5, t, "the chat deleted on the server")
        gone(t, f"{tok} idle1", 3)
        idle.append(new_chat(f"{tok} idle2", aid))
        expect(t, f"{tok} idle2", 5)
        t.key("home")
        for _ in range(at + 1):
            t.key("j", 0.12)
        t.key("d", 0.5)
        expect(t, "Delete KeysBulk's idle chats?", 3)
        t.key("enter", 1.0)
        expect(t, "Deleted 2 chats", 3, "idle0 (the open one) and idle2; not the pinned or the routine's")
        expect(t, "Pick an agent.", 3, "the open chat was among them: back to the welcome page")
        left = {c["id"] for c in chats() if c["agent_id"] == aid}
        assert left == {pinned, routine_chat}, (left, pinned, routine_chat)
        t.key("home")
        t.key("d", 0.5)
        expect(t, "Delete all idle chats?", 3)
        t.key("esc", 0.5)
        absent(t, "Delete all idle chats?")
        close(t)
        # every idle chat: on the scratch app, which no one else uses
        s = scratch()
        keep = new_chat("Keep me pinned", base=s.url)
        s.api("PATCH", f"/api/chats/{keep}", {"pinned": True})
        for i in range(2):
            new_chat(f"Bulk {i}", base=s.url)
        n = len([c for c in chats(s.url) if c["parent"] is None and c["status"] == "idle" and not c["pinned"] and not c["routine_id"]])
        t = launch(url=s.url)
        expect(t, "Keep me pinned")
        t.key("home")
        t.key("d", 0.5)
        expect(t, "Delete all idle chats?", 3)
        t.key("enter", 1.0)
        expect(t, f"Deleted {n} chats", 3)
        assert [c["id"] for c in chats(s.url) if c["parent"] is None and not c["routine_id"]] == [keep], chats(s.url)
        expect(t, "Keep me pinned", 2)
    finally:
        api("DELETE", f"/api/routines/{rid}")
        api("PATCH", f"/api/chats/{pinned}", {"pinned": False})


@sect
def s_unread():
    section("unread dots: a reply that came in while away marks the chat •, opening it clears that, m marks every chat read (kept in state.json); ' Pinned' above a pinned chat; the open chat's row marked ▎")
    tok = f"ur{STAMP}"
    a = new_chat(f"{tok} first")
    t = launch()
    expect(t, "Pick an agent.")
    t.key("/")
    t.send(tok, 0.8)
    t.key("enter", 0.4)
    expect(t, f"{tok} first", 3)
    seed(f"{tok} second")  # answered while the TUI watches
    expect(t, f"{tok} second", 5)
    until(lambda: side_line(t, f"{tok} second")[27] == "•", 5, t, "the unread dot on the answered chat")
    assert side_line(t, f"{tok} first")[27] == " ", side_line(t, f"{tok} first")
    assert fg(t, 27, next(y for y, l in enumerate(t.display()) if f"{tok} second" in l[:29])) == ACCENT
    assert side_selected(t) and f"{tok} second" in side_selected(t)
    t.key("enter", 1.0)
    assert f"{tok} second" in t.lines()[0]
    assert side_line(t, f"{tok} second")[0] == "▎", "the open chat's row"
    t.key("esc")
    t.key("esc")
    t.key("j")
    t.key("enter", 1.0)  # open the other: the first is no longer the open chat
    assert side_line(t, f"{tok} second")[27] == " ", "opening a chat marks it read"
    seed(f"{tok} third")
    expect(t, f"{tok} third", 5)
    until(lambda: side_line(t, f"{tok} third")[27] == "•", 5, t, "a new unread chat")
    t.key("esc")
    t.key("esc")
    t.key("m", 0.5)
    expect(t, "Marked all as read", 3)
    assert side_line(t, f"{tok} third")[27] == " ", side_line(t, f"{tok} third")
    quit_ok(t)
    t = launch(xdg=t.xdg)
    expect(t, "Pick an agent.")
    t.key("/")
    t.send(tok, 0.8)
    expect(t, f"{tok} third", 3)
    assert side_line(t, f"{tok} third")[27] == " ", "read marks kept across a restart"
    t.key("esc", 0.3)
    close(t)
    # the Pinned group
    t = launch(extra=("--chat", a))
    expect(t, f"{tok} first")
    try:
        t.key("alt-p", 0.8)  # (the sidebar has the focus, on the open chat)
        expect(t, f"⚲{tok} first", 3)
        side = [l[:29] for l in t.lines()]
        y = next(i for i, l in enumerate(side) if f"⚲{tok} first" in l)
        pins = next((i for i, l in enumerate(side) if l.strip() == "Pinned"), None)
        assert pins is not None and pins < y and all("⚲" in l for l in side[pins + 1:y + 1]), "the ' Pinned' header heads the pinned chats"
        today = next((i for i, l in enumerate(side) if l.strip() == "Today"), None)
        assert today is not None and today > y, "'Today' comes after the pinned chats"
    finally:
        api("PATCH", f"/api/chats/{a}", {"pinned": False})


@sect
def s_focus():
    section("focus: Tab and Shift+Tab cycle sidebar, trace, box and drawer and wrap; Esc and i/Enter in the trace; Esc in the drawer; a click in the trace focuses the box; clicks under a dialog do nothing")
    title = f"Focus {STAMP}"
    cid = seed(title)
    t = launch(extra=("--chat", cid), side=False)
    expect(t, "Done.", 10)
    assert box_focused(t) and not side_selected(t), "a chat opened with --chat starts with its message box focused"
    order = []
    for key in ["tab", "tab", "tab", "backtab", "backtab", "backtab"]:
        t.key(key, 0.25)
        order.append("sidebar" if side_selected(t) else "trace" if trace_focused(t) else "box" if box_focused(t) else "?")
    assert order == ["sidebar", "trace", "box", "trace", "sidebar", "box"], order
    to_side(t)
    assert title in (side_selected(t) or ""), "the sidebar's selection is on the open chat"
    t.key("alt-f", 0.8)
    expect(t, "Workspace", 3)
    assert files_selected(t), "Alt+F gives the drawer the focus"
    t.key("tab", 0.25)
    assert side_selected(t), "Tab from the drawer wraps to the sidebar"
    t.key("backtab", 0.25)
    assert files_selected(t)
    t.key("backtab", 0.25)
    assert box_focused(t), "Shift+Tab from the drawer goes to the box"
    t.key("tab", 0.25)
    t.type("jz")
    assert files_selected(t) and box_text(t) == [""], f"keys in the drawer reached the box: {box_text(t)}"
    t.key("esc", 0.3)
    assert box_focused(t), "Esc in the drawer goes to the box"
    t.key("alt-f", 0.5)
    absent(t, "Workspace")
    t.type("zz")
    assert box_text(t)[0] == "zz", box_text(t)
    t.key("ctrl-c")
    t.key("esc")
    assert trace_focused(t), "Esc in an idle box goes to the trace"
    t.key("i")
    assert box_focused(t)
    t.key("esc")
    t.key("enter")
    assert box_focused(t), "Enter in the trace goes to the box"
    t.key("esc")
    t.key("esc")
    assert side_selected(t), "Esc in the trace goes to the sidebar"
    to_box(t)
    assert box_focused(t), "a click in the trace focuses the box"
    t.key("alt-b", 0.4)
    absent(t, "Agent Chat")
    t.key("esc")
    t.key("esc")
    assert box_focused(t, 0), "with the sidebar hidden, Esc in the trace goes to the box"
    t.key("alt-b", 0.4)
    expect(t, "Agent Chat", 2)
    y = next(y for y in range(list_top(t), t.rows - 1) if t.lines()[y][:29].strip() and title not in t.lines()[y] and t.lines()[y][:29].strip() not in ("Today", "Pinned"))
    t.key("f1", 0.5)
    expect(t, "Keyboard shortcuts", 3)
    t.click(8, y, 0.8)
    expect(t, "Keyboard shortcuts", 1, "the shortcuts, still open after a click outside")
    assert title in t.lines()[0], "a click under a dialog opened another chat"
    t.key("esc", 0.4)


@sect
def s_composer():
    section("message box: the placeholder and footer; blank text doesn't send; Alt+Enter and Ctrl+J add lines; it grows to 8 lines, then scrolls; ↑ on one line and PgUp go to the trace, PgDn scrolls it from the box; Esc cancels an edit, stops a reply, else goes to the trace; the Send button stays on a narrow screen")
    title = f"Composer {STAMP}"
    cid = new_chat(title)
    t = launch(extra=("--chat", cid))
    expect(t, "Assistant is ready", 10)
    to_box(t)
    expect(t, "Message  (Enter sends · Alt+Enter new line", 2, "the placeholder")
    expect(t, "Send ↑", 1)
    expect(t, "⛨ asks before acting", 1)
    expect(t, "Enter sends · Alt+Enter new line · Ctrl+U attach", 1)
    t.type("   ")
    t.key("enter", 0.8)
    absent(t, "● running")
    assert summary(cid)["messages"] == 0, "blank text was sent"
    t.key("ctrl-c")
    t.type("line1")
    t.key("alt-enter")
    t.type("line2")
    t.key("ctrl-j")
    t.type("line3")
    assert box_text(t)[:3] == ["line1", "line2", "line3"], box_text(t)
    t.key("enter", 0.5)
    settle(cid, 1, t)
    assert mine(cid) == ["line1\nline2\nline3"], mine(cid)
    for n in (1, 2, 3):
        expect(t, f"▎ line{n}", 2)
    for i in range(12):
        t.send(f"L{i:02d}", 0.1)
        if i < 11:
            t.key("ctrl-j", 0.1)
    rows = box_text(t)
    assert len(rows) == 8 and rows[-1] == "L11" and "L00" not in rows, f"the box should show its last 8 lines: {rows}"
    d = t.display()
    top = max(y for y in range(t.rows) if d[y][30] == "┌")
    assert top - 2 >= 3, "the trace keeps at least 3 rows"
    t.key("ctrl-c")
    assert box_text(t) == [""], box_text(t)
    say(t, "big please")
    settle(cid, 2, t)
    t.key("up", 0.4)
    assert trace_focused(t), "↑ in a one-line box goes to the trace"
    expect(t, "End for newest", 2)
    t.key("i")
    t.key("end")  # (in the box: the line's end)
    t.type("ab")
    t.key("ctrl-j")
    t.type("cd")
    t.key("up")
    assert box_focused(t), "↑ on the second line moves the cursor, not the focus"
    t.type("Z")
    assert box_text(t)[:2] == ["abZ", "cd"], box_text(t)
    t.key("ctrl-c")
    t.key("pgup", 0.4)
    assert trace_focused(t), "PgUp goes to the trace"
    expect(t, "End for newest", 2)
    t.key("i")
    for _ in range(6):
        t.key("pgdn", 0.15)
    absent(t, "End for newest", "the scroll pill (PgDn in the box scrolls to the end)")
    assert box_focused(t)
    t.type("q")
    assert box_text(t)[0] == "q"
    t.key("ctrl-c")
    t.key("ctrl-e", 0.5)
    expect(t, "editing your last message", 2)
    expect(t, "Resend ↑", 1)
    t.key("esc", 0.4)
    expect(t, "Edit cancelled", 2)
    absent(t, "editing your last message")
    assert box_text(t) == [""], box_text(t)
    t.key("esc")
    assert trace_focused(t)
    t.type("k")
    assert box_text(t) == [""], "a key in the trace was typed into the box"
    t.key("i")
    say(t, "slow please")
    expect(t, "● running", 10)
    expect(t, "Enter queues · Ctrl+S stops", 2)
    expect(t, "Stop ■", 1)
    t.key("esc", 0.5)
    expect(t, "stopped here", 10, "Esc in the box stops a reply")
    settle(cid, 4, t)
    t.resize(30, 60)
    expect(t, "Send ↑", 3, "the Send button on a 60-column screen")
    t.resize(40, 120)


@sect
def s_queue():
    section("queue: messages sent while it works are queued (three shown, with a count), ↑ in an empty box takes them all back; one queued during an approval reaches the agent; one the run never read comes back to the box, or to its chat's draft")
    title = f"Queue {STAMP}"
    cid = new_chat(title)
    new_chat(f"Queue other {STAMP}")
    t = launch(extra=("--chat", cid))
    expect(t, "Assistant is ready", 10)
    to_box(t)
    say(t, "slow please")
    expect(t, "● running", 10)
    for i in range(4):
        say(t, f"queued note {i}", 0.6)
    expect(t, "queued (4)", 5)
    assert sum("◦ queued note" in l for l in t.lines()) == 3, "three queued messages shown"
    t.key("up", 0.8)
    absent(t, "queued (")
    assert [r for r in box_text(t) if r] == [f"queued note {i}" for i in range(4)], box_text(t)
    t.key("ctrl-c")
    t.key("ctrl-s", 0.5)
    expect(t, "stopped here", 10)
    settle(cid, 1, t)
    assert mine(cid) == ["slow please"], mine(cid)
    # a run that ends before it reads the queue gives the text back
    say(t, "slow please")
    expect(t, "word2", 10)
    say(t, "late note")
    expect(t, "queued (1)", 5)
    t.key("ctrl-s", 0.5)
    toasted(t, "The run ended before the agent read your queued message; it's back in the message box.")
    assert box_text(t)[0] == "late note", box_text(t)
    settle(cid, 3, t)
    assert "late note" not in mine(cid)
    t.key("ctrl-c")
    # ... into that chat's draft when another chat is open
    say(t, "slow please")
    expect(t, "word2", 10)
    say(t, "late draft note")
    expect(t, "queued (1)", 5)
    open_by(t, f"Queue other {STAMP}")
    api("POST", f"/api/chats/{cid}/stop")
    settle(cid, 5, t)
    open_by(t, title)
    assert box_text(t)[0] == "late draft note", f"the returned message should wait in its chat's box: {box_text(t)}"
    t.key("ctrl-c")
    # queued while an approval waits: it reaches the agent at its next step
    new_chat(f"Queue coder {STAMP}", "coder")
    open_by(t, f"Queue coder {STAMP}")
    say(t, "slow shell")
    expect(t, "needs approval", 25)
    say(t, "an aside")
    expect(t, "queued (1)", 5)
    t.key("alt-y", 0.5)
    expect(t, "sent while it worked", 25)
    expect(t, "Noted your aside: an aside", 25)
    gone(t, "queued (1)", 10)


@sect
def s_drafts():
    section("drafts stay with their chat (Ctrl+K away and back); an edit started in one chat doesn't follow to another")
    a = seed(f"Draft A {STAMP}", ["one", "two"])
    b = seed(f"Draft B {STAMP}", ["one", "two", "three"])
    t = launch(extra=("--chat", a))
    expect(t, f"Draft A {STAMP}")
    to_box(t)
    t.send("draft A text", 0.3)
    open_by(t, f"Draft B {STAMP}")
    assert box_text(t) == [""], box_text(t)
    open_by(t, f"Draft A {STAMP}")
    assert box_text(t)[0] == "draft A text", box_text(t)
    t.type(" more")
    assert box_text(t)[0] == "draft A text more", "the cursor at the draft's end"
    t.key("ctrl-c")
    t.key("ctrl-e", 0.5)
    expect(t, "editing your last message", 2)
    open_by(t, f"Draft B {STAMP}")
    absent(t, "editing your last message")
    say(t, "x")
    settle(b, 6, t)
    assert mine(b) == ["one", "two", "three", "x"], mine(b)


PNG = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==")


@sect
def s_attach():
    section("attachments: Ctrl+U/Alt+U ask for a path ('~/' is home, a pasted line works), a missing file and one over 20 MB say so, Backspace in an empty box drops the last; text files go in fenced (```` around ```), images as images; nothing to attach to without a chat")
    home = tempfile.mkdtemp(prefix="home-", dir=WORK)
    open(os.path.join(home, f"note{STAMP}.txt"), "w").write("note from home\n")
    fence = os.path.join(WORK, f"fence{STAMP}.md")  # (names of their own: uploads keep earlier runs' files)
    open(fence, "w").write("```py\nx = 1\n```\n")
    png = os.path.join(WORK, f"dot{STAMP}.png")
    open(png, "wb").write(PNG)
    big = os.path.join(WORK, "big.bin")
    with open(big, "wb") as f:
        f.truncate(21 * 1024 * 1024)
    cid = new_chat(f"Attach {STAMP}")
    t = launch(env={"HOME": home}, extra=("--chat", cid))
    expect(t, "Assistant is ready", 10)
    to_box(t)

    def attach(path, key="ctrl-u"):
        t.key(key, 0.5)
        expect(t, "Attach a file (path)", 3)
        t.send(path, 0.3)
        t.key("enter", 1.0)

    attach("~/missing-file.txt", "alt-u")
    toasted(t, f"Couldn't read {home}/missing-file.txt")  # (the path expanded)
    attach(big)
    expect(t, "larger than 20 MB", 3)
    attach(f"~/note{STAMP}.txt")
    chip(t, "📎", f"note{STAMP}.txt")
    attach(fence)
    chip(t, "📎", f"fence{STAMP}.md")
    expect(t, "Backspace in an empty box removes the last", 1)
    t.key("backspace", 0.4)
    chip(t, "📎", f"fence{STAMP}.md", False)
    chip(t, "📎", f"note{STAMP}.txt")
    t.key("backspace", 0.4)
    chip(t, "📎", f"note{STAMP}.txt", False)
    absent(t, "Backspace in an empty box removes the last")
    t.key("ctrl-u", 0.5)
    t.paste(png + "\n")
    t.key("enter", 1.0)
    chip(t, "🖼", f"dot{STAMP}.png")  # (the pasted path's line break dropped)
    expect(t, ", as an image", 2)
    attach(fence)
    say(t, "describe the image")
    settle(cid, 1, t)
    expect(t, "I see 1 image(s).", 5)
    m = next(m for m in chat(cid)["messages"] if m["role"] == "user")
    assert m["content"].startswith("describe the image") and "````" in m["content"] and "x = 1" in m["content"], m["content"]
    assert m.get("_images") == [f"uploads/dot{STAMP}.png"], m.get("_images")
    chip(t, "🖼", f"dot{STAMP}.png", False)  # (sent: the chips go)
    say(t, "plain hello")
    settle(cid, 3, t)
    assert mine(cid)[-1] == "plain hello", mine(cid)[-1]
    close(t)
    t = launch()
    expect(t, "Pick an agent.")
    t.key("ctrl-u", 0.4)
    t.key("alt-u", 0.4)
    absent(t, "Attach a file", "the attach prompt with no chat open")


@sect
def s_chat_keys():
    section("chat shortcuts: Ctrl+R regenerates the last reply; Ctrl+E puts the last message back (Resend, how many messages it replaces); mid-reply Ctrl+R, Ctrl+E and Ctrl+L wait, and Alt+P/F2 don't surface the server's 'busy' error; Ctrl+G branches (not an empty chat); Ctrl+L compacts; Alt+P pins; an empty rename keeps the title; Alt+D asks (n keeps it, Enter deletes it)")
    title = f"Manage {STAMP}"
    cid = seed(title, ["first point", "second point"])
    cwd = tempfile.mkdtemp(prefix="exports-", dir=WORK)
    t = launch(extra=("--chat", cid), cwd=cwd)
    expect(t, "Done.", 10)
    to_box(t)
    last = chat(cid)["messages"][-1]["_ts"]
    t.key("ctrl-r", 0.3)
    until(lambda: chat(cid)["messages"][-1].get("_ts") != last and summary(cid)["status"] == "idle", 15, t, "the last reply made again")
    assert len(chat(cid)["messages"]) == 4 and mine(cid) == ["first point", "second point"], chat(cid)["messages"]
    t.key("ctrl-e", 0.5)
    expect(t, "editing your last message", 2)
    expect(t, "Resend ↑", 1)
    toasted(t, "the 1 message after it will be replaced", 3)
    assert box_text(t)[0] == "second point", box_text(t)
    t.send(" edited", 0.3)
    t.key("enter", 0.5)
    settle(cid, 3, t)
    assert mine(cid) == ["first point", "second point edited"] and len(chat(cid)["messages"]) == 4, chat(cid)["messages"]
    # a reply with a tool call is several messages: the count is of messages, not trace entries
    coder = seed(f"Manage tools {STAMP}", ["write: tools"], "coder")
    n = len(chat(coder)["messages"])
    open_by(t, f"Manage tools {STAMP}")
    t.key("ctrl-e", 0.5)
    toasted(t, f"the {n - 1} messages after it will be replaced", 3)
    t.key("esc", 0.3)
    open_by(t, title)
    say(t, "slow please")
    expect(t, "● running", 10)
    for key in ("ctrl-r", "ctrl-e", "ctrl-l"):
        t.key(key, 0.4)
        toasted(t, "Wait for the current reply", 3)
        assert box_text(t) == [""], f"{key} mid-reply changed the box: {box_text(t)}"
    # mid-reply the server refuses to pin or rename ("409 chat is busy"): the keys say to wait,
    # as /pin and /rename do, and F2 opens no prompt to type a title into for nothing
    t.key("alt-p", 0.8)
    toasted(t, "then pin it", 3)
    t.key("f2", 0.6)
    toasted(t, "then rename it", 3)
    absent(t, "Rename chat", "the rename prompt mid-reply")
    shown = t.text()
    t.key("ctrl-s", 0.5)
    expect(t, "stopped here", 10)
    settle(cid, 5, t)
    soft("409" not in shown, "mid-reply Alt+P and F2 show the server's raw '409 chat is busy' error instead of saying to wait (as /pin and /rename do)")
    assert not summary(cid)["pinned"] and summary(cid)["title"] == title, summary(cid)
    # branch from the end
    n = len(chat(cid)["messages"])
    before = {c["id"] for c in chats()}
    t.key("ctrl-g", 1.0)
    toasted(t, "Branched.", 3)
    assert f"{title} (branch)" in t.lines()[0], t.lines()[0]
    fork = next(c["id"] for c in chats() if c["id"] not in before)
    assert len(chat(fork)["messages"]) == n and len(chat(cid)["messages"]) == n
    # an empty chat: nothing to branch, regenerate or edit
    empty = new_chat(f"Manage empty {STAMP}")
    open_by(t, f"Manage empty {STAMP}")
    count = len(chats())
    t.key("ctrl-g", 0.8)
    assert len(chats()) == count, "Ctrl+G branched an empty chat"
    t.key("ctrl-r", 0.4)
    toasted(t, "Nothing to regenerate yet.", 3)
    t.key("ctrl-e", 0.4)
    toasted(t, "No message of yours to edit yet.", 3)
    # compact on request
    open_by(t, title)
    t.key("ctrl-l", 0.5)
    until(lambda: chat(cid).get("compactions"), 20, t, "a compaction")
    expect(t, "earlier messages summarized on request", 10)
    settle(cid, 0, t)
    # pin, and an empty rename
    t.key("alt-p", 0.8)
    toasted(t, "Pinned to the top", 3)
    assert summary(cid)["pinned"]
    t.key("alt-p", 0.8)
    toasted(t, "Unpinned", 3)
    assert not summary(cid)["pinned"]
    t.key("alt-2", 0.5)
    expect(t, "Rename chat", 3)
    t.key("ctrl-u")
    t.key("enter", 0.8)
    assert summary(cid)["title"] == title and title in t.lines()[0], "an empty name changed the title"
    # export a title with no letters: chat.md
    seed("!!! ???")
    open_by(t, "!!! ???")
    t.key("alt-x", 1.0)
    until(lambda: os.path.exists(os.path.join(cwd, "chat.md")), 5, t, "chat.md for a title with no letters or digits")
    # delete: n keeps it, Enter deletes it and goes to the welcome page; keys typed then go nowhere
    open_by(t, f"Manage empty {STAMP}")
    t.key("alt-d", 0.5)
    expect(t, f"Delete “Manage empty {STAMP}”?", 3)
    t.key("n", 0.5)
    absent(t, "Delete “")
    assert summary(empty)
    t.key("alt-d", 0.5)
    t.key("enter", 1.0)
    toasted(t, "Chat deleted", 3)
    expect(t, "Pick an agent.", 3)
    assert summary(empty) is None
    assert side_line(t, f"Manage empty {STAMP}") is None, "the deleted chat's sidebar row"
    t.type("zz")
    t.key("n", 0.5)
    expect(t, "New chat with", 3)
    t.type("1")
    expect(t, "is ready", 5)
    assert box_text(t) == [""], f"keys typed with no chat open leaked into the new chat: {box_text(t)}"


@sect
def s_new_chat_keys():
    section("Ctrl+N / Alt+N: the agent picker (j/k/Tab move, a number or Enter starts, other digits and keys do nothing, Esc closes); with an agent filter they start a chat with it at once; a chat with another agent moves the filter to it")
    agents = api("GET", "/api/state")["agents"]
    t = launch()
    expect(t, "Pick an agent.")

    def picked():
        """The picker's highlighted row."""
        top = row_of(t, "New chat with")
        return next((l for y, l in enumerate(t.lines()) if y > top and any(bg(t, x, y) == SURFACE3 for x in range(30, 90))), None)

    t.key("ctrl-n", 0.5)
    expect(t, "New chat with", 3)
    expect(t, "Enter or a number starts the chat · Esc closes", 1)
    assert agents[0]["name"] in picked(), picked()
    t.key("j")
    assert agents[1]["name"] in picked(), picked()
    t.key("k")
    assert agents[0]["name"] in picked()
    t.key("tab")
    assert agents[1]["name"] in picked()
    t.key("0", 0.3)
    t.key("x", 0.3)
    expect(t, "New chat with", 1, "the picker, still open after 0 and x")
    t.key("esc", 0.4)
    absent(t, "New chat with")
    t.key("alt-n", 0.5)
    t.key("j")
    t.key("enter", 1.0)
    expect(t, f"{agents[1]['name']} is ready", 5)
    t.key("ctrl-n", 0.5)
    t.type("3")
    expect(t, f"{agents[2]['name']} is ready", 5)
    # filtered: at once
    t.key("esc")
    t.key("esc")  # -> sidebar
    t.key("home")
    for _ in range(agent_index("coder") + 1):
        t.key("j", 0.12)
    t.key("enter", 0.6)
    expect(t, "Coder chats", 3)
    n = len([c for c in chats() if c["agent_id"] == "coder"])
    t.key("ctrl-n", 1.0)
    absent(t, "New chat with")
    expect(t, "Coder is ready", 5)
    t.key("alt-n", 1.0)
    until(lambda: len([c for c in chats() if c["agent_id"] == "coder"]) == n + 2, 5, t, "two Coder chats from Ctrl+N and Alt+N")
    t.key("ctrl-k", 0.5)
    t.send("New Assistant chat", 0.8)
    t.key("enter", 1.0)
    expect(t, "Assistant chats", 3, "the filter moved to the new chat's agent")
    until(lambda: (state_of(t.xdg) or {}).get("filter") == "assistant", 3, t, "the moved filter in state.json")


@sect
def s_palette():
    section("palette: Ctrl+K toggles it, ↑/↓/Tab move the ▸ and the list scrolls with it, Backspace edits, 'Nothing matches.'; 'Recent chats' lists 8; 'This chat' only in a chat you write in; every command does its job; labels follow the state; Quit quits")
    title = f"Palette {STAMP}"
    cid = seed(title, ["hello", "hello again"])  # (two exchanges: something older to compact)
    cwd = tempfile.mkdtemp(prefix="exports-", dir=WORK)
    agents = api("GET", "/api/state")["agents"]
    t = launch(extra=("--chat", cid), cwd=cwd)
    expect(t, "Done.", 10)
    t.key("ctrl-k", 0.5)
    expect(t, "Search and commands", 2)
    t.key("ctrl-k", 0.5)
    absent(t, "Search and commands")
    t.key("ctrl-k", 0.5)
    assert f"▸ New {agents[0]['name']} chat" in palette_selected(t), palette_selected(t)
    for _ in range(3):
        t.key("down", 0.1)
    assert f"▸ New {agents[3]['name']} chat" in palette_selected(t), palette_selected(t)
    t.key("up", 0.1)
    assert f"▸ New {agents[2]['name']} chat" in palette_selected(t)
    t.key("tab", 0.1)
    assert f"▸ New {agents[3]['name']} chat" in palette_selected(t)
    for _ in range(80):
        t.send("\x1b[B", 0.02)
    t.pump(0.4)
    soft("▸ Delete chat" in (palette_selected(t) or ""), "the palette's selection scrolled out of view: after moving to its last item ('Delete chat') no row is marked ▸ (the group headers push it below the list)")
    t.key("esc", 0.4)
    t.key("ctrl-k", 0.5)
    lines = t.lines()
    head = next(i for i, l in enumerate(lines) if "│ Recent chats " in l)
    x0 = lines[head].index("│ Recent chats")
    rows = []
    for l in lines[head + 1:]:
        inner = l[x0 + 1:].split("│")[0]
        if not inner.startswith(("   ", " ▸ ")):
            break
        rows.append(inner)
    assert len(rows) == min(8, len([c for c in chats() if not c["parent"]])), rows
    t.send("zzqqxx", 0.8)
    expect(t, "Nothing matches.", 2)
    for _ in range(6):
        t.key("backspace", 0.1)
    absent(t, "Nothing matches.")
    expect(t, f"New {agents[0]['name']} chat", 2)
    t.send("Export", 0.8)
    assert re.search(r"│ This chat\s+│", t.text()), "the 'This chat' group in a chat"
    t.key("esc", 0.4)
    # every command
    for item, sub, shows in [
        ("Settings", "Model server", "Model server URL"), ("Memory", "What your agents", "Memory ·"), ("Routines", "Agents that run", "r run now"),
        ("New agent", "From a blank", "Instructions"), ("Keyboard shortcuts", None, "search chats and commands"),
        ("Email account", "IMAP/SMTP", "Provider preset"), ("Calendars", "iCal links", "iCal link or .ics path"),
        ("New chat", "Pick an agent", "Enter or a number starts the chat"), ("Rename chat", None, "Enter  ok"),
        ("Delete chat", None, f"Delete “{title}”?"), ("Edit this agent", None, "Edit Assistant"),
        ("Bypass permissions", "Let agents", "Bypass permissions?"),
    ]:
        palette_pick(t, item, item, sub)
        t.key("enter", 1.0)
        expect(t, shows, 3, f"what '{item}' opens")
        t.key("esc", 0.4)
        absent(t, shows)
    assert summary(cid) and not api("GET", "/api/state")["settings"]["bypass_approvals"], "Esc kept the chat and the settings"
    palette_pick(t, "Pin or unpin", "Pin or unpin chat", None)
    t.key("enter", 0.8)
    until(lambda: summary(cid)["pinned"], 5, t, "pinned from the palette")
    palette_pick(t, "Pin or unpin", "Pin or unpin chat", None)
    t.key("enter", 0.8)
    until(lambda: not summary(cid)["pinned"], 5, t, "unpinned from the palette")
    for item, ext in [("Export as Markdown", "md"), ("Export as JSON", "json")]:
        palette_pick(t, item, item, "")
        t.key("enter", 1.0)
        until(lambda: os.path.exists(os.path.join(cwd, f"Palette-{STAMP}.{ext}")), 5, t, f"the {ext} export")
    palette_pick(t, "Compact older", "Compact older messages", "Summarize")
    t.key("enter", 0.5)
    until(lambda: chat(cid).get("compactions"), 20, t, "a compaction from the palette")
    settle(cid, 0, t)
    # labels that follow the state
    for query, on, off, check in [
        ("workspace files", "Show workspace files", "Hide workspace files", lambda: t.find("Workspace")),
        ("thinking", "Show all thinking and tool details", "Collapse thinking and tool details", lambda: t.find("Summarize the transcript.") or t.find("The user said:")),
        ("sidebar", "Hide the sidebar", "Show the sidebar", lambda: not t.find("Agent Chat")),
    ]:
        palette_pick(t, query, on, "")
        t.key("enter", 0.8)
        assert check(), f"'{on}' did nothing"
        palette_pick(t, query, off, "")
        t.key("enter", 0.8)
        assert not check(), f"'{off}' did nothing"
    palette_pick(t, "Branch from", "Branch from the end", "A new chat")
    t.key("enter", 1.0)
    expect(t, f"{title} (branch)", 5)
    palette_pick(t, "Quit", "Quit", None)
    t.key("enter", 0.5)
    status = t.wait_exit(10)
    assert status is not None and os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0, status
    for seq in (b"\x1b[?1049l", b"\x1b[?1000l", b"\x1b[?2004l", b"\x1b[?25h"):
        assert seq in t.log, f"{seq!r} not written on the way out"
    # no 'This chat' on the welcome page
    t = launch()
    expect(t, "Pick an agent.")
    t.key("ctrl-k", 0.5)
    t.send("Export", 0.8)
    assert not re.search(r"│ This chat\s+│", t.text()), "'This chat' with no chat open"


@sect
def s_memory():
    section("memory panel: a adds a fact (importance 7, 'added by you'; Esc goes back; a near-duplicate is merged), / searches (Enter or Esc end typing), Tab/→/← cycle the kind and wrap, p pins, +/= and - change importance (1..10), t asks before tidying (Esc keeps it), facts saved elsewhere show up; q and Esc close")
    f1 = f"keys{STAMP} tea{STAMP} oolong{STAMP}"
    f2 = f"keys{STAMP} code{STAMP} rust{STAMP}"
    t = launch(cols=160)  # (toasts show beside the dialog)
    expect(t, "Pick an agent.")
    t.key("alt-m", 1.0)
    total = api("GET", "/api/memory")["total"]
    expect(t, f"Memory · {total} remembered", 3)
    t.key("a", 0.5)
    expect(t, "Remember something", 3)
    t.key("esc", 0.5)
    expect(t, "Memory ·", 2, "the panel back after Esc")
    absent(t, "Remember something")
    t.key("a", 0.5)
    t.send(f1, 0.3)
    t.key("enter", 1.0)
    expect(t, f"Memory · {total + 1} remembered", 3)
    m1 = until(lambda: next((m for m in memories(f"tea{STAMP}") if m["fact"] == f1), None), 5, t, "the fact saved")
    assert m1["importance"] == 7 and m1["source"] == "user", m1
    t.key("/", 0.3)
    t.send(f"tea{STAMP}", 0.8)
    expect(t, f1, 3)
    expect(t, "importance 7 · added by you", 2)
    t.key("enter", 0.3)
    t.key("a", 0.5)
    t.send(f1, 0.3)
    t.key("enter", 1.0)
    toasted(t, "Updated a similar memory instead of adding a duplicate", 5)
    expect(t, f"Memory · {total + 1} remembered", 2, "the count unchanged by the merged duplicate")
    expect(t, f1, 2, "the panel back with its search")
    # pin and importance, on the one row the search leaves
    t.key("p", 0.6)
    expect(t, f" ⚲ {f1}", 3)
    assert memories(f"tea{STAMP}")[0]["pinned"]
    for key, want in [("+", 8), ("=", 9), ("+", 10), ("+", 10)]:
        t.key(key, 0.5)
        until(lambda: memories(f"tea{STAMP}")[0]["importance"] == want, 3, t, f"importance {want}")
    expect(t, "importance 10", 2)
    for _ in range(11):
        t.key("-", 0.25)
    until(lambda: memories(f"tea{STAMP}")[0]["importance"] == 1, 3, t, "importance clamped at 1")
    expect(t, "importance 1 ·", 2)
    t.key("p", 0.6)
    until(lambda: not memories(f"tea{STAMP}")[0]["pinned"], 3, t, "unpinned")
    # search: no match; Esc ends typing
    t.key("/", 0.3)
    t.send("zz", 0.6)
    expect(t, "Nothing matches.", 3)
    t.key("esc", 0.4)
    expect(t, "Memory ·", 1, "Esc while typing only ends the typing")
    t.key("/", 0.3)
    for _ in range(2 + len(f"tea{STAMP}")):
        t.key("backspace", 0.05)
    t.send(f"keys{STAMP}", 0.8)
    t.key("enter", 0.4)
    api("POST", "/api/memory", {"fact": f2, "category": "project"})
    expect(t, f2, 5, "a fact added elsewhere, shown in the open panel")
    expect(t, f1, 1)
    # kinds: Tab/→ forward, ← back, both wrap
    cats = api("GET", "/api/memory")["categories"]
    t.key("tab", 0.6)
    expect(t, f"◂ {cats[0]} ▸", 2)
    t.key("left", 0.6)
    expect(t, "◂ all kinds ▸", 2)
    t.key("left", 0.6)
    expect(t, f"◂ {cats[-1]} ▸", 2, "← from 'all kinds' wraps to the last kind")
    assert cats[-1] == "general"
    expect(t, f1, 2)
    absent(t, f2, "a 'project' fact under 'general'")
    t.key("right", 0.6)
    expect(t, "◂ all kinds ▸", 2, "→ from the last kind wraps to 'all kinds'")
    for _ in range(cats.index("project") + 1):
        t.key("right", 0.3)
    expect(t, "◂ project ▸", 2)
    expect(t, f2, 2)
    absent(t, f1)
    t.key("left", 0.3)
    for _ in range(cats.index("project")):
        t.key("left", 0.2)
    expect(t, "◂ all kinds ▸", 2)
    # tidy: asks; Esc goes back to the panel
    t.key("t", 0.5)
    expect(t, "Tidy up memory?", 3)
    t.key("esc", 0.6)
    expect(t, "Memory ·", 2)
    absent(t, "Tidy up memory?")
    # an agent's own remembering, as it happens
    cid = new_chat(f"Memory chat {STAMP}")
    api("POST", f"/api/chats/{cid}/run", {"content": f"remember that I like keys{STAMP} jasmine{STAMP}"})
    toasted(t, "Assistant:", 15)
    settle(cid, 1, t)
    t.key("/", 0.3)
    for _ in range(len(f"keys{STAMP}")):
        t.key("backspace", 0.05)
    t.send(f"jasmine{STAMP}", 0.8)
    t.key("enter", 0.4)
    expect(t, "saved by Assistant", 5)
    t.key("q", 0.4)
    absent(t, "Memory ·")
    t.key("alt-m", 1.0)
    expect(t, "Memory ·", 3)
    t.key("esc", 0.4)
    absent(t, "Memory ·")
    # tidy for real, on the scratch app (it rewrites whatever memory it finds)
    s = scratch()
    s.api("POST", "/api/memory", {"fact": f"scratch fact {STAMP}"})
    t2 = launch(url=s.url, cols=160)
    expect(t2, "Pick an agent.")
    t2.key("alt-m", 1.0)
    expect(t2, "Memory ·", 3)
    t2.key("t", 0.5)
    t2.key("enter", 1.0)
    until(lambda: any(("tidy" in x or "Tidied" in x or "Nothing to tidy" in x) for x in toasts(t2)), 15, t2, "the tidy-up's answer")
    expect(t2, "Memory ·", 3, "the panel back after the tidy-up")


@sect
def s_memory_auto():
    section("memory learned on its own: 'Remembered: …' when the model saves something new (auto memory on)")
    before = api("GET", "/api/state")["settings"]["auto_memory"]
    api("PUT", "/api/settings", {"auto_memory": True})
    try:
        cid = new_chat(f"Auto memory {STAMP}")
        t = launch(extra=("--chat", cid))
        expect(t, "Assistant is ready", 10)
        api("POST", f"/api/chats/{cid}/run", {"content": f"my name is Keys{STAMP}"})
        toasted(t, f"Remembered: The user's name is Keys{STAMP}", 20)
    finally:
        api("PUT", "/api/settings", {"auto_memory": before})


@sect
def s_routines():
    section("routines: n makes one from a template, or blank (every N minutes); a time the server refuses keeps the form open and says why; Enter/e edit one; Space switches it off and on; r runs it now and opens its chat, or says why not; o opens its chat (nothing for one never run); d asks first (Esc keeps it); the list follows changes made elsewhere; q/Esc close")
    before = {r["id"] for r in routines()}
    t = launch(cols=200)  # (toasts show beside the dialog)
    try:
        expect(t, "Pick an agent.")
        t.key("alt-r", 1.0)
        expect(t, "r run now", 3)
        t.key("n", 0.6)
        expect(t, "New routine", 3)
        expect(t, "◂ Blank ▸", 2)
        t.key("right", 0.3)
        expect(t, "◂ Morning briefing ▸", 2)
        t.key("ctrl-s", 1.0)
        toasted(t, "Routine saved", 5)
        made = until(lambda: [r for r in routines() if r["id"] not in before], 5, t, "the routine")
        assert made[0]["name"] == "Morning briefing" and made[0]["schedule"] == {"type": "daily", "time": "07:30", "days": [0, 1, 2, 3, 4]}, made
        api("DELETE", f"/api/routines/{made[0]['id']}")  # (before it could ever come due)
        expect(t, "r run now", 3, "the list back after saving")
        # blank, every 30 minutes, its prompt a slow reply (so a second run finds it busy)
        name = f"Every30 {STAMP}"
        t.key("n", 0.6)
        t.key("tab")  # Start from -> Name
        t.send(name, 0.3)
        t.key("tab")  # -> Agent
        t.key("tab")  # -> What should it do?
        t.send("slow please", 0.3)
        t.key("tab")  # -> Repeat
        t.key("right", 0.3)
        expect(t, "◂ Every N minutes ▸", 2)
        t.key("tab")  # -> Time
        t.key("tab")  # -> Every (minutes)
        t.key("ctrl-u")
        t.type("30")
        t.key("ctrl-s", 1.0)
        toasted(t, "Routine saved", 5)
        rid = until(lambda: next((r["id"] for r in routines() if r["name"] == name), None), 5, t, "the blank routine")
        assert next(r for r in routines() if r["id"] == rid)["schedule"] == {"type": "interval", "minutes": 30}
        expect(t, "every 30 min", 3)
        expect(t, "never run", 2)
        # a time the server refuses
        t.key("n", 0.6)
        t.key("tab")
        t.send(f"Bad {STAMP}", 0.3)
        t.key("tab")
        t.key("tab")
        t.send("hello", 0.3)
        t.key("tab")
        t.key("tab")  # -> Time
        t.key("ctrl-u")
        t.type("25:99")
        t.key("ctrl-s", 1.0)
        toasted(t, "400 time must look like 08:30", 5)
        expect(t, "New routine", 1, "the form, still open")
        assert not any(r["name"] == f"Bad {STAMP}" for r in routines())
        t.key("esc", 0.5)
        soft(t.find("r run now"), "Esc in the routine editor closes the Routines list too (saving returns to it, as Esc does from its delete question and the memory panel's prompts)")
        # edit: back to daily at 09:15 on weekdays
        t.key("esc", 0.4)
        t.key("alt-r", 1.0)
        select_routine(t, rid)
        t.key("enter", 0.6)
        expect(t, f"Edit routine “{name}”", 3)
        absent(t, "Start from")
        for _ in range(3):
            t.key("tab")  # Name -> Agent -> prompt -> Repeat
        t.key("left", 0.3)
        expect(t, "◂ At a set time ▸", 2)
        t.key("tab")  # -> Time
        t.key("ctrl-u")
        t.type("09:15")
        for _ in range(7):
            t.key("tab", 0.1)  # -> Every (minutes) -> Mon .. Sat
        t.key(" ")
        t.key("tab")
        t.key(" ")  # Sun off too
        t.key("ctrl-s", 1.0)
        toasted(t, "Routine saved", 5)
        assert next(r for r in routines() if r["id"] == rid)["schedule"] == {"type": "daily", "time": "09:15", "days": [0, 1, 2, 3, 4]}
        expect(t, "weekdays at 09:15", 3)
        select_routine(t, rid)
        t.key("e", 0.6)
        expect(t, f"Edit routine “{name}”", 3, "e edits too")
        t.key("esc", 0.4)
        # Space: off and on
        t.key("esc", 0.4)
        t.key("alt-r", 1.0)
        select_routine(t, rid)
        t.key(" ", 0.8)
        until(lambda: not next(r for r in routines() if r["id"] == rid)["enabled"], 3, t, "switched off")
        assert " [ ] " in line_with(t, name) and "off" in t.lines()[row_of(t, name) + 1], line_with(t, name)
        t.key(" ", 0.8)
        until(lambda: next(r for r in routines() if r["id"] == rid)["enabled"], 3, t, "switched on")
        assert " [x] " in line_with(t, name)
        api("PUT", f"/api/routines/{rid}", {"enabled": False})  # (it must not come due by itself)
        # o on one never run does nothing; r runs it and opens its chat; r again finds it busy
        t.key("o", 0.6)
        expect(t, "r run now", 1, "the list (o on a routine that never ran)")
        t.key("r", 1.5)
        absent(t, "r run now")
        expect(t, f"Routine · {name}", 5, "its chat, opened")
        expect(t, "● running", 5)
        t.key("alt-r", 1.0)
        select_routine(t, rid)
        t.key("r", 1.0)
        toasted(t, "Not started: skipped", 5)
        expect(t, "r run now", 1, "the list, still open")
        chat_id = next(r for r in routines() if r["id"] == rid)["chat_id"]
        api("POST", f"/api/chats/{chat_id}/stop")
        settle(chat_id, 1, t)
        t.key("esc", 0.4)
        t.key("ctrl-k", 0.5)
        t.send("New Assistant chat", 0.8)
        t.key("enter", 1.0)
        assert name not in t.lines()[0]
        t.key("alt-r", 1.0)
        select_routine(t, rid)
        t.key("o", 1.0)
        assert f"Routine · {name}" in t.lines()[0], "o opens the routine's chat"
        # the list follows changes made elsewhere; d asks first
        t.key("alt-r", 1.0)
        live = api("POST", "/api/routines", {"name": f"Live {STAMP}", "agent_id": "assistant", "prompt": "hello", "schedule": {"type": "interval", "minutes": 600}, "enabled": False})["id"]
        expect(t, f"Live {STAMP}", 5, "a routine made elsewhere")
        select_routine(t, live)
        t.key("d", 0.5)
        expect(t, f"Delete the routine “Live {STAMP}”?", 3)
        t.key("esc", 0.8)
        expect(t, f"Live {STAMP}", 3, "the list back, the routine kept")
        assert any(r["id"] == live for r in routines())
        select_routine(t, rid)
        t.key("d", 0.5)
        expect(t, f"Delete the routine “{name}”?", 3)
        t.key("enter", 1.0)
        expect(t, "r run now", 3, "the list back")
        assert not any(r["id"] == rid for r in routines())
        assert summary(chat_id), "a deleted routine's chat is kept"
        assert not any(name in l and ("[x]" in l or "[ ]" in l) for l in t.lines()), "the deleted routine still listed"
        t.key("q", 0.4)
        absent(t, "r run now")
    finally:
        for r in routines():
            if r["id"] not in before and STAMP in r["name"]:
                api("DELETE", f"/api/routines/{r['id']}")


@sect
def s_settings():
    section("Settings: Tab/↓ and Shift+Tab/↑ move and wrap past the note; Ctrl+S saves (a number that doesn't parse is left out), Enter in a text field saves, Esc throws edits away; Space/Enter/←/→ flip a switch, ←/→ cycle a choice; text keys and a long value; Bypass in amber when on")
    keep = api("GET", "/api/state")["settings"]
    t = launch()
    try:
        expect(t, "Pick an agent.")
        t.key("alt-s", 1.0)
        expect(t, "Server: connected (llama-server), 1 model(s), context 85000", 3)
        expect(t, "Agent Chat " + api("GET", "/api/state")["version"], 1)
        assert focused_label(t, "Model server URL")
        t.key("backtab")
        assert focused_label(t, "Run the browser headless"), "Shift+Tab from the first field wraps to the last (past the note)"
        t.key("down")
        assert focused_label(t, "Model server URL"), "↓ from the last field wraps to the first"
        t.key("up")
        t.key("up")
        assert focused_label(t, "Browser executable")
        t.key("esc", 0.4)
        t.key("alt-s", 1.0)
        for _ in range(6):
            t.key("tab", 0.1)
        assert focused_label(t, "Max steps per turn"), [l for l in t.lines() if "Max steps" in l]
        t.key("ctrl-u")
        t.type("7")
        t.key("ctrl-s", 1.0)
        toasted(t, "Settings saved", 3)
        assert api("GET", "/api/state")["settings"]["max_steps"] == 7
        t.key("alt-s", 1.0)
        for _ in range(6):
            t.key("tab", 0.1)
        t.key("ctrl-u")
        t.type("abc")
        t.key("ctrl-s", 1.0)
        toasted(t, "Settings saved", 3)
        assert api("GET", "/api/state")["settings"]["max_steps"] == 7, "a value that isn't a number should be left out"
        t.key("alt-s", 1.0)
        for _ in range(6):
            t.key("tab", 0.1)
        t.key("ctrl-u")
        t.type("12")
        t.key("enter", 1.0)
        absent(t, "Model server URL", "the form (Enter in a text field saves it)")
        assert api("GET", "/api/state")["settings"]["max_steps"] == 12
        # switches: Space, Enter, ←, → each flip; Esc throws it all away
        t.key("alt-s", 1.0)
        for _ in range(4):
            t.key("tab", 0.1)
        assert focused_label(t, "Compact long chats automatically")
        was = api("GET", "/api/state")["settings"]["auto_compact"]
        def box():
            return "[x]" if "[x] Compact long" in t.text() else "[ ]"

        start = box()
        for key in (" ", "enter", "left", "right"):
            prev = box()
            t.key(key, 0.2)
            assert box() != prev, f"{key!r} didn't flip the switch"
        assert box() == start
        t.key(" ")
        assert box() != start
        t.key("esc", 0.6)
        assert api("GET", "/api/state")["settings"]["auto_compact"] == was, "Esc saved the form"
        # a choice: → and ← cycle and wrap
        t.key("alt-s", 1.0)
        t.key("tab")
        t.key("tab")
        assert focused_label(t, "Default model")
        default = "◂ Server default / first model ▸"
        expect(t, default, 1)
        t.key("right")
        expect(t, "◂ mock-model ▸", 1)
        t.key("right")
        expect(t, default, 1, "→ past the last option wraps")
        t.key("left")
        expect(t, "◂ mock-model ▸", 1, "← from the first wraps to the last")
        t.key("esc", 0.5)
        # text keys on the search engine URL (not saved)
        t.key("alt-s", 1.0)
        for _ in range(11):
            t.key("tab", 0.1)
        assert focused_label(t, "Search engine URL")
        t.key("ctrl-u")
        t.type("abc")
        t.key("home")
        t.type("X")
        t.key("end")
        t.key("backspace")
        t.key("left")
        t.key("delete")
        assert field_value(t, "Search engine URL") == "Xa", field_value(t, "Search engine URL")
        t.key("ctrl-u")
        assert field_value(t, "Search engine URL") == "", field_value(t, "Search engine URL")
        t.paste("0123456789" * 10 + "END")
        v = field_value(t, "Search engine URL")
        assert v.endswith("789END") and not v.startswith("0123"), f"a long value should scroll to keep the cursor's end in view: {v!r}"
        t.paste("\x1b[31mred\x1b[0m\ttab")
        assert field_value(t, "Search engine URL").endswith("END red tab") or field_value(t, "Search engine URL").endswith("ENDred tab"), field_value(t, "Search engine URL")
        t.key("esc", 0.5)
        assert api("GET", "/api/state")["settings"].get("search_url") == keep.get("search_url")
        # Bypass in amber once on
        t.key("alt-s", 1.0)
        for _ in range(10):
            t.key("tab", 0.1)
        assert focused_label(t, "Bypass permissions")
        absent(t, "[x] Bypass permissions")
        t.key(" ")
        y = row_of(t, "[x] Bypass permissions")
        assert y is not None and fg(t, t.lines()[y].index("Bypass permissions"), y) == AMBER, "the risky switch, on, in amber"
        t.key("ctrl-s", 1.0)
        assert api("GET", "/api/state")["settings"]["bypass_approvals"] is True
    finally:
        api("PUT", "/api/settings", {k: keep[k] for k in SETTINGS if k in keep})


@sect
def s_email_calendars():
    section("Email account: typing starts at the provider preset (not the note), a preset fills the servers on save, the password shows as dots and isn't shown back, Ctrl+S saves, 'Save and test' says what failed; Calendars: Add and Test buttons, Disconnect, Esc")
    s = scratch()
    t = launch(url=s.url, cols=200)  # (toasts show beside the dialog)
    expect(t, "Pick an agent.")
    palette_pick(t, "Email account", "Email account", "IMAP/SMTP")
    t.key("enter", 1.0)
    expect(t, "Provider preset", 3)
    expect(t, "◂ keep servers below ▸", 1)
    assert focused_label(t, "Provider preset"), "the form starts on its first field, not the note"
    t.key("right", 0.3)
    expect(t, "◂ Gmail ▸", 1)
    t.key("tab")
    t.send(f"me{STAMP}@example.com", 0.3)
    t.key("tab")
    assert focused_label(t, "App password")
    t.send("hunter2secret", 0.3)
    expect(t, "•" * 13, 1, "the password as dots")
    absent(t, "hunter2secret")
    t.key("ctrl-s", 1.0)
    toasted(t, "Email settings saved", 3)
    absent(t, "Provider preset")
    cfg = s.api("GET", "/api/email")
    assert (cfg["imap_host"], cfg["imap_port"], cfg["smtp_host"], cfg["smtp_port"], cfg["username"], cfg["has_password"]) == ("imap.gmail.com", 993, "smtp.gmail.com", 465, f"me{STAMP}@example.com", True), cfg
    assert cfg["password"] == ""
    palette_pick(t, "Email account", "Email account", "IMAP/SMTP")
    t.key("enter", 1.0)
    expect(t, "saved; leave empty to keep", 3)
    expect(t, "◂ keep servers below ▸", 1)
    for _ in range(4):
        t.key("tab", 0.1)  # -> Incoming server (IMAP)
    assert focused_label(t, "Incoming server (IMAP)")
    t.key("ctrl-u")
    t.send("127.0.0.1", 0.2)
    t.key("tab")
    t.key("ctrl-u")
    t.type("1")
    t.key("backtab")
    t.key("backtab")
    for _ in range(4):
        t.key("backtab", 0.1)  # (wraps past the note) -> Save and test
    t.key("enter", 2.0)
    toasted(t, "Email settings saved", 5)
    until(lambda: any(fg(t, x, y) == DANGER for y in range(2, 20) for x in range(143, 200) if t.display()[y][x] in "┌│"), 10, t, "a red toast for the failed test")
    assert s.api("GET", "/api/email")["imap_host"] == "127.0.0.1"
    t.key("esc", 0.5)
    absent(t, "Provider preset")
    # calendars
    ics = os.path.join(WORK, "cal.ics")
    open(ics, "w").write("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//t//EN\r\nEND:VCALENDAR\r\n")
    name = f"Cal{STAMP}"
    palette_pick(t, "Calendars", "Calendars", "iCal links")
    t.key("enter", 1.0)
    expect(t, "iCal link or .ics path", 3)
    assert focused_label(t, "Name")
    t.send(name, 0.3)
    t.key("tab")
    t.send(ics, 0.3)
    t.key("tab")  # -> Add calendar
    t.key("enter", 1.5)
    toasted(t, "Calendar connected", 3)
    expect(t, f"Disconnect {name}", 3)
    assert [c["name"] for c in s.api("GET", "/api/calendars")] == [name]
    for _ in range(3):
        t.key("tab", 0.1)  # Name -> link -> Add -> Test
    t.key("enter", 1.5)
    toasted(t, f"✓ {name}: 0 event(s) in the next 30 days", 5)
    for _ in range(4):
        t.key("backtab", 0.1)  # -> Disconnect
    t.key("enter", 1.0)
    toasted(t, "Calendar disconnected", 3)
    assert s.api("GET", "/api/calendars") == []
    absent(t, f"Disconnect {name}")
    t.key("esc", 0.5)
    absent(t, "iCal link or .ics path")


@sect
def s_agent_editor():
    section("agent editor: Alt+A with nothing picked is a blank 'New agent'; Ctrl+S refuses an empty name and stays open; a bad color and an empty icon fall back; F1 and Ctrl+K keep the form; Duplicate; Delete asks (Esc keeps it); a filter on a deleted agent goes and its chats say 'Deleted agent'; on a short screen the form scrolls to the focused field")
    name = f"KB{STAMP}"  # (short: the sidebar's filter header cuts long names)
    t = launch(cols=200)  # (toasts show beside the dialog)
    try:
        expect(t, "Pick an agent.")
        t.key("alt-a", 0.6)
        expect(t, " New agent ", 3)
        assert field_value(t, "Icon") == "🤖" and field_value(t, "Color") == "#7c6cff", (field_value(t, "Icon"), field_value(t, "Color"))
        t.key("ctrl-s", 0.8)
        toasted(t, "The agent needs a name.", 3)
        expect(t, " New agent ", 1, "the form, still open")
        t.send(name, 0.3)
        t.key("tab")
        t.key("ctrl-u")  # no icon
        t.key("tab")
        t.key("ctrl-u")
        t.send("nope", 0.3)  # not a color
        t.key("ctrl-s", 1.0)
        toasted(t, f"Saved {name}", 3)
        a = until(lambda: next((a for a in api("GET", "/api/state")["agents"] if a["name"] == name), None), 5, t, "the agent")
        assert (a["emoji"], a["color"], a["tools"]) == ("🤖", "#7c6cff", ["ask_agent"]), a
        expect(t, name, 3, "the new agent in the sidebar")
        # F1 and Ctrl+K don't throw the edits away
        to_agent_row(t, a["id"])
        t.key("e", 0.6)
        expect(t, f"Edit {name}", 3)
        for _ in range(3):
            t.key("tab", 0.1)
        assert focused_label(t, "Purpose")
        t.send(" changed", 0.3)
        t.key("f1", 0.5)
        t.key("ctrl-k", 0.5)
        absent(t, "Keyboard shortcuts")
        absent(t, "Search and commands")
        expect(t, f"Edit {name}", 1)
        assert field_value(t, "Purpose").endswith("changed")
        t.key("esc", 0.5)
        assert next(x for x in api("GET", "/api/state")["agents"] if x["id"] == a["id"])["purpose"] == "", "Esc saved the edit"
        # duplicate, then delete the copy (Esc first keeps it)
        t.key("alt-a", 0.6)
        expect(t, f"Edit {name}", 3)
        t.key("backtab")
        t.key("backtab")  # (wraps) -> Delete this agent -> Duplicate this agent
        t.key("enter", 1.0)
        toasted(t, f"Created {name} copy", 3)
        expect(t, f"Edit {name} copy", 3)
        copy = next(x["id"] for x in api("GET", "/api/state")["agents"] if x["name"] == f"{name} copy")
        t.key("backtab")
        t.key("enter", 0.6)
        expect(t, f"Delete {name} copy?", 3)
        t.key("esc", 0.5)
        absent(t, f"Delete {name} copy?")
        assert any(x["id"] == copy for x in api("GET", "/api/state")["agents"]), "Esc deleted the agent"
        to_agent_row(t, copy)
        t.key("e", 0.6)
        expect(t, f"Edit {name} copy", 3)
        t.key("backtab")
        t.key("enter", 0.6)
        t.key("enter", 1.0)
        toasted(t, "Agent deleted", 3)
        assert not any(x["id"] == copy for x in api("GET", "/api/state")["agents"])
        assert side_line(t, f"{name} copy") is None, "the deleted agent's sidebar row"
        # a filter on an agent that is deleted goes; its chats say so
        cid = new_chat(f"Orphan {STAMP}", a["id"])
        to_agent_row(t, a["id"])
        t.key("enter", 0.6)
        expect(t, f"{name} chats", 3)
        t.key("e", 0.6)
        t.key("backtab")
        t.key("enter", 0.6)
        expect(t, f"Delete {name}?", 3)
        t.key("enter", 1.0)
        toasted(t, "Agent deleted", 3)
        expect(t, " Chats ", 3, "the sidebar back to every chat")
        absent(t, f"{name} chats")
        open_by(t, f"Orphan {STAMP}")
        expect(t, "Deleted agent", 3, "the header of a chat whose agent was deleted")
        say(t, "hello")
        toasted(t, "this chat's agent was deleted", 5)
        assert summary(cid)["messages"] == 0
        close(t)
        # Coder's editor on a short screen: the form scrolls to whatever has the focus
        t = launch(rows=24, cols=120)
        expect(t, "Agent Chat")
        to_agent_row(t, "coder")
        t.key("alt-a", 0.6)
        expect(t, "Edit Coder", 3)
        seen, risky = set(), False
        for i in range(70):
            t.key("tab", 0.08)
            assert focus_shown(t), f"after {i + 1} Tabs the focused field is off screen"
            risky = risky or t.find("Shell commands  (risky)")
            if focused_label(t, "Workspace folder"):
                seen.add("workspace")
            if focused_label(t, "Name"):
                break
        assert seen == {"workspace"} and risky, (seen, risky)
        for _ in range(3):
            t.key("backtab", 0.08)
            assert focus_shown(t)
        t.key("esc", 0.5)
    finally:
        for x in api("GET", "/api/state")["agents"]:
            if x["name"].startswith(name):
                api("DELETE", f"/api/agents/{x['id']}")


@sect
def s_trace_keys():
    section("trace keys: [ and ] select a reply (▶), c copies it, b branches at it, g/Home and G/End scroll (stopping or resuming the follow), k and PgUp too; r on the last reply regenerates it, on an earlier one asks first (n and Esc keep the chat, Enter regenerates from there)")
    title = f"Trace {STAMP}"
    cid = seed(title, ["first question", "big please"])
    t = launch(extra=("--chat", cid))
    expect(t, "token499", 10)
    t.key("tab")
    assert trace_focused(t)

    def selected():
        return next((y for y, l in enumerate(t.lines()) if l[30:].startswith(" ▶ ")), None)

    t.key("[", 0.4)
    y = selected()
    assert y is not None and "Assistant" in t.lines()[y], "[ marks the last reply"
    assert any("Big reply." in l for l in t.lines()[y:y + 4]), t.lines()[y:y + 4]
    mark = len(t.log)
    t.key("c", 0.4)
    toasted(t, "Copied the reply", 3)
    assert (osc52(t, mark) or "").startswith("Big reply. token0"), osc52(t, mark)
    t.key("[", 0.4)
    mark = len(t.log)
    t.key("c", 0.4)
    assert (osc52(t, mark) or "").startswith("Here is an answer with **Markdown**"), osc52(t, mark)
    y = selected()
    assert y is not None and any("Here is an answer" in l for l in t.lines()[y:y + 4]), "[ again marks the first reply, scrolled into view"
    t.key("]", 0.4)
    mark = len(t.log)
    t.key("c", 0.4)
    assert (osc52(t, mark) or "").startswith("Big reply."), "] goes back to the later reply"
    # branch at the first reply
    t.key("[", 0.4)
    before = {c["id"] for c in chats()}
    t.key("b", 1.0)
    toasted(t, "Branched.", 3)
    assert f"{title} (branch)" in t.lines()[0]
    fork = next(c["id"] for c in chats() if c["id"] not in before)
    assert [m["content"] for m in chat(fork)["messages"] if m["role"] == "user"] == ["first question"], chat(fork)["messages"]
    open_by(t, title)
    t.key("esc")  # box -> trace
    # r on the last reply: at once
    n = len(chat(cid)["messages"])
    last = chat(cid)["messages"][-1]["_ts"]
    t.key("[", 0.3)
    t.key("r", 0.5)
    absent(t, "Regenerate this reply?")
    until(lambda: chat(cid)["messages"][-1].get("_ts") != last and summary(cid)["status"] == "idle", 15, t, "the last reply regenerated")
    assert len(chat(cid)["messages"]) == n
    t.pump(0.5)
    # scrolling
    t.key("g", 0.4)
    expect(t, "▎ first question", 2)
    expect(t, "End for newest", 1)
    t.key("G", 0.4)
    absent(t, "End for newest")
    t.key("home", 0.4)
    expect(t, "End for newest", 1)
    t.key("end", 0.4)
    absent(t, "End for newest")
    t.key("k", 0.4)
    expect(t, "↓ 1 more line · End for newest", 1)
    t.key("j", 0.4)
    absent(t, "End for newest", "j back at the end follows again")
    t.key("pgup", 0.4)
    expect(t, "↓ 20 more lines · End for newest", 1)
    t.key("pgdn", 0.4)
    absent(t, "End for newest")
    t.send("\x1b[<64;70;10M", 0.4)  # the wheel over the trace: up 3
    expect(t, "↓ 3 more lines · End for newest", 1)
    t.send("\x1b[<65;70;10M", 0.4)
    absent(t, "End for newest", "the wheel back down to the end follows again")
    # while a reply streams: scrolled up it stays put and counts what is below; End follows again
    t.key("i")
    say(t, "slow please")
    expect(t, "word5 ", 10)
    y = next(y for y, l in enumerate(t.lines()) if title in l[:29])
    assert t.lines()[y][27] == "●" and fg(t, 27, y) == ACCENT, f"the running dot: {t.lines()[y][:29]!r}"
    assert "working" in (side_line(t, "  Assistant ") or ""), side_line(t, "  Assistant ")
    t.key("backtab")  # box -> trace
    t.key("pgup", 0.4)
    top = t.lines()[3]

    def below():
        m = re.search(r"↓ (\d+) more lines? · End for newest", t.text())
        return int(m[1]) if m else None

    first = below()
    assert first is not None
    until(lambda: (below() or 0) > first, 10, t, "the count of lines below to grow")
    assert t.lines()[3] == top, "the trace moved while scrolled up"
    t.key("end", 0.4)
    absent(t, "End for newest")
    t.key("ctrl-s", 0.5)
    settle(cid, 5, t)
    # r on an earlier reply asks first
    n = len(chat(cid)["messages"])
    for _ in range(4):
        t.key("[", 0.2)  # (to the first)
    for key in ("n", "esc"):
        t.key("r", 0.5)
        expect(t, "Regenerate this reply?", 3)
        t.key(key, 0.5)
        absent(t, "Regenerate this reply?")
        assert len(chat(cid)["messages"]) == n, f"{key} answered yes"
    t.key("r", 0.5)
    t.key("enter", 0.5)
    settle(cid, 1, t)
    assert mine(cid) == ["first question"] and len(chat(cid)["messages"]) == 2, chat(cid)["messages"]


@sect
def s_files():
    section("files drawer: Enter/→/l open a folder and ←/h/Backspace or ↰ .. leave it; an empty folder says so; sizes; r refreshes; the viewer (j/k, PgUp/PgDn, Home/End, c copies, q and Esc close; binary files); the drawer follows the open chat's agent and its file tools")
    ws = api("GET", "/api/workspace?agent_id=coder&path=.")["workspace"]
    d = f"kd{STAMP}"
    os.makedirs(os.path.join(ws, d, "empty"))
    open(os.path.join(ws, d, "a.txt"), "w").write("hi\n")
    long = "".join(f"line {i} of the long file\n" for i in range(1, 201))
    open(os.path.join(ws, d, "long.txt"), "w").write(long)
    open(os.path.join(ws, d, "blob.bin"), "wb").write(b"\x00\x01\x02binary\x00" * 20)
    cid = new_chat(f"Files {STAMP}", "coder")
    t = launch(extra=("--chat", cid))
    expect(t, f"Files {STAMP}")
    t.key("alt-f", 0.8)
    expect(t, "↻ refresh", 3)

    def go(name, path="."):
        """Select an entry of the folder shown (row 0 is ↻ refresh or ↰ ..)."""
        names = [e["name"] for e in api("GET", f"/api/workspace?agent_id=coder&path={path}")["entries"]]
        t.key("home")  # (the drawer has no Home: back up to row 0)
        for _ in range(len(names) + 2):
            t.key("k", 0.03)
        for _ in range(names.index(name) + 1):
            t.key("j", 0.05)
        t.pump(0.2)
        assert name in (files_selected(t) or ""), (name, files_selected(t))

    def where():
        return t.lines()[1][t.cols - 31:].strip()

    go(d)
    t.key("enter", 0.8)
    assert where() == f"/{d}", where()
    expect(t, "↰ ..", 1)
    expect(t, "· a.txt", 1)
    assert re.search(r"· a\.txt\s+3 B", t.text()), "a.txt with its size"
    expect(t, "▸ empty", 1)
    go("empty", d)
    t.key("l", 0.8)
    assert where() == f"/{d}/empty", where()
    expect(t, "Empty folder.", 2)
    t.key("h", 0.8)
    assert where() == f"/{d}", where()
    t.key("backspace", 0.8)
    assert where() == "/", where()
    go(d)
    t.key("right", 0.8)
    assert where() == f"/{d}", where()
    t.key("k", 0.1)
    t.key("k", 0.1)
    t.key("enter", 0.8)  # on ↰ ..
    assert where() == "/", where()
    t.key("left", 0.8)  # at the root: stays (and lists again)
    assert where() == "/", where()
    # the viewer
    go(d)
    t.key("enter", 0.8)
    go("long.txt", d)
    t.key("enter", 1.0)
    expect(t, f"{d}/long.txt  (200 lines)", 3)

    def first_number():
        m = next((re.search(r"│\s*(\d+) line ", l) for l in t.lines() if re.search(r"│\s*\d+ line \d+ of", l)), None)
        return int(m[1]) if m else None

    assert first_number() == 1
    t.key("end", 0.3)
    assert first_number() == 200, first_number()
    t.key("home", 0.3)
    assert first_number() == 1
    for _ in range(3):
        t.key("j", 0.1)
    assert first_number() == 4, first_number()
    t.key("pgdn", 0.3)
    assert first_number() == 24, first_number()
    t.key("pgup", 0.3)
    assert first_number() == 4, first_number()
    t.key("k", 0.2)
    assert first_number() == 3
    mark = len(t.log)
    t.key("c", 0.4)
    assert osc52(t, mark) == long.rstrip("\n"), (osc52(t, mark) or "")[:80]
    t.pump(0.5)
    soft(any("Copied the file" in x for x in toasts(t)), "'Copied the file' can't be seen: toasts are drawn under the viewer (and under any dialog they overlap)")
    t.key("q", 0.4)
    absent(t, "(200 lines)")
    t.key("enter", 1.0)
    expect(t, "(200 lines)", 3)
    t.key("esc", 0.4)
    absent(t, "(200 lines)")
    go("blob.bin", d)
    t.key("enter", 1.0)
    expect(t, "Binary file (", 3)
    t.key("esc", 0.4)
    # r lists again
    fresh = f"new{STAMP}.txt"
    open(os.path.join(ws, d, fresh), "w").write("x")
    absent(t, fresh)
    t.key("r", 0.8)
    expect(t, fresh, 2)
    # the Coder's own writes show without r
    t.key("h", 0.8)
    t.key("esc", 0.3)
    assert box_focused(t, 30), "Esc in the drawer goes to the box"
    text = f"hello {STAMP}"
    say(t, f"write: {text}")
    settle(cid, 1, t)
    assert re.search(rf"· notes\.txt\s+{len(text)} B", t.text()), "notes.txt with its new size, listed without pressing r"
    # it follows the open chat's agent
    t.key("ctrl-k", 0.5)
    t.send("New Assistant chat", 0.8)
    t.key("enter", 1.0)
    assert "Assistant" in t.lines()[0][t.cols - 31:], t.lines()[0]
    t.key("alt-f", 0.5)
    absent(t, "↻ refresh")


@sect
def s_approvals():
    section("approvals from every pane: Alt+Y in the sidebar, Alt+A in the drawer (the run's next command 'auto-approved (this run)'), n and y in the trace; plain letters elsewhere aren't answers (n in the sidebar asks which agent, a keystroke paste in the trace is text); an open dialog keeps its keys")
    title = f"Approve {STAMP}"
    cid = new_chat(title, "coder")
    t = launch(extra=("--chat", cid))
    expect(t, title)
    assert side_selected(t), "the sidebar has the focus at start"

    def ask(cmd):
        n = summary(cid)["messages"]
        api("POST", f"/api/chats/{cid}/run", {"content": cmd})
        expect(t, "needs approval", 25)
        t.pump(0.5)
        return n

    def waiting():
        return summary(cid)["status"] == "waiting"

    n = ask("shell twice: sleep 1; echo two")  # (first: the mock asks twice only in a chat with no tool calls yet)
    t.key("alt-f", 0.8)
    assert files_selected(t)
    t.type("a")
    assert waiting(), "a in the drawer answered the approval"
    t.key("alt-a", 0.3)
    t.key("alt-f", 0.3)  # (hidden, for the trace's width; the box has the focus)
    toasted(t, "Approved for the rest of this run", 5)
    expect(t, "echo two again  auto-approved (this run)", 25, "the run's second command, approved by Alt+A (while it runs)")
    settle(cid, n + 4, t)
    expect(t, "echo two again  auto-approved", 3, "the label in the saved chat (which keeps no reason)")
    t.key("tab")  # box -> sidebar
    assert side_selected(t)
    n = ask("shell: echo one")
    t.type("y")
    assert waiting(), "y in the sidebar answered the approval"
    t.key("n", 0.6)
    expect(t, "New chat with", 3, "the agent picker (n in the sidebar)")
    t.key("esc", 0.4)
    assert waiting(), "n in the sidebar answered the approval"
    t.key("alt-y", 0.5)
    expect(t, "echo one  done", 20)
    settle(cid, n + 2, t)
    to_box(t)
    n = ask("shell: echo three")
    t.key("f1", 0.5)
    t.key("alt-y", 0.5)
    expect(t, "Keyboard shortcuts", 1, "the shortcuts, still open")
    assert waiting(), "Alt+Y under a dialog answered the approval"
    t.key("esc", 0.4)
    t.key("backtab")  # box -> trace
    assert trace_focused(t)
    t.send("yay", 0.8)  # a keystroke paste: text, wherever it lands
    assert waiting(), "a keystroke paste answered the approval"
    assert box_text(t) == ["yay"], box_text(t)
    t.key("ctrl-c")
    t.key("backtab")
    t.key("n", 0.5)
    expect(t, "echo three  denied", 20)
    settle(cid, n + 2, t)
    n = ask("shell: echo four")
    t.key("y", 0.5)
    expect(t, "echo four  done", 20)
    settle(cid, n + 2, t)

    section("a handoff waiting for approval: the sidebar says who works for whom (→ Coder, needs you, the ◌ dot); Enter on the Coder row opens its waiting chat, read-only, where y in the trace approves and Ctrl+U/Ctrl+G/Ctrl+E do nothing; o in the parent's trace opens it")
    parent = f"Delegate {STAMP}"
    o = new_chat(parent, "orchestrator")
    open_by(t, parent)
    api("POST", f"/api/chats/{o}/run", {"content": "please delegate this"})
    expect(t, "needs approval", 25)
    until(lambda: "→ Coder" in (side_line(t, "Orchestrator") or ""), 5, t, "'→ Coder' on the Orchestrator row")
    until(lambda: "needs you" in (side_line(t, "  Coder ") or ""), 5, t, "'needs you' on the Coder row")
    y = next(y for y, l in enumerate(t.lines()) if parent in l[:29])
    assert t.lines()[y][27] == "◌" and fg(t, 27, y) == ACCENT, f"the delegated dot: {t.lines()[y][:29]!r}"
    t.key("backtab")  # box -> trace (Esc in the box would stop the run)
    t.key("esc")  # -> sidebar
    to_agent_row(t, "coder")
    t.key("enter", 1.0)
    expect(t, "Coder chats", 3)
    expect(t, "working for Orchestrator", 5, "the waiting sub-chat, opened by Enter on its agent")
    expect(t, "This agent is working for another agent", 2)
    tail = " ".join(l[30:].strip() for l in t.lines()[-3:])
    assert f"to reply, open “{parent}”" in tail, tail
    y = next((y for y, l in enumerate(t.lines()) if "↳" in l[:29] and "▎" in l[:2]), None)
    assert y is not None and t.lines()[y][27] == "●" and fg(t, 27, y) == AMBER, "the open sub-chat's row: '↳', an amber dot"
    count = len(chats())
    t.key("ctrl-u", 0.4)
    absent(t, "Attach a file")
    t.key("ctrl-g", 0.6)
    assert len(chats()) == count, "Ctrl+G branched a sub-chat"
    t.key("ctrl-e", 0.5)
    toasted(t, "reply in the chat that started it", 3)
    t.key("ctrl-k", 0.5)
    t.send("Export", 0.8)
    assert not re.search(r"│ This chat\s+│", t.text()), "the 'This chat' commands in a sub-chat"
    t.key("esc", 0.4)
    t.key("tab")
    assert trace_focused(t)
    t.key("y", 0.5)
    expect(t, "hello from coder", 25)
    until(lambda: summary(o)["status"] == "idle", 30, t, "the parent's run to end")
    t.key("esc")  # trace -> sidebar
    t.key("home")
    t.key("enter", 0.6)
    expect(t, " Chats ", 3)
    open_by(t, parent)
    t.key("esc")  # box -> trace
    t.key("[", 0.3)
    t.key("o", 1.0)
    expect(t, "working for Orchestrator", 5, "o opens the selected reply's handoff chat")

    section("bypass: Ctrl+Y asks first (n and Esc say no), Enter turns it on (the chip, 'auto-approved' without '(this run)', kept in the history; the palette offers 'Ask before acting again'); turning it off is immediate")
    keep = api("GET", "/api/state")["settings"]["bypass_approvals"]
    try:
        bypass = f"Bypass {STAMP}"
        b = new_chat(bypass, "coder")
        open_by(t, bypass)
        for no in ("n", "esc"):
            t.key("ctrl-y", 0.5)
            expect(t, "Bypass permissions?", 3)
            t.key(no, 0.5)
            absent(t, "Bypass permissions?")
            assert api("GET", "/api/state")["settings"]["bypass_approvals"] is False, f"{no} turned bypass on"
        t.key("ctrl-y", 0.5)
        t.key("enter", 0.8)
        toasted(t, "Permissions bypassed", 3)
        expect(t, "⚠ bypassing permissions", 2)
        assert api("GET", "/api/state")["settings"]["bypass_approvals"] is True
        say(t, "shell: echo bypassed")
        settle(b, 1, t)
        line = line_with(t, "echo bypassed  auto-approved")
        assert line and "(this run)" not in line, line
        open_by(t, title)
        open_by(t, bypass)
        expect(t, "echo bypassed  auto-approved", 3, "the label in the history")
        assert summary(b)["status"] == "idle" and not any(m["role"] == "tool" and "denied" in m["content"] for m in chat(b)["messages"])
        palette_pick(t, "Ask before", "Ask before acting again", "Let agents")
        t.key("enter", 0.8)
        toasted(t, "Agents will ask before acting again", 3)
        expect(t, "⛨ asks before acting", 2)
        assert api("GET", "/api/state")["settings"]["bypass_approvals"] is False
        t.key("ctrl-y", 0.5)
        t.key("enter", 0.8)
        assert api("GET", "/api/state")["settings"]["bypass_approvals"] is True
        t.key("ctrl-y", 0.8)
        absent(t, "Bypass permissions?", "a question when turning it off")
        assert api("GET", "/api/state")["settings"]["bypass_approvals"] is False
    finally:
        api("PUT", "/api/settings", {"bypass_approvals": keep})


@sect
def s_dialogs():
    section("dialog keys: the shortcuts scroll (j/k, PgUp/PgDn, Home/End) and close with Esc, Enter, q, ? or F1; /help-style panels close with Esc, Enter, q or Space; /model's list moves with j/k/Tab, closes with Esc or q; a prompt edits at its cursor; F1 and Ctrl+K leave a prompt alone but replace other panels; Ctrl+C closes a dialog; pastes go into prompts, the palette, the memory search and forms")
    title = f"Dialogs {STAMP}"
    cid = seed(title)
    t = launch(rows=24, cols=100, extra=("--chat", cid))
    expect(t, "Done.", 10)
    to_box(t)

    def span():
        m = re.search(r"scroll \((\d+)–(\d+) of (\d+)\)", t.text())
        return m and (int(m[1]), int(m[2]), int(m[3]))

    t.key("f1", 0.5)
    expect(t, "Keyboard shortcuts", 3)
    start, end, total = span()
    assert start == 1
    for key, first in [("j", 2), ("k", 1), ("down", 2), ("up", 1), ("pgdn", 11), ("pgup", 1)]:
        t.key(key, 0.25)
        assert span()[0] == min(first, total - (end - start)), (key, span())
    t.key("end", 0.3)
    assert span()[1] == total, span()
    t.key("home", 0.3)
    assert span()[0] == 1
    t.key("x", 0.3)
    expect(t, "Keyboard shortcuts", 1, "the shortcuts, still open after x")
    t.key("esc", 0.4)
    absent(t, "Keyboard shortcuts")
    for key in ("enter", "q", "?", "f1"):
        t.key("f1", 0.5)
        expect(t, "Keyboard shortcuts", 2)
        t.key(key, 0.4)
        absent(t, "Keyboard shortcuts", f"the shortcuts after {key}")
    for key in ("esc", "enter", "q", " "):
        say(t, "/help", 0.8)
        expect(t, "List every command and the keyboard shortcuts", 3)
        t.key(key, 0.4)
        absent(t, "List every command", f"/help after {key!r}")
    assert box_focused(t)
    say(t, "/help", 0.8)
    before = t.lines()
    t.key("j", 0.3)
    assert t.lines() != before, "j scrolls a long /help panel"
    t.key("k", 0.3)
    assert t.lines() == before
    t.key("esc", 0.4)
    # /model's list
    for close_key in ("esc", "q"):
        say(t, "/model", 1.0)
        expect(t, "Model for", 3)

        def picked():
            return next((l for l in t.lines() if " ▸ " in l and ("default ·" in l or "mock-model" in l)), "")

        assert "default ·" in picked(), picked()
        t.key("j", 0.3)
        assert "mock-model" in picked() and "default" not in picked(), picked()
        t.key("k", 0.3)
        assert "default ·" in picked()
        t.key("tab", 0.3)
        assert "default" not in picked()
        t.key(close_key, 0.4)
        absent(t, "Model for")
    # a prompt
    t.key("f2", 0.5)
    expect(t, "Rename chat", 3)

    def value():
        y = row_of(t, "Rename chat")
        return t.lines()[y + 1].split("│")[1].strip() if y is not None else None

    assert value() == title, value()
    t.key("ctrl-u")
    t.type("abc")
    t.key("left")
    t.key("left")
    t.type("X")
    assert value() == "aXbc", value()
    t.key("delete")
    assert value() == "aXc", value()
    t.key("home")
    t.key("delete")
    t.key("end")
    t.key("backspace")
    t.key("right")
    assert value() == "X", value()
    t.key("f1", 0.4)
    t.key("ctrl-k", 0.4)
    absent(t, "Keyboard shortcuts")
    absent(t, "Search and commands")
    expect(t, "Rename chat", 1, "the prompt, kept over F1 and Ctrl+K")
    t.paste("pasted\ttwo\nlines\n")
    assert value() == "Xpasted two lines", value()
    t.key("ctrl-c", 0.4)
    absent(t, "Rename chat")
    assert alive(t) and summary(cid)["title"] == title
    # panels that hold no typing are replaced
    t.key("alt-m", 1.0)
    expect(t, "Memory ·", 3)
    t.key("ctrl-k", 0.5)
    expect(t, "Search and commands", 2)
    absent(t, "Memory ·")
    t.key("f1", 0.5)
    expect(t, "Keyboard shortcuts", 2)
    absent(t, "Search and commands")
    t.key("ctrl-k", 0.5)
    expect(t, "Search and commands", 2)
    absent(t, "Keyboard shortcuts")
    t.key("ctrl-c", 0.4)
    absent(t, "Search and commands")
    assert alive(t), "Ctrl+C in the palette quit"
    # pastes
    t.key("ctrl-k", 0.5)
    t.paste(title)
    expect(t, f"▸ {title}", 3, "the pasted query's best match")
    t.key("esc", 0.4)
    t.key("alt-m", 1.0)
    t.key("/", 0.3)
    t.paste("zzqq\n")
    expect(t, "Nothing matches.", 3)
    t.key("esc", 0.3)
    t.key("esc", 0.3)
    absent(t, "Memory ·")
    t.key("alt-s", 1.0)
    for _ in range(11):
        t.key("tab", 0.08)
    t.key("ctrl-u")
    t.paste(MOCK)
    assert field_value(t, "Search engine URL") == MOCK, field_value(t, "Search engine URL")
    t.key("esc", 0.4)


@sect
def s_quit():
    section("Ctrl+C clears the box first, then quits when there's nothing to clear; the terminal is handed back")
    cid = new_chat(f"Quit {STAMP}")
    t = launch(extra=("--chat", cid))
    expect(t, "Assistant is ready", 10)
    to_box(t)
    t.type("abc")
    t.key("ctrl-c", 0.5)
    assert box_text(t) == [""] and alive(t), "Ctrl+C should empty the box and keep the app"
    quit_ok(t, "ctrl-c")
    for seq in (b"\x1b[?1049l", b"\x1b[?1000l", b"\x1b[?2004l", b"\x1b[?25h"):
        assert seq in t.log, f"{seq!r} not written on the way out"


# ======================================================================= main

def main():
    assert BIN, "build the TUI first: cd tui && cargo build --release"
    only = {s for s in os.environ.get("KEYS_ONLY", "").split(",") if s}
    before = api("GET", "/api/state")["settings"]
    api("PUT", "/api/settings", {"base_url": MOCK, "bypass_approvals": False, "auto_title": True, "auto_memory": False})
    for c in chats():
        if c["status"] != "idle":
            api("POST", f"/api/chats/{c['id']}/stop")
    failed = []
    try:
        for fn in SECTIONS:
            if only and fn.__name__ not in only:
                continue
            try:
                SOFT.clear()
                fn()
                if SOFT:
                    raise AssertionError(f"{len(SOFT)} check(s) failed: " + "; ".join(SOFT))
            except Exception:
                failed.append(fn.__name__)
                if OPEN:
                    print("--- screen at failure ---")
                    print("\n".join(OPEN[-1].lines()))
                traceback.print_exc()
                sys.stdout.flush()
            finally:
                for t in OPEN:
                    close(t)
                OPEN.clear()
    finally:
        api("PUT", "/api/settings", {k: before[k] for k in SETTINGS if k in before})
        if SCRATCH:
            SCRATCH.kill()
        print(f"(files from this run: {WORK})")
    if failed:
        print(f"\nFAILED: {' '.join(failed)}")
        sys.exit(1)
    print("\nall TUI key tests passed")


if __name__ == "__main__":
    main()
