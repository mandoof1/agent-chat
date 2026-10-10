"""Slash commands in the Rust TUI, exhaustively: every command and every alias, with no arguments,
good ones, bad ones, unicode, very long ones, extra spaces and MiXeD case, in the states that
matter. Driven in a pseudo-terminal against the app on AC_URL wired to tests/mock_llm.py (MOCK_URL):
`uv run --with pyte python tests/e2e_tui_slash.py` (TUI_BIN picks the binary; release, then debug).

The app may be one other suites already used: everything here makes its own agents and chats with
names stamped for this run, runs its TUIs with a config folder of their own, and puts back the
settings it changes. SLASH_ONLY=menu,model runs just the sections whose names contain those words.

Covers: the welcome page (/ filters the sidebar there; nothing runs or sends without a chat); the
menu (opens only for a / at the very start while the cursor is in the first word, all 27 commands in
order, 8 rows that scroll, ↑↓ and Ctrl+P/Ctrl+N wrapping, filtering by names and aliases with the
alias shown, Tab completing with and without arguments, Enter on a partial name, Esc keeping the
text, Backspace closing it, the keys it takes over, narrow and short terminals, a resize); every
panel a command opens, by name and alias in every case and with ignored arguments; unknown
commands, // and paths sent as messages, edit-last escaping a leading /; pastes that start with /
(bracketed and keystrokes, small and large) never running; /new /resume /rename /pin /branch
/retry /edit /stop /copy /delete /export /theme /files /verbose /remember /memory /permissions
/model with each kind of argument, checked on the server; /compact twice and the chat footer's
numbers; /context /usage /status /model against GET /api/chats/{id} and /api/server (also with the
model server down); a running chat (idle-only commands refused with the text kept, the rest run at
once and never queue), a waiting approval, a sub-agent's chat, a chat whose agent was deleted, and
/quit /exit (also mid-reply).
"""
import base64
import json
import os
import re
import sys
import tempfile
import time

import httpx

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tui_driver import Tui  # noqa: E402

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")
MOCK = os.environ.get("MOCK_URL", "http://127.0.0.1:8766/v1")
BIN = os.environ.get("TUI_BIN") or next((p for p in ("tui/target/release/agent-chat-tui", "tui/target/debug/agent-chat-tui") if os.path.exists(p)), None)
H = {"X-Agent-Chat": "1"}
STAMP = f"{int(time.time()) % 10**6:06d}{os.getpid() % 100:02d}"  # in every name this run makes
ROOT = tempfile.mkdtemp(prefix="agent-chat-tui-slash-")
WORK = os.path.join(ROOT, "cwd")  # where the TUIs run, so where /export writes
XDG = os.path.join(os.environ.get("XDG_CONFIG_HOME") or ROOT, f"slash-{STAMP}")  # a state.json of our own
CHANGED = ("base_url", "bypass_approvals", "auto_title", "auto_memory")  # settings this suite sets (and puts back)
HINT = "Message  (Enter sends · Alt+Enter new line · / commands · F1 help)"  # the empty box's placeholder
KEY = {"ctrl-p": "\x10", "ctrl-n": "\x0e", "home": "\x1b[H", "end": "\x1b[F", "left": "\x1b[D", "right": "\x1b[C", "alt-enter": "\x1b\r", "alt-y": "\x1by"}

# The spec's commands (and the TUI's /quit), in the registry's order: name, aliases, arguments, idle-only
# (refused while the agent works; /pin too: the server answers "chat is busy" mid-run)
SPEC = [
    ("help", [], "", False), ("new", ["clear"], "[agent]", False), ("resume", ["chats"], "[search]", False),
    ("rename", [], "[title]", True), ("pin", [], "", True), ("branch", ["fork"], "", False),
    ("retry", ["regenerate"], "", True), ("edit", [], "", True), ("stop", [], "", False),
    ("compact", [], "[instructions]", True), ("export", [], "[md|json]", False), ("copy", [], "", False),
    ("delete", [], "", True), ("agents", [], "", False), ("model", [], "[name]", False),
    ("memory", [], "", False), ("remember", [], "<fact>", False), ("permissions", [], "[ask|bypass]", False),
    ("settings", ["config"], "", False), ("routines", [], "", False), ("files", [], "", False),
    ("verbose", [], "", False), ("theme", [], "[dark|light|plain]", False), ("context", [], "", False),
    ("usage", ["cost", "stats"], "", False), ("status", [], "", False), ("quit", ["exit"], "", False),
]
IDLE = [n for n, _, _, idle in SPEC if idle]


# ------------------------------------------------------------------ the app

def api(method, path, body=None):
    r = httpx.request(method, B + path, json=body, headers=H, timeout=30)
    r.raise_for_status()
    return r.json()


def chats():
    return api("GET", "/api/chats")


def chat(cid):
    return api("GET", f"/api/chats/{cid}")


def summary(cid):
    return next((c for c in chats() if c["id"] == cid), None)


def ids():
    return {c["id"] for c in chats()}


def new_since(before):
    """Chats made since `before` (a set of ids), leaving out any a routine made meanwhile."""
    return [c for c in chats() if c["id"] not in before and not c.get("routine_id")]


def users(cid):
    return [m["content"] for m in chat(cid)["messages"] if m["role"] == "user"]


def agent(aid):
    return next(a for a in api("GET", "/api/state")["agents"] if a["id"] == aid)


def settings():
    return api("GET", "/api/state")["settings"]


def make_agent(name, **kw):
    return api("POST", "/api/agents", {"name": name, "purpose": "", "tools": ["run_shell", "ask_agent"], "confirm_shell": True, "memory": False} | kw)


def make_chat(agent_id, title):
    cid = api("POST", "/api/chats", {"agent_id": agent_id})["id"]
    api("PATCH", f"/api/chats/{cid}", {"title": title})
    return cid


def exchange(cid, text, timeout=30):
    """Send a message through the API and wait for the reply to be saved."""
    n = len(chat(cid)["messages"])
    api("POST", f"/api/chats/{cid}/run", {"content": text})
    end = time.time() + timeout
    while time.time() < end:
        s = summary(cid)
        if s["status"] == "idle" and s["messages"] > n + 1:
            return
        time.sleep(0.2)
    raise AssertionError(f"no reply to {text!r} in {cid}")


def memories():
    return [m["fact"] for m in api("GET", "/api/memory")["memories"]]


# ------------------------------------------------------------------ the screen

def section(name):
    print(f"== {name}", flush=True)


def until(t, cond, timeout=10, what="the condition"):
    """Pump the TUI until cond() is truthy; returns its value."""
    end = time.time() + timeout
    while True:
        v = cond()
        if v:
            return v
        if time.time() > end:
            print("\n".join(t.lines()))
            raise AssertionError(f"timed out waiting for {what}")
        t.pump(0.15)


def expect(t, needle, timeout=10, what=None):
    until(t, lambda: find(t, needle), timeout, f"{what or needle!r} on screen")


def absent(t, needle, what=None):
    if find(t, needle):
        print("\n".join(t.lines()))
        raise AssertionError(f"did not expect {what or needle!r} on screen")


def stays_absent(t, needle, seconds=0.8, what=None):
    """Not on screen now nor for a while (for things that would show up after a delay)."""
    end = time.time() + seconds
    while time.time() < end:
        absent(t, needle, what)
        t.pump(0.1)
    absent(t, needle, what)


def row(t, y, x0=0, x1=None):
    """Row y between columns x0 and x1 as read: the blank cell pyte keeps after a wide character
    dropped, so CJK and emoji compare as typed."""
    cells = t.screen.buffer[y]
    return "".join((cells[x].data if x in cells else " ") for x in range(x0, t.cols if x1 is None else x1))


def cols(t):
    """The screen with one character per column, so a string index is a column: a cell's first
    character (pyte keeps combining marks in their letter's cell; the display() text would shift
    the rest of a row by one for each, and other suites leave chats titled with plenty of them in
    the sidebar), a blank for the second half of a wide one."""
    return ["".join(((t.screen.buffer[y][x].data or " ")[0]) if x in t.screen.buffer[y] else " " for x in range(t.cols)) for y in range(t.rows)]


def rows(t):
    return [row(t, y).rstrip() for y in range(t.rows)]


def find(t, needle):
    return any(needle in r for r in rows(t))


def screen(t):
    return "\n".join(rows(t))


GOING = re.compile(r"going for \d+[smhd]( \d+[mh])?|\d+ \w+ ago")


def intact(t, what):
    """The screen matches a full repaint (running times masked: the footer's and the welcome page's
    "16 min ago" count on). Toasts are waited out first: one expiring mid-check would read as damage."""
    end = time.time() + 9
    while toasts_up(t) and time.time() < end:
        t.pump(0.2)
    diff = [(y, a, b) for y, a, b in t.repaint_diff() if GOING.sub("going for #", a) != GOING.sub("going for #", b)]
    if diff:
        for y, a, b in diff[:6]:
            print(f"row {y}\n  shown:   {a!r}\n  redrawn: {b!r}")
        raise AssertionError(f"the screen was corrupted after {what}")


def box(t):
    """The message box as drawn: (its text rows, the bar under them). The placeholder reads as ''."""
    d = cols(t)
    y1 = max(y for y, l in enumerate(d) if "└" in l)
    x = d[y1].index("└")
    right = d[y1].index("┘", x)
    tops = [y for y in range(y1) if d[y][x] == "┌"]
    assert tops, "the message box isn't on screen (covered by a dialog?)"
    y0 = tops[-1]
    text = [row(t, y, x + 2, right).rstrip() for y in range(y0 + 1, y1 - 1)]
    if len(text) == 1 and text[0].strip().startswith("Message  (") and HINT.startswith(text[0].strip()):  # (after the cursor's cell, cut to fit)
        text = [""]
    return text, row(t, y1 - 1, x + 1, right).strip()


def box_text(t):
    return "\n".join(box(t)[0])


def bar(t):
    return box(t)[1]


def menu(t):
    """The open command menu: (the commands in its rows, the selected one, (n, of) or None), else None."""
    d = cols(t)
    top = next((y for y, l in enumerate(d) if "┌ commands " in l), None)
    if top is None:
        return None
    x = d[top].index("┌ commands ")
    bottom = next((y for y in range(top + 1, t.rows) if d[y][x] == "└"), None)
    if bottom is None:
        return None
    names, sel = [], None
    for y in range(top + 1, bottom):
        m = re.match(r"( ▸ |   )(/[a-z]+)", d[y][x + 1:])
        assert m, f"a menu row: {d[y]!r}"
        names.append(m[2])
        if m[1] == " ▸ ":
            sel = m[2]
    pos = re.search(r" (\d+)/(\d+) ┐", d[top])
    return names, sel, pos and (int(pos[1]), int(pos[2]))


def menu_row(t, name):
    """The menu row of /name as drawn (args hint, description, an alias that matched)."""
    return next((r for r in rows(t) if re.search(rf"(▸|│)\s+/{name}\b", r)), None)


def no_menu(t, what):
    if menu(t):
        print("\n".join(t.lines()))
        raise AssertionError(f"the menu is open {what}")


def alive(t):
    assert t.pid and os.waitpid(t.pid, os.WNOHANG) == (0, 0), "the TUI quit"


def exited(t, timeout=8):
    """The TUI's exit status once it has ended (None: still running)."""
    end = time.time() + timeout
    while time.time() < end:
        done, status = os.waitpid(t.pid, os.WNOHANG)
        if done:
            t.pump(0.1)
            t.pid = None
            return status
        t.pump(0.1)
    return None


def close(t):
    if t.pid:
        t.close()


def bgs(t):
    return {t.screen.buffer[y][x].bg for y in range(t.rows) for x in range(t.cols)}


# ------------------------------------------------------------------ driving it

def start(cid=None, cols=120, rows_=40, focus=True):
    """A TUI on chat `cid` (or the welcome page), its snapshot drawn and the message box focused."""
    os.makedirs(WORK, exist_ok=True)
    t = Tui(BIN, B, cols=cols, rows=rows_, cwd=WORK, env={"XDG_CONFIG_HOME": XDG}, extra=("--chat", cid) if cid else ())
    if not cid:
        expect(t, "Pick an agent.", 15)
        return t
    c = chat(cid)
    loaded = "going for" if c["messages"] else "is ready." if any(a["id"] == c["agent_id"] for a in api("GET", "/api/state")["agents"]) else "Ready."
    expect(t, loaded, 15, "the chat's snapshot")
    if focus:
        focus_box(t)
    return t


def focus_box(t):
    """Click into the chat (it focuses the message box) and make sure typing lands there."""
    if t.cols >= 90:
        t.click(t.cols - 20, 3, 0.3)
    else:
        t.key("tab", 0.2)
        t.key("tab", 0.3)
    t.type("z", wait=0.1)
    until(t, lambda: box_text(t) == "z", 5, "a key typed into the message box")
    t.key("backspace", 0.1)
    until(t, lambda: box_text(t) == "", 5, "the emptied box")


def put(t, text):
    """Put `text` in the empty message box in one write (it arrives as a paste, menu and all)."""
    assert box_text(t) == "", f"the box isn't empty: {box_text(t)!r}"
    t.send(text, 0.05)
    # (it shows once the burst is over, so an Enter after this is a key of its own)
    until(t, lambda: box_text(t) != "", 5, f"{text[:40]!r} in the message box")


def run(t, text, wait=0.25):
    """Type a command (or message) into the empty box and press Enter."""
    put(t, text)
    t.key("enter", wait)


def clear(t):
    """Empty the box (Ctrl+C: it clears text, and would quit with none, so only with some)."""
    if box_text(t):
        t.key("ctrl-c", 0.2)
    until(t, lambda: box_text(t) == "", 5, "the emptied box")


def kept(t, text, what):
    """The refused text is still in the box, as typed (trailing spaces don't show)."""
    want = "\n".join(l.rstrip() for l in text.split("\n"))
    until(t, lambda: box_text(t) == want, 5, f"{what}: {text!r} kept in the box")


def cleared(t, what):
    until(t, lambda: box_text(t) == "", 5, f"the box emptied by {what}")


def esc_closes(t, marker):
    t.key("esc", 0.15)
    until(t, lambda: not find(t, marker), 5, f"{marker!r} closed by Esc")


def running(t, cid, timeout=10):
    """Until the server and the TUI both see the chat's run going."""
    until(t, lambda: summary(cid)["status"] != "idle", timeout, "the run to start")
    until(t, lambda: "Stop ■" in bar(t) or "not queued" in bar(t), timeout, "the TUI to see the run")


def settled(t, cid, timeout=40):
    """Until the chat's run is over, on the server and in the TUI."""
    until(t, lambda: summary(cid)["status"] == "idle", timeout, "the run to end")
    until(t, lambda: "Stop ■" not in bar(t) and "not queued" not in bar(t), 10, "the TUI to see the run end")


def toast_n(t, text):
    """How many times the toast starting with `text` is counted on screen (×N; 0 when it isn't
    there). A toast merges only with the newest one, so this counts repeats in a row."""
    d, probe, n = cols(t), text[:40], 0
    for y, r in enumerate(d):
        i = r.find(probe)
        x0, x1 = r.rfind("│", 0, max(i, 0)), r.find("│", i + 1)
        if i < 0 or x0 < 0 or x1 < 0:
            continue
        parts = []
        for yy in range(y, min(y + 5, t.rows)):
            if d[yy][x0] != "│":
                break
            parts.append(d[yy][x0 + 1:x1].strip())
        m = re.search(r" ×(\d+)$", " ".join(parts))
        n = max(n, int(m[1]) if m else 1)
    return n


def toasts_up(t):
    """Whether a toast shows: they sit at the trace's right, one column in from the edge."""
    return any(l[-2] in "┐│┘" and l[-1] == " " for l in cols(t)[1:-4])


def toasted(t, text, before=0, timeout=5):
    """Until the toast `text` shows more often than `before` (the count when the command ran)."""
    until(t, lambda: toast_n(t, text) > before, timeout, f"the toast {text!r}" + (f" (×{before + 1})" if before else ""))


def fresh_toast(t, text, timeout=5):
    """A toast that wasn't on screen: wait for any copy left from earlier to expire first."""
    until(t, lambda: toast_n(t, text) == 0, 9, f"an earlier {text!r} toast to expire")
    return 0


# ------------------------------------------------------------------ sections

def s_welcome():
    section("welcome page: no message box, so / is the sidebar's filter; nothing typed or pasted runs, sends or quits")
    before = ids()
    t = start(rows_=max(40, len(api("GET", "/api/state")["agents"]) + 20))  # (the chats, and their filter, are listed under every agent)
    try:
        expect(t, "/ filter", 3, "the sidebar's hint that / filters")
        t.type("/help")
        expect(t, "/help▏", 3, "the text in the sidebar's filter")
        t.key("enter", 0.4)
        stays_absent(t, "Start a message with //", what="the /help panel")
        t.key("esc", 0.3)
        until(t, lambda: not find(t, "/help"), 3, "the filter cleared")
        t.paste("/new\r")
        t.send("/quit\r", 0.6)  # a keystroke paste
        t.paste("/delete\r\n/exit\r\n")
        t.pump(0.5)
        alive(t)
        absent(t, "New chat with")
        absent(t, "Delete “")
        assert not new_since(before), new_since(before)
        intact(t, "commands typed and pasted on the welcome page")
        palette_commands(t, "the welcome page")
        assert not new_since(before), new_since(before)
    finally:
        close(t)


def palette_commands(t, where):
    """With no message box, Ctrl+K then / lists the slash commands and runs them: /help shows the
    commands, /new (no chat to take the agent from) the agent picker, /status its panel; one that
    needs a chat isn't listed, and nothing typed runs anything by itself."""
    t.key("ctrl-k", 0.4)
    expect(t, "Search and commands", 3)
    t.type("/he")
    expect(t, "▸ /help", 3, f"/help listed in the palette on {where}")
    t.key("enter", 0.5)
    expect(t, "Start a message with //", 3, f"the /help panel, from the palette on {where}")
    t.key("esc", 0.3)
    t.key("ctrl-k", 0.4)
    t.type("/STATUS")
    t.key("enter", 0.6)
    expect(t, "terminal client", 5, f"/status from the palette on {where}")
    t.key("esc", 0.3)
    t.key("ctrl-k", 0.4)
    t.type("/rename now")
    expect(t, "Nothing matches.", 3, f"/rename (it needs a chat you write in) listed on {where}")
    t.key("enter", 0.4)
    t.key("esc", 0.3)
    until(t, lambda: not find(t, "Search and commands"), 3, "the palette to close")
    t.key("ctrl-k", 0.4)
    t.type("/new")
    t.key("enter", 0.5)
    expect(t, "New chat with", 3, f"/new from the palette on {where}: the agent picker")
    t.key("esc", 0.3)
    until(t, lambda: not find(t, "New chat with"), 3, "the agent picker to close")


def s_menu(ops):
    section("the menu: every command in order, 8 rows scrolling with the selection, ↑↓ Ctrl+P Ctrl+N wrapping, the keys it takes over")
    cid = make_chat(ops["id"], f"SC{STAMP} menu")
    t = start(cid)
    try:
        t.type("/")
        names, sel, pos = until(t, lambda: menu(t), 5, "the command menu")
        assert (len(names), sel, pos) == (8, "/help", (1, 27)), (names, sel, pos)
        assert box_text(t) == "/" and "Enter runs /help" in bar(t) and "Run ▸" in bar(t), box(t)
        assert "/new [agent]" in menu_row(t, "new") and "New chat with this chat's agent" in menu_row(t, "new"), menu_row(t, "new")
        seen, drawn = [sel], {sel: menu_row(t, "help")}
        for i in range(2, 28):
            t.key("down", 0.05)
            names, sel, pos = until(t, lambda: (m := menu(t)) and m[2] == (i, 27) and m, 3, f"the selection on row {i}")
            assert len(names) == 8 and names[-1 if i > 8 else i - 1] == sel, (i, names, sel)
            seen.append(sel)
            drawn[sel] = menu_row(t, sel[1:])
        assert seen == ["/" + c[0] for c in SPEC], seen
        for name, _, args, _ in SPEC:  # each row: /name, its arguments if it takes any, a description
            m = re.search(rf"▸ /{name}( \S+)?\s+(\S.*?)\s*│$", drawn["/" + name])
            assert m and (m[1] or "").strip() == args and len(m[2]) > 10, (name, drawn["/" + name])
        t.key("down", 0.1)
        until(t, lambda: menu(t)[1:] == ("/help", (1, 27)), 3, "Down from the last row wraps to the first")
        t.key("up", 0.1)
        until(t, lambda: menu(t)[1:] == ("/quit", (27, 27)), 3, "Up from the first row wraps to the last")
        t.send(KEY["ctrl-n"], 0.2)
        until(t, lambda: menu(t)[1:] == ("/help", (1, 27)), 3, "Ctrl+N wraps down")
        t.send(KEY["ctrl-p"], 0.2)
        until(t, lambda: menu(t)[1:] == ("/quit", (27, 27)), 3, "Ctrl+P wraps up")
        t.send(KEY["ctrl-p"], 0.2)
        until(t, lambda: menu(t)[1:] == ("/status", (26, 27)), 3, "Ctrl+P moves up")
        absent(t, "New chat with", "the agent picker (Ctrl+N belongs to the menu while it is open)")
        t.key("tab", 0.2)  # completes the pick instead of moving the focus
        until(t, lambda: box_text(t) == "/status", 3, "Tab completing /status")
        t.key("up", 0.2)  # moves in the menu, not up into the trace
        t.type("x", wait=0.1)
        until(t, lambda: box_text(t) == "/statusx", 3, "typing still in the box")
        clear(t)

        section("the menu opens only for a / at the very start with the cursor in the first word; Esc and Backspace close it")
        for text, what in [(" /", "after a leading space"), ("hi /", "for a / later in the text"), ("//", "for // (a message starting with /)"),
                           ("/re/", "once the first word has a second / (a path)"), ("/日本", "when nothing matches"), ("/zzz", "when nothing matches")]:
            t.type(text)
            no_menu(t, what)
            clear(t)
        t.type("/re")
        assert menu(t)[0] == ["/resume", "/rename", "/retry", "/remember"], menu(t)
        t.type(" x")
        no_menu(t, "with the cursor past the first word")
        t.send(KEY["home"], 0.3)
        until(t, lambda: menu(t) and menu(t)[0][0] == "/resume", 3, "the menu back with the cursor at the start of the word")
        t.send(KEY["right"], 0.2)
        t.send(KEY["right"], 0.2)
        assert menu(t), "the cursor inside the word keeps it open"
        t.send(KEY["end"], 0.3)
        no_menu(t, "with the cursor at the end again")
        t.send(KEY["home"], 0.3)
        assert menu(t)
        t.send(KEY["end"], 0.2)
        t.send(KEY["alt-enter"], 0.3)
        no_menu(t, "with the cursor on a second line")
        assert box(t)[0] == ["/re x", ""], box(t)
        clear(t)
        t.type("/he")
        assert menu(t)[0] == ["/help", "/theme"], menu(t)
        t.key("backspace", 0.1)
        t.key("backspace", 0.2)
        until(t, lambda: menu(t) and menu(t)[2] == (1, 27), 3, "every command again for a lone /")
        t.key("backspace", 0.3)
        no_menu(t, "once the / is deleted")
        assert box_text(t) == ""
        t.type("/re")
        t.key("esc", 0.3)
        no_menu(t, "after Esc")
        assert box_text(t) == "/re", "Esc keeps the text"
        t.type("m")
        until(t, lambda: menu(t) and menu(t)[0] == ["/remember"], 3, "typing on opens it again, filtered")
        t.key("esc", 0.2)
        t.key("esc", 0.3)  # a second Esc is the box's: the trace takes the focus, the text stays
        no_menu(t, "after Esc twice")
        assert box_text(t) == "/rem"
        focus_box_keep(t)
        clear(t)

        section("filtering: an exact name or alias first, then names that start with it, then aliases that do, then names that only contain it (registry order within each); an alias that matched shows")
        for typed, want, shown in [
            ("/re", ["/resume", "/rename", "/retry", "/remember"], {}),
            ("/reg", ["/retry"], {"retry": "/retry /regenerate"}),
            ("/cl", ["/new"], {"new": "/new /clear [agent]"}),
            ("/fo", ["/branch"], {"branch": "/branch /fork"}),
            ("/ch", ["/resume", "/branch"], {"resume": "/resume /chats [search]"}),
            ("/co", ["/compact", "/copy", "/context", "/settings", "/usage"], {"settings": "/settings /config", "usage": "/usage /cost"}),
            ("/sta", ["/status", "/usage"], {"usage": "/usage /stats"}),  # (Enter on "/sta" runs /status)
            ("/stats", ["/usage"], {"usage": "/usage /stats"}),
            ("/ex", ["/export", "/quit", "/context"], {"quit": "/quit /exit"}),
            ("/ry", ["/retry", "/memory"], {}),
            ("/m", ["/model", "/memory", "/resume", "/rename", "/compact", "/remember", "/permissions", "/theme"], {}),
            ("/HELP", ["/help"], {}),
            ("/ReS", ["/resume"], {}),
        ]:
            t.type(typed)
            got = until(t, lambda: menu(t), 3, f"the menu for {typed}")
            assert got[0] == want and got[1] == want[0], (typed, got)
            assert got[2] is None, f"{typed}: no counter while every match fits"
            for name, text in shown.items():
                assert text in menu_row(t, name), (typed, menu_row(t, name))
            clear(t)

        section("Tab completes the selected command (a space when it takes arguments), keeps what follows; an alias completes to the name")
        for typed, downs, want in [("/ren", 0, "/rename "), ("/he", 0, "/help"), ("/reg", 0, "/retry"), ("/cos", 0, "/usage"), ("/exi", 0, "/quit"),
                                   ("/re", 2, "/retry"), ("/", 3, "/rename "), ("/MOD", 0, "/model ")]:
            t.type(typed)
            for _ in range(downs):
                t.key("down", 0.1)
            t.key("tab", 0.3)
            until(t, lambda: box_text(t) == want.rstrip() or box_text(t) == want, 3, f"{typed} completed")
            t.type("Z", wait=0.1)
            until(t, lambda: box_text(t) == want + "Z", 3, f"the cursor right after {want!r}")
            clear(t)
        t.type("/ren")
        t.key("tab", 0.3)
        no_menu(t, "once the completion added the space")
        assert "Enter runs /rename" in bar(t), bar(t)
        t.type(f"Tabbed {STAMP}")
        t.key("enter", 0.6)
        until(t, lambda: chat(cid)["title"] == f"Tabbed {STAMP}", 5, "the chat renamed by the completed command")
        cleared(t, "running /rename")
        t.type(f"/ren mid {STAMP}")
        for _ in range(len(f" mid {STAMP}") + 1):
            t.send(KEY["left"], 0.03)
        t.pump(0.3)
        until(t, lambda: menu(t) and menu(t)[1] == "/rename", 3, "the menu with the cursor back in the word")
        t.key("tab", 0.3)
        until(t, lambda: box_text(t) == f"/rename mid {STAMP}", 3, "the name completed, the arguments kept")
        t.key("enter", 0.6)
        until(t, lambda: chat(cid)["title"] == f"mid {STAMP}", 5, "the arguments that followed the completed name")

        section("Enter runs the selected command: a partial name, a moved selection, an alias's prefix; past the word Enter reads the text")
        t.type("/ren")
        t.key("enter", 0.5)
        expect(t, "Rename chat", 3, "/rename's prompt from /ren")
        cleared(t, "Enter on /ren")
        esc_closes(t, "Rename chat")
        t.type("/he")
        t.key("enter", 0.5)
        expect(t, "Start a message with //", 3, "/help from /he")
        esc_closes(t, "Start a message with //")
        t.type("/re")
        t.key("down", 0.1)
        t.key("enter", 0.5)
        expect(t, "Rename chat", 3, "the second match, /rename")
        esc_closes(t, "Rename chat")
        n = fresh_toast(t, "Nothing to regenerate yet.")
        t.type("/regen")
        t.key("enter", 0.5)
        toasted(t, "Nothing to regenerate yet.", n)  # /retry, in a chat with no reply
        cleared(t, "/regen")
        t.type("/ren x")
        t.key("enter", 0.5)
        expect(t, "Unknown command /ren.", 3, "/ren followed by text is not /rename")
        kept(t, "/ren x", "an unknown command")
        clear(t)
        assert users(cid) == [], "a command reached the agent"
        intact(t, "the menu")
    finally:
        close(t)


def focus_box_keep(t):
    """Back to the message box without touching its text."""
    t.click(t.cols - 20, 3, 0.3)


def s_narrow(ops):
    section("the menu at narrow widths and short heights (the terminal resized under it), and its selection across a resize")
    cid = make_chat(ops["id"], f"SC{STAMP} narrow")
    t = start(cid)
    try:
        for cols, rows_, shown in [(70, 24, 8), (50, 20, 8), (34, 14, None), (22, 12, None)]:
            t.resize(rows_, cols)
            t.type("/")
            m = until(t, lambda: menu(t), 5, f"the menu at {cols}x{rows_}")
            assert m[1] == "/help", m
            if shown:
                assert len(m[0]) == shown and m[2] == (1, 27), m
            if cols >= 40:
                keys = "↑↓ select · Tab complete · Enter run · Esc close" if cols >= 56 else "↑↓ · Tab complete · Enter run · Esc"
                assert find(t, keys), (cols, t.lines())
            if cols >= 50:
                assert "List every command" in menu_row(t, "help"), "descriptions while there is room"
            for _ in range(9):
                t.key("down", 0.05)
            until(t, lambda: menu(t) and menu(t)[1] == "/compact", 3, "the selection scrolled into view")
            t.type("co", wait=0.05)
            until(t, lambda: menu(t) and menu(t)[0][:2] == ["/compact", "/copy"], 3, "filtered at this width")
            intact(t, f"the menu at {cols}x{rows_}")
            t.key("down", 0.1)
            t.key("down", 0.1)
            t.key("down", 0.1)
            t.key("enter", 0.6)  # /settings (by its /config alias, after the names that start with "co")
            expect(t, "Settings", 4, f"/settings from the menu at {cols}x{rows_}")
            esc_closes(t, "Settings")
            alive(t)
            assert box_text(t) == "", box(t)
        t.resize(40, 120)
        t.type("/s")
        for _ in range(2):
            t.key("down", 0.1)
        sel = until(t, lambda: menu(t), 3, "the menu")[1]
        t.resize(30, 64)
        until(t, lambda: menu(t) and menu(t)[1] == sel, 3, "the same selection after a resize")
        intact(t, "the menu after a resize")
        t.resize(40, 120)
        until(t, lambda: menu(t) and menu(t)[1] == sel, 3, "the same selection back at 120 columns")
        intact(t, "the menu after resizing back")
    finally:
        close(t)


# the panels and dialogs the commands open (no arguments or ignored ones), how to tell they're up
PANELS = [
    ("help", "Start a message with //"), ("resume", "Search and commands"), ("chats", "Search and commands"),
    ("rename", "Rename chat"), ("delete", "Its messages, and any work other agents did"), ("agents", "Edit Slash"),
    ("model", "Model for "), ("memory", "Memory ·"), ("settings", "Model server URL"), ("config", "Model server URL"),
    ("routines", "r run now"), ("context", "/compact summarizes older messages now"), ("usage", "prompts summed over"),
    ("cost", "prompts summed over"), ("stats", "prompts summed over"), ("status", "terminal client"),
]
TAKES_ARGS = {"resume", "chats", "rename", "model"}  # (their arguments do something: their own sections)


def s_panels(ops):
    section("every panel a command opens, by name and alias, in any case, with trailing spaces and with arguments it ignores")
    cid = make_chat(ops["id"], f"SC{STAMP} panels")
    exchange(cid, f"hello panels {STAMP}")
    t = start(cid)
    try:
        for name, marker in PANELS:
            forms = [f"/{name}", f"/{name.upper()}", f"/{name[0].upper()}{name[1:]}", f"/{name}   "]
            if name not in TAKES_ARGS:
                forms += [f"/{name} ignored ünïcødé 日本語 ✓", f"/{name} " + "long " * 120]
            for form in forms:
                run(t, form, 0.4)
                expect(t, marker, 5, f"what {form!r} opens")
                esc_closes(t, marker)
                assert box_text(t) == "", f"{form!r} left {box_text(t)!r} in the box"
        expect(t, f"hello panels {STAMP}", 3)
        assert users(cid) == [f"hello panels {STAMP}"] and chat(cid)["title"] == f"SC{STAMP} panels", "a panel command changed the chat"
        assert summary(cid) is not None, "/delete's question, answered with Esc, kept the chat"

        section("/help lists every command and alias with its arguments, and how to send a message starting with /")
        run(t, "/help")
        expect(t, "Start a message with //", 5)
        text = screen(t)
        for _ in range(12):  # (scrolled, should it not fit)
            t.key("down", 0.05)
        text += screen(t)
        for name, aliases, _, _ in SPEC:
            assert re.search(rf"/{name}\b", text), f"/{name} missing from /help"
            for a in aliases:
                assert f"/{a}" in text, f"/{a} (an alias of /{name}) missing from /help"
        for call in ["/new [agent]", "/compact [instructions]", "/remember <fact>", "/export [md|json]", "/theme [dark|light|plain]", "/permissions [ask|bypass]", "/model [name]"]:
            assert call in text, call
        esc_closes(t, "Start a message with //")
        intact(t, "the panels")
    finally:
        close(t)


def s_unknown(ops):
    section("unknown commands say so and keep the text; // and paths go to the agent as messages; a leading space is no command")
    cid = make_chat(ops["id"], f"SC{STAMP} unknown")
    t = start(cid)
    try:
        for typed, word in [(f"/nosuch{STAMP}", f"nosuch{STAMP}"), (f"/NoSuch{STAMP}x and words", f"NoSuch{STAMP}x"), ("/ヘルプ", "ヘルプ"), ("/hélp", "hélp")]:
            run(t, typed)
            expect(t, f"Unknown command /{word}.", 5)
            kept(t, typed, f"unknown {typed}")
            clear(t)
        n = fresh_toast(t, "Unknown command /. ")
        t.type("/")
        t.key("esc", 0.2)
        t.key("enter", 0.4)
        toasted(t, "Unknown command /. ", n)
        kept(t, "/", "a lone / (menu closed)")
        clear(t)
        run(t, "/ spaced out")
        toasted(t, "Unknown command /. ", 1)
        kept(t, "/ spaced out", "/ followed by a space")
        clear(t)
        long_word = "q" * 600
        t.type("/")
        t.paste(long_word)
        t.key("enter", 0.6)
        expect(t, "│ /qqqqqqqqqqqqqqqqqqqq", 5, "the long unknown command's toast (the word wraps under \"Unknown command\")")
        until(t, lambda: box_text(t).endswith("qqqq") and "[Pasted" not in box_text(t), 3, "the long text kept")
        intact(t, "a very long unknown command")
        clear(t)
        assert users(cid) == [], users(cid)

        sent = []
        for typed, message in [(f"//hi {STAMP}", f"/hi {STAMP}"), (f"///triple {STAMP}", f"//triple {STAMP}"), ("//", "/"), ("//help", "/help"),
                               (f"/etc/hosts is broken {STAMP}", f"/etc/hosts is broken {STAMP}"), ("/help/me", "/help/me"), ("/HELP/ME", "/HELP/ME"),
                               ("/usr/bin/env", "/usr/bin/env"), ("  /help", "/help")]:
            run(t, typed, 0.3)
            sent.append(message)
            until(t, lambda: users(cid) == sent, 10, f"{typed!r} sent as {message!r}")
            settled(t, cid)
            absent(t, "Start a message with //", f"/help run by {typed!r}")
        absent(t, "Unknown command /help/me")

        section("edit-last puts a message starting with / back escaped, so it is resent as it was")
        run(t, "/edit")
        kept(t, "//help", "the last message (\"/help\", sent by a leading space), escaped")
        clear(t)
        run(t, f"//again {STAMP}", 0.3)
        sent.append(f"/again {STAMP}")
        until(t, lambda: users(cid) == sent, 10)
        settled(t, cid)
        run(t, "/edit")
        kept(t, f"//again {STAMP}", "the last message, escaped")
        t.type(" edited")
        t.key("enter", 0.5)
        sent[-1] = f"/again {STAMP} edited"
        until(t, lambda: users(cid) == sent, 10, "the edited message resent in place")
        settled(t, cid)
        run(t, "/edit")
        kept(t, f"//again {STAMP} edited", "the edited message, escaped again")
        clear(t)
        intact(t, "messages starting with /")

        section("the bar under the box says what Enter will do with an unknown /word (it neither sends nor runs it)")
        for typed in [f"/nosuch{STAMP}", "/ spaced"]:
            put(t, typed)
            hint = bar(t)
            clear(t)
            assert "Enter sends" not in hint and "Send ↑" not in hint, f"with {typed!r} in the box the bar says {hint!r}, but Enter only says it is an unknown command"
    finally:
        close(t)


def s_pastes(ops):
    section("pasted text starting with / never runs (bracketed or keystrokes, with line breaks); a bracketed one goes in escaped, to be sent as pasted; large pastes become tokens, also in arguments")
    cid = make_chat(ops["id"], f"SC{STAMP} pastes")
    before = ids()
    t = start(cid)
    try:
        t.paste("/help")
        until(t, lambda: box_text(t) == "//help", 3, "a bracketed paste at the start of the box escaped (Enter sends \"/help\")")
        stays_absent(t, "Start a message with //", what="/help run by a paste")
        clear(t)
        for name, paste in [("bracketed", lambda s: t.paste(s)), ("keystroke", lambda s: t.send(s, 0.6)), ("slow keystroke", lambda s: t.send_chunked(s, size=3, gap=0.004, wait=0.6))]:
            for text, lines in [("/delete\r", ["/delete", ""]), ("/new\r", ["/new", ""]), ("/quit\r/exit\r", ["/quit", "/exit", ""]), ("/stop\r/compact now\r/pin", ["/stop", "/compact now", "/pin"])]:
                if name == "bracketed":  # (a keystroke paste can't be told from fast typing, so it isn't escaped)
                    lines = ["/" + lines[0]] + lines[1:]
                paste(text)
                until(t, lambda: box(t)[0] == lines, 5, f"the {name} paste {text!r} in the box as text")
                t.pump(0.3)
                alive(t)
                absent(t, "Delete “", f"/delete run by a {name} paste")
                clear(t)
        assert not new_since(before), f"a paste made a chat: {new_since(before)}"
        s = summary(cid)
        assert s["messages"] == 0 and not s["pinned"], s
        # a large paste is a token whatever it starts with; Enter sends it as a message, unexpanded on screen
        big = "\n".join([f"/rename Big{STAMP}"] + [f"line {i} of a pasted note" for i in range(1, 20)])
        t.paste(big)
        until(t, lambda: box_text(t) == "[Pasted text #1 +20 lines]", 5)
        no_menu(t, "for a placeholder")
        t.key("enter", 0.5)
        until(t, lambda: users(cid) == [big], 10, "the large paste sent as a message")
        settled(t, cid)
        assert chat(cid)["title"] == f"SC{STAMP} pastes", "a pasted /rename ran"
        # a large paste in a command's arguments is expanded when the command runs
        fact = f"pastefact{STAMP} " + " ".join(f"lorem{i}x{STAMP}" for i in range(150))  # (all its words this run's own)
        t.type("/remember ")
        t.paste(fact)
        until(t, lambda: box_text(t) == f"/remember [Pasted text #1, {len(fact)} chars]", 5)
        t.key("enter", 0.6)
        until(t, lambda: any(f.startswith(f"pastefact{STAMP} lorem0x") for f in memories()), 5, "the expanded fact saved")
        saved = next(f for f in memories() if f.startswith(f"pastefact{STAMP}"))
        assert saved == fact[:500], saved
        cleared(t, "/remember with a pasted fact")
        # code that starts with a comment is sent with both its slashes, and "/*" is no command
        for code in [f"// fix the off-by-one {STAMP}\nfor i in 0..=n {{}}", f"/* {STAMP} */ int x;"]:
            t.paste(code)
            t.key("enter", 0.5)
            until(t, lambda: users(cid)[-1:] == [code], 10, f"the pasted {code.split()[0]!r} code sent as pasted")
            settled(t, cid)
        intact(t, "pastes starting with /")
    finally:
        close(t)


def s_new(ops, uni):
    section("/new and /clear: this chat's agent, or one named by id, name or prefix in any case; an unknown name says so")
    cid = make_chat(ops["id"], f"SC{STAMP} new")
    t = start(cid)
    try:
        for typed, want in [("/new", ops), ("/clear", ops), ("/NEW   ", ops), (f"/new {ops['id']}", ops), ("/new coder", "coder"), ("/CLEAR CODER", "coder"),
                            ("/new    cod   ", "coder"), (f"/new {uni['name'].upper()}", uni), (f"/Clear ünï{STAMP}", uni), (f"/new slash{STAMP}", ops)]:
            before = ids()
            run(t, typed, 0.8)
            made = until(t, lambda: new_since(before), 5, f"a chat made by {typed!r}")
            want_id = want if isinstance(want, str) else want["id"]
            assert len(made) == 1 and made[0]["agent_id"] == want_id, (typed, made)
            name = agent(want_id)["name"]
            expect(t, f"{name} is ready", 5, f"the new chat with {name}")
            cleared(t, typed)
        for typed, who in [(f"/new nobody{STAMP}", f"No agent called “nobody{STAMP}”"), ("/new 日本語エージェント", "No agent called “日本語エージェント”"),
                           ("/clear " + "z" * 300, "“" + "z" * 60)]:  # (a long name wraps under "No agent called")
            before = ids()
            run(t, typed, 0.6)
            expect(t, who, 5)
            assert not new_since(before), new_since(before)
            cleared(t, typed)
        intact(t, "/new")
    finally:
        close(t)


def s_resume(ops):
    section("/resume and /chats: the palette, prefilled with the search (any case, unicode, trimmed); Enter opens the chat")
    target = make_chat(ops["id"], f"Résumé 日本語 {STAMP}")
    cid = make_chat(ops["id"], f"SC{STAMP} resume")
    t = start(cid)
    try:
        for typed in ["/resume", "/chats", "/Resume   "]:
            run(t, typed)
            expect(t, "Search and commands", 5)
            until(t, lambda: any(re.search(r"│ ⌕ +│", r) for r in rows(t)), 3, "an empty search")
            esc_closes(t, "Search and commands")
        for typed, q in [(f"/chats Résumé 日本語 {STAMP}", f"Résumé 日本語 {STAMP}"), (f"/resume    {STAMP}   ", STAMP), (f"/RESUME résumé 日本語 {STAMP}", f"résumé 日本語 {STAMP}")]:
            run(t, typed)
            expect(t, f"⌕ {q}", 5, f"the search {q!r} (trimmed, case kept)")
            expect(t, f"Résumé 日本語 {STAMP}", 5, "the chat found")
            if q != STAMP:  # (the stamp is in other names too)
                expect(t, f"▸ Résumé 日本語 {STAMP}", 2, "the chat picked")
            if typed.startswith("/RESUME"):
                t.key("enter", 0.8)
                until(t, lambda: f"Résumé 日本語 {STAMP}" in row(t, 0), 5, "the chat opened")
                focus_box(t)
            else:
                esc_closes(t, "Search and commands")
        for typed in [f"/resume zzz{STAMP}nothing", f"/chats zz{STAMP}" + "w" * 300]:  # (other suites' chats hold runs of w)
            run(t, typed)
            expect(t, "Nothing matches.", 5, f"no chat for {typed[:30]!r}")
            intact(t, f"{typed[:20]!r}")
            esc_closes(t, "Search and commands")
        assert summary(target)["messages"] == 0
    finally:
        close(t)


def s_rename(ops):
    section("/rename: the prompt with no title; titles plain, MiXeD, spaced, unicode, very long and pasted, as the server keeps them")
    cid = make_chat(ops["id"], f"SC{STAMP} rename")
    t = start(cid)
    try:
        for typed in ["/rename", "/rename     ", "/RENAME"]:
            run(t, typed)
            expect(t, "Rename chat", 5)
            esc_closes(t, "Rename chat")
        assert chat(cid)["title"] == f"SC{STAMP} rename"
        run(t, "/rename")
        expect(t, "Rename chat", 5)
        t.type("X")  # the prompt holds the current title, the cursor after it
        t.key("enter", 0.6)
        until(t, lambda: chat(cid)["title"] == f"SC{STAMP} renameX", 5, "the prompt's title (the old one plus X) saved")
        run(t, "/rename")
        expect(t, "Rename chat", 5)
        t.key("ctrl-u", 0.2)
        t.type(f"Prompted {STAMP}")
        t.key("enter", 0.6)
        until(t, lambda: chat(cid)["title"] == f"Prompted {STAMP}", 5, "the prompt's title saved")
        long1, long2 = "L" + "o" * 299, "P" + "a" * 1499
        for typed, title in [(f"/rename Plain {STAMP}", f"Plain {STAMP}"), (f"/RENAME Upper {STAMP}", f"Upper {STAMP}"),
                             (f"/rename    Spaced   out {STAMP}   ", f"Spaced   out {STAMP}"), (f"/rename 日本語 ✓ émoji 🚀 {STAMP}", f"日本語 ✓ émoji 🚀 {STAMP}"),
                             ("/rename " + long1, long1[:120])]:
            run(t, typed, 0.6)
            until(t, lambda: chat(cid)["title"] == title, 5, f"the title from {typed[:30]!r}")
            expect(t, f"Renamed to “{title[:30]}" if len(title) < 60 else f"“{title[:30]}", 5)  # (a long one wraps under "Renamed to")
            cleared(t, typed[:20])
        expect(t, long1[:40], 3, "the long title in the header, cut to fit")
        t.type("/rename ")
        t.paste(long2)
        until(t, lambda: box_text(t) == "/rename [Pasted text #1, 1500 chars]", 5)
        t.key("enter", 0.6)
        until(t, lambda: chat(cid)["title"] == long2[:120], 5, "the pasted title, expanded")
        run(t, f"/rename 日本語 {STAMP}", 0.6)
        until(t, lambda: f"日本語 {STAMP}" in row(t, 0), 5, "the unicode title in the header")
        intact(t, "/rename")
    finally:
        close(t)


def s_chat_actions(ops):
    section("/pin /stop /copy /retry /regenerate /edit /branch /fork /delete on a chat with a reply, and on one without")
    cid = make_chat(ops["id"], f"SC{STAMP} actions")
    exchange(cid, f"first {STAMP}")
    t = start(cid)
    try:
        for typed, pinned, said in [("/pin", True, "Pinned to the top"), ("/PIN and ignored words", False, "Unpinned"), ("/Pin   ", True, "Pinned to the top")]:
            run(t, typed)
            until(t, lambda: summary(cid)["pinned"] == pinned, 5, f"{typed!r} pinning")
            expect(t, said, 3)
            cleared(t, typed)
        api("PATCH", f"/api/chats/{cid}", {"pinned": False})
        n = fresh_toast(t, "Nothing is running")
        for i, typed in enumerate(["/stop", "/STOP now", "/stop   "]):
            run(t, typed)
            toasted(t, "Nothing is running", n + i)
            cleared(t, typed)
        reply = next(m["content"] for m in reversed(chat(cid)["messages"]) if m["role"] == "assistant").strip()
        mark = len(t.log)
        run(t, "/copy")
        expect(t, f"Copied the last reply ({len(reply)} characters)", 5)
        osc = re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", t.log[mark:])
        assert osc and base64.b64decode(osc[1]).decode() == reply, osc
        for typed in ["/retry", "/regenerate", "/RETRY again", "/ReGenerate"]:
            last = chat(cid)["messages"][-1]["_ts"]
            run(t, typed)
            until(t, lambda: chat(cid)["messages"][-1]["_ts"] > last and summary(cid)["status"] == "idle", 15, f"the reply regenerated by {typed!r}")
            settled(t, cid)
            assert users(cid) == [f"first {STAMP}"], users(cid)
        for typed in ["/edit", "/EDIT these words are ignored"]:
            run(t, typed)
            kept(t, f"first {STAMP}", "the last message put back")
            expect(t, "Editing your last message", 3)
            assert "editing your last message" in bar(t) and "Resend" in bar(t), bar(t)
            clear(t)
        n_msgs = len(chat(cid)["messages"])
        for typed in ["/branch", "/fork", "/FORK x"]:
            before = ids()
            run(t, typed, 0.8)
            made = until(t, lambda: new_since(before), 5, f"the branch made by {typed!r}")
            assert len(made) == 1 and made[0]["title"].endswith("(branch)") and made[0]["messages"] == n_msgs, made
            expect(t, "Branched.", 3)
            until(t, lambda: made[0]["title"][:30] in row(t, 0), 5, "the branch opened")
            cleared(t, typed)

        section("the same commands on a chat with no messages yet")
        empty = make_chat(ops["id"], f"SC{STAMP} empty")
        close(t)
        t = start(empty)
        for typed, said in [("/branch", "Nothing to branch yet"), ("/fork", "Nothing to branch yet"), ("/retry", "Nothing to regenerate yet."), ("/regenerate", "Nothing to regenerate yet."),
                            ("/edit", "No message of yours to edit yet."), ("/copy", "No reply to copy yet")]:
            n = toast_n(t, said)
            run(t, typed)
            toasted(t, said, n)
            cleared(t, typed)
        assert summary(empty)["messages"] == 0

        section("/delete asks first: Esc and n keep the chat, Enter deletes it and the welcome page comes back")
        for typed, answer in [("/delete", "esc"), ("/DELETE now", "n")]:
            run(t, typed)
            expect(t, f"Delete “SC{STAMP} empty”?", 5)
            t.key(answer, 0.5)
            until(t, lambda: not find(t, "Delete “"), 3, "the question closed")
            assert summary(empty), f"{answer} deleted the chat"
        run(t, "/delete")
        expect(t, f"Delete “SC{STAMP} empty”?", 5)
        t.key("enter", 0.6)
        until(t, lambda: summary(empty) is None, 5, "the chat deleted")
        expect(t, "Pick an agent.", 5, "the welcome page")
        t.type("/help")  # (no message box now: the sidebar's filter takes it)
        t.key("enter", 0.3)
        stays_absent(t, "Start a message with //", what="/help with no chat open")
        t.key("esc", 0.3)
        alive(t)
        intact(t, "the chat actions")
    finally:
        close(t)


def s_export(ops):
    section("/export writes this chat to the current folder: md by default, json, in any case or spelling; anything else says how")
    title = f"Exp {STAMP} plain ✓"
    cid = make_chat(ops["id"], title)
    exchange(cid, f"export me {STAMP}")
    md, js = os.path.join(WORK, f"Exp-{STAMP}-plain.md"), os.path.join(WORK, f"Exp-{STAMP}-plain.json")
    t = start(cid)
    try:
        def wrote(path, typed, after=0.0):
            until(t, lambda: os.path.exists(path) and os.path.getmtime(path) > after, 5, f"{os.path.basename(path)} written by {typed!r}")
            expect(t, "Saved ", 3)
            return os.path.getmtime(path)

        run(t, "/export")
        last_md = wrote(md, "/export")
        text = open(md, encoding="utf-8").read()
        assert text.startswith(f"# {title}") and f"export me {STAMP}" in text, text[:200]
        run(t, "/export json")
        last_js = wrote(js, "/export json")
        data = json.load(open(js, encoding="utf-8"))
        assert data["chat"]["title"] == title and data["chat"]["id"] == cid, data["chat"].keys()
        for typed, kind in [("/EXPORT JSON", "json"), ("/export .json", "json"), ("/export markdown", "md"), ("/export   md   ", "md"), ("/Export MD", "md")]:
            run(t, typed)
            if kind == "md":
                last_md = wrote(md, typed, last_md)
            else:
                last_js = wrote(js, typed, last_js)
            cleared(t, typed)
        n = fresh_toast(t, "Export as md or json, like /export json")
        for i, typed in enumerate(["/export pdf", "/export 日本語", "/export " + "x" * 300]):
            run(t, typed)
            toasted(t, "Export as md or json, like /export json", n + i)
            cleared(t, typed[:20])
        assert sorted(f for f in os.listdir(WORK) if f.startswith(f"Exp-{STAMP}")) == sorted([os.path.basename(md), os.path.basename(js)]), os.listdir(WORK)
        run(t, f"/rename Long{STAMP} " + "y" * 150, 0.6)
        until(t, lambda: chat(cid)["title"].startswith(f"Long{STAMP}"), 5, "the long title")
        run(t, "/export")
        name = (f"Long{STAMP}-" + "y" * 150)[:60] + ".md"
        until(t, lambda: os.path.exists(os.path.join(WORK, name)), 5, "the long title cut to 60 characters in the file name")
        # a file of that name that isn't this chat's export (a project's README.md) is never replaced
        mine_, taken = os.path.join(WORK, f"Taken{STAMP}.md"), os.path.join(WORK, f"Taken{STAMP}-2.md")
        with open(mine_, "w") as f:
            f.write("precious, hand-written\n")
        run(t, f"/rename Taken{STAMP}", 0.6)
        until(t, lambda: chat(cid)["title"] == f"Taken{STAMP}", 5)
        for _ in range(2):  # (exported again: the same new file, rewritten)
            run(t, "/export")
            until(t, lambda: os.path.exists(taken), 5, f"Taken{STAMP}-2.md beside the file that was there")
            expect(t, f"Taken{STAMP}-2.md", 3, "the toast naming where it went")
        assert open(mine_).read() == "precious, hand-written\n", "/export wrote over a file that was there"
        assert not os.path.exists(os.path.join(WORK, f"Taken{STAMP}-3.md")), "a second export of the same chat made another file"

        section("/export of a chat titled in another script (日本語, Ελληνικά, кириллица) saves it like any other")
        for script, word in [("CJK", "日本語"), ("Greek", "Ελληνικά"), ("Cyrillic", "кириллица")]:
            run(t, f"/rename Exp {STAMP} {word}", 0.6)
            until(t, lambda: chat(cid)["title"] == f"Exp {STAMP} {word}", 5)
            cleared(t, "/rename")
            path = os.path.join(WORK, f"Exp-{STAMP}-{word}.md")
            run(t, "/export")
            until(t, lambda: os.path.exists(path) or find(t, "Internal Server Error"), 5, f"the {script} export")
            assert os.path.exists(path), f"/export of a chat titled “Exp {STAMP} {word}” failed: the app answered 500 (its Content-Disposition filename isn't Latin-1)"
            assert open(path, encoding="utf-8").read().startswith(f"# Exp {STAMP} {word}")
    finally:
        close(t)


def state_json():
    try:
        return json.load(open(os.path.join(XDG, "agent-chat-tui", "state.json")))
    except (OSError, ValueError):
        return {}


def s_toggles(ops):
    section("/theme cycles and names a palette (kept for next time); a wrong name says which exist; /files and /verbose toggle")
    cid = make_chat(ops["id"], f"SC{STAMP} toggles")
    exchange(cid, f"think about toggles {STAMP}")
    t = start(cid)
    try:
        dark, light = "0b0d12", "f2f3f7"
        assert dark in bgs(t), "starts dark (a config folder of our own)"
        for typed, theme in [("/theme", "light"), ("/THEME", "plain"), ("/theme  ", "dark"), ("/theme LIGHT", "light"), ("/theme   plain   ", "plain"), ("/Theme Dark", "dark")]:
            run(t, typed)
            expect(t, f"Theme: {theme}", 3)
            until(t, lambda: (b := bgs(t)) and (dark in b) == (theme == "dark") and (light in b) == (theme == "light"), 3, f"the {theme} palette")
            until(t, lambda: state_json().get("theme") == theme, 3, f"{theme} saved for next time")
            cleared(t, typed)
            if theme == "light":
                intact(t, "/theme light")
        n = fresh_toast(t, "Themes: dark, light, plain")
        for i, typed in enumerate(["/theme neon", "/theme ダーク", "/theme " + "d" * 300]):
            run(t, typed)
            toasted(t, "Themes: dark, light, plain", n + i)
            cleared(t, typed[:20])
        assert dark in bgs(t) and state_json().get("theme") == "dark", "a wrong name left the palette alone"
        close(t)
        t = start(cid)
        run(t, "/theme light")
        until(t, lambda: state_json().get("theme") == "light", 3)
        close(t)
        t = start(cid)
        assert light in bgs(t), "the last /theme is used at start"
        run(t, "/theme dark")
        until(t, lambda: state_json().get("theme") == "dark", 3)

        for typed, shown in [("/files", True), ("/FILES x", False), ("/Files", True), ("/files   ", False)]:
            run(t, typed, 0.6)
            if shown:
                expect(t, "Workspace", 5, f"the drawer opened by {typed!r}")
                t.key("esc", 0.3)  # the drawer took the focus; back to the box
            else:
                until(t, lambda: not find(t, "Workspace"), 5, f"the drawer closed by {typed!r}")
            focus_box_keep(t)
            until(t, lambda: state_json().get("files") == shown, 3)
        for typed, shown in [("/verbose", True), ("/VERBOSE x", False), ("/Verbose", True), ("/verbose   ", False)]:
            run(t, typed)
            if shown:
                expect(t, "The user said: think about toggles", 5, f"the thinking text shown by {typed!r}")
            else:
                until(t, lambda: not find(t, "The user said: think about toggles"), 5, f"the thinking text hidden by {typed!r}")
            cleared(t, typed)
        intact(t, "the toggles")
    finally:
        close(t)


def s_memory(ops):
    section("/remember saves a fact (trimmed, unicode, capped at 500, merged when repeated); without one it says how; /memory opens the panel")
    cid = make_chat(ops["id"], f"SC{STAMP} memory")
    t = start(cid)
    try:
        n = fresh_toast(t, "Say what to remember, like /remember I prefer short answers")
        for i, typed in enumerate(["/remember", "/remember      ", "/REMEMBER"]):
            run(t, typed)
            toasted(t, "Say what to remember, like /remember I prefer short answers", n + i)
            cleared(t, typed)
        # (every word stamped: a fact sharing most words with one already saved, by an earlier run say, is merged into it)
        long_fact = f"longfact{STAMP} " + " ".join(f"w{i}x{STAMP}" for i in range(55))  # (under 1000 with the command, or the whole is a paste token)
        for typed, fact in [(f"/remember slashfact{STAMP} likes{STAMP} terminals{STAMP}", f"slashfact{STAMP} likes{STAMP} terminals{STAMP}"),
                            (f"/REMEMBER    spaced{STAMP}    out{STAMP}   fact{STAMP}   ", f"spaced{STAMP} out{STAMP} fact{STAMP}"),
                            (f"/remember 日本語のファクト {STAMP} ünïcødé ✓", f"日本語のファクト {STAMP} ünïcødé ✓"),
                            ("/remember " + long_fact, long_fact[:500])]:
            run(t, typed, 0.6)
            until(t, lambda: fact in memories(), 5, f"the fact from {typed[:30]!r}")
            expect(t, f"Remembered: {fact[:20]}", 5)
            cleared(t, typed[:20])
        n = len(memories())
        run(t, f"/remember slashfact{STAMP} likes{STAMP} terminals{STAMP}")
        expect(t, f"Updated a memory: slashfact{STAMP}", 5, "a repeated fact merged")
        assert len(memories()) == n
        for typed in ["/memory", "/MEMORY x", "/Memory"]:
            run(t, typed)
            expect(t, "Memory ·", 5)
            esc_closes(t, "Memory ·")
        assert users(cid) == []
    finally:
        close(t)


def s_permissions(ops):
    section("/permissions shows the mode; bypass asks first (Esc keeps asking), ask switches back; already-set and wrong words say so")
    cid = make_chat(ops["id"], f"SC{STAMP} permissions")
    t = start(cid)
    asking = "Agents ask before running shell commands or sending email. /permissions bypass stops the asking."
    try:
        assert settings()["bypass_approvals"] is False
        for i, typed in enumerate(["/permissions", "/PERMISSIONS   "]):
            n = toast_n(t, asking) if i else fresh_toast(t, asking)
            run(t, typed)
            toasted(t, asking, n)
            cleared(t, typed)
        for typed in ["/permissions bypass", "/permissions   BYPASS  "]:
            run(t, typed)
            expect(t, "Bypass permissions?", 5)
            esc_closes(t, "Bypass permissions?")
            assert settings()["bypass_approvals"] is False, "Esc bypassed"
        run(t, "/Permissions Bypass")
        expect(t, "Bypass permissions?", 5)
        t.key("enter", 0.6)
        until(t, lambda: settings()["bypass_approvals"] is True, 5, "bypass on")
        expect(t, "⚠ bypassing permissions", 5, "the mode chip")
        for typed, said in [("/permissions", "Bypassing permissions: agents run commands and send email without asking."), ("/permissions bypass", "Permissions are already bypassed."),
                            ("/permissions ask", "Agents will ask before acting again"), ("/permissions ASK", "Agents already ask before running commands or sending email."),
                            ("/permissions maybe", "Use /permissions ask or /permissions bypass"), ("/permissions はい", "Use /permissions ask or /permissions bypass"),
                            ("/permissions " + "b" * 300, "Use /permissions ask or /permissions bypass")]:
            n = toast_n(t, said)
            run(t, typed)
            toasted(t, said, n)
            cleared(t, typed[:20])
            if typed == "/permissions ask":
                until(t, lambda: settings()["bypass_approvals"] is False, 5, "asking again")
                expect(t, "⛨ asks before acting", 5)
        assert settings()["bypass_approvals"] is False
    finally:
        close(t)
        api("PUT", "/api/settings", {"bypass_approvals": False})


def model_rows(t):
    """The /model list as drawn: [(marked current, selected, label)]."""
    d = cols(t)
    top = next((y for y, l in enumerate(d) if "┌ Model for " in l), None)
    if top is None:
        return []
    x = d[top].index("┌ Model for ")
    x1 = d[top].index("┐", x)
    out = []
    for y in range(top + 1, t.rows):
        if d[y][x] != "│":
            break
        m = re.match(r"( ▸ |   )(● |  )(\S.*?)\s*$", row(t, y, x + 1, x1))
        if m:
            out.append((m[2] == "● ", m[1] == " ▸ ", m[3]))
    return out


def s_model(ops):
    section("/model with no name lists the default and the server's models (as GET /api/server has them); a name sets this chat's agent's model and nothing else")
    cid = make_chat(ops["id"], f"SC{STAMP} model")
    t = start(cid)
    aid = ops["id"]
    try:
        server = api("GET", "/api/server")
        fallback = settings().get("model") or (server["models"][0] if server["models"] else "the server's first model")
        before = agent(aid)
        assert before["model"] == ""
        run(t, "/model")
        expect(t, f"Model for 🤖 {ops['name']}", 5)
        want = [(True, True, f"default · {fallback}")] + [(False, False, m) for m in server["models"]]
        until(t, lambda: model_rows(t) == want, 3, f"the list {want}")
        t.key("down", 0.2)
        t.key("enter", 0.6)
        first = server["models"][0]
        until(t, lambda: agent(aid)["model"] == first, 5, "the picked model saved")
        expect(t, f"now uses {first}", 5)
        assert agent(aid) == before | {"model": first}, "only the model changed"
        run(t, "/MODEL")
        until(t, lambda: model_rows(t) == [(False, False, f"default · {fallback}")] + [(m == first, m == first, m) for m in server["models"]], 3, "the current model marked and picked")
        esc_closes(t, "Model for")
        unicode_model, long_model = f"модель-{STAMP}", f"m{STAMP}-" + "x" * 290
        for typed, model, said in [("/model DEFAULT", "", "uses the default model again"), (f"/model {first[:4]}", first, f"now uses {first}"),
                                   (f"/model    {first}    ", first, f"now uses {first}"), ("/model default", "", "uses the default model again"),
                                   (f"/model {unicode_model}", unicode_model, f"now uses {unicode_model} (the server doesn't list it)"),
                                   ("/model " + long_model, long_model, f"m{STAMP}-xxxxxxxxxx")]:  # (a long name wraps under "now uses")
            run(t, typed, 0.6)
            until(t, lambda: agent(aid)["model"] == model, 5, f"the model set by {typed[:30]!r}")
            expect(t, said, 5)
            assert agent(aid) == before | {"model": model}, "only the model changed"
            cleared(t, typed[:20])
        run(t, "/model")
        until(t, lambda: (r := model_rows(t)) and r[-1][0] and r[-1][2].startswith(f"m{STAMP}-xxx") and r[-1][2].endswith("…"), 3, "an unlisted model listed, marked, cut to fit")
        intact(t, "a long model name in the list")
        esc_closes(t, "Model for")
        run(t, "/model default", 0.6)
        until(t, lambda: agent(aid)["model"] == "", 5)
        assert agent(aid) == before
    finally:
        close(t)
        api("PUT", f"/api/agents/{aid}", agent(aid) | {"model": ""})


def k(n):
    """The TUI's short count: 950, 12.3k, 85k."""
    return f"{n / 1000:.1f}".removesuffix(".0") + "k" if n >= 1000 else str(n)


def thousands(n):
    return f"{n:,}"


def going(t):
    """The chat footer: (compactions, seconds it has been going, the precision of that), or None
    when it isn't drawn."""
    for r in rows(t):
        m = re.search(r"│ (not compacted yet|compacted (\d+)×) · going for (\d+)([smhd])(?: (\d+)[mh])?\s*$", r)
        if m:
            unit = {"s": 1, "m": 60, "h": 3600, "d": 86400}[m[4]]
            low = {"s": 0, "m": 0, "h": 60, "d": 3600}[m[4]]
            return int(m[2] or 0), int(m[3]) * unit + int(m[5] or 0) * low, low or unit
    return None


def pct(used, window):
    """Rounded half away from zero, as Rust's f64::round."""
    return int(used / window * 100 + 0.5)


def expected_usage(c):
    tin = tout = calls = replies = 0
    speed, prev = None, ""
    for m in c["messages"]:
        if m["role"] == "assistant":
            if prev not in ("assistant", "tool"):
                replies += 1
            st = m.get("_stats")
            if st:
                calls += 1
                tin += st.get("prompt_tokens") or 0
                tout += st.get("completion_tokens") or 0
                speed = st.get("tok_per_s") or speed
        prev = m["role"]
    return tin, tout, calls, replies, speed


def s_compact(ops):
    section("/compact twice: the footer counts each (with instructions, unicode, trimmed); nothing new to compact says so; the numbers match the server")
    cid = make_chat(ops["id"], f"SC{STAMP} compact")
    t = start(cid)
    try:
        assert going(t) is None, "no footer before the first message"
        for i, text in enumerate([f"first point {STAMP}", f"second point {STAMP}"]):
            run(t, text, 0.3)
            until(t, lambda: len(users(cid)) == i + 1, 5)
            settled(t, cid)
        until(t, lambda: going(t) and going(t)[0] == 0, 5, "the footer: not compacted yet")
        expect(t, "not compacted yet · going for ", 2)
        created = chat(cid)["created"]
        run(t, "/compact    保持: the API 設計 decisions   ", 0.5)
        until(t, lambda: len(chat(cid).get("compactions") or []) == 1, 20, "the first compaction")
        comp = chat(cid)["compactions"][0]
        assert comp.get("instructions") == "保持: the API 設計 decisions" and "Kept as asked: 保持: the API 設計 decisions" in comp["summary"], comp
        settled(t, cid)
        until(t, lambda: going(t) and going(t)[0] == 1, 5, "the footer: compacted 1×")
        expect(t, "compacted 1× · going for ", 2)
        run(t, f"third point {STAMP}", 0.3)
        until(t, lambda: len(users(cid)) == 3, 5)
        settled(t, cid)
        run(t, "/compact", 0.5)
        until(t, lambda: len(chat(cid).get("compactions") or []) == 2, 20, "the second compaction")
        assert "instructions" not in chat(cid)["compactions"][1] or not chat(cid)["compactions"][1]["instructions"]
        settled(t, cid)
        until(t, lambda: going(t) and going(t)[0] == 2, 5, "the footer: compacted 2×")
        expect(t, "compacted 2× · going for ", 2)
        _, secs, step = going(t)
        assert -3 <= time.time() - created - secs <= step + 2, f"the footer says {secs}s, the chat was made {time.time() - created:.1f}s ago"
        until(t, lambda: going(t)[1] > secs, step + 5, "the footer counting up")
        assert users(cid) == [f"first point {STAMP}", f"second point {STAMP}", f"third point {STAMP}"], "a /compact reached the agent"

        section("/context, /usage (/cost, /stats) and /status say what GET /api/chats/{id}, /api/server and the settings say")
        c, server, st = chat(cid), api("GET", "/api/server"), settings()
        used, window = c["stats"]["context_tokens"], server["n_ctx"] or st.get("context_size") or None
        auto = f"auto-compacts at {st.get('compact_at') or 70}%" if st.get("auto_compact", True) else "auto-compact is off"
        want = f"ctx {k(used)} / {k(window)} ({pct(used, window)}%) · {auto}"
        age = f"compacted 2× · started {time.strftime('%H:%M', time.localtime(created))} (going for "
        for typed in ["/context", "/CONTEXT", "/context ignored"]:
            run(t, typed)
            expect(t, want, 5, f"{typed}: {want}")
            expect(t, age, 2, "the chat's age")
            m = re.search(r"\(going for (\d+)([sm])\)", screen(t))
            assert m and -3 <= time.time() - created - int(m[1]) * {"s": 1, "m": 60}[m[2]] <= {"s": 2, "m": 62}[m[2]], m
            esc_closes(t, want)
        tin, tout, calls, replies, speed = expected_usage(c)
        assert replies == 3 and calls == 3, (replies, calls)
        for typed in ["/usage", "/cost", "/stats", "/STATS x", "/Cost"]:
            run(t, typed)
            expect(t, f"{k(tin)} in · {k(tout)} out · {replies} replies", 5, f"{typed}: the totals")
            expect(t, f"tokens in  {thousands(tin)} (prompts summed over {calls} model calls)", 2)
            expect(t, f"tokens out  {thousands(tout)}", 2)
            expect(t, f"replies  {replies}", 2)
            expect(t, f"last speed  {speed:.1f} tok/s", 2)
            expect(t, age, 2, "the chat's age")
            esc_closes(t, "prompts summed over")
        app_version = api("GET", "/api/state")["version"]
        tui_version = re.search(r'^version = "([^"]+)"', open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tui", "Cargo.toml")).read(), re.M)[1]
        for typed in ["/status", "/STATUS now"]:
            run(t, typed, 0.6)
            expect(t, "server  reachable", 5)
            expect(t, f"address  {server['base_url']}", 2)
            expect(t, "kind  " + ("llama-server" if server["kind"] == "llama" else "OpenAI-compatible server"), 2)
            expect(t, f"models  {', '.join(server['models'])}", 2)
            expect(t, "default  " + (st.get("model") or f"{server['models'][0]} (the server's first)"), 2)
            expect(t, f"context  {thousands(window)} tokens", 2)
            expect(t, f"app  Agent Chat {app_version} · terminal client {tui_version}", 2)
            esc_closes(t, "terminal client")
        assert len(users(cid)) == 3
        intact(t, "the footer and the informational panels")

        section("/compact with very long instructions sends them whole")
        long_cid = make_chat(ops["id"], f"SC{STAMP} compact long")
        for text in [f"one {STAMP}", f"two {STAMP}"]:
            exchange(long_cid, text)
        close(t)
        t = start(long_cid)
        keep = "keep every decision about " + " ".join(f"item{i}" for i in range(60))
        run(t, "/compact " + keep, 0.5)
        until(t, lambda: len(chat(long_cid).get("compactions") or []) == 1, 20, "the compaction")
        assert chat(long_cid)["compactions"][0].get("instructions") == keep
        settled(t, long_cid)
        expect(t, "compacted 1× · going for ", 5)

        section("/compact with nothing new since the last compaction says so (and compacts nothing)")
        run(t, "/COMPACT")
        settled(t, long_cid)
        assert len(chat(long_cid)["compactions"]) == 1
        expect(t, "Nothing to compact yet", 3, "any word that /compact had nothing to do (the server's notice: \"Nothing to compact yet: the conversation is still short.\")")
    finally:
        close(t)


def s_server_down(ops):
    section("/status and /model with the model server down: not reachable (the error the API gives), no models listed")
    cid = make_chat(ops["id"], f"SC{STAMP} down")
    t = start(cid)
    dead = "http://127.0.0.1:9/v1"
    try:
        api("PUT", "/api/settings", {"base_url": dead})
        server = api("GET", "/api/server")
        assert not server["ok"] and server["error"], server
        run(t, "/status", 1.0)
        expect(t, "server  not reachable: ", 8)
        expect(t, f"address  {dead}", 2)
        expect(t, "models  none listed", 2)
        assert server["error"][:30] in screen(t), (server["error"], screen(t))
        esc_closes(t, "terminal client")
        run(t, "/model", 1.0)
        expect(t, "The model server isn't reachable, so no models are listed.", 5)
        esc_closes(t, "Model for")
    finally:
        api("PUT", "/api/settings", {"base_url": MOCK})
        close(t)


def s_running(ops):
    section("mid-reply: idle-only commands (and their aliases, typed or picked from the menu) are refused and stay in the box")
    cid = make_chat(ops["id"], f"SC{STAMP} running")
    exchange(cid, f"before the slow one {STAMP}")
    t = start(cid)
    started = [0.0]

    def busy():
        """A slow reply going, with time left on it (a fresh one when the last is nearly over)."""
        if summary(cid)["status"] != "idle" and time.time() - started[0] < 8:
            return
        if summary(cid)["status"] != "idle":
            api("POST", f"/api/chats/{cid}/stop")
            settled(t, cid)
        api("POST", f"/api/chats/{cid}/run", {"content": "slow please"})
        started[0] = time.time()
        running(t, cid)

    try:
        refused = [(f"/{n}", n) for n in IDLE] + [("/regenerate", "retry"), (f"/rename Busy {STAMP}", "rename"), ("/compact keep it", "compact"),
                                                  ("/RETRY", "retry"), ("/Delete please", "delete"), ("/pin   ", "pin")]
        for typed, name in refused:
            busy()
            run(t, typed)
            expect(t, f"The agent is working. Wait for it, or /stop it first, then /{name}.", 5, f"{typed!r} refused")
            kept(t, typed, f"{typed!r} refused")
            clear(t)
        for typed, name in [("/ren", "rename"), ("/regen", "retry"), ("/del", "delete")]:
            busy()
            t.type(typed)
            until(t, lambda: menu(t) and menu(t)[1] == f"/{name}", 3, f"the menu picking /{name}")
            t.key("enter", 0.5)
            expect(t, f"then /{name}.", 5, f"{typed!r} from the menu refused")
            kept(t, typed, f"{typed!r} from the menu")
            clear(t)
        c = summary(cid)
        assert not c["pinned"] and c["title"] == f"SC{STAMP} running" and not chat(cid).get("compactions"), c

        section("mid-reply: every other command runs at once, says so in the box, and nothing is queued for the agent")
        busy()
        put(t, "/usage")
        assert "Enter runs /usage now (not queued)" in bar(t) and "Run ▸" in bar(t), bar(t)
        t.key("enter", 0.5)
        expect(t, "prompts summed over", 5)
        esc_closes(t, "prompts summed over")
        for typed, marker in [("/help", "Start a message with //"), ("/context", "/compact summarizes older messages now"), ("/cost", "prompts summed over"), ("/stats", "prompts summed over"),
                              ("/status", "terminal client"), ("/model", "Model for"), ("/memory", "Memory ·"), ("/resume", "Search and commands"), ("/chats", "Search and commands"),
                              ("/agents", "Edit Slash"), ("/settings", "Model server URL"), ("/config", "Model server URL"), ("/routines", "r run now")]:
            busy()
            run(t, typed)
            expect(t, marker, 5, f"{typed} mid-reply")
            esc_closes(t, marker)
            cleared(t, typed)
        for typed, said in [("/theme light", "Theme: light"), ("/theme dark", "Theme: dark"), (f"/remember busyfact{STAMP} midreply{STAMP}", f"Remembered: busyfact{STAMP}"),
                            ("/permissions", "Agents ask before"), ("/export", "Saved "), ("/copy", "Copied the last reply")]:
            busy()
            run(t, typed)
            expect(t, said, 5, f"{typed} mid-reply")
            cleared(t, typed)
        for typed in ["/verbose", "/verbose"]:
            busy()
            run(t, typed)
            cleared(t, typed)
        busy()
        run(t, "/files", 0.5)
        expect(t, "Workspace", 5)
        t.key("esc", 0.3)  # (the drawer took the focus)
        run(t, "/files", 0.5)
        until(t, lambda: not find(t, "Workspace"), 5)
        busy()
        t.type("/")
        until(t, lambda: menu(t), 3)
        t.key("esc", 0.4)  # closes the menu: it doesn't stop the reply (Esc in the box would)
        no_menu(t, "after Esc")
        t.pump(0.5)
        # (a lone "/" left in the box is no command: the bar says so, and still offers to stop)
        assert summary(cid)["status"] != "idle" and "Stop ■" in bar(t), "Esc on the menu stopped the reply"
        clear(t)
        busy()
        t.type("/re")
        t.key("down", 0.1)
        picked = until(t, lambda: menu(t) and menu(t)[1] == "/rename" and menu(t), 3, "the menu with /rename picked")
        t.pump(1.5)  # the reply streams on under it
        assert menu(t) == picked and box_text(t) == "/re", "the menu or its pick changed as the reply streamed"
        assert summary(cid)["status"] != "idle", "the reply ended while the menu was checked"
        clear(t)
        # a message that starts with / queues (escaped with //), and comes back escaped
        busy()
        run(t, f"//queued {STAMP}", 0.6)
        expect(t, "queued (1)", 5)
        t.key("up", 0.6)
        kept(t, f"//queued {STAMP}", "the queued message taken back, escaped")
        clear(t)
        busy()
        run(t, "/STOP", 0.6)
        settled(t, cid)
        expect(t, "stopped here", 10)
        assert set(users(cid)) == {f"before the slow one {STAMP}", "slow please"}, f"a command reached the agent: {users(cid)}"

        section("mid-reply: /fork branches the chat as saved so far, /new starts another; the reply goes on")
        busy()
        before = ids()
        run(t, "/fork", 0.8)
        made = until(t, lambda: new_since(before), 5, "the branch")
        assert len(made) == 1 and made[0]["title"].endswith("(branch)"), made
        expect(t, "Branched.", 3)
        assert summary(cid)["status"] != "idle", "branching stopped the reply"
        before = ids()
        run(t, "/new", 0.8)
        made = until(t, lambda: new_since(before), 5, "the new chat")
        assert made[0]["agent_id"] == ops["id"], made
        api("POST", f"/api/chats/{cid}/stop")
        until(t, lambda: summary(cid)["status"] == "idle", 10)
        intact(t, "commands mid-reply")
    finally:
        close(t)


def s_approval(ops):
    section("an approval waiting: commands run or are refused as mid-reply; typing /yan in the box answers nothing; Alt+Y works with the menu open; /stop cancels")
    cid = make_chat(ops["id"], f"SC{STAMP} approval")
    t = start(cid)
    try:
        run(t, f"shell: echo slash-{STAMP}", 0.5)
        expect(t, "needs approval", 15)
        until(t, lambda: summary(cid)["status"] == "waiting", 5)
        for typed in ["/yan", "/agents", "/new", "/any"]:  # y, a and n typed in the box are text
            t.type(typed)
            until(t, lambda: box_text(t) == typed, 3)
            assert summary(cid)["status"] == "waiting", f"typing {typed!r} answered the approval"
            clear(t)
        for typed, name in [("/retry", "retry"), ("/compact", "compact"), ("/edit", "edit"), ("/delete", "delete"), ("/rename x", "rename"), ("/pin", "pin")]:
            run(t, typed)
            expect(t, f"then /{name}.", 5, f"{typed!r} refused while the approval waits")
            kept(t, typed, typed)
            clear(t)
        for typed, marker in [("/help", "Start a message with //"), ("/usage", "prompts summed over"), ("/context", "/compact summarizes older messages now"), ("/status", "terminal client")]:
            run(t, typed)
            expect(t, marker, 5)
            esc_closes(t, marker)
        assert summary(cid)["status"] == "waiting"
        t.type("/he")
        until(t, lambda: menu(t), 3)
        t.send(KEY["alt-y"], 0.6)
        until(t, lambda: summary(cid)["status"] == "idle", 15, "Alt+Y approving with the menu open")
        assert any(m["role"] == "tool" and f"slash-{STAMP}" in str(m["content"]) for m in chat(cid)["messages"]), "the approved command ran"
        assert menu(t) and box_text(t) == "/he", "the menu and its text stay"
        clear(t)
        settled(t, cid)
        run(t, f"shell: echo second-{STAMP}", 0.5)
        expect(t, "needs approval", 15)
        until(t, lambda: summary(cid)["status"] == "waiting", 5)
        run(t, "/Stop", 0.8)
        until(t, lambda: summary(cid)["status"] == "idle", 10, "/stop cancelling the waiting run")
        settled(t, cid)
        expect(t, "cancelled", 5)
        assert users(cid) == [f"shell: echo slash-{STAMP}", f"shell: echo second-{STAMP}"], users(cid)
        intact(t, "commands while an approval waits")
    finally:
        close(t)


def s_subchat(ops):
    section("a sub-agent's chat has no message box: typed and pasted commands run nothing and send nothing, there or in the chat that started it")
    cid = make_chat(ops["id"], f"SC{STAMP} parent")
    exchange(cid, "qa")
    sub = chat(cid)["subchats"][0]
    n_parent, n_sub, before = len(chat(cid)["messages"]), len(chat(sub)["messages"]), ids()
    t = start(sub, focus=False)
    try:
        expect(t, "working for", 5)
        expect(t, "This agent is working for another agent", 3)
        t.click(t.cols - 20, 3, 0.3)  # the trace (no box to focus)
        for typed in ["/help", "/new", "/quit", "/status"]:
            t.type(typed)
            t.key("enter", 0.3)
        t.paste("/help\r")
        t.send("/exit\r", 0.5)
        stays_absent(t, "Start a message with //", what="/help in a sub-agent's chat")
        alive(t)
        absent(t, "terminal client")
        assert len(chat(cid)["messages"]) == n_parent and len(chat(sub)["messages"]) == n_sub and not new_since(before)
        intact(t, "commands typed in a sub-agent's chat")
        palette_commands(t, "a sub-agent's chat")
        assert len(chat(cid)["messages"]) == n_parent and len(chat(sub)["messages"]) == n_sub and not new_since(before)
    finally:
        close(t)


def s_deleted_agent():
    section("a chat whose agent was deleted: /new picks an agent, /agents makes one, /model says the agent is gone; the rest still work")
    gone = make_agent(f"Gone{STAMP}")
    cid = make_chat(gone["id"], f"SC{STAMP} orphan")
    api("DELETE", f"/api/agents/{gone['id']}")
    t = start(cid)
    try:
        expect(t, "Deleted agent", 5)
        run(t, "/new")
        expect(t, "New chat with", 5, "the agent picker")
        esc_closes(t, "New chat with")
        run(t, "/clear")
        expect(t, "New chat with", 5)
        esc_closes(t, "New chat with")
        run(t, "/agents")
        expect(t, "New agent", 5, "the new-agent editor")
        esc_closes(t, "New agent")
        n = fresh_toast(t, "This chat's agent was deleted.")
        for i, typed in enumerate(["/model", "/model mock-model"]):
            run(t, typed)
            toasted(t, "This chat's agent was deleted.", n + i)
            cleared(t, typed)
        run(t, f"/rename Orphan {STAMP}")
        until(t, lambda: chat(cid)["title"] == f"Orphan {STAMP}", 5)
        run(t, "/usage")
        expect(t, "0 in · 0 out · 0 replies", 5)
        esc_closes(t, "prompts summed over")
    finally:
        close(t)


def s_quit(ops):
    section("/quit and /exit quit (any case, ignored arguments, from the menu, mid-reply) and hand the terminal back")
    cid = make_chat(ops["id"], f"SC{STAMP} quit")
    exchange(cid, f"before quitting {STAMP}")
    for how in ["/quit", "/exit", "/QUIT", "/Exit now please", "/qu", "/exi", "mid-reply"]:
        t = start(cid)
        try:
            if how == "mid-reply":
                api("POST", f"/api/chats/{cid}/run", {"content": "slow please"})
                running(t, cid)
                how = "/exit"
            if how in ("/qu", "/exi"):
                t.type(how)
                until(t, lambda: menu(t) and menu(t)[1] == "/quit", 3)
                t.key("enter", 0.3)
            else:
                run(t, how, 0.3)
            status = exited(t)
            assert status is not None and os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0, f"{how}: exit status {status}"
            for seq in (b"\x1b[?1049l", b"\x1b[?1000l", b"\x1b[?2004l", b"\x1b[?25h"):
                assert seq in t.log, f"{how}: {seq!r} not written on the way out"
        finally:
            close(t)
    assert summary(cid)["status"] != "idle", "quitting stopped the reply"
    api("POST", f"/api/chats/{cid}/stop")


def main():
    assert BIN, "build the TUI first: cd tui && cargo build --release"
    orig = settings()
    print(f"run {STAMP}: files in {ROOT}, config in {XDG}")
    api("PUT", "/api/settings", {"base_url": MOCK, "bypass_approvals": False, "auto_title": False, "auto_memory": False})
    ops = make_agent(f"Slash{STAMP} Ops")
    uni = make_agent(f"Ünï{STAMP} Çødé Ağent")
    only = [w for w in os.environ.get("SLASH_ONLY", "").split(",") if w]
    sections = [("welcome", s_welcome), ("menu", lambda: s_menu(ops)), ("narrow", lambda: s_narrow(ops)), ("panels", lambda: s_panels(ops)),
                ("unknown", lambda: s_unknown(ops)), ("pastes", lambda: s_pastes(ops)), ("new", lambda: s_new(ops, uni)), ("resume", lambda: s_resume(ops)),
                ("rename", lambda: s_rename(ops)), ("actions", lambda: s_chat_actions(ops)), ("export", lambda: s_export(ops)), ("toggles", lambda: s_toggles(ops)),
                ("memory", lambda: s_memory(ops)), ("permissions", lambda: s_permissions(ops)), ("model", lambda: s_model(ops)), ("compact", lambda: s_compact(ops)),
                ("down", lambda: s_server_down(ops)), ("running", lambda: s_running(ops)), ("approval", lambda: s_approval(ops)), ("subchat", lambda: s_subchat(ops)),
                ("deleted", s_deleted_agent), ("quit", lambda: s_quit(ops))]
    failed = []
    try:
        for name, fn in sections:  # each makes its own chats and TUI, so one failing doesn't stop the rest
            if only and not any(w in name for w in only):
                continue
            try:
                fn()
            except Exception as e:  # noqa: BLE001 (reported below, and the run fails)
                print(f"!! FAILED {name}: {type(e).__name__}: {e}", flush=True)
                failed.append(name)
    finally:
        for c in chats():
            if c["agent_id"] == ops["id"] and c["status"] != "idle":
                api("POST", f"/api/chats/{c['id']}/stop")
        api("PUT", "/api/settings", {k: orig[k] for k in CHANGED if k in orig})
    if failed:
        raise SystemExit(f"FAILED sections: {', '.join(failed)}")
    print("ok: tui slash commands")


if __name__ == "__main__":
    main()
