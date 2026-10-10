"""The Rust TUI, driven in a pseudo-terminal against the app on AC_URL (default :8767) wired to
tests/mock_llm.py. Needs the binary (TUI_BIN, default tui/target/release/agent-chat-tui; falls back
to the debug build) and pyte: `uv run --with pyte python tests/e2e_tui.py`.

Covers: the welcome page, starting chats, a streamed reply rendered as a trace, a handoff with an
approval (approve, approve-all), live shell output, verbose details, the files drawer and viewer,
the command palette, memory, routines, settings, the agent editor, rename, pin, branch, edit-last,
stop, queued messages, attachments, help, pastes (bracketed and keystroke, small and large, with
CRs, tabs and escape codes, checked against a forced full repaint), slash commands (the menu,
completion, the commands themselves, // and paths as messages, commands mid-reply, refused ones
kept in the box), the chat footer (compactions and a running time that counts up, also in /context
and /usage), quitting cleanly (Ctrl+Q and /quit, the terminal handed back), and where keys go:
approvals (y / a / n only in the trace, Alt+Y / Alt+A / Alt+N anywhere, never typed text), prompts
and Ctrl+C in dialogs, the memory delete question, Calendars (Enter adds), one chat stream per
visit, a chat deleted elsewhere, the welcome page's numbers, a missing --chat, clicks and the
drawer below 90 columns, a restored drawer and its refresh row, UTF-8 split across network reads,
and how things are drawn: emoji whose width terminals disagree on (agent icons, titles, messages,
the message box) against a forced full repaint, a wide title in the header, a resize and back,
90-95 columns with both side panes, and the shortcuts scrolling on a short screen.
"""
import base64
import json
import os
import re
import subprocess
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


def api(method, path, body=None):
    r = httpx.request(method, B + path, json=body, headers=H, timeout=30)
    r.raise_for_status()
    return r.json()


def section(name):
    print(f"== {name}")


def expect(t, needle, timeout=15, what=None):
    if not t.wait_for(needle, timeout):
        print("\n".join(t.lines()))
        raise AssertionError(f"expected {what or needle!r} on screen")


def absent(t, needle, what=None):
    if t.find(needle):
        print("\n".join(t.lines()))
        raise AssertionError(f"did not expect {what or needle!r} on screen")


GOING = re.compile(r"going for \d+[smhd]( \d+[mh])?")


def intact(t, what):
    """The screen matches a full repaint: nothing drawn earlier has drifted out of place. The chat
    footer's running time ("going for 12s") counts on during the check, so it is masked."""
    diff = [(y, inc, ref) for y, inc, ref in t.repaint_diff() if GOING.sub("going for #", inc) != GOING.sub("going for #", ref)]
    if diff:
        for y, inc, ref in diff[:6]:
            print(f"row {y}\n  shown:   {inc!r}\n  redrawn: {ref!r}")
        raise AssertionError(f"the screen was corrupted after {what}")


def new_chat_titled(title, agent="assistant"):
    cid = api("POST", "/api/chats", {"agent_id": agent})["id"]
    api("PATCH", f"/api/chats/{cid}", {"title": title})
    return cid


def open_streams(pid):
    """Connections the TUI holds open to the app (None where `ss` can't tell)."""
    try:
        out = subprocess.run(["ss", "-tnpH", "state", "established", f"( dport = :{B.rsplit(':', 1)[1]} )"], capture_output=True, text=True, timeout=10).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    return sum(f"pid={pid}," in line for line in out.splitlines()) if "pid=" in out else None


def main():
    assert BIN, "build the TUI first: cd tui && cargo build --release"
    api("PUT", "/api/settings", {"base_url": MOCK, "bypass_approvals": False, "auto_title": True, "auto_memory": False})
    # stop anything left waiting from other tests, so the sidebar status words are ours
    for c in api("GET", "/api/chats"):
        if c["status"] != "idle":
            api("POST", f"/api/chats/{c['id']}/stop")
    state_file = os.path.join(os.environ.get("XDG_CONFIG_HOME", os.path.expanduser("~/.config")), "agent-chat-tui", "state.json")
    if os.path.exists(state_file):
        os.remove(state_file)

    workdir = tempfile.mkdtemp(prefix="agent-chat-tui-cwd-")  # where /export writes
    t = Tui(BIN, B, cols=120, rows=40, cwd=workdir)
    try:
        section("welcome: agents listed, sidebar present")
        expect(t, "Pick an agent.")
        expect(t, "Agent Chat")
        expect(t, "Orchestrator")
        expect(t, "n  new chat")

        section("new chat by number, reply streams into a trace with a table and code")
        t.send("n", 0.6)
        expect(t, "New chat with")
        t.send("1", 1.0)
        expect(t, "Assistant is ready", what="the empty-chat note")
        stamp = f"stamp{int(time.time())}"
        t.send(f"hello there, show me a table {stamp}", 0.2)
        t.key("enter", 0.5)
        expect(t, "tok/s", 20, "the reply's stats")
        expect(t, "thought for", 5)
        expect(t, "│ a  b", 5, "the table")
        expect(t, "│ │ def hello():", 5, "the code block")
        expect(t, "Titled: hello there", 10, "the model-written title")

        section("verbose shows the thinking text and tool results")
        t.key("alt-o", 0.6)
        expect(t, "The user said: hello there", 3)
        t.key("alt-o", 0.4)
        absent(t, "The user said: hello there")

        section("handoff with an approval: Alt+Y from the message box approves, the nested trace finishes")
        t.key("ctrl-k", 0.5)
        t.send("New Orchestrator", 0.6)
        t.key("enter", 1.2)
        expect(t, "Orchestrator is ready", 5)
        t.send("please delegate this", 0.2)
        t.key("enter", 0.5)
        expect(t, "needs approval", 25)
        expect(t, "handoff", 2)
        expect(t, "Alt+Y approve", 2)
        expect(t, "needs you", 3, "the sidebar status word")
        t.key("alt-y", 0.5)
        expect(t, "│ ● run_shell", 20, "the approved command marked done")
        expect(t, "hello from coder", 20)
        expect(t, "│ ● Finished.", 25, "the sub-agent's reply")

        section("approve all for the run covers the second command (Alt+A over a draft, not the agent editor); live shell output streams")
        t.key("ctrl-k", 0.5)
        t.send("New Coder", 0.6)
        t.key("enter", 1.2)
        expect(t, "Coder is ready", 5)
        t.send("shell twice: echo one", 0.2)
        t.key("enter", 0.5)
        expect(t, "needs approval", 25)
        t.type("draft")
        t.key("alt-a", 0.5)
        expect(t, "auto-approved", 25)
        absent(t, "Edit Coder", "the agent editor (Alt+A answers a waiting approval first)")
        expect(t, "one again", 25)
        expect(t, "│ draft ", 2, "the draft still in the box")
        t.key("ctrl-c", 0.3)
        t.send("slow shell", 0.2)
        t.key("enter", 0.5)
        expect(t, "needs approval", 25)
        t.key("up", 0.3)  # an empty box: ↑ goes to the trace, where y answers
        t.type("y")
        t.key("i", 0.3)  # back to the message box
        expect(t, "output so far", 10)
        expect(t, "tick1", 10)
        expect(t, "│ ● run_shell  for i in", 25, "the finished command")
        absent(t, "output so far")

        section("files drawer lists the workspace; Enter opens a file in the viewer")
        t.send("write code please", 0.2)
        t.key("enter", 0.5)
        expect(t, "write_file  greet.py", 25)
        t.key("alt-f", 1.0)
        expect(t, "Workspace", 3)
        expect(t, "greet.py", 5)
        # move to greet.py (row 0 is ↻ refresh, then the listing; folders left by earlier runs too) and open it
        entries = api("GET", "/api/workspace?agent_id=coder&path=.")["entries"]
        for _ in range(next(i for i, e in enumerate(entries) if e["name"] == "greet.py") + 1):
            t.key("down", 0.1)
        t.key("enter", 1.0)
        expect(t, "def greet(name: str", 5, "the file viewer")
        t.key("esc", 0.4)
        t.key("alt-f", 0.5)
        absent(t, "Workspace 💻")

        section("command palette searches message text")
        t.key("ctrl-k", 0.5)
        t.send(stamp, 1.0)
        expect(t, "Titled: hello there", 5)
        t.key("enter", 1.0)
        expect(t, "│ a  b", 5)
        expect(t, stamp, 3, "this run's chat")

        section("branch, rename, pin, edit-last")
        t.key("ctrl-g", 1.5)
        expect(t, "(branch)", 5)
        t.key("f2", 0.5)
        expect(t, "Rename chat", 3)
        t.key("ctrl-u", 0.2)
        t.send("TUI renamed it", 0.2)
        t.key("enter", 1.0)
        expect(t, "TUI renamed it", 5)
        t.key("alt-p", 1.0)
        expect(t, "⚲TUI renamed", 5, "the pin mark in the sidebar")
        t.key("ctrl-e", 0.5)
        expect(t, "editing your last message", 3)
        t.send(" and a list", 0.2)
        t.key("enter", 0.5)
        expect(t, f"{stamp} and a list", 10)
        expect(t, "tok/s", 20)

        section("stop a slow reply, queue while it works, take the queue back")
        t.send("slow please", 0.2)
        t.key("enter", 0.5)
        expect(t, "● running", 5)
        t.send("a queued thought", 0.2)
        t.key("enter", 0.6)
        expect(t, "queued (1)", 5)
        t.key("up", 0.8)
        expect(t, "a queued thought", 3, "the queued text back in the composer")
        absent(t, "queued (1)")
        t.key("ctrl-u", 0.1)  # (in the composer Ctrl+U opens attach; clear the box with backspaces instead)
        t.key("esc", 0.3)
        for _ in range(20):
            t.key("backspace", 0.02)
        t.key("ctrl-s", 1.0)
        expect(t, "stopped here", 10)

        section("attach a file by path")
        tmp = tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False)
        tmp.write("attached contents here\n"); tmp.close()
        t.key("ctrl-u", 0.5)
        expect(t, "Attach a file", 3)
        t.send(tmp.name, 0.2)
        t.key("enter", 1.0)
        expect(t, "📎", 5, "the attachment chip")
        t.send("here is a file", 0.2)
        t.key("enter", 0.5)
        expect(t, "Attached file:", 10)

        section("memory, routines, settings, agent editor, help")
        t.key("alt-m", 1.0)
        expect(t, "Memory ·", 5)
        t.key("esc", 0.4)
        t.key("alt-r", 1.0)
        expect(t, "Routines", 5)
        t.send("n", 0.6)
        expect(t, "New routine", 3)
        t.key("esc", 0.4)
        t.key("esc", 0.4)
        t.key("alt-s", 1.0)
        expect(t, "Model server URL", 5)
        expect(t, MOCK.rsplit(":", 1)[1].split("/")[0], 2)
        t.key("esc", 0.4)
        t.key("alt-a", 1.0)
        expect(t, "Edit Assistant", 5)
        expect(t, "Instructions", 2)
        t.key("esc", 0.4)
        t.key("f1", 0.5)
        expect(t, "Keyboard shortcuts", 3)
        t.key("esc", 0.4)

        section("paste: line endings, tabs and escapes keep the frame; large pastes are placeholders; keystroke pastes are one message")
        before = {c["id"] for c in api("GET", "/api/chats")}
        t.key("ctrl-k", 0.5)
        t.send("New Assistant", 0.6)
        t.key("enter", 1.2)
        expect(t, "Assistant is ready", 5)
        cid = next(c["id"] for c in api("GET", "/api/chats") if c["id"] not in before)
        rows = [f"row {i}: the quick brown fox\tjumps\tover {i}" for i in range(5)]
        for name, text in [("LF", "\n".join(rows)), ("CRLF", "\r\n".join(rows)), ("CR", "\r".join(rows)),
                           ("tabs and ANSI", "\r\n".join(f"\x1b[1;31m{r}\x1b[0m\x1b]0;title\x07" for r in rows))]:
            t.paste(text)
            expect(t, "row 4: the quick brown fox", 3, f"the {name} paste in the message box")
            assert t.lines()[0].startswith(" ⟟ Agent Chat"), f"the {name} paste overwrote the sidebar"
            absent(t, "[1;31m")
            intact(t, f"a bracketed paste with {name}")
            t.key("ctrl-c", 0.4)  # empties the box (and does not quit)
            expect(t, "Message  (Enter sends", 3, "the emptied message box")

        def user_messages():
            return [m["content"] for m in api("GET", f"/api/chats/{cid}")["messages"] if m["role"] == "user"]

        def wait_idle(titled=False):
            """Until this chat's run is over (and, the first time, the model has named it), so a
            repaint check compares a screen that isn't still changing."""
            end = time.time() + 30
            while time.time() < end:
                c = next(c for c in api("GET", "/api/chats") if c["id"] == cid)
                if c["status"] == "idle" and (c["title"].startswith("Titled:") or not titled):
                    break
                t.pump(0.3)
            t.pump(1.0)

        # a small paste goes in as is; the sent message (tabs, ANSI stripped, CRs as line breaks) renders cleanly
        t.paste("\r\n".join(f"\x1b[32m{r}\x1b[0m" for r in rows))
        t.key("enter", 0.5)
        wait_idle(titled=True)
        expect(t, "Titled: row 0:", 5, "the model-written title")
        assert user_messages() == ["\n".join(rows)], user_messages()
        intact(t, "the reply to a message with tabs")

        # large pastes: a token each in the box, the full text on the server
        big = [f"line {i}:\tthe quick brown fox jumps over the lazy dog" for i in range(200)]
        t.paste("\r\n".join(big))
        expect(t, "[Pasted text #1 +200 lines]", 5)
        absent(t, "line 150:")
        t.send(" plus ", 0.3)
        long_line = "x" * 3000
        t.paste(long_line)
        expect(t, "[Pasted text #2, 3000 chars]", 5)
        intact(t, "two large pastes")
        t.key("enter", 0.5)
        wait_idle()
        assert user_messages()[-1] == "\n".join(big) + " plus " + long_line, user_messages()[-1][:200]
        intact(t, "sending a large paste")

        # keystroke pastes (no bracketed paste): every CR is a line break, and it all goes as one message
        n = len(user_messages())
        lines = [f"typed line {i} of a pasted note" for i in range(8)]
        t.send("\r".join(lines), 1.0)
        expect(t, "typed line 7 of a pasted note", 3)
        assert len(user_messages()) == n, "a keystroke paste sent something before Enter"
        absent(t, "Queued")
        intact(t, "a keystroke paste")
        t.key("enter", 0.5)
        wait_idle()
        assert user_messages()[n:] == ["\n".join(lines)], user_messages()[n:]
        long = [f"keyed {i}: lorem ipsum dolor sit amet" for i in range(30)]
        t.send_chunked("\r".join(long), wait=1.0)
        expect(t, "[Pasted text #1 +30 lines]", 3, "a large keystroke paste as a placeholder")
        t.key("enter", 0.5)
        wait_idle()
        assert user_messages()[n + 1:] == ["\n".join(long)], user_messages()[n + 1:]
        # a slow keystroke paste (small pieces with gaps, like a remote terminal) is still one message
        t.send_chunked("\n".join(lines), size=64, gap=0.01, wait=1.0)
        t.key("enter", 0.5)
        wait_idle()
        assert user_messages()[n + 2:] == ["\n".join(lines)], user_messages()[n + 2:]
        intact(t, "the keystroke-paste replies")

        section("slash commands: the menu over the message box, completion, the commands, // and paths as messages")
        before = {c["id"] for c in api("GET", "/api/chats")}

        def row_of(needle):
            return next((y for y, l in enumerate(t.lines()) if needle in l), None)

        def chat(chat_id):
            return api("GET", f"/api/chats/{chat_id}")

        def mine(chat_id):
            return [m["content"] for m in chat(chat_id)["messages"] if m["role"] == "user"]

        def settle(chat_id, n_msgs, timeout=30):
            """Until the chat has more than n_msgs messages and its run is over."""
            end = time.time() + timeout
            while time.time() < end:
                c = next(c for c in api("GET", "/api/chats") if c["id"] == chat_id)
                if c["status"] == "idle" and c["messages"] > n_msgs:
                    break
                t.pump(0.3)
            t.pump(0.6)

        def run(command, wait=0.8):
            """Type a command (the menu may open) and press Enter."""
            t.send(command, 0.4)
            t.key("enter", wait)

        # "/" typed in the box opens the menu right above it; the box keeps the text and the focus
        t.key("/", 0.6)
        expect(t, "Tab complete", 3, "the command menu")
        expect(t, "List every command", 2)
        expect(t, "1/27", 2, "the position counter (more commands than rows)")
        y = row_of("Tab complete")
        assert "┌" in t.lines()[y + 1], f"the message box's top border should follow the menu: {t.lines()[y + 1]!r}"
        assert "│ /  " in t.lines()[y + 2], t.lines()[y + 2]
        # typing filters it; prefixes first
        t.send("re", 0.5)
        expect(t, "/remember <fact>", 3)
        absent(t, "List every command")
        assert row_of("/resume") < row_of("/rename") < row_of("/retry") < row_of("/remember"), "registry order"
        # Esc closes it and keeps the text; typing on reopens it
        t.key("esc", 0.4)
        absent(t, "Tab complete")
        expect(t, "│ /re ", 2, "the text kept in the box")
        t.send("m", 0.5)
        expect(t, "Tab complete", 3)
        absent(t, "/resume")
        # Tab completes (a space after a command that takes arguments) and the menu closes
        t.key("tab", 0.4)
        absent(t, "Tab complete")
        expect(t, "Enter runs /remember", 2, "the hint that Enter runs it")
        t.send("the user likes slash commands", 0.3)
        t.key("enter", 1.0)
        expect(t, ": the user likes slash commands", 5, "the remembered toast")
        facts = [m["fact"] for m in api("GET", "/api/memory")["memories"]]
        assert any("likes slash commands" in f for f in facts), facts

        # /help lists the commands in a panel
        run("/help")
        expect(t, "/compact [instructions]", 3)
        expect(t, "Start a message with //", 2)
        t.key("esc", 0.4)
        absent(t, "Start a message with //")
        t.key("f1", 0.5)
        expect(t, "slash commands (/help lists them", 3, "F1 points at the commands")
        t.key("esc", 0.4)

        # /new: a chat with this chat's agent
        run("/new", 1.2)
        expect(t, "Assistant is ready", 5)
        absent(t, "going for", "the footer of a chat with no messages yet")
        new = [c for c in api("GET", "/api/chats") if c["id"] not in before]
        assert len(new) == 1 and new[0]["agent_id"] == "assistant", new
        cid = new[0]["id"]

        # /rename by Tab completion, checked on the server
        t.send("/ren", 0.5)
        expect(t, "/rename [title]", 3)
        t.key("tab", 0.3)
        t.send("Slash renamed", 0.3)
        t.key("enter", 1.0)
        expect(t, "Renamed to “Slash renamed”", 5)
        assert chat(cid)["title"] == "Slash renamed"

        # /pin
        run("/pin", 1.0)
        expect(t, "⚲Slash renamed", 5, "the pin mark")
        assert next(c for c in api("GET", "/api/chats") if c["id"] == cid)["pinned"]

        def going():
            """The chat footer's running time, in seconds (None when it isn't on screen)."""
            m = next(filter(None, (re.search(r" · going for (\d+)([smhd])(?: (\d+)[mh])?", l) for l in t.lines())), None)
            return m and int(m[1]) * {"s": 1, "m": 60, "h": 3600, "d": 86400}[m[2]] + int(m[3] or 0) * {"s": 0, "m": 1, "h": 60, "d": 3600}[m[2]]

        # two exchanges, then /compact with instructions: the summary request carries them
        for i, text in enumerate(["first point about the API", "second point about the API"]):
            n = len(chat(cid)["messages"])
            t.send(text, 0.3)
            t.key("enter", 0.5)
            settle(cid, n)
        # the footer after the last turn: not compacted yet, and how long the chat has been going,
        # counting up on its own (seconds while it is young: a minute at most to see it move)
        expect(t, "not compacted yet · going for ", 3, "the chat footer")
        asked = row_of("second point about the API")  # (None: scrolled off the top)
        assert (asked or -1) < row_of("going for") and row_of("Done.") < row_of("going for"), "the footer comes after the last turn"
        first = going()
        end = time.time() + 65
        while time.time() < end and going() == first:
            t.pump(0.3)
        assert going() > first, (first, going())
        run("/compact the API design decisions", 0.5)
        end = time.time() + 20
        while time.time() < end and not chat(cid).get("compactions"):
            t.pump(0.3)
        comp = chat(cid).get("compactions") or [{}]
        assert comp[-1].get("instructions") == "the API design decisions", comp
        assert "Kept as asked: the API design decisions" in comp[-1].get("summary", ""), comp[-1].get("summary")
        settle(cid, 0)
        expect(t, "compacted 1× · going for ", 5, "the footer counting the compaction")
        absent(t, "not compacted yet")

        # /context, /usage, /status: panels, not messages
        n_user = len(mine(cid))
        run("/context")
        expect(t, " / 85k (", 3, "the context panel")
        expect(t, "auto-compacts at 70%", 2)
        age = r"compacted 1× · started [^(]+ \(going for \d+[sm]\)"  # a chat made during this run
        expect(t, "compacted 1× · started ", 2, "the chat's age in /context")
        assert re.search(age, t.text()), "\n".join(t.lines())
        t.key("esc", 0.4)
        run("/usage")
        expect(t, " in · ", 3, "the usage panel")
        expect(t, "2 replies", 2)
        expect(t, "33.3 tok/s", 2)
        expect(t, "compacted 1× · started ", 2, "the chat's age in /usage")
        assert re.search(age, t.text()), "\n".join(t.lines())
        t.key("esc", 0.4)
        run("/status", 1.0)
        expect(t, "reachable", 3, "the status panel")
        expect(t, "llama-server", 2)
        expect(t, "Agent Chat " + api("GET", "/api/state")["version"], 2)
        t.key("esc", 0.4)
        assert len(mine(cid)) == n_user, "a command reached the agent"

        # /model lists the default and the server's models; picking one changes only the model
        agent_before = next(a for a in api("GET", "/api/state")["agents"] if a["id"] == "assistant")
        run("/model", 1.0)
        expect(t, "Model for", 3)
        expect(t, "default · mock-model", 2)
        t.key("down", 0.3)
        t.key("enter", 1.0)
        expect(t, "now uses mock-model", 5)
        agent_after = next(a for a in api("GET", "/api/state")["agents"] if a["id"] == "assistant")
        assert agent_after == agent_before | {"model": "mock-model"}, (agent_before, agent_after)
        run("/model default", 1.0)
        expect(t, "uses the default model again", 5)
        assert next(a for a in api("GET", "/api/state")["agents"] if a["id"] == "assistant") == agent_before

        # /theme switches the palette at once (light's background is #f2f3f7), and back
        run("/theme")
        expect(t, "Theme: light", 3)
        bgs = {t.screen.buffer[y][x].bg for y in range(t.rows) for x in range(t.cols)}
        assert "f2f3f7" in bgs, sorted(bgs)[:12]
        run("/theme dark")
        expect(t, "Theme: dark", 3)
        assert "f2f3f7" not in {t.screen.buffer[y][x].bg for y in range(t.rows) for x in range(t.cols)}

        # /export writes to the folder the TUI runs in
        run("/export", 1.0)
        md = os.path.join(workdir, "Slash-renamed.md")
        expect(t, "Saved", 3)
        assert os.path.exists(md), os.listdir(workdir)
        assert open(md).read().startswith("# Slash renamed"), open(md).read()[:80]
        # (its header, written by the server, has the footer's facts too)
        assert re.search(r"^Agent: Assistant · compacted 1× · started [^(]+ \(going for \d+[sm]\)$", open(md).read(), re.M), open(md).read()[:200]
        run("/export json", 1.0)
        assert json.load(open(os.path.join(workdir, "Slash-renamed.json")))["chat"]["title"] == "Slash renamed"

        # an unknown command keeps the text and says so
        run("/nosuch thing")
        expect(t, "Unknown command /nosuch", 3)
        expect(t, "│ /nosuch thing ", 2, "the text kept in the box")
        t.key("ctrl-c", 0.3)
        assert len(mine(cid)) == n_user

        # "//" sends a message starting with "/", and so does a first word that is a path
        for typed, sent in [("//hi there", "/hi there"), ("/etc/hosts x", "/etc/hosts x")]:
            n = len(chat(cid)["messages"])
            run(typed, 0.5)
            settle(cid, n)
            assert mine(cid)[-1] == sent, mine(cid)[-1]

        # /copy hands the last reply to the terminal clipboard (OSC 52)
        mark = len(t.log)
        run("/copy")
        expect(t, "Copied the last reply", 3)
        osc = re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07", t.log[mark:])
        assert osc and base64.b64decode(osc[1]).decode().startswith("Here is an answer with **Markdown**"), osc

        # panels and pickers the other commands open
        for command, shows in [("/resume Slash", "Search and commands"), ("/agents", "Edit Assistant"), ("/memory", "Memory ·"),
                               ("/config", "Model server URL"), ("/routines", "r run now"), ("/delete", "Delete “Slash renamed”?")]:
            run(command, 1.0)
            expect(t, shows, 3, f"what {command} opens")
            t.key("esc", 0.4)
            absent(t, shows)
        assert any(c["id"] == cid for c in api("GET", "/api/chats")), "Esc kept the chat"
        run("/permissions")
        expect(t, "Agents ask before running shell commands", 3)
        run("/edit", 0.6)
        expect(t, "editing your last message", 3)
        expect(t, "│ /etc/hosts x ", 2, "the last message back in the box")
        t.key("ctrl-c", 0.3)

        # while the agent works a command runs at once (never queued); the ones that would
        # change its work say to wait or /stop; /stop stops it
        n = len(chat(cid)["messages"])
        t.send("slow please", 0.2)
        t.key("enter", 0.5)
        expect(t, "● running", 5)
        expect(t, "compacted 1× · going for ", 3, "the footer under the running turn")
        n_user = len(mine(cid))
        run("/usage", 0.6)
        expect(t, "tokens out", 3, "the usage panel mid-reply")
        t.key("esc", 0.3)
        # refused while it works: the text stays in the box to run later (/pin too: the server
        # would answer "chat is busy")
        for refused in ["/compact now", "/pin"]:
            run(refused)
            expect(t, f"then {refused.split()[0]}.", 3, f"{refused} refused mid-reply")
            expect(t, f"│ {refused} ", 2, f"{refused} kept in the box")
            t.key("ctrl-c", 0.3)
            absent(t, f"│ {refused} ")
        assert next(c for c in api("GET", "/api/chats") if c["id"] == cid)["pinned"], "still pinned"
        run("/context")
        expect(t, "auto-compacts at 70%", 3, "the context panel mid-reply")
        t.key("esc", 0.3)
        absent(t, "queued (")
        absent(t, "Queued")
        run("/stop", 1.0)
        expect(t, "stopped here", 10)
        settle(cid, n)
        assert len(mine(cid)) == n_user, "a command was queued as a message"
        t.pump(4.0)  # let the toasts expire, so the repaint check compares a settled screen
        intact(t, "the slash commands")

        section("approvals: y / a / n only in the trace (Alt+key anywhere); typing in the box or the filter is just typing")
        before = {c["id"] for c in api("GET", "/api/chats")}
        t.key("ctrl-k", 0.5)
        t.send("New Coder", 0.6)
        t.key("enter", 1.2)
        expect(t, "Coder is ready", 5)
        coder = next(c["id"] for c in api("GET", "/api/chats") if c["id"] not in before)
        t.send("shell: echo nope", 0.2)
        t.key("enter", 0.5)
        expect(t, "needs approval", 25)
        t.type("no thanks")  # its first letter used to deny, in an empty box
        expect(t, "│ no thanks ", 2, "the words typed in the box")
        expect(t, "needs approval", 1, "the approval still waiting")
        t.key("backtab", 0.3)  # message box -> trace -> sidebar
        t.key("backtab", 0.3)
        t.type("/yan")
        expect(t, "/yan▏", 2, "the letters in the sidebar filter")
        expect(t, "needs approval", 1)
        t.key("esc", 0.3)
        absent(t, "/yan")
        t.key("tab", 0.3)  # sidebar -> trace
        t.type("n")
        expect(t, "echo nope  denied", 10, "the denied command")
        absent(t, "Let the agent do this?")
        expect(t, "tok/s", 20)
        assert any(m["role"] == "tool" and m["content"].startswith("The user denied") for m in chat(coder)["messages"]), chat(coder)["messages"]
        t.key("i", 0.3)
        expect(t, "│ no thanks ", 2, "the box kept its text")
        # Alt+N over a draft denies (rather than starting a new chat)
        t.key("ctrl-c", 0.3)
        n = len(chat(coder)["messages"])
        t.send("shell: echo again", 0.2)
        t.key("enter", 0.5)
        expect(t, "needs approval", 25)
        t.type("draft")
        t.key("alt-n", 0.8)
        absent(t, "New chat with", "the new-chat picker (Alt+N answers a waiting approval first)")
        expect(t, "echo again  denied", 10)
        settle(coder, n + 2)
        t.key("ctrl-c", 0.3)

        section("prompts move their cursor; Ctrl+C in a dialog closes it and keeps the app")
        t.key("f2", 0.5)
        expect(t, "Rename chat", 3)
        t.key("home", 0.2)
        t.type("Re: ")
        t.key("end", 0.2)
        t.type("!")
        expect(t, "│ Re: ", 3, "text typed at the start")
        assert any(l.startswith("│ Re: ", l.find("│ Re: ")) and "echo nope!" in l for l in t.lines()), "and at the end"
        t.key("ctrl-c", 0.4)
        absent(t, "Rename chat")
        t.key("alt-s", 1.0)
        expect(t, "Model server URL", 5)
        t.type("zz")
        t.key("ctrl-k", 0.4)
        expect(t, "Model server URL", 2, "the form (Ctrl+K would have thrown its edits away)")
        t.key("ctrl-c", 0.6)
        absent(t, "Model server URL")
        t.pump(0.5)
        assert os.waitpid(t.pid, os.WNOHANG) == (0, 0), "Ctrl+C in a form quit the app"
        assert api("GET", "/api/state")["settings"]["base_url"] == MOCK, "the form was saved"

        section("memory: d asks before forgetting, and the panel comes back as it was")
        fact = f"tui forget-me fact {stamp}"
        api("POST", "/api/memory", {"fact": fact})
        t.key("alt-m", 1.0)
        expect(t, "Memory ·", 5)
        t.type("/")
        t.send("forget-me", 0.8)
        t.key("enter", 0.3)
        expect(t, fact, 3)
        t.type("d")
        expect(t, "Forget “tui forget-me", 3, "the question")
        t.key("esc", 0.6)
        expect(t, fact, 3, "the panel back, the fact kept")
        assert any(m["fact"] == fact for m in api("GET", "/api/memory?q=forget-me")["memories"])
        t.type("d")
        t.key("enter", 1.0)
        expect(t, "Memory ·", 3)
        absent(t, fact)
        assert not any(m["fact"] == fact for m in api("GET", "/api/memory?q=forget-me")["memories"])
        t.key("esc", 0.4)

        section("Calendars: the palette finds it by name, typing starts in Name, Enter adds")
        for c in api("GET", "/api/calendars"):
            if c["name"] in ("TuiCal", "Existing"):  # left by an earlier run
                api("DELETE", f"/api/calendars/{c['id']}")
        others = {c["id"] for c in api("GET", "/api/calendars")}  # other suites' calendars stay as they are
        api("POST", "/api/calendars", {"name": "Existing", "url": "https://example.com/existing.ics"})
        ics = os.path.join(workdir, "tui-test.ics")
        with open(ics, "w") as f:
            f.write("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//t//EN\r\nEND:VCALENDAR\r\n")
        t.key("ctrl-k", 0.5)
        t.send("Calendars", 0.8)
        t.key("enter", 1.0)
        expect(t, "iCal link or .ics path", 3, "the Calendars form (not Settings)")
        t.type("TuiCal")
        t.key("enter", 0.4)  # from the name on to the link
        t.send(ics, 0.4)
        t.key("enter", 1.5)
        expect(t, "Disconnect TuiCal", 5, "the calendar added (the form reopens listing it)")
        cals = [c for c in api("GET", "/api/calendars") if c["id"] not in others]
        assert sorted(c["name"] for c in cals) == ["Existing", "TuiCal"], cals
        t.key("esc", 0.4)
        for c in cals:
            api("DELETE", f"/api/calendars/{c['id']}")

        section("one stream per visit: switching away and back doesn't double the reply")
        a_id = new_chat_titled(f"Stream A {stamp}")
        b_id = new_chat_titled(f"Stream B {stamp}")
        for title in ["Stream A", "Stream B", "Stream A", "Stream B", "Stream A"]:
            t.key("ctrl-k", 0.4)
            t.send(f"{title} {stamp}", 0.8)
            t.key("enter", 0.8)
        expect(t, f"Stream A {stamp}", 3)
        n = len(chat(a_id)["messages"])
        t.send("slow please", 0.2)
        t.key("enter", 0.3)
        expect(t, "word3 ", 10)
        garbled, end = [], time.time() + 4
        while time.time() < end:  # each word once, in order (a second stream would apply each delta twice)
            t.pump(0.15)
            nums = [int(x) for x in re.findall(r"word(\d+)", t.text())][:-1]  # (the last may be half streamed)
            garbled += [(a, b) for a, b in zip(nums, nums[1:]) if b != a + 1]
        assert not garbled, f"the reply's words out of order: {garbled[:5]}"
        streams = open_streams(t.pid)
        # the event stream, this chat's stream and maybe one pooled idle request connection
        assert streams is None or streams <= 3, f"{streams} connections open to the app"
        t.key("ctrl-s", 0.5)
        settle(a_id, n)

        section("a chat deleted elsewhere closes; the welcome page's numbers start chats; nothing typed leaks")
        api("DELETE", f"/api/chats/{a_id}")
        expect(t, "That chat no longer exists.", 8)
        expect(t, "Pick an agent.", 3)
        t.type("zz")  # with no chat open, no message box takes this
        t.type("2")
        expect(t, "Mail is ready", 5, "a chat with the second agent")
        expect(t, "Message  (Enter sends", 2, "an empty message box")
        api("DELETE", f"/api/chats/{b_id}")

        section("quits cleanly")
        t.key("ctrl-q", 0.8)
        _, status = os.waitpid(t.pid, 0)
        assert os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0, f"exit status {status}"
        t.pid = None

        section("/quit quits too")
        q = Tui(BIN, B, cols=110, rows=32, extra=("--chat", cid), cwd=workdir)
        try:
            expect(q, "Slash renamed", 10)
            # (a chat opened with --chat starts with its message box focused)
            q.key("/", 0.4)
            q.send("qui", 0.4)
            expect(q, "/quit", 3)
            q.key("enter", 0.8)
            _, status = os.waitpid(q.pid, 0)
            assert os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0, f"exit status {status}"
            q.pid = None
        finally:
            if q.pid:
                print("\n".join(q.lines()))
                q.close()
        section("Ctrl+Q handed the terminal back: main screen, no mouse capture or bracketed paste, a cursor")
        for seq in (b"\x1b[?1049l", b"\x1b[?1000l", b"\x1b[?2004l", b"\x1b[?25h"):
            assert seq in t.log, f"{seq!r} not written on the way out"

        section("--chat with a chat that doesn't exist: one notice, then the welcome page")
        q = Tui(BIN, B, cols=110, rows=32, extra=("--chat", "no-such-chat"), cwd=workdir)
        try:
            expect(q, "That chat no longer exists.", 8)
            expect(q, "Pick an agent.", 3)
            q.pump(4.5)  # the old client retried every 2 s, a toast each time
            absent(q, "×2", "a repeated toast")
            absent(q, "Lost the chat stream")
        finally:
            q.close()

        section("below 90 columns: clicks hit what is drawn, the drawer shows while focused, nothing types into hidden panes")
        a_id = new_chat_titled(f"Narrow A {stamp[-4:]}")
        b_id = new_chat_titled(f"Narrow B {stamp[-4:]}")  # newer: listed right above A, in view with it
        q = Tui(BIN, B, cols=80, rows=30, extra=("--chat", a_id), cwd=workdir)
        try:
            na, nb = f"Narrow A {stamp[-4:]}", f"Narrow B {stamp[-4:]}"
            expect(q, na, 10)
            q.key("backtab", 0.3)  # (--chat starts in the message box) box -> trace -> sidebar
            q.key("backtab", 0.4)
            expect(q, nb, 3, "chat B in the sidebar (it has the focus, so it shows)")
            row = next(y for y, l in enumerate(q.lines()) if nb in l)
            q.key("tab", 0.4)  # to the trace: the sidebar hides
            absent(q, "Agent Chat")
            q.click(5, row, 1.0)  # (and the click focuses the message box)
            assert na in q.lines()[0], f"a click where the hidden sidebar's row was opened it: {q.lines()[0]!r}"
            q.key("alt-f", 0.8)
            expect(q, "Workspace", 3, "the drawer, shown while it has the focus")
            q.key("esc", 0.4)
            absent(q, "Workspace")
            q.type("abc")
            expect(q, "│ abc ", 2, "typing in the message box")
            q.key("alt-f", 0.5)  # (hidden again for the next ones)
        finally:
            q.close()

        section("a drawer left open last time lists its folder at start; ↻ refresh and a vanished folder")
        xdg = tempfile.mkdtemp(prefix="agent-chat-tui-xdg-")
        os.makedirs(os.path.join(xdg, "agent-chat-tui"))
        with open(os.path.join(xdg, "agent-chat-tui", "state.json"), "w") as f:
            json.dump({"files": True, "filter": "ghost-agent"}, f)
        ws = api("GET", "/api/workspace?agent_id=coder&path=.")["workspace"]
        q = Tui(BIN, B, cols=120, rows=40, extra=("--chat", coder), cwd=workdir, env={"XDG_CONFIG_HOME": xdg})
        try:
            expect(q, "greet.py", 10, "the restored drawer's listing")
            absent(q, "Empty folder.")
            expect(q, " Chats ", 2, "a saved filter for an agent that is gone falls back to every chat")
            q.key("tab", 0.3)  # (--chat starts in the message box) box -> drawer
            fresh, gone = f"new{stamp[-6:]}.txt", f"gone{stamp[-6:]}"  # short enough for the drawer
            open(os.path.join(ws, fresh), "w").write("new\n")
            os.makedirs(os.path.join(ws, gone))
            q.key("home", 0.2)
            q.key("enter", 1.0)  # the ↻ refresh row
            expect(q, fresh, 3, "the new file after ↻ refresh")
            expect(q, gone, 2)
            idx = next(i for i, e in enumerate(api("GET", "/api/workspace?agent_id=coder&path=.")["entries"]) if e["name"] == gone)
            for _ in range(idx + 1):
                q.key("down", 0.1)
            q.key("enter", 1.0)
            expect(q, f" /{gone}", 3, "inside the folder")
            os.rmdir(os.path.join(ws, gone))
            q.key("r", 1.0)
            expect(q, "greet.py", 3, "back at the top, listed")
            absent(q, gone)
            absent(q, "404")
        finally:
            q.close()

        section("UTF-8 split across network reads arrives intact")
        wide = new_chat_titled("Wide text")
        text = "中文—…🎉 " * 3000  # ~50 KB of multi-byte characters in the snapshot
        api("POST", f"/api/chats/{wide}/run", {"content": text})
        end = time.time() + 30
        while time.time() < end and next(c for c in api("GET", "/api/chats") if c["id"] == wide)["status"] != "idle":
            time.sleep(0.3)
        q = Tui(BIN, B, cols=120, rows=40, extra=("--chat", wide), cwd=workdir)
        try:
            expect(q, "🎉", 10)  # (pyte shows a wide character with a blank cell after it)
            q.key("backtab", 0.3)  # (--chat starts in the message box) to the trace
            for _ in range(10):
                assert "\ufffd" not in q.text(), "a character broken where a read ended"
                q.key("pgup", 0.25)
        finally:
            q.close()

        section("emoji that terminals measure differently keep the frame (VS16, ZWJ, skin tones, flags, keycaps), icons line names up; a wide title keeps the header's stats; resize; 90-95 columns with both panes; the help scrolls")
        fragile = "✍️ ❤️ 👨‍👩‍👧 🧑🏽‍💻 👋🏽 🇦🇪 1️⃣ #️⃣ 🏳️‍🌈 ☀️ 🏴󠁧󠁢󠁥󠁮󠁧󠁿"
        have = {a["name"]: a["id"] for a in api("GET", "/api/state")["agents"]}
        icons = {"Quill": "✍️", "Kin": "👨‍👩‍👧", "Wave": "👋🏽", "Flag": "🇦🇪", "Key": "1️⃣"}  # (made once, kept for later runs)
        for name, emo in icons.items():
            if name not in have:
                have[name] = api("POST", "/api/agents", {"name": name, "emoji": emo, "purpose": f"{emo} a {name} test {emo}"})["id"]
        emoji_title = f"✍️ ❤️ {stamp[-4:]} 👨‍👩‍👧 🇦🇪 x"
        for aid in [have[n] for n in icons] + ["writer"]:
            new_chat_titled(emoji_title, aid)  # the Writer's ✍️ (app/defaults.py) among them
        wide = new_chat_titled("测试" * 40 + f" {stamp[-4:]} ✍️", have["Quill"])
        api("POST", f"/api/chats/{wide}/run", {"content": (f"emoji row: {fragile} end\n") * 6})
        end = time.time() + 30
        while time.time() < end and next(c for c in api("GET", "/api/chats") if c["id"] == wide)["status"] != "idle":
            time.sleep(0.3)
        xdg = tempfile.mkdtemp(prefix="agent-chat-tui-xdg-")
        q = Tui(BIN, B, cols=120, rows=40, cwd=workdir, env={"XDG_CONFIG_HOME": xdg})
        try:
            expect(q, "Quill", 10)
            # every agent's name starts in one column, whichever icon is before it
            at = {n: next(l.index(n) for l in q.lines() if n in l[:30]) for n in ["Writer", "Coder", *icons]}
            assert len(set(at.values())) == 1, f"agent names out of line: {at}"
            intact(q, "the welcome page and sidebar with emoji icons and titles")
            q.key("ctrl-k", 0.5)
            q.send("测试测试", 1.0)
            q.key("enter", 1.5)
            expect(q, "emoji row:", 5, "the message full of emoji")
            intact(q, "opening a chat full of emoji")
            row0 = q.lines()[0]
            assert "测" in row0 and "ctx" in row0 and "tok/s" in row0, f"a wide title pushed the header's stats off: {row0!r}"
            q.key("backtab", 0.3)  # message box -> trace
            q.key("pgup", 0.4)
            q.key("pgdn", 0.4)
            intact(q, "scrolling over emoji")
            q.key("backtab", 0.3)  # -> sidebar: walk the emoji titled chats, opening each
            for _ in range(4):
                q.key("j", 0.2)
                q.key("enter", 0.8)
            intact(q, "switching between emoji titled chats")
            q.key("ctrl-k", 0.5)
            q.send("测试测试", 1.0)
            q.key("enter", 1.5)
            q.paste(fragile * 2)  # the message box draws what it holds
            expect(q, "AE 1", 3, "the pasted flag and keycap, drawn plainly")
            intact(q, "emoji typed into the message box")
            q.key("ctrl-c", 0.4)

            # a resize re-wraps: below 90 the sidebar goes; back at 120 the frame is whole
            q.resize(40, 70)
            absent(q, "Agent Chat", "the sidebar at 70 columns")
            assert "ctx" in q.lines()[0] and q.find("Message  (Enter sends"), "\n".join(q.lines())
            q.resize(40, 120)
            expect(q, "emoji row:", 3)
            intact(q, "a resize to 70 columns and back")

            # 90-95 columns with the sidebar and the drawer: the trace keeps 30, nothing overlaps
            q.key("alt-f", 0.8)
            for cols in (90, 91, 92, 95):
                q.resize(40, cols)
                expect(q, "Workspace", 3, f"the drawer at {cols} columns")
                files = min(32, cols - 60)
                lines = q.display()
                bad = [y for y, l in enumerate(lines) if l[29] != "│" or l[cols - files] != "│"]
                assert not bad, f"a pane ran over its separator at {cols} columns, rows {bad}:\n" + "\n".join(lines)
                assert "测" in lines[0][30:cols - files], f"the header at {cols}: {lines[0]!r}"
                intact(q, f"the three panes at {cols} columns")
            q.key("alt-f", 0.5)

            # 24 rows: the shortcuts scroll instead of losing their last rows
            q.resize(24, 100)
            q.key("f1", 0.5)
            expect(q, "Keyboard shortcuts", 3)
            expect(q, "scroll (1–", 2, "a scroll hint on a short screen")
            absent(q, "Ctrl+Q")
            intact(q, "the shortcuts on a short screen")  # (scrolled to the end, a taller screen would show more)
            q.key("end", 0.4)
            expect(q, "Ctrl+Q", 2, "the last row, scrolled to")
            expect(q, "m mark all read", 2, "a long row, wrapped whole")
            q.key("esc", 0.4)
            absent(q, "Keyboard shortcuts")
            # back to 40 rows by way of 41. The 24-row screen scrolled the chat list down to its
            # selection; when that leaves the list at its end (few chats, as with fresh data), a
            # taller screen shows one more chat above, and shrinking back doesn't undo that (the
            # list only scrolls to keep the selection in view). The repaint check grows the screen
            # a row, so coming straight back it would see the list one row off, which is no damage.
            q.resize(41, 120)
            q.resize(40, 120)
            intact(q, "back at 120x40")
        finally:
            q.close()

        print("\nall TUI tests passed")
    except BaseException:
        print("\n--- screen at failure ---")
        print("\n".join(t.lines()))
        raise
    finally:
        if t.pid:
            t.close()


if __name__ == "__main__":
    main()
