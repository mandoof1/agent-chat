"""The Rust TUI, driven in a pseudo-terminal against the app on AC_URL (default :8767) wired to
tests/mock_llm.py. Needs the binary (TUI_BIN, default tui/target/release/agent-chat-tui; falls back
to the debug build) and pyte: `uv run --with pyte python tests/e2e_tui.py`.

Covers: the welcome page, starting chats, a streamed reply rendered as a trace, a handoff with an
approval (approve, approve-all), live shell output, verbose details, the files drawer and viewer,
the command palette, memory, routines, settings, the agent editor, rename, pin, branch, edit-last,
stop, queued messages, attachments, help, and quitting cleanly.
"""
import os
import sys
import tempfile
import time

import httpx

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tui_driver import Tui  # noqa: E402

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")
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


def main():
    assert BIN, "build the TUI first: cd tui && cargo build --release"
    api("PUT", "/api/settings", {"base_url": "http://127.0.0.1:8766/v1", "bypass_approvals": False, "auto_title": True, "auto_memory": False})
    # stop anything left waiting from other tests, so the sidebar status words are ours
    for c in api("GET", "/api/chats"):
        if c["status"] != "idle":
            api("POST", f"/api/chats/{c['id']}/stop")
    state_file = os.path.join(os.environ.get("XDG_CONFIG_HOME", os.path.expanduser("~/.config")), "agent-chat-tui", "state.json")
    if os.path.exists(state_file):
        os.remove(state_file)

    t = Tui(BIN, B, cols=120, rows=40)
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

        section("handoff with an approval: y approves, the nested trace finishes")
        t.key("ctrl-k", 0.5)
        t.send("New Orchestrator", 0.6)
        t.key("enter", 1.2)
        expect(t, "Orchestrator is ready", 5)
        t.send("please delegate this", 0.2)
        t.key("enter", 0.5)
        expect(t, "needs approval", 25)
        expect(t, "handoff", 2)
        expect(t, "y approve", 2)
        expect(t, "needs you", 3, "the sidebar status word")
        t.send("y", 0.5)
        expect(t, "│ ● run_shell", 20, "the approved command marked done")
        expect(t, "hello from coder", 20)
        expect(t, "│ ● Finished.", 25, "the sub-agent's reply")

        section("approve all for the run covers the second command; live shell output streams")
        t.key("ctrl-k", 0.5)
        t.send("New Coder", 0.6)
        t.key("enter", 1.2)
        expect(t, "Coder is ready", 5)
        t.send("shell twice: echo one", 0.2)
        t.key("enter", 0.5)
        expect(t, "needs approval", 25)
        t.send("a", 0.5)
        expect(t, "auto-approved", 25)
        expect(t, "one again", 25)
        t.send("slow shell", 0.2)
        t.key("enter", 0.5)
        expect(t, "needs approval", 25)
        t.send("y", 0.5)
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
        # move to greet.py (rows: refresh, uploads, greet.py, notes.txt) and open it
        t.key("down"); t.key("down")
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
        expect(t, "8766", 2)
        t.key("esc", 0.4)
        t.key("alt-a", 1.0)
        expect(t, "Edit Assistant", 5)
        expect(t, "Instructions", 2)
        t.key("esc", 0.4)
        t.key("f1", 0.5)
        expect(t, "Keyboard shortcuts", 3)
        t.key("esc", 0.4)

        section("quits cleanly")
        t.key("ctrl-q", 0.8)
        _, status = os.waitpid(t.pid, 0)
        assert os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0, f"exit status {status}"
        t.pid = None
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
