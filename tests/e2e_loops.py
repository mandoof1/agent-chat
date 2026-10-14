"""The loop guard (app/loops.py): a reply that keeps writing the same passage is stopped and the
model told to move on; the same tool call returning the same result is flagged, then ends the turn.

Needs the app on AC_URL (default :8767) and the mock on MOCK_URL (default :8766).
"""
import asyncio
import json
import os
import sys

import httpx

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mock_llm import LOOP_PASSAGE  # noqa: E402

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")
MOCK = os.environ.get("MOCK_URL", "http://127.0.0.1:8766/v1")
WHOLE = len("Let me plan the board.\n\n" + LOOP_PASSAGE * 15)  # what the mock would think if left to


async def events(c, url):
    async with c.stream("GET", url) as r:
        async for line in r.aiter_lines():
            if line.startswith("data:"):
                yield json.loads(line[5:])


async def settings(c, **kw):
    r = await c.put(f"{B}/api/settings", json=kw)
    assert r.status_code == 200, r.text


async def run_turn(c, chat_id, content):
    """Send a message and return every event of the run (sub-chats too) once it is done."""
    seen, started = [], False
    async for ev in events(c, f"{B}/api/chats/{chat_id}/stream"):
        seen.append(ev)
        if ev["type"] == "snapshot" and not started:
            started = True
            r = await c.post(f"{B}/api/chats/{chat_id}/run", json={"content": content})
            assert r.status_code == 200, r.text
        if ev["type"] == "done":
            return seen


async def turn(c, agent, content):
    chat = (await c.post(f"{B}/api/chats", json={"agent_id": agent})).json()["id"]
    evs = await run_turn(c, chat, content)
    msgs = (await c.get(f"{B}/api/chats/{chat}")).json()["messages"]
    notices = [e["text"] for e in evs if e["type"] == "notice"]
    errors = [e["message"] for e in evs if e["type"] == "error"]
    return msgs, notices, errors


async def test_thinking_loop(c):
    print("\n== thinking that goes round in circles is stopped, and the model moves on")
    msgs, notices, errors = await turn(c, "coder", "stuck think")
    looped = [m for m in msgs if m.get("_looped")]
    print("  notices:", notices, "| errors:", errors)
    print("  looped:", [(m["_looped"], len(m.get("reasoning_content") or "")) for m in looped], "| final:", msgs[-1]["content"])
    assert not errors and len(looped) == 1
    info = looped[0]["_looped"]
    assert info["channel"] == "reasoning" and info["repeats"] >= 3, info
    assert len(looped[0]["reasoning_content"]) < WHOLE, "the stream was stopped before the model finished looping"
    assert not looped[0].get("tool_calls")
    assert any(n.startswith("Loop guard: the thinking kept repeating") for n in notices), notices
    assert msgs[-1]["content"] == "Moved on. passages_in_note=1 answer_in_note=False", "the note holds the passage once"


async def test_answer_loop(c):
    print("\n== an answer that repeats itself is stopped too; the note hands back the answer so far")
    msgs, notices, errors = await turn(c, "coder", "stuck answer")
    looped = [m for m in msgs if m.get("_looped")]
    print("  notices:", notices, "| final:", msgs[-1]["content"])
    assert not errors and len(looped) == 1 and looped[0]["_looped"]["channel"] == "content"
    assert len(looped[0]["content"]) < len("Here is the plan.\n\n" + LOOP_PASSAGE * 15)
    assert any(n.startswith("Loop guard: the answer kept repeating") for n in notices), notices
    assert msgs[-1]["content"] == "Moved on. passages_in_note=1 answer_in_note=True"


async def test_gives_up(c):
    print("\n== a model that loops again after every note: the turn ends with an error")
    msgs, notices, errors = await turn(c, "coder", "stuck forever")
    print("  errors:", errors, "| looped replies:", sum(bool(m.get("_looped")) for m in msgs))
    assert len(errors) == 1 and "stuck repeating itself 3 times" in errors[0], errors
    assert sum(bool(m.get("_looped")) for m in msgs) == 3


async def test_delegated(c):
    print("\n== a delegated agent's loop is broken in its own chat; the caller gets its answer")
    msgs, notices, errors = await turn(c, "orchestrator", "delegate: stuck think")
    results = [m for m in msgs if m["role"] == "tool"]
    print("  handoff result:", results[0]["content"][:80] if results else None, "| notices:", notices)
    assert not errors and results and "Moved on. passages_in_note=1" in results[0]["content"]
    sub = results[0]["_sub"]["messages"]
    assert sum(bool(m.get("_looped")) for m in sub) == 1


async def test_repeated_call_heeded(c):
    print("\n== the same call with the same result: the third is flagged, and the model heeds it")
    msgs, notices, errors = await turn(c, "coder", "loop 8 heed")
    tools = [m for m in msgs if m["role"] == "tool"]
    print("  calls:", len(tools), "| repeats:", [m.get("_repeat") for m in tools], "| final:", msgs[-1]["content"])
    assert not errors and len(tools) == 3 and [m.get("_repeat") for m in tools] == [None, None, 3]
    assert msgs[-1]["content"] == "Heeded the loop guard after 3 calls.", "the model saw the note"
    assert any("same list_dir call returned the same result 3 times" in n for n in notices), notices


async def test_repeated_call_stops(c):
    print("\n== ...and a model that ignores it is stopped at the fifth")
    msgs, notices, errors = await turn(c, "coder", "loop 8")
    tools = [m for m in msgs if m["role"] == "tool"]
    print("  calls:", len(tools), "| repeats:", [m.get("_repeat") for m in tools], "| notices:", notices[-1:])
    assert not errors and len(tools) == 5 and [m.get("_repeat") for m in tools] == [None, None, 3, 4, 5]
    assert any(n.startswith("Loop guard: stopped.") for n in notices), notices
    assert msgs[-1]["role"] == "tool", "the turn ended there"


async def test_off(c):
    print("\n== with the loop guard off, nothing is stopped or flagged")
    await settings(c, loop_guard=False)
    try:
        msgs, notices, errors = await turn(c, "coder", "stuck think")
        assert not errors and not any(m.get("_looped") for m in msgs) and msgs[-1]["content"] == "never reached"
        assert len(msgs[-1]["reasoning_content"]) == WHOLE
        msgs, notices, errors = await turn(c, "coder", "loop 6")
        tools = [m for m in msgs if m["role"] == "tool"]
        assert not errors and len(tools) == 6 and not any(m.get("_repeat") for m in tools)
        assert not any(n.startswith("Loop guard") for n in notices), notices
        print("  ok: the whole loop ran, 6 calls")
    finally:
        await settings(c, loop_guard=True)


async def main():
    async with httpx.AsyncClient(timeout=None, headers={"X-Agent-Chat": "1"}) as c:
        await settings(c, base_url=MOCK, bypass_approvals=True, max_steps=30, loop_guard=True)
        try:
            await test_thinking_loop(c)
            await test_answer_loop(c)
            await test_gives_up(c)
            await test_delegated(c)
            await test_repeated_call_heeded(c)
            await test_repeated_call_stops(c)
            await test_off(c)
        finally:
            await settings(c, base_url=MOCK, bypass_approvals=False)
    print("\nall loop guard tests passed")

asyncio.run(asyncio.wait_for(main(), 180))
