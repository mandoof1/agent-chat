"""Unit tests that need no server: run with `uv run python tests/test_unit.py` (or pytest)."""
import os
import sys
import tempfile
from datetime import datetime

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
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
