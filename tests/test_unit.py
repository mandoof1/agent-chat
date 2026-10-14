"""Unit tests that need no server: run with `uv run python tests/test_unit.py` (or pytest)."""
import asyncio
import json
import os
import re
import sys
import tempfile
from datetime import datetime

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)
os.environ.setdefault("AGENT_CHAT_DATA", tempfile.mkdtemp(prefix="ac-unit-"))

from app import history, loops, memory, routines, store, tools  # noqa: E402
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


def test_loop_guard_stops_repetition_not_length():
    passage = ("Plan: build the board with three columns, wire the drag handlers between them, save every change "
               "to localStorage, then open it in a browser and check that a reload keeps the cards in place. ")
    looping = "First some honest thinking about the task.\n\n" + passage * 20
    rep = loops.Repetition()
    try:
        for i in range(0, len(looping), 7):
            rep.feed("reasoning", looping[i:i + 7])
        raise AssertionError("the loop went unnoticed")
    except loops.Looping as e:
        info = e.info
    seen = rep.text["reasoning"]
    assert len(seen) < len(looping) and info["channel"] == "reasoning" and info["repeats"] >= loops.REPEATS
    assert seen[:info["keep"]].count(passage) == 1 and seen[:info["keep"]].startswith("First some honest"), info
    # long, but always saying something new (numbered steps, code with the same shape): never stopped
    long = "\n".join(f"Step {i}: check option {i} against the {i % 7}th constraint, then note what it costs." for i in range(3000))
    code = "\n".join(f"    <li class=\"item\"><a href=\"#s{i}\">Section {i}</a></li>" for i in range(500))
    rule = "=" * 5000 + "0" * 5000  # few distinct characters: a rule, a run of zeros
    for text in (long, code, rule):
        rep = loops.Repetition()
        for i in range(0, len(text), 50):
            rep.feed("content", text[i:i + 50])
    rep.feed("tool_args", passage * 20)  # tool arguments (a file's content) are not watched


def test_loop_note_replaces_the_looping_reply():
    passage = "the same plan again and again " * 4
    m = {"role": "assistant", "content": "", "reasoning_content": "intro. " + passage * 6,
         "_looped": {"channel": "reasoning", "repeats": 6, "keep": len("intro. " + passage)}}
    out = history.to_llm([{"role": "user", "content": "go"}, m])
    assert len(out) == 2 and out[1]["role"] == "user" and "stuck in a loop" in out[1]["content"]
    assert out[1]["content"].count(passage) == 1 and "intro." in out[1]["content"]


def test_repeated_tool_calls_are_flagged_from_the_third():
    def call(i, args='{"path": "."}'):
        return [{"role": "assistant", "content": "", "tool_calls": [{"id": f"c{i}", "type": "function", "function": {"name": "list_dir", "arguments": args}}]},
                {"role": "tool", "tool_call_id": f"c{i}", "content": "a.txt"}]
    msgs = [{"role": "user", "content": "go"}]
    for i in range(2):
        msgs += call(i)
        assert loops.mark_repeats(msgs, 0) == (0, "")
    msgs += call(2, '{"path":"."}')  # the same arguments, written differently
    assert loops.mark_repeats(msgs, 0) == (3, "list_dir") and msgs[-1]["_repeat"] == 3
    assert loops.repeat_note(3) in history.to_llm(msgs)[-1]["content"]
    msgs += call(3)
    msgs[-1]["content"] = "a.txt b.txt"  # a different result is progress, not a loop
    assert loops.mark_repeats(msgs, 0) == (0, "")
    msgs[2]["_cleared"] = {"at": 1}  # an output the model no longer sees doesn't count
    msgs += call(4)
    assert loops.mark_repeats(msgs, 0) == (3, "list_dir")
    assert loops.mark_repeats(msgs, len(msgs) - 2) == (0, ""), "only this task's calls count"


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


def test_continuation_hands_back_only_what_fits():
    m = {"role": "assistant", "content": "a" * 50 + "END", "reasoning_content": "r" * 500 + "TAIL", "_carry": 40}
    note = history.continuation(m)["content"]
    assert "The end of your reasoning" in note and "r" * 36 + "TAIL" in note and "r" * 37 + "TAIL" not in note and "END" in note
    assert history.carry({"stats": {"chars_per_token": 3.0}}, 8192) == int(8192 * history.CONTINUE_FRACTION * 3.0)
    assert history.carry({}, 1_000_000) == history.CONTINUE_TAIL


class FakeRun:
    llama = True

    def __init__(self):
        self.events = []

    def emit(self, type_, **data):
        self.events.append(type_)


def compact_with(chat, reason, n_ctx, fixed_chars=0, summary="- the gist"):
    """Run history.compact with a fake summarizer. Returns (compacted?, summarizer calls)."""
    calls = []

    async def fake_stream(client, settings, *, out, **kw):
        calls.append(kw["messages"][-1]["content"])
        out.content = summary
        return out
    real, history.llm.stream_chat = history.llm.stream_chat, fake_stream
    try:
        done = asyncio.run(history.compact(FakeRun(), {"model": "", "temperature": None}, chat, path=[], n_ctx=n_ctx,
                                           reason=reason, client=None, settings={"compact_at": 70}, fixed_chars=fixed_chars))
    finally:
        history.llm.stream_chat = real
    return done, calls


def tool_steps(n, result_chars):
    out = []
    for i in range(n):
        out += [{"role": "assistant", "content": "", "tool_calls": [{"id": f"c{i}", "type": "function", "function": {"name": "read_file", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": f"c{i}", "content": "x" * result_chars}]
    return out


def test_compaction_that_frees_nothing_is_not_done():
    # Older cut-off replies aren't sent (only the newest is), so summarizing them frees nothing: it
    # used to add a summary on top (the context grew) and repeat at every cut-off.
    chat = {"messages": [{"role": "user", "content": "think hard"}] +
            [{"role": "assistant", "content": "", "reasoning_content": "r" * 30000, "_truncated": True} for _ in range(3)],
            "stats": {"chars_per_token": 3.6}}
    assert compact_with(chat, "cutoff", 16384) == (False, [])
    assert compact_with(chat, "auto", 16384) == (False, []) and "compactions" not in chat


def test_auto_compaction_leaves_room_to_grow():
    # A big pasted request and a big system prompt can't be summarized away: the verbatim tail has to
    # shrink instead, or the chat lands just under the trigger and compacts again a step later.
    n_ctx, cpt = 32768, 3.6
    chat = {"messages": [{"role": "user", "content": "p" * int(12000 * cpt)}] + tool_steps(12, 3000),
            "stats": {"chars_per_token": cpt}}
    done, calls = compact_with(chat, "auto", n_ctx, fixed_chars=int(2000 * cpt))
    entry = chat["compactions"][-1]
    assert done and len(calls) == 1 and "at most about" in calls[0]
    assert entry["request"] == chat["messages"][0]["content"]  # still verbatim
    assert entry["after_tokens"] < n_ctx * (0.7 - history.MIN_GAIN), entry
    assert entry["before_tokens"] - entry["after_tokens"] > n_ctx * history.MIN_GAIN
    # right after it, there is nothing worth compacting again
    chat["messages"] += tool_steps(1, 3000)
    assert compact_with(chat, "auto", n_ctx, fixed_chars=int(2000 * cpt)) == (False, [])


def test_summary_longer_than_what_it_replaces_is_dropped():
    chat = {"messages": [{"role": "user", "content": "go"}] + tool_steps(25, 3000), "stats": {"chars_per_token": 3.6}}
    done, calls = compact_with(chat, "auto", 32768, summary="s" * 200000)
    assert len(calls) == 1 and not done and "compactions" not in chat


def test_old_tool_outputs_are_cleared_oldest_first():
    n_ctx, cpt = 32768, 3.6
    steps = tool_steps(14, 4000)
    steps[2]["tool_calls"][0]["function"]["name"] = "ask_agent"  # a teammate's report stays
    steps[5]["content"] = "short"  # not worth a placeholder
    chat = {"messages": [{"role": "user", "content": "go"}] + steps, "stats": {"chars_per_token": cpt}}
    before = history.estimate(chat, history.size(history.payload(chat), None))
    ids = history.clear_outputs(chat, n_ctx=n_ctx, trigger=n_ctx * 0.7)
    after = history.estimate(chat, history.size(history.payload(chat), None))
    assert ids and ids[0] == "c0" and "c1" not in ids and "c2" not in ids, ids  # c1: the report, c2: short
    assert not {"c11", "c12", "c13"} & set(ids), "the newest outputs are kept"
    assert before - after >= n_ctx * history.MIN_GAIN and after <= n_ctx * 0.7 * history.CLEAR_TO + 1200, (before, after)
    sent = {m.get("tool_call_id"): m["content"] for m in history.to_llm(chat["messages"]) if m["role"] == "tool"}
    assert sent["c0"].startswith("[Output cleared to save context: it was 4,000 characters.")
    assert chat["messages"][2]["content"] == "x" * 4000, "the chat keeps the output"
    assert history.clear_outputs(chat, n_ctx=n_ctx, trigger=n_ctx * 0.7) == [], "nothing more worth clearing"
    # an edit back to before the clearing brings the outputs back (runner.truncate)
    chat["messages"] = chat["messages"][:10]
    history.prune(chat, 10)
    assert not any(m.get("_cleared") for m in chat["messages"])


def test_old_write_file_contents_are_cleared_too():
    # coding in a small window: the biggest items are files written in full, which are on disk anyway
    cpt, body = 3.6, "x = 1\n" * 1200
    def write(i):
        return [{"role": "assistant", "content": "", "tool_calls": [{"id": f"w{i}", "type": "function", "function": {
                    "name": "write_file", "arguments": json.dumps({"path": f"f{i}.py", "content": body})}}]},
                {"role": "tool", "tool_call_id": f"w{i}", "content": f"Wrote {len(body)} bytes to f{i}.py"}]
    chat = {"messages": [{"role": "user", "content": "go"}] + write(0) + write(1) + write(2), "stats": {"chars_per_token": cpt}}
    ids = history.clear_outputs(chat, n_ctx=8192, trigger=8192 * 0.7)
    assert ids and ids[0] == "w0" and "w2" not in ids, ids  # w2: the current step
    calls = [m["tool_calls"][0] for m in history.to_llm(chat["messages"]) if m.get("tool_calls")]
    args = json.loads(calls[0]["function"]["arguments"])  # still valid JSON for the chat template
    assert args["path"] == "f0.py" and args["content"].startswith("[7,200 characters, cleared to save context: read f0.py")
    assert json.loads(calls[2]["function"]["arguments"])["content"] == body
    assert json.loads(chat["messages"][1]["tool_calls"][0]["function"]["arguments"])["content"] == body, "the chat keeps it"
    chat["messages"] = chat["messages"][:3]
    history.prune(chat, 3)
    assert "_cleared_args" not in chat["messages"][1], "an edit back to before the clearing restores it"


def test_reading_past_the_end_says_how_long_the_file_is():
    from pathlib import Path
    ws = Path(tempfile.mkdtemp(prefix="ac-read-"))
    (ws / "a.py").write_text("one\ntwo\n")
    assert tools.read_file(ws, "a.py", 200, 300) == "(nothing from line 200 on: a.py has 2 lines)"
    (ws / "e.py").write_text("")
    assert tools.read_file(ws, "e.py") == "(empty file)"


def test_in_a_small_window_fewer_outputs_are_kept():
    # three whole-file reads of ~2.3k tokens fill an 8k window: keep what fits in KEEP_FRACTION of
    # it (the current step's always), so clearing works instead of leaving it all to compaction
    cpt = 3.6
    chat = {"messages": [{"role": "user", "content": "go"}] + tool_steps(4, int(2300 * cpt)), "stats": {"chars_per_token": cpt}}
    ids = history.clear_outputs(chat, n_ctx=8192, trigger=8192 * 0.7)
    assert ids and "c3" not in ids and {"c0", "c1"} <= set(ids), ids
    chat = {"messages": [{"role": "user", "content": "go"}] + tool_steps(4, int(2300 * cpt)), "stats": {"chars_per_token": cpt}}
    assert history.clear_outputs(chat, n_ctx=65536, trigger=65536 * 0.7) == [], "a big window keeps them"


def test_clearing_that_frees_little_is_skipped():
    chat = {"messages": [{"role": "user", "content": "go"}] + tool_steps(5, 1500), "stats": {"chars_per_token": 3.6}}
    assert history.clear_outputs(chat, n_ctx=32768, trigger=1000) == [] and not any(m.get("_cleared") for m in chat["messages"])
    assert history.clear_outputs(chat, n_ctx=32768, trigger=1000, everything=True) == ["c0", "c1"]  # the server said it's full


def test_one_tool_output_cannot_fill_a_small_window():
    chat = {"stats": {"chars_per_token": 3.0}}
    out = history.fit_output("a" * 5000 + "b" * 5000, chat, 8192)
    assert len(out) < 6500 and out.startswith("a" * 100) and out.endswith("b" * 100) and "omitted to fit" in out
    assert history.fit_output("short", chat, 8192) == "short" and history.fit_output("x" * 50000, chat, None) == "x" * 50000


def test_agent_loop_does_not_compact_over_and_over():
    """The whole loop against a fake llama-server (counts the rendered prompt, rejects one over the
    window, cuts replies off where the window ends). Both cases used to compact at nearly every step."""
    from app import llm, runner
    cpt = 3.6
    cleared = []

    def loop(n_ctx, request_tokens, steps, think_tokens):
        calls, comps = [], []
        cleared.clear()

        async def server(client, settings, *, messages, tools, out, **kw):
            prompt = int(sum(len(str(m.get("content") or "")) + len(str(m.get("reasoning_content") or "")) +
                             len(json.dumps(m.get("tool_calls") or "")) for m in messages) / cpt + len(json.dumps(tools or [])) / cpt)
            if prompt > n_ctx:
                raise llm.LLMError("the request exceeds the available context size")
            if messages[0]["content"].startswith("You compress"):
                out.content = "- the gist " * 100
            else:
                calls.append(prompt)
                out.reasoning = "hmm " * int(min(think_tokens, n_ctx - prompt) * cpt / 4)
                out.finish_reason = "length" if think_tokens + 50 > n_ctx - prompt else "stop"
                if out.finish_reason == "stop" and len(calls) <= steps:
                    out.calls[0] = {"id": f"c{len(calls)}", "name": "read_file", "args": "{}"}
                elif out.finish_reason == "stop":
                    out.content = "done"
            out.usage = {"prompt_tokens": prompt, "completion_tokens": 50}
            return out

        async def read_file(run, agent, tc, **kw):
            return "line of file content\n" * 140, None  # ~800 tokens per step
        store.seed_defaults()
        store.save_settings({"loop_guard": False})  # the same read, the same result, step after step: not a loop here
        agent, chat = store.get_agent("coder"), store.create_chat("coder")
        run = runner.Run(chat)
        run.n_ctx = n_ctx
        emit = run.emit
        run.emit = lambda t, **d: (comps.append(d["compaction"]) if t == "compact_end" else
                                   cleared.append(d["call_ids"]) if t == "outputs_cleared" else None, emit(t, **d))
        runner.add_user_message(chat, agent, "p" * int(request_tokens * cpt))
        real = llm.stream_chat, runner.execute_tool
        llm.stream_chat, runner.execute_tool = server, read_file
        try:
            answer = asyncio.run(runner.agent_loop(run, agent, chat, depth=0, path=[], chain=[agent["id"]]))
        except llm.LLMError as e:
            answer = str(e)
        finally:
            llm.stream_chat, runner.execute_tool = real
            runner.views.pop(chat["id"], None)
            store.save_settings({"loop_guard": True})
        return answer, calls, comps

    # a model that thinks past the window: no compaction can help, so none is done, and it stops
    answer, calls, comps = loop(16384, 300, steps=3, think_tokens=20000)
    assert comps == [] and len(calls) == runner.MAX_CUTOFFS and "4 times in a row" in answer, (answer, calls, comps)
    assert max(calls) < 16384 * 0.4, calls  # the continuation note leaves the reply room
    # a big pasted request and many tool steps: clearing old outputs keeps it under the compaction
    # point, and the few clears each free real space
    answer, calls, comps = loop(32768, 12000, steps=25, think_tokens=0)
    assert answer == "done" and comps == [] and max(calls) < 32768 * 0.7, (calls, comps)
    assert 1 <= len(cleared) <= 6, cleared


def test_edit_file_forgives_what_models_copy_from_read_file():
    from pathlib import Path
    ws = Path(tempfile.mkdtemp(prefix="ac-edit-"))
    (ws / "a.py").write_text("def f(x):\n    return x\n\n\ndef g():\n    pass\n\n\ndef h():\n    pass\n")
    assert tools.read_file(ws, "a.py").startswith("     1\tdef f(x):\n     2\t    return x")
    # Qwen3.5-4B, 8k window: every line copied with the two spaces that used to follow the number
    out = tools.edit_file(ws, "a.py", "  def f(x):\n      return x", "  def f(x):\n      return x + 1")
    assert "2 spaces less indentation" in out
    out = tools.edit_file(ws, "a.py", "     5\tdef g():\n     6\t    pass", "     5\tdef g():\n     6\t    return 2")
    assert "line numbers taken out" in out
    assert (ws / "a.py").read_text().startswith("def f(x):\n    return x + 1\n\n\ndef g():\n    return 2\n")
    for bad in ("    return", "nothing like it"):  # twice, or nowhere even after the fixes: still refused
        try:
            tools.edit_file(ws, "a.py", bad, "x")
            raise AssertionError(bad)
        except tools.ToolError as e:
            assert "must match exactly once" in str(e)


def test_shell_commands_run_outside_the_apps_virtualenv():
    # `uv run` puts the app's .venv first on PATH: an agent's `python` / `pip` must be the user's
    out = asyncio.run(tools.run_shell(tools.workspace_for({"workspace": tempfile.mkdtemp(prefix="ac-shell-")}), 'echo "venv=[$VIRTUAL_ENV]"; command -v python3 || true'))
    assert "venv=[]" in out, out
    if sys.prefix != sys.base_prefix:  # these tests run in one
        assert os.path.realpath(sys.prefix) not in out, out


def test_thinking_is_switched_off_only_where_asked_and_understood():
    import httpx
    from app import llm
    sent = []

    def reply(request):
        sent.append(json.loads(request.content))
        return httpx.Response(200, text='data: {"choices": [{"delta": {"content": "ok"}, "finish_reason": "stop"}]}\n\ndata: [DONE]\n\n')

    async def go(**kw):
        async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as c:
            out = llm.Completion()
            async def nop(*a): pass
            await llm.stream_chat(c, {"base_url": "http://m/v1"}, model="m", messages=[], tools=None, temperature=None,
                                  out=out, on_delta=nop, **kw)
            return out.content
    assert asyncio.run(go(llama=True, thinking=False)) == "ok"
    asyncio.run(go(llama=True))
    asyncio.run(go(llama=False, thinking=False))  # other servers may reject fields they don't know
    assert sent[0]["chat_template_kwargs"] == {"enable_thinking": False}
    assert "chat_template_kwargs" not in sent[1] and "chat_template_kwargs" not in sent[2]


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


def test_tool_batches_group_only_neighbouring_subagent_calls():
    from app import runner
    calls = [{"id": i, "function": {"name": n}} for i, n in
             enumerate(["subagent", "subagent", "read_file", "subagent", "ask_agent", "subagent", "subagent"])]
    assert [[c["id"] for c in b] for b in runner.tool_batches(calls)] == [[0, 1], [2], [3], [4], [5, 6]]


def test_added_tools_reach_existing_starter_agents_once():
    store.seed_defaults()
    coder = store.get_agent("coder")
    store.save_agent(coder | {"tools": [t for t in coder["tools"] if t != "subagent"]})
    raw = json.loads(store.SETTINGS.read_text())
    raw.pop("added_tools", None)  # an install from before subagents
    store.SETTINGS.write_text(json.dumps(raw))
    store.seed_defaults()
    assert store.get_agent("coder")["tools"].count("subagent") == 1
    assert "subagent" not in store.get_agent("writer")["tools"]
    store.save_agent(store.get_agent("coder") | {"tools": ["read_file"]})  # the user takes it away...
    store.seed_defaults()
    assert store.get_agent("coder")["tools"] == ["read_file"], "...and it stays away"
    store.save_agent(coder)


def test_chat_status_with_subagents_side_by_side():
    from app import runner
    root = store.create_chat("coder")
    subs = [store.create_chat("coder") for _ in range(2)]
    run = runner.Run(root)
    try:
        for k, sub in enumerate(subs):
            run.stack.append(runner.View(run, sub, [f"c{k}"], 0))
        assert [run.status_of(c["id"]) for c in [root, *subs]] == ["delegated", "running", "running"]
        run.approval_paths["a"] = ("c1",)
        assert [run.status_of(c["id"]) for c in [root, *subs]] == ["delegated", "running", "waiting"]
        assert run.status_of("nobody") == "idle"
    finally:
        runner.views.pop(root["id"], None)
        for c in [root, *subs]:
            store.delete_chat(c["id"])


def test_a_subagent_gets_its_own_prompt_and_no_subagent_tool():
    from app import runner
    store.seed_defaults()
    coder = store.get_agent("coder")
    names = lambda schemas: [t["function"]["name"] for t in schemas]
    assert "subagent" in names(runner.agent_tools(coder, []))
    assert "subagent" not in names(runner.agent_tools(coder, [], subagent=True))
    chat = {"parent": {"root_chat_id": "r", "caller_id": "coder", "subagent": True}, "memory_profile": {"text": ""}}
    prompt = runner.system_prompt(coder, coder, [], chat)
    assert "You are working as a subagent" in prompt and "end your turn with a clear question" not in prompt
    chat["parent"].pop("subagent")
    assert "end your turn with a clear question" in runner.system_prompt(coder, coder, [], chat)


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
