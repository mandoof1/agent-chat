"""The Rust TUI never corrupts the screen or dies, whatever it is given and at whatever size: driven
in a pseudo-terminal against the app on AC_URL wired to tests/mock_llm.py (its "verbatim:" prompt
answers with exactly the text it was sent). Needs the binary (TUI_BIN) and pyte:
`uv run --with pyte python tests/e2e_tui_render.py`.

Hostile text (VS16, ZWJ, flags, keycaps, skin tones, CJK, RTL and bidi overrides, combining marks,
zero-width and control characters, ANSI/OSC/DCS escapes, tabs, CRs, 10k-character words) goes in
through every path the TUI draws: agent names, icons, purposes and colours, chat titles, user
messages, replies, thinking, tool commands and shell output (colours, CR progress bars), memory
facts, routines, settings, workspace file names and contents (drawer and viewer), the message box,
prompts, toasts, and markdown torture (nested lists, quotes, tables, code; whole, cut short and
stopped half-streamed). Sizes from 1x1 to 300x80, 89-95 columns with both side panes, every overlay
at small sizes, 30 rapid resizes during a streaming reply, the light and plain themes, the agents
deleted under their open chats.

After each step: the process is alive, the screen holds still on its own (no flicker), it matches
a forced full repaint (a resize a column wider and back, waiting for each frame), the side panes
keep their separators, and nothing but plain text and the TUI's own CSI sequences reached the
terminal (no C0/C1 control characters, no OSC/DCS, no cursor reports, no charset switches).
Each section runs its own TUI; a failing section is reported and the rest still run. Checks
marked soft (lines starting "!!") report a defect without stopping their section; any of them
still fails the suite. Shares an app that other suites used: everything it makes carries this
run's stamp; the agents it made are deleted at the end (that is the last section), the settings
it changed are put back.
"""
import json
import os
import random
import re
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
H = {"X-Agent-Chat": "1"}
S = str(int(time.time() * 10))[-7:]  # this run's stamp

# ------------------------------------------------------------------ hostile text

HOSTILE = {
    "vs16": "✍️ ❤️ ☀️ ✌️ ☝️",
    "zwj": "👨\u200d👩\u200d👧 🧑🏽\u200d💻 🏳️\u200d🌈 👩\u200d❤️\u200d👨",
    "flags": "🇦🇪 🇯🇵 🇺🇸 🏴\U000e0067\U000e0062\U000e0065\U000e006e\U000e0067\U000e007f",
    "keycaps": "1️⃣ #️⃣ *️⃣ 9⃣",
    "skin": "👋🏽 👍🏿 🤝🏻",
    "cjk": "测试中文字符 日本語テキスト 한국어",
    "rtl": "مرحبا بالعالم שלום עולם \u202eRLO override\u202c \u2067isolate\u2069 \u200fmark",
    "combining": "Z\u0351\u0352\u0353a\u0300\u0301\u0302\u0303\u0304l\u0336g\u0337o e\u0301 n\u0303 ש\u05b8 ب\u064e\u0651",
    "zero": "a\u200bb\u200cc\u200dd\u2060e\ufefff\u00adg",
    "controls": "nul\x00 soh\x01 bel\x07 bs\x08 vt\x0b ff\x0c so\x0e si\x0f del\x7f nel\x85 csi\x9b2J osc\x9d0;x\x9c",
    "ansi": "\x1b[31mRED\x1b[0m \x1b[1;4;5mBLINK\x1b[0m \x1b[2J\x1b[H \x1b[10;10Hjump \x1b]0;PWNTITLE\x07 "
            "\x1b]8;;http://x.y\x1b\\link\x1b]8;;\x1b\\ \x1b[6n \x1b[?1049l \x1bc \x1b(0lqk\x1b(B \x1b)0\x0eLQK\x0f \x1bP+q\x1b\\ \x1b[?25l",
    "tabs": "a\tb\t\tc\t\t\td\te",
    "cr": "progress 10%\rprogress 50%\rDONE\r\nnext line\r",
    "long": "W" * 10000,
}
MIX = " ".join(v for k, v in HOSTILE.items() if k != "long")
# what would show if an escape sequence lost only its ESC (the shell command spells its own escapes
# in octal, so these never appear as typed text either)
LEAKS = ["[31mRED", "[1;4;5mBLINK", "]0;PWNTITLE", "[35mMAGENTA", "]0;SHTITLE", "(0lqk"]
# colours, a CR progress bar, a line clear, a title, a cursor report and a charset switch
SHELL = ("printf '\\033\\13335mMAGENTA\\033\\1330m\\tTAB\\n'; for i in 1 2 3 4 5 6; do printf '\\rBAR%d %d%%' $i $((i*16)); sleep 0.2; done; "
         "printf '\\n\\033\\1332KCLEARED\\n\\033\\1350;SHTITLE\\007\\033\\1336n\\033\\050\\060lqk\\033\\050B SHELLDONE\\n'")

MD = "\n".join([
    f"# Heading ✍️ 测试 \x1b[31mred\x1b[0m {HOSTILE['rtl'][:20]}",
    "Setext heading 👨\u200d👩\u200d👧",
    "==============",
    "",
    "Para with **bold *nested italic `code \t tab` ~~strike~~* end** and [link ✍️](http://example.com/\x1b[31m) and "
    "<http://auto.link/👨\u200d👩\u200d👧> and ![img 🇦🇪](data:image/png;base64,AAAA) and &#27;[31m entity &#x9b;2J &#13; &#9; &#0; &amp;",
    "hard break here  ",
    "next line\\",
    "after backslash break",
    "",
    "- level 1 ✍️",
    "  - level 2 👨\u200d👩\u200d👧",
    "    - level 3 🇦🇪",
    "      - level 4 1️⃣",
    "        - level 5 👋🏽",
    "          - level 6 " + "测试" * 30,
    "            1. ordered, deep",
    "               ```py",
    "               code in a list\ttab \x1b[2J",
    "               ```",
    "- [ ] task ✍️",
    "- [x] done 🇦🇪",
    "",
    "> quote 1",
    "> > quote 2 测试",
    "> > > quote 3 with `code`",
    "> > > > quote 4",
    "> > > > > quote 5 " + "Q" * 300,
    "> > > > > ```",
    "> > > > > code in quote 5 \x1b[31mred",
    "> > > > > ```",
    "> > > > > | a | b |",
    "> > > > > |---|---|",
    "> > > > > | 测试 | 👨\u200d👩\u200d👧 |",
    "",
    "| " + " | ".join(f"col{i} ✍️测试" for i in range(12)) + " |",
    "|" + "---|" * 12,
    "| " + " | ".join(("x" * 50 if i % 2 else "测" * 25) for i in range(12)) + " |",
    "| ragged | row |",
    "| a\tb | `c\x07` | **bold** | [l](u) | 🇦🇪1️⃣ | \x1b[31mr |",
    "",
    "```" + "x" * 200,
    "\tindented with a tab " + "y" * 300,
    "\x1b[31mred in code\x1b[0m \r carriage",
    "测试" * 100,
    "```",
    "",
    "~~~",
    "tilde fence ✍️",
    "~~~",
    "",
    "<details><summary>html \x1b[31m</summary>",
    "",
    "inside an html block 👨\u200d👩\u200d👧",
    "",
    "</details>",
    "",
    "Footnote ref[^1].",
    "",
    "[^1]: The footnote ✍️.",
    "",
    "---",
    "***",
    "",
    "Trailing tabs\t\t\t",
    "```unterminated",
    "still code " + "z" * 100,
])
# markdown cut off mid-construct, the way a stopped or still streaming reply leaves it
MD_CUTS = [MD.index("| col5") + 3, MD.index("\tindented") + 12, MD.index("**bold *nested") + 6, MD.index("[link") + 3,
           MD.index("<details>") + 4, MD.index("> > > > > ```") + 14, MD.index("- level 4") + 5]


# ------------------------------------------------------------------ helpers

def api(method, path, body=None, ok=True):
    r = httpx.request(method, B + path, json=body, headers=H, timeout=60)
    if ok:
        r.raise_for_status()
    return r.json() if r.headers.get("content-type", "").startswith("application/json") else r.text


def section(name):
    print(f"== {name}", flush=True)


def expect(t, needle, timeout=15, what=None):
    if not t.wait_for(needle, timeout):
        print("\n".join(t.lines()))
        raise AssertionError(f"expected {what or needle!r} on screen")


def absent(t, needle, what=None):
    if t.find(needle):
        print("\n".join(t.lines()))
        raise AssertionError(f"did not expect {what or needle!r} on screen")


def gone(t, needle, timeout=10, what=None):
    end = time.time() + timeout
    while time.time() < end and t.find(needle):
        t.pump(0.2)
    absent(t, needle, what)


def alive(t, what):
    """The TUI process is still running (a panic would have ended it)."""
    pid, status = os.waitpid(t.pid, os.WNOHANG)
    if pid:
        t.pid = None
        tail = t.log[-1500:].decode("utf-8", "replace")
        raise AssertionError(f"the TUI died after {what} (status {status}); its last output:\n{tail}")


GOING = re.compile(r"going for \d+[smhd]( \d+[mh])?")
AGO = re.compile(r"just now|\d+ (min|h|d) ago")
CLOCK = re.compile(r"\d\d:\d\d")
DEFECTS = []


def soft(ok, what):
    """A check whose failure is a defect to report, not a reason to stop the section."""
    if not ok:
        print(f"!! {what}", flush=True)
        DEFECTS.append(what)
    return ok


# what ratatui writes after every frame, changed or not: colours reset, the cursor hidden or put back
NOOP = re.compile(rb"\x1b\[39m\x1b\[49m\x1b\[59m\x1b\[0m(?:\x1b\[\?25l|\x1b\[\?25h\x1b\[\d+;\d+H|\x1b\[\d+;\d+H\x1b\[\?25h)")
MOVE = re.compile(rb"\x1b\[\d+;\d+H")


def quiet(t, idle=0.3, timeout=20):
    """Read the TUI's output until it has drawn nothing for `idle` seconds (it ends every frame,
    even an unchanged one, with the same few sequences; those don't count)."""
    end, n, last = time.time() + timeout, len(t.log), time.time()
    while time.time() < end:
        t.pump(0.05)
        if NOOP.sub(b"", t.log[n:]):
            last = time.time()
        n = len(t.log)
        if time.time() - last >= idle:
            return True
    return False


def redraw(t, rows, cols, timeout=20):
    """Resize and wait for the frame at the new size: ratatui clears the screen on a resize, then
    draws the frame (which can take a while, so a fixed wait isn't enough), then goes quiet.
    Returns how long the frame took to arrive."""
    if (rows, cols) == (t.rows, t.cols):
        quiet(t, 0.25, 5)
        return 0.0
    mark, start = len(t.log), time.time()
    t.resize(rows, cols, 0)
    while time.time() - start < timeout:
        t.pump(0.02)
        out = t.log[mark:]
        if b"\x1b[2J" in out and MOVE.search(out, out.index(b"\x1b[2J")):
            break
    took = time.time() - start
    quiet(t, 0.25, 5)
    return took


def masked(t):
    return [AGO.sub("#ago", GOING.sub("going for #", CLOCK.sub("##:##", l))) for l in t.lines()]


def steady(t, seconds=0.6):
    """None when the screen holds still on its own for a few ticks; otherwise the two frames it
    keeps flipping between (a change that sticks, like a reply landing, is just waited out)."""
    seen = [masked(t)]
    end = time.time() + seconds
    while time.time() < end:
        t.pump(0.1)
        now = masked(t)
        if now != seen[-1]:
            if now in seen:
                return seen[-1], now
            seen.append(now)
    return None


def intact(t, what):
    """The screen matches a full repaint: nothing drawn earlier has drifted out of place. Like
    Tui.repaint_diff (a resize and back makes ratatui draw everything), but a column wider rather
    than a row taller (lists, forms and the help keep their scroll when only the width changes),
    and each step waits for its frame rather than a fixed time. Clock times, the chat footer's
    running time and the panels' "N min ago" can tick over during the check, so they are masked."""
    alive(t, what)
    flips = steady(t)
    if flips:  # (a flickering screen can't be compared with a repaint: report the flicker instead)
        y = next(y for y, (a, b) in enumerate(zip(*flips)) if a != b)
        soft(False, f"the screen flickers by itself after {what} ({t.cols}x{t.rows}): row {y} keeps switching between "
                    f"{flips[0][y]!r} and {flips[1][y]!r}")
        return
    before = masked(t)
    rows, cols = t.rows, t.cols
    redraw(t, rows, cols + 1)
    redraw(t, rows, cols)
    alive(t, what)
    diff = [(y, b, a) for y, (b, a) in enumerate(zip(before, masked(t))) if b != a]
    if diff:
        for y, inc, ref in diff[:6]:
            print(f"row {y}\n  shown:   {inc!r}\n  redrawn: {ref!r}")
        raise AssertionError(f"the screen was corrupted after {what}")


# the TUI's own output: cursor moves, colours, clears, modes; and /copy's OSC 52 with base64 in it
OWN = re.compile(rb"\x1b\[[0-9;?]*[HmJhlKABCDG]|\x1b\]52;c;[A-Za-z0-9+/=]*\x07")


def clean_output(t, what, quitting=False):
    """Nothing reached the terminal but text and the TUI's own sequences: no control characters
    (C0 or C1), no OSC (titles, links), DCS, cursor reports or charset switches, and the
    alternate screen, mouse and paste modes are left only on the way out."""
    log = t.log
    rest = OWN.sub(b"", log)
    bad = re.search(rb"[\x00-\x1f\x7f]|\xc2[\x80-\x9f]", rest)
    if bad:
        at = bad.start()
        raise AssertionError(f"{rest[at:at + 1]!r} reached the terminal after {what}: …{rest[max(0, at - 60):at + 40]!r}…")
    if not quitting:
        for seq in (b"\x1b[?1049l", b"\x1b[?1000l", b"\x1b[?2004l"):
            assert seq not in log, f"{seq!r} (a mode switched off) reached the terminal after {what}"


def frame(t, what, side=False, files=False):
    """All of it, on a settled screen: alive, clean output, the separators of the panes that are
    drawn (sidebar at column 29, drawer at its left edge) on every row, intact."""
    alive(t, what)
    clean_output(t, what)
    for needle in LEAKS:
        absent(t, needle, f"{needle!r}, an escape sequence drawn as text, after {what}")
    lines = t.lines()

    def cell(y, x):  # (by column: a row's text holds combining marks in with their letters)
        return t.screen.buffer[y][x].data
    if side:
        bad = [y for y in range(t.rows) if cell(y, 29) != "│"]
        assert not bad, f"the sidebar's separator broken after {what}, rows {bad}:\n" + "\n".join(lines)
        assert lines[0].startswith(" ⟟ Agent Chat"), f"the sidebar's first row after {what}: {lines[0]!r}"
    if files:
        x = t.cols - min(32, t.cols - 60)
        bad = [y for y in range(t.rows) if cell(y, x) != "│"]
        assert not bad, f"the drawer's separator (column {x}) broken after {what}, rows {bad}:\n" + "\n".join(lines)
    intact(t, what)


def quiesce(ids):
    """Stop and wait out any run in this suite's chats (one a failed section left going would keep
    changing the screen under the next section's checks)."""
    mine = {ids.get("chat"), ids.get("md"), *(ids.get(f"chat_{k}") for k in ("N1", "N2", "N3"))}
    for c in api("GET", "/api/chats"):
        if c["id"] in mine and c["status"] != "idle":
            api("POST", f"/api/chats/{c['id']}/stop")
    end = time.time() + 30
    while time.time() < end and any(c["id"] in mine and c["status"] != "idle" for c in api("GET", "/api/chats")):
        time.sleep(0.3)
    # and the main chat ends on its usual last reply, which the sections look for
    if ids.get("chat") and not ids.get("deleted") and f"END{S}" not in str(api("GET", f"/api/chats/{ids['chat']}")["messages"][-1].get("content")):
        wait_idle(ids["chat"], send(ids["chat"], f"verbatim: END{S} ✍️ 👨\u200d👩\u200d👧 🇦🇪"))


def finish(ids, t):
    """End the main chat on its usual last reply again (the sections after look for it)."""
    n = send(ids["chat"], f"verbatim: END{S} ✍️ 👨\u200d👩\u200d👧 🇦🇪")
    wait_idle(ids["chat"], n, t)
    expect(t, f"END{S}", 5)


def chat_summary(cid):
    return next(c for c in api("GET", "/api/chats") if c["id"] == cid)


def send(cid, content):
    """Send through the API (a TUI showing the chat follows it live); returns the message count before."""
    n = chat_summary(cid)["messages"]
    api("POST", f"/api/chats/{cid}/run", {"content": content})
    return n


def wait_idle(cid, n, t=None, timeout=60):
    """Until the chat has more than n messages and its run is over (reading the TUI meanwhile)."""
    end = time.time() + timeout
    while time.time() < end:
        c = chat_summary(cid)
        if c["status"] == "idle" and c["messages"] > n:
            break
        if t:
            t.pump(0.3)
        else:
            time.sleep(0.3)
    else:
        raise AssertionError(f"chat {cid} still {chat_summary(cid)['status']} after {timeout} s")
    if t:
        t.pump(1.0)


XDG = os.environ.get("XDG_CONFIG_HOME") or tempfile.mkdtemp(prefix="agent-chat-tui-render-xdg-")
WORKDIR = tempfile.mkdtemp(prefix="agent-chat-tui-render-cwd-")


def launch(name, cols, rows, chat=None, state=None, extra=()):
    """A TUI with a config folder of its own (state.json from `state`), so sections don't share
    remembered panes, themes or verbosity."""
    xdg = os.path.join(XDG, f"render-{name}-{S}")
    os.makedirs(os.path.join(xdg, "agent-chat-tui"), exist_ok=True)
    with open(os.path.join(xdg, "agent-chat-tui", "state.json"), "w") as f:
        json.dump(state or {}, f)
    t = Tui(BIN, B, cols=cols, rows=rows, extra=(*(("--chat", chat) if chat else ()), *extra), cwd=WORKDIR, env={"XDG_CONFIG_HOME": xdg})
    if chat:
        # a chat opened with --chat starts with its message box focused; the sections were written
        # for the sidebar's focus: box -> trace -> sidebar
        expect(t, "Agent Chat", 15, "the first frame")
        t.key("backtab", 0.2)
        t.key("backtab", 0.3)
    return t


def close(t):
    if t.pid:  # (alive() clears it when the TUI has already exited)
        t.close()


def to_box(t):
    """From the sidebar (where `launch` leaves a TUI) to the message box: sidebar -> trace -> box."""
    t.key("tab", 0.3)
    t.key("tab", 0.3)
    t.send("q", 0.4)  # (a letter that lands in the box shows there)
    expect(t, "│ q", 3, "a letter typed in the message box")
    t.key("backspace", 0.3)


def slash(t, command, wait=0.8):
    t.send(command, 0.4)
    t.key("enter", wait)


# ------------------------------------------------------------------ the data

def seed():
    ws = tempfile.mkdtemp(prefix=f"agent-chat-render-ws-{S}-")
    ids = {"ws": ws}
    ids["main"] = api("POST", "/api/agents", {
        "name": f"Rndr{S}", "emoji": "🧑🏽\u200d💻", "color": "#zz\x1b[31m", "purpose": f"draws {MIX}", "system_prompt": MIX,
        "tools": ["run_shell", "write_file", "read_file", "list_dir"], "confirm_shell": False, "memory": False, "workspace": ws})["id"]
    hostile = {
        "N1": {"name": f"N1{S} {HOSTILE['vs16']} {HOSTILE['zwj']} {HOSTILE['flags']} {HOSTILE['keycaps']} {HOSTILE['skin']} {HOSTILE['cjk']}",
               "emoji": "1️⃣", "purpose": MIX},
        "N2": {"name": f"N2{S} " + " ".join(HOSTILE[k] for k in ("rtl", "combining", "zero", "controls", "ansi", "tabs", "cr")),
               "emoji": "\x1b[2J\t\r", "purpose": HOSTILE["ansi"], "color": "\x1b[31m"},
        "N3": {"name": f"N3{S}" + "W" * 10000, "emoji": "🇦🇪" * 3000, "purpose": "P" * 10000, "color": "+fffff"},
    }
    ids["agents"] = {k: api("POST", "/api/agents", v | {"memory": False})["id"] for k, v in hostile.items()}

    # chats titled with each kind of hostile text, and one per hostile agent (turn headers, subtitles)
    ids["titled"] = []
    for k, v in HOSTILE.items():
        cid = api("POST", "/api/chats", {"agent_id": ids["main"]})["id"]
        api("PATCH", f"/api/chats/{cid}", {"title": f"T{S} {k} {v}"})
        ids["titled"].append(cid)
    runs = []
    for k, aid in ids["agents"].items():
        cid = api("POST", "/api/chats", {"agent_id": aid})["id"]
        runs.append((cid, send(cid, f"verbatim: hi from {k}{S} {MIX}")))
        ids[f"chat_{k}"] = cid

    # the markdown chat: whole, with CRLF line ends, then cut short in the middle of things
    ids["md"] = api("POST", "/api/chats", {"agent_id": ids["main"]})["id"]

    def md_msgs():
        yield "verbatim: " + MD
        yield "verbatim: " + MD.replace("\n", "\r\n")
        for cut in MD_CUTS:
            yield "verbatim: " + MD[:cut]
        yield f"verbatim: MDEND{S}"

    # the main chat: hostile replies and thinking, shell output, raw escapes in a command, long words
    ids["chat"] = api("POST", "/api/chats", {"agent_id": ids["main"]})["id"]

    def main_msgs():
        yield "verbatim: " + MIX
        yield "shell: " + SHELL
        yield "shell: printf 'RAW\x1b[31mESC\x1b[0m\\ttab\\rCR\\n'"
        yield "verbatim: " + "W" * 10000 + " " + "测" * 3000 + " " + "👨\u200d👩\u200d👧" * 500
        yield "verbatim: " + MD
        yield f"verbatim: END{S} ✍️ 👨\u200d👩\u200d👧 🇦🇪"

    # (the two chats' messages go one at a time each, both chats at once)
    queues = {ids["md"]: md_msgs(), ids["chat"]: main_msgs()}
    pending = {}
    while queues or pending or runs:
        for cid, gen in list(queues.items()):
            if cid not in pending:
                msg = next(gen, None)
                if msg is None:
                    del queues[cid]
                else:
                    pending[cid] = send(cid, msg)
        for cid, n in list(pending.items()):
            c = chat_summary(cid)
            if c["status"] == "idle" and c["messages"] > n:
                del pending[cid]
        runs = [(cid, n) for cid, n in runs if not (chat_summary(cid)["status"] == "idle" and chat_summary(cid)["messages"] > n)]
        time.sleep(0.3)
    api("PATCH", f"/api/chats/{ids['chat']}", {"title": f"Main{S} {HOSTILE['vs16']} {HOSTILE['cjk']} {HOSTILE['ansi']}"})
    api("PATCH", f"/api/chats/{ids['md']}", {"title": f"Md{S} markdown torture"})

    for k, v in HOSTILE.items():
        api("POST", "/api/memory", {"fact": f"rndr{S} {k}fact quirk{k}{S} {v}", "importance": 1})
    api("POST", "/api/routines", {"name": f"R{S} {MIX}", "prompt": MIX, "agent_id": ids["main"],
                                  "schedule": {"type": "interval", "minutes": 100000}, "enabled": False})

    # workspace: hostile names, each file says which it is on its first line
    names = ["tab\tname.txt", "cr\rname.txt", "nl\nname.txt", "esc\x1b[31mred.txt", "osc\x1b]0;PWN\x07.txt", "c1 \u009b2J \u0085.txt",
             "emoji ✍️👨\u200d👩\u200d👧🇦🇪1️⃣👋🏽.txt", "cjk 测试文件名测试文件名.txt", "rtl مرحبا שלום \u202e.txt", "zw\u200b\u200d\ufeff\u2060.txt",
             "comb e\u0301\u0302\u0303 Z\u0351\u0352.txt", "url ?#&%20+ chars.txt", "L" * 200 + ".txt", "bigline.txt", "manylines.txt", "binary.bin"]
    ids["files"] = {}
    for i, name in enumerate(names):
        marker = f"VIEW{i}x{S}"
        if name == "bigline.txt":
            body = marker + " " + ("long" + MIX.replace("\n", " ")) * 150 + "\n"
        elif name == "manylines.txt":
            body = marker + "\n" + "".join(f"{j}\t{MIX}\n" for j in range(3000))
        elif name == "binary.bin":
            body = marker + "\x00\x01\x1b[31m\xff" * 50
        else:
            body = f"{marker}\n{MIX}\n" + MD + "\n\tTAB\x1b[31mRED\x1b[0m\rCR\n"
        with open(os.path.join(ws, name), "w", encoding="utf-8", errors="surrogateescape") as f:
            f.write(body)
        ids["files"][name] = marker
    hostile_dir = os.path.join(ws, "dir \x1b[31m✍️测试\tx")
    os.makedirs(hostile_dir)
    with open(os.path.join(hostile_dir, "inner.txt"), "w") as f:
        f.write(f"INNER{S}\n")
    sub = os.path.join(ws, f"sub{S}")
    os.makedirs(sub)
    with open(os.path.join(sub, f"ok-{S}.txt"), "w") as f:
        f.write("fine\n")
    with open(os.path.join(sub.encode(), b"bad\xff\xfe name.txt"), "w") as f:  # not UTF-8
        f.write("bytes\n")
    return ids


# ------------------------------------------------------------------ the sections

def welcome(ids):
    section("welcome page and sidebar: hostile agent names, icons, purposes and chat titles; live renames; the palette over them")
    t = launch("welcome", 120, 40, state={"filter": ids["main"]})  # (its chats listed, however many agents there are)
    try:
        expect(t, "Pick an agent.")
        expect(t, f"N1{S}", what="the agent with an emoji-laden name")
        expect(t, f"N3{S}WWW", what="the agent with a 10k-character name")
        expect(t, f"T{S} ", what="a chat with a hostile title")
        frame(t, "the welcome page with hostile agents and titles", side=True)
        # walk the sidebar over the hostile chat rows (selection highlight on each)
        for _ in range(12):
            t.key("j", 0.08)
        t.pump(0.5)
        frame(t, "moving the selection over hostile chat titles", side=True)
        # renames that arrive while the page is up
        api("PUT", f"/api/agents/{ids['agents']['N1']}", {"name": f"M1{S} {HOSTILE['ansi']} {HOSTILE['zwj']}", "purpose": HOSTILE["cr"],
                                                          "emoji": "🏴\U000e0067\U000e0062\U000e0065\U000e006e\U000e0067\U000e007f", "memory": False})
        api("PUT", f"/api/agents/{ids['agents']['N2']}", {"name": f"M2{S} {HOSTILE['cjk'] * 4}", "purpose": HOSTILE["combining"],
                                                          "emoji": "👩\u200d❤️\u200d👨", "memory": False})
        api("PATCH", f"/api/chats/{ids['titled'][0]}", {"title": f"T{S} renamed live {HOSTILE['controls']} {HOSTILE['flags']}"})
        expect(t, "renamed live", 10, "the renamed chat")
        soft(t.wait_for(f"M1{S}", 10), "an agent renamed through the API (PUT /api/agents) never shows in a running TUI: the server "
             "broadcasts no event for agent changes, so the sidebar and welcome page keep the old name until restart")
        t.pump(1.0)
        frame(t, "live renames to hostile names", side=True)
        close(t)
        t = launch("welcome2", 120, 40)  # (a new TUI loads the renamed agents)
        expect(t, f"M1{S}", 10, "the renamed agent")
        expect(t, f"M2{S}", 3)
        frame(t, "hostile agent names after a rename", side=True)
        # the sidebar filter with hostile text typed into it
        t.key("/", 0.3)
        t.paste(f"T{S} {HOSTILE['zwj']}\t{HOSTILE['ansi']}")
        t.pump(0.5)
        frame(t, "a hostile sidebar filter", side=True)
        t.key("esc", 0.4)
        # the palette lists the agents (name and purpose) and the titles
        t.key("ctrl-k", 0.6)
        t.send(S, 1.0)
        expect(t, f"T{S}", 5, "the hostile titles in the palette")
        intact(t, "the palette listing hostile titles and agents")
        t.paste(HOSTILE["cjk"][:6] + HOSTILE["zwj"] + HOSTILE["ansi"])
        t.pump(1.0)
        intact(t, "a hostile query in the palette")
        t.key("esc", 0.4)
        absent(t, "Search and commands")
        frame(t, "the palette closed", side=True)
    finally:
        close(t)


def chat_paths(ids):
    section("a chat of hostile text: user entries, replies, thinking, shell output and commands, 10k words, markdown; history, verbose, scrolling, live replies, the message box, prompts, the agent editor")
    t = launch("chat", 120, 40, chat=ids["chat"], state={"files": True})
    try:
        expect(t, f"END{S}", 15, "the last reply")
        expect(t, "Workspace", 3, "the drawer")
        frame(t, "opening a chat full of hostile text", side=True, files=True)
        head = t.lines()[0]
        assert f"Main{S}" in head and "ctx" in head and "tok/s" in head, f"a hostile title pushed the header's stats off: {head!r}"
        t.key("tab", 0.3)  # sidebar -> trace
        for i in range(12):
            t.key("pgup", 0.25)
            if i % 3 == 2:
                frame(t, f"scrolling up the trace ({i + 1} pages)", side=True, files=True)
        t.key("end", 0.4)
        t.key("alt-o", 0.8)  # verbose: the hostile thinking, tool bodies and results
        frame(t, "verbose over hostile thinking and shell output", side=True, files=True)
        for i in range(12):
            t.key("pgup", 0.25)
            if i % 4 == 3:
                frame(t, f"scrolling the verbose trace ({i + 1} pages)", side=True, files=True)
        t.key("end", 0.4)

        # live: the same kinds of text arriving as deltas, with the TUI following the stream
        n = send(ids["chat"], "verbatim: " + MIX)
        wait_idle(ids["chat"], n, t)
        frame(t, "a hostile reply streamed live (verbose)", side=True, files=True)
        n = send(ids["chat"], "shell: " + SHELL)
        expect(t, "BAR", 15, "the shell's progress bar while it runs")
        alive(t, "live shell output with colours and CRs")
        clean_output(t, "live shell output with colours and CRs")
        wait_idle(ids["chat"], n, t)
        expect(t, "SHELLDONE", 5)
        frame(t, "shell output with colours, CR progress bars and escapes", side=True, files=True)
        t.key("alt-o", 0.8)
        n = send(ids["chat"], "verbatim: " + MD)
        wait_idle(ids["chat"], n, t)
        frame(t, "markdown torture streamed live", side=True, files=True)
        finish(ids, t)

        # the message box holding hostile text (small paste inline, a large one as a token)
        t.key("tab", 0.3)  # trace -> box
        t.paste(MIX)
        expect(t, "│ next line ", 3, "the end of the pasted text in the box")
        frame(t, "hostile text in the message box", side=True, files=True)
        t.key("ctrl-c", 0.4)
        t.paste((MIX + "\n") * 12)
        expect(t, "[Pasted text #1", 3)
        frame(t, "a large hostile paste in the box", side=True, files=True)
        t.key("ctrl-c", 0.4)
        # the rename prompt holds the hostile title; the agent editor its name, icon, colour, purpose
        t.key("f2", 0.6)
        expect(t, "Rename chat", 3)
        intact(t, "the rename prompt holding a hostile title")
        t.key("end", 0.2)
        t.paste(" " + HOSTILE["zwj"] + HOSTILE["tabs"])
        intact(t, "typing hostile text into the rename prompt")
        t.key("esc", 0.4)
        t.key("alt-a", 0.8)
        expect(t, f"Edit Rndr{S}", 3)
        intact(t, "the agent editor with hostile fields")
        for _ in range(4):
            t.key("tab", 0.2)
        intact(t, "moving through the agent editor's hostile fields")
        t.key("esc", 0.5)
        absent(t, f"Edit Rndr{S}")
        frame(t, "the dialogs closed", side=True, files=True)
    finally:
        close(t)


def files_paths(ids):
    section("files drawer and viewer: hostile file and folder names, hostile, huge and binary contents")
    t = launch("files", 120, 40, chat=ids["chat"], state={"files": True})
    try:
        expect(t, "Workspace", 10)
        expect(t, "binary.bin", 5, "the listing")
        t.key("backtab", 0.4)  # sidebar -> drawer
        frame(t, "the drawer listing hostile names", side=True, files=True)
        entries = api("GET", f"/api/workspace?agent_id={ids['main']}&path=.")["entries"]
        for i, e in enumerate(entries):
            t.key("down", 0.15)
            if e["dir"]:
                continue
            marker = "Binary file (" if e["name"] == "binary.bin" else ids["files"].get(e["name"])
            press(t, "enter")
            if marker:
                expect(t, marker, 5, f"{e['name']!r} in the viewer")
            frame(t, f"viewing {e['name']!r}")
            for k in ("pgdn", "end", "pgup"):
                press(t, k)
            alive(t, f"scrolling {e['name']!r}")
            clean_output(t, f"scrolling {e['name']!r}")
            if e["name"] == "manylines.txt":
                intact(t, "the end of a 3000-line file")
            if e["name"].startswith("emoji "):  # the viewer shrunk with hostile text in it
                press(t, "home")
                for rows, cols in SMALL:
                    redraw(t, rows, cols)
                    intact(t, f"the viewer shrunk to {cols}x{rows}")
                redraw(t, 40, 120)
                expect(t, marker, 3)
            press(t, "esc")
            if marker:
                absent(t, marker, "the viewer, closed")
        frame(t, "every file viewed", side=True, files=True)
        # into the hostile folder and back out
        for _ in range(len(entries) + 2):
            t.key("up", 0.05)
        t.pump(0.4)
        hostile_dir = next(i for i, e in enumerate(entries) if e["name"].startswith("dir "))
        for _ in range(hostile_dir + 1):
            t.key("down", 0.1)
        t.key("enter", 1.0)
        expect(t, "inner.txt", 5, "inside the folder with a hostile name")
        frame(t, "a folder with a hostile name", side=True, files=True)
        t.key("left", 1.0)
        expect(t, "binary.bin", 5, "back at the top")
        # a folder holding a name that isn't UTF-8: still listed, the TUI still running
        sub = next(i for i, e in enumerate(entries) if e["name"] == f"sub{S}")
        for _ in range(sub + 1):
            t.key("down", 0.1)
        t.key("enter", 1.5)
        alive(t, "opening a folder with a file name that isn't UTF-8")
        soft(t.wait_for(f"ok-{S}.txt", 5), "a workspace folder holding a file name that isn't UTF-8 can't be listed: "
             "GET /api/workspace answers 500, and the drawer only says 'request failed'")
        gone(t, "request failed", 10, "the error toast")
        frame(t, "a folder with a name that isn't UTF-8", side=True, files=True)
    finally:
        close(t)


def markdown(ids):
    section("markdown torture: whole, with CRLF, cut short mid-construct, at 40 columns, stopped half-streamed")
    t = launch("md", 120, 40, chat=ids["md"])
    try:
        expect(t, f"MDEND{S}", 15)
        frame(t, "the markdown chat", side=True)
        t.key("tab", 0.3)  # -> trace
        for i in range(15):
            t.key("pgup", 0.25)
            if i % 3 == 2:
                frame(t, f"scrolling the markdown ({i + 1} pages)", side=True)
        t.key("alt-o", 0.8)
        frame(t, "markdown and its thinking, verbose", side=True)
        t.key("alt-o", 0.6)
        t.key("end", 0.4)
        redraw(t, 30, 40)
        intact(t, "the markdown at 40 columns")
        for _ in range(6):
            t.key("pgup", 0.25)
        intact(t, "scrolled markdown at 40 columns")
        t.key("end", 0.3)
        redraw(t, 40, 120)
        expect(t, f"MDEND{S}", 5)
        frame(t, "back at 120 columns", side=True)
        # stopped while streaming: whatever prefix it got to stays drawn whole
        for wait in (1.5, 4.0):
            n = send(ids["md"], "verbatim slow: " + MD)
            end = time.time() + wait
            while time.time() < end:
                t.pump(0.25)
                alive(t, "markdown streaming in")
            clean_output(t, "markdown streaming in")
            api("POST", f"/api/chats/{ids['md']}/stop")
            wait_idle(ids["md"], n, t)
            expect(t, "stopped here", 5)
            frame(t, f"markdown stopped after {wait} s", side=False)
    finally:
        close(t)


SIZES = [(1, 1), (3, 10), (6, 20), (10, 40), (24, 80), (30, 89), (30, 90), (30, 91), (30, 92), (30, 95), (80, 300), (2, 300), (7, 100), (80, 2), (40, 120)]


def sizes(ids):
    section("every size, sidebar and drawer on: 1x1 to 300x80, 89-95 columns, with the sidebar focused and with the trace focused")
    t = launch("sizes", 120, 40, chat=ids["chat"], state={"files": True})
    try:
        expect(t, f"END{S}", 15)
        slow = []
        for focus in ("sidebar", "trace"):
            if focus == "trace":
                t.key("tab", 0.3)
            for rows, cols in SIZES:
                took = redraw(t, rows, cols)
                if took > 0.5:
                    slow.append(f"{cols}x{rows} {focus} {took:.1f} s")
                what = f"{cols}x{rows}, {focus} focused"
                if cols >= 90 and rows >= 2:
                    frame(t, what, side=True, files=True)
                else:
                    frame(t, what)
        expect(t, f"END{S}", 5)
        # (a frame normally comes within 0.1 s of a resize; this chat holds ~20k characters of long words)
        soft(not slow, f"the TUI froze for over half a second redrawing a long chat after a resize: {', '.join(slow)}")
    finally:
        close(t)


SMALL = [(3, 10), (4, 30), (6, 20), (12, 18), (10, 40)]
OVERLAY_KEYS = [("f1", "help"), ("ctrl-k", "palette"), ("f2", "rename prompt"), ("ctrl-u", "attach prompt"), ("alt-d", "delete question"),
                ("alt-n", "agent picker"), ("alt-m", "memory"), ("alt-r", "routines"), ("alt-s", "settings"), ("alt-a", "agent editor")]


def press(t, key):
    """A key, then until the TUI has drawn what it does (a panel that loads first draws later)."""
    t.key(key, 0.05)
    quiet(t, 0.2, 3)


def clear_of_tick(created):
    """Until the chat's age ("going for 3m", which /usage and /context count up) won't tick over
    for a while: squeezed into a narrow panel it wraps, and a wrapped number can't be masked."""
    while not (time.time() - created >= 60 and 2 <= (time.time() - created) % 60 <= 50):
        time.sleep(0.5)


def overlays(ids):
    section("every overlay at small sizes: opened there, and opened large then shrunk; the command menu too")
    t = launch("overlays", 120, 40, chat=ids["chat_N1"], state={"files": True})  # (a short chat: small-size redraws stay quick)
    try:
        expect(t, f"hi from N1{S}", 15)
        to_box(t)
        created = api("GET", f"/api/chats/{ids['chat_N1']}")["created"]
        # what the slash commands show, opened at full size, then shrunk
        for command, shows in [("/help", "/compact [instructions]"), ("/context", "auto-compacts at"), ("/usage", "replies"),
                               ("/status", "reachable"), ("/model", "Model for"), ("/", "Tab complete")]:
            if command == "/":
                t.send("/", 0.6)
            else:
                slash(t, command, 1.0)
            expect(t, shows, 5, f"what {command} opens")
            for rows, cols in SMALL:
                redraw(t, rows, cols)
                if command in ("/context", "/usage"):
                    clear_of_tick(created)
                intact(t, f"{command} shrunk to {cols}x{rows}")
            redraw(t, 40, 120)
            expect(t, shows, 3, f"what {command} opened, back at full size")
            press(t, "esc")
            if command == "/":
                press(t, "ctrl-c")
            absent(t, shows)
        # dialogs opened at each small size (the agent editor holds the hostile agent)
        for rows, cols in SMALL:
            redraw(t, rows, cols)
            for key, name in OVERLAY_KEYS:
                press(t, key)
                intact(t, f"the {name} opened at {cols}x{rows}")
                press(t, "esc")
                alive(t, f"closing the {name} at {cols}x{rows}")
            press(t, "alt-r")
            if t.find("Routines"):  # (the new-routine form, where the panel shows)
                press(t, "n")
                intact(t, f"the new-routine form at {cols}x{rows}")
                press(t, "esc")
            press(t, "esc")
        # a short but ordinary terminal: every field of the agent editor in turn holds still
        redraw(t, 12, 100)
        t.key("alt-a", 0.8)
        expect(t, "Edit ", 3, "the agent editor")
        for i in range(7):
            flips = steady(t, 1.0)
            soft(not flips, f"the agent editor flickers at 100x12 with field {i + 1} focused (forms.rs render: a focused field "
                            f"taller than the room left flips the scroll between its top and bottom every frame)")
            if flips:
                break
            t.key("tab", 0.3)
        t.key("esc", 0.5)
        redraw(t, 40, 120)
        expect(t, f"hi from N1{S}", 5)
        absent(t, "Delete “")
        frame(t, "the dialogs closed, back at full size", side=True, files=True)
        assert any(c["id"] == ids["chat_N1"] for c in api("GET", "/api/chats")), "Esc on the delete question kept the chat"
    finally:
        close(t)


STORM = [(1, 1), (3, 10), (6, 20), (10, 40), (24, 80), (30, 89), (30, 90), (30, 95), (80, 300), (2, 300), (80, 2), (40, 120), (17, 63), (5, 200)]


def storm(ids):
    section("resize storms during a streaming hostile reply with a hostile message queued: 20 rapid resizes, then 10 more with the help open")
    t = launch("storm", 120, 40, chat=ids["chat"], state={"files": True})
    try:
        expect(t, f"END{S}", 15)
        n = send(ids["chat"], "verbatim slow: " + (MIX + "\n") * 2 + f"STORMEND{S}")
        end = time.time() + 10
        while time.time() < end and chat_summary(ids["chat"])["status"] != "running":
            t.pump(0.2)
        t.pump(0.5)
        soft(t.find("● running"), "the header's '● running' is pushed off the screen by a long agent purpose (draw_header fits the "
             "subtitle to the full width, then appends the indicator)")
        api("POST", f"/api/chats/{ids['chat']}/run", {"content": f"verbatim: queued {MIX} QEND{S}"})  # (shown queued over the box)
        expect(t, "queued (1)", 5, "the queued message over the message box")
        rnd = random.Random(20261010)
        for i in range(20):
            rows, cols = rnd.choice(STORM)
            t.resize(rows, cols, 0.05)
            alive(t, f"resize {i + 1} of the storm ({cols}x{rows})")
        redraw(t, 40, 120)
        t.key("f1", 0.3)
        for i in range(10):
            rows, cols = rnd.choice(STORM)
            t.resize(rows, cols, 0.05)
            alive(t, f"resize {i + 1} of the storm with the help open ({cols}x{rows})")
        redraw(t, 40, 120)
        t.key("esc", 0.3)
        clean_output(t, "the resize storms")
        wait_idle(ids["chat"], n + 2, t)  # (the reply, then the queued message and its answer)
        expect(t, f"QEND{S}", 5, "the answer to the queued message")
        assert any(f"STORMEND{S}" in str(m.get("content")) for m in api("GET", f"/api/chats/{ids['chat']}")["messages"]), "the storm's reply saved whole"
        frame(t, "resize storms during a streaming reply", side=True, files=True)
        finish(ids, t)
    finally:
        close(t)


def themes(ids):
    section("light and plain themes (flags and /theme): the hostile chat, small sizes, the help")
    for flag, bg in (("--light", "f2f3f7"), ("--plain", None)):
        t = launch(f"theme{flag}", 100, 30, chat=ids["chat"], extra=(flag,))
        try:
            expect(t, f"END{S}", 15)
            frame(t, f"the {flag[2:]} theme", side=True)
            bgs = {t.screen.buffer[y][x].bg for y in range(t.rows) for x in range(t.cols)}
            if bg:
                assert bg in bgs, sorted(bgs)[:12]
            else:
                assert "f2f3f7" not in bgs and "default" in bgs, sorted(bgs)[:12]
            t.key("tab", 0.3)
            t.key("alt-o", 0.8)
            frame(t, f"verbose in the {flag[2:]} theme", side=True)
            t.key("alt-o", 0.6)
            redraw(t, 10, 40)
            intact(t, f"the {flag[2:]} theme at 40x10")
            t.key("f1", 0.5)
            intact(t, f"the help in the {flag[2:]} theme at 40x10")
            t.key("esc", 0.4)
            redraw(t, 30, 100)
            t.key("backtab", 0.3)  # back to the sidebar, then the box
            to_box(t)
            other = "plain" if flag == "--light" else "light"
            slash(t, f"/theme {other}")
            expect(t, f"Theme: {other}", 3)
            gone(t, f"Theme: {other}", 8, "the theme toast")
            frame(t, f"/theme {other} over the {flag[2:]} theme", side=True)
        finally:
            close(t)


def toasts(ids):
    section("toasts: hostile text in them, several at once on small screens")
    t = launch("toasts", 120, 40, chat=ids["chat_N2"])  # (a short chat: redraws at small sizes stay quick)
    try:
        expect(t, f"hi from N2{S}", 15)
        to_box(t)

        def attach(path):
            """A path that can't be read: the error toast quotes it."""
            t.key("ctrl-u", 0.4)
            t.paste(path, 0.2)
            t.key("enter", 0.3)

        # each toast checked while it shows (well inside its 3.5 or 7 s), then waited out
        attach(f"/nonexistent-{S}/{HOSTILE['ansi']}{HOSTILE['zwj']}\t{HOSTILE['cjk']}/" + "W" * 300)
        expect(t, f"nonexistent-{S}", 3, "the error toast")
        clean_output(t, "an error toast quoting a hostile path")
        intact(t, "an error toast quoting a hostile path")
        gone(t, f"nonexistent-{S}", 10, "the error toast, expired")
        t.send(f"/nosuch{HOSTILE['zwj']}", 0.3)  # (an unknown command stays in the box; Ctrl+C clears it)
        t.key("enter", 0.6)
        expect(t, "Unknown command", 3)
        intact(t, "an unknown-command toast with emoji")
        t.key("ctrl-c", 0.3)
        gone(t, "Unknown command", 10)
        t.send("/rename ", 0.3)
        t.paste(f"N2{S} {HOSTILE['vs16']} {HOSTILE['ansi']} {HOSTILE['cjk']}")
        t.key("enter", 1.0)
        expect(t, "Renamed to", 3)
        intact(t, "a rename toast quoting a hostile title")
        gone(t, "Renamed to", 10)
        # more toasts than show at once (three), at small sizes and full size
        # (the newest three show; four new ones at each size push out the last size's)
        for rows, cols in ((12, 60), (6, 20), (3, 10), (40, 120)):
            redraw(t, rows, cols)
            for i in range(4):
                attach(f"/nonexistent-{S}/{i}/{HOSTILE['flags']}{HOSTILE['cjk']}" + "x" * (40 * i))
            quiet(t, 0.2, 3)
            alive(t, f"four error toasts at {cols}x{rows}")
            clean_output(t, f"four error toasts at {cols}x{rows}")
            intact(t, f"four error toasts at {cols}x{rows}")
        gone(t, f"nonexistent-{S}", 12, "the error toasts, expired")
        frame(t, "the toasts gone", side=True)
    finally:
        close(t)


def deleted(ids):
    section("the agents deleted under their open chats: 'Deleted agent' in the header, sidebar and turns; quitting hands the terminal back")
    t = launch("deleted", 120, 40, chat=ids["chat"], state={"files": True})
    try:
        expect(t, f"END{S}", 15)
        for aid in [*ids["agents"].values(), ids["main"]]:
            api("DELETE", f"/api/agents/{aid}")
        ids["deleted"] = True
        soft(t.wait_for("Deleted agent", 10), "agents deleted through the API stay in a running TUI's sidebar and header (no event "
             "for agent changes reaches it), so its chats never show 'Deleted agent' until restart")
        t.pump(0.5)
        frame(t, "the open chat's agent deleted elsewhere", side=True, files=True)
        close(t)
        t = launch("deleted2", 120, 40, chat=ids["chat"], state={"files": True})
        expect(t, "Deleted agent", 10)
        expect(t, f"END{S}", 5)
        frame(t, "the open chat's agent deleted", side=True, files=True)
        t.key("tab", 0.3)
        t.key("pgup", 0.3)
        frame(t, "scrolling a deleted agent's chat", side=True, files=True)
        t.key("ctrl-k", 0.5)
        t.send(f"T{S}", 1.0)
        t.key("enter", 1.0)
        intact(t, "opening a deleted agent's hostile-titled chat")
        t.key("ctrl-q", 0.8)
        _, status = os.waitpid(t.pid, 0)
        t.pid = None
        assert os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0, f"exit status {status}"
        clean_output(t, "quitting", quitting=True)
        log = t.log
        assert log.rfind(b"\x1b[?1049l") > max(log.rfind(b"\x1b[?1049h"), 0), "the alternate screen left last"
    finally:
        close(t)


def main():
    assert BIN, "build the TUI first: cd tui && cargo build --release"
    settings = api("GET", "/api/state")["settings"]
    old = {k: settings.get(k) for k in ("base_url", "search_url")}
    api("PUT", "/api/settings", {"base_url": MOCK, "search_url": f"http://x/\x1b[31m{HOSTILE['zwj']}\t测试?q=" + "W" * 300})
    for c in api("GET", "/api/chats"):  # (a run left going by another suite would change the screen mid-check)
        if c["status"] != "idle":
            api("POST", f"/api/chats/{c['id']}/stop")
    # agents a run of this suite that was killed midway left behind (a finished run deletes its own)
    for a in api("GET", "/api/state")["agents"]:
        if re.fullmatch(r"(Rndr\d{7}|[NM][123]\d{7}.*)", a["name"], re.S):
            api("DELETE", f"/api/agents/{a['id']}", ok=False)
    failed = []
    ids = {}
    try:
        section(f"seeding hostile agents, chats, replies, memory, a routine and workspace files (stamp {S})")
        ids = seed()
        for step in (welcome, chat_paths, files_paths, markdown, sizes, overlays, storm, themes, toasts, deleted):
            started = time.time()
            try:
                quiesce(ids)
                step(ids)
            except Exception as e:  # report it and go on: the next section starts its own TUI
                traceback.print_exc()
                failed.append(f"{step.__name__}: {e}".splitlines()[0])
            print(f"   ({time.time() - started:.0f} s)", flush=True)
    finally:
        api("PUT", "/api/settings", {k: v for k, v in old.items() if v is not None})
        if ids and not ids.get("deleted"):
            for aid in [*ids.get("agents", {}).values(), ids.get("main")]:
                api("DELETE", f"/api/agents/{aid}", ok=False)
    if DEFECTS:
        print("\nDEFECTS (checks that failed without stopping their section):\n  " + "\n  ".join(DEFECTS))
    if failed:
        print("\nFAILED sections:\n  " + "\n  ".join(failed))
    if failed or DEFECTS:
        sys.exit(1)
    print("\nall TUI render tests passed")


if __name__ == "__main__":
    main()
