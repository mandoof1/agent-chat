"""Unit tests that need no server: run with `uv run python tests/test_unit.py` (or pytest)."""
import asyncio
import os
import re
import sys
import tempfile
from datetime import datetime

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)
os.environ.setdefault("AGENT_CHAT_DATA", tempfile.mkdtemp(prefix="ac-unit-"))

from app import history, memory, routines, store, tools  # noqa: E402
from app.history import QUEUED_NOTE  # noqa: E402


def test_to_llm_strips_private_fields_and_labels_queued():
    msgs = [{"role": "user", "content": "task", "_ts": 1},
            {"role": "assistant", "content": "", "tool_calls": [], "_stats": {}},
            {"role": "user", "content": "aside", "_queued": True, "_recall": "- [1] fact"},
            {"role": "user", "content": "new turn"}]
    out = history.to_llm(msgs)
    assert out[0] == {"role": "user", "content": "task"}
    assert out[2]["content"].startswith(QUEUED_NOTE + "\naside\n\n[From your memory")
    assert all(not k.startswith("_") for m in out for k in m)


def test_to_llm_images_become_parts_only_when_asked():
    msgs = [{"role": "user", "content": "look", "_images": ["uploads/a.png"]}]
    assert history.to_llm(msgs)[0]["content"] == "look"
    parts = history.to_llm(msgs, images=True)[0]["content"]
    assert parts[0] == {"type": "text", "text": "look"}
    assert parts[1]["image_url"]["url"] == "attach://uploads/a.png"


def test_old_thinking_is_dropped_but_current_turn_kept():
    msgs = [{"role": "user", "content": "a"},
            {"role": "assistant", "content": "x", "reasoning_content": "old"},
            {"role": "user", "content": "b"},
            {"role": "assistant", "content": "", "reasoning_content": "new", "tool_calls": [{"id": "1", "type": "function", "function": {"name": "f", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "1", "content": "r"}]
    out = history.to_llm(msgs)
    assert "reasoning_content" not in out[1] and out[3]["reasoning_content"] == "new"


def test_cutoff_replies_collapse_into_one_continuation():
    msgs = [{"role": "user", "content": "go"},
            {"role": "assistant", "content": "p1", "reasoning_content": "r1", "_truncated": True},
            {"role": "assistant", "content": "p2", "reasoning_content": "r2", "_truncated": True}]
    out = history.to_llm(msgs)
    assert len(out) == 2 and out[1]["role"] == "user" and "r2" in out[1]["content"] and "r1" not in out[1]["content"]


def test_repair_answers_orphan_tool_calls():
    msgs = [{"role": "assistant", "content": "", "tool_calls": [{"id": "a", "type": "function", "function": {"name": "f", "arguments": "{}"}},
                                                                {"id": "b", "type": "function", "function": {"name": "f", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "a", "content": "ok"}]
    history.repair(msgs)
    assert [m.get("tool_call_id") for m in msgs[1:]] == ["a", "b"] and "cancelled" in msgs[2]["content"]


def test_choose_cut_keeps_tool_results_with_their_call():
    msgs = [{"role": "user", "content": "x" * 100}, {"role": "assistant", "content": "", "tool_calls": [{"id": "1"}]},
            {"role": "tool", "tool_call_id": "1", "content": "y" * 100}, {"role": "assistant", "content": "z" * 100},
            {"role": "user", "content": "q"}, {"role": "assistant", "content": "w" * 100}]
    cut = history.choose_cut(msgs, 0, keep_chars=250)
    assert cut is not None and msgs[cut]["role"] != "tool"
    assert history.choose_cut(msgs, 0, None) == 4  # manual: from the last user message
    assert history.choose_cut(msgs[:2], 0, None) is None


def test_overflow_detection():
    assert history.is_overflow(Exception("the request exceeds the available context size"))
    assert not history.is_overflow(Exception("connection refused"))


def test_partial_args_in_js_matches_python_json():  # sanity for the same JSON shapes the UI parses
    import json
    assert json.loads('{"path": "a.py", "content": "x"}')["path"] == "a.py"


def test_next_run_schedule_math():
    thu = datetime(2026, 10, 1, 9, 0)
    wk = {"type": "daily", "time": "08:00", "days": [0, 1, 2, 3, 4]}
    assert datetime.fromtimestamp(routines.next_run(wk, thu)).strftime("%a %H:%M") == "Fri 08:00"
    fri = datetime(2026, 10, 2, 9, 0)
    assert datetime.fromtimestamp(routines.next_run(wk, fri)).strftime("%a %H:%M") == "Mon 08:00"
    assert datetime.fromtimestamp(routines.next_run({"type": "interval", "minutes": 90}, thu)).strftime("%H:%M") == "10:30"


def test_routine_validation():
    store.seed_defaults()
    for bad in ({"name": "x", "agent_id": "assistant", "prompt": "p", "schedule": {"type": "daily", "time": "25:00"}},
                {"name": "x", "agent_id": "assistant", "prompt": "p", "schedule": {"type": "interval", "minutes": 1}},
                {"name": "", "agent_id": "assistant", "prompt": "p", "schedule": {"type": "interval", "minutes": 10}}):
        try:
            routines.validate(bad)
        except ValueError:
            continue
        raise AssertionError(bad)
    ok = routines.validate({"name": "x", "agent_id": "assistant", "prompt": "p", "schedule": {"type": "daily", "time": "08:30", "days": [6, 0]}})
    assert ok["schedule"]["days"] == [0, 6]


def test_memory_similarity_and_dedup():
    assert memory._similarity("The user prefers short answers", "User prefers short answers.") >= 0.6
    assert memory._similarity("The user has a cat", "The user prefers tea") < 0.6
    m1, a1 = memory.add("The user drinks green tea every morning", "preference", 5)
    m2, a2 = memory.add("The user drinks green tea every morning!", "preference", 7)
    assert a1 == "added" and a2 == "merged" and m1["id"] == m2["id"] and m2["importance"] == 7
    memory.delete(m1["id"])


def test_store_ids_and_workspace_sandbox():
    assert store.get_chat("../../etc/passwd") is None and store.get_agent("x/y") is None
    ws = tools.workspace_for({"workspace": tempfile.mkdtemp(prefix="ac-ws-")})
    try:
        tools._resolve(ws, "../outside")
    except tools.ToolError:
        pass
    else:
        raise AssertionError("escape allowed")
    assert tools._resolve(ws, "sub/../file.txt") == ws / "file.txt"


def test_fork_prunes_compactions_and_threads():
    chat = {"id": "orig", "title": "T", "agent_id": "assistant", "created": 0, "updated": 0, "pinned": True,
            "messages": [{"role": "user", "content": "a"}, {"role": "assistant", "content": "b"}, {"role": "user", "content": "c"}],
            "compactions": [{"upto": 2, "summary": "s"}, {"upto": 3, "summary": "t"}], "threads": {"x>y": "sub"}, "subchats": ["sub"]}
    fork = store.fork_chat(chat, 2)
    assert len(fork["messages"]) == 2 and fork["compactions"] == [{"upto": 2, "summary": "s"}]
    assert "threads" not in fork and "subchats" not in fork and not fork["pinned"] and fork["title"] == "T (branch)"
    store.delete_chat(fork["id"])


def test_agent_order():
    store.seed_defaults()
    ids = [a["id"] for a in store.list_agents()]
    assert ids[:3] == ["assistant", "mail", "planner"]
    store.reorder_agents(["writer", "coder"])
    assert [a["id"] for a in store.list_agents()][:2] == ["writer", "coder"]
    store.reorder_agents(ids)
    assert [a["id"] for a in store.list_agents()] == ids


def test_chat_age_in_markdown_export():  # the web footer's two facts: e2e_ui checks the same spans in the browser
    from app import main
    for secs, want in ((-5, "0s"), (0, "0s"), (45, "45s"), (59.9, "59s"), (60, "1m"), (719, "11m"), (3599, "59m"),
                       (3600, "1h 0m"), (11520, "3h 12m"), (86399, "23h 59m"), (86400, "1d 0h"), (187200, "2d 4h")):
        assert main._span(secs) == want, (secs, main._span(secs))
    t0 = datetime(2026, 10, 10, 9, 14).timestamp()
    assert main._chat_age({"created": t0, "compactions": [{}, {}]}, t0 + 11520) == "compacted 2× · started 2026-10-10 09:14 (going for 3h 12m)"
    assert main._chat_age({"created": t0, "compactions": []}, t0 + 240) == "not compacted yet · started 2026-10-10 09:14 (going for 4m)"
    store.seed_defaults()
    chat = store.create_chat("assistant")
    chat.update(messages=[{"role": "user", "content": "a"}, {"role": "assistant", "content": "b"}, {"role": "user", "content": "c"}],
                compactions=[{"upto": 2, "summary": "s"}])
    store.save_chat(chat)
    md = asyncio.run(main.export_chat(chat["id"])).body.decode()
    assert re.match(r"# New chat\n\nAgent: Assistant · compacted 1× · started \d{4}-\d\d-\d\d \d\d:\d\d \(going for \d+s\)\n\n## You", md), md[:160]
    store.delete_chat(chat["id"])


def test_downloads_named_in_any_script_and_text_in_any_shape():
    from urllib.parse import unquote

    from app import main
    for name in ("محادثة-日本語-Ελληνικά.md", 'a "quoted" \\ name.json', "plain.md"):
        cd = main._attachment(name)
        cd.encode("latin-1")  # a header value must be Latin-1, or the response dies (/export of a chat titled 日本語 was a 500)
        plain = cd.split('filename="')[1].split('"; ')[0]
        assert plain.isascii() and '"' not in plain and "\\" not in plain and len(plain) == len(name), cd
        assert unquote(cd.split("filename*=UTF-8''")[1]) == name, cd
    store.seed_defaults()
    chat = store.create_chat("assistant")
    chat["title"] = "Exp 日本語"
    store.save_chat(chat)
    md = asyncio.run(main.export_chat(chat["id"]))
    assert md.headers["content-disposition"].endswith("filename*=UTF-8''Exp-%E6%97%A5%E6%9C%AC%E8%AA%9E.md"), md.headers
    assert md.body.decode().startswith("# Exp 日本語")
    store.delete_chat(chat["id"])
    # a file name that isn't UTF-8 (Python keeps the bytes as surrogate escapes) is listed, not a 500
    assert main._shown(os.fsdecode(b"bad\xff\xfe name.txt")) == "bad\ufffd\ufffd name.txt"
    # lone UTF-16 surrogates ("\\ud800" alone is valid JSON) become U+FFFD in every request body, nested too
    assert main.RunIn.model_validate({"content": "a \ud800 b \udfff 😀"}).content == "a \ufffd b \ufffd 😀"
    agent = main.AgentIn.model_validate({"name": "x\udc00", "tools": ["t\ud800"]})
    assert (agent.name, agent.tools) == ("x\ufffd", ["t\ufffd"])
    assert main.RoutineIn.model_validate({"schedule": {"k\ud800": "v\udbff"}}).schedule == {"k\ufffd": "v\ufffd"}


def _registry(path, start):
    """A client's slash-command registry as data: the lines between `start` and the closing `];`, one entry each."""
    block = open(os.path.join(ROOT, path), encoding="utf-8").read().split(start, 1)[1].split("\n];", 1)[0]
    out = []
    for line in block.splitlines():
        line = line.strip()
        if not line or line.startswith("//"):
            continue
        assert re.match(r"(Cmd )?\{ name: \"\w+\", .*\},$", line) and line.count('{ name: "') == 1, f"{path}: not one entry per line: {line}"
        head = line.split(", desc: ", 1)[0]  # the description and run function differ between the clients
        flag = lambda k: (re.search(rf"\b{k}: (true|false)\b", head) or [None, "false"])[1] == "true"
        aliases = re.search(r"\baliases: &?\[([^\]]*)\]", head)
        args = re.search(r'\bargs: "([^"]*)"', head)
        out.append({"name": re.search(r'\bname: "(\w+)"', head)[1], "aliases": re.findall(r'"(\w+)"', aliases[1]) if aliases else [],
                    "args": args[1] if args else "", "chat": flag("chat"), "idle": flag("idle")})
    return out


def test_slash_registries_match():  # static/js/commands.js and tui/src/commands.rs list the same commands
    web = _registry("static/js/commands.js", "export const COMMANDS = [")
    tui = _registry("tui/src/commands.rs", "pub const COMMANDS: &[Cmd] = &[")
    assert [c for c in tui if c["name"] == "quit"] == [{"name": "quit", "aliases": ["exit"], "args": "", "chat": False, "idle": False}]
    tui = [c for c in tui if c["name"] != "quit"]  # only a terminal has something to quit
    for c in web + tui:
        names = [c["name"], *c["aliases"]]
        assert "quit" not in names and "exit" not in names, c
        if c["name"] == "theme":  # each client lists its own themes (web auto/light/dark, TUI dark/light/plain)
            assert re.fullmatch(r"\[\w+(\|\w+)+\]", c["args"]), c
            c["args"] = "[themes]"
    assert [c["name"] for c in web] == [c["name"] for c in tui], ([c["name"] for c in web], [c["name"] for c in tui])
    diffs = [f"/{w['name']} {k}: web {w[k]!r}, tui {t[k]!r}" for w, t in zip(web, tui) for k in w if w[k] != t[k]]
    assert not diffs, "; ".join(diffs)
    every = [n for c in web for n in (c["name"], *c["aliases"])]
    assert len(every) == len(set(every)) and len(web) == 26, every


if __name__ == "__main__":
    tests = [(n, f) for n, f in sorted(globals().items()) if n.startswith("test_") and callable(f)]
    failed = 0
    for name, fn in tests:
        try:
            fn()
            print(f"  ok   {name}")
        except Exception as e:  # noqa: BLE001
            failed += 1
            print(f"  FAIL {name}: {type(e).__name__}: {e}")
    print(f"\n{len(tests) - failed}/{len(tests)} unit tests passed")
    sys.exit(1 if failed else 0)
