"""Replies that fill the context window (finish_reason "length") must continue, not end the run.

Needs the app on AC_URL (default :8767) and two mock servers:
  :8766 normal, :8768 MOCK_NCTX=3000.
"""
import asyncio
import json
import os

import httpx

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")
NOTICE = "filled the context window"


async def events(c, url):
    async with c.stream("GET", url) as r:
        async for line in r.aiter_lines():
            if line.startswith("data:"):
                yield json.loads(line[5:])


async def settings(c, **kw):
    await c.put(f"{B}/api/settings", json=kw)


async def run_turn(c, chat_id, content):
    """Send a message and return the root stream's event list once the run is done."""
    seen, started = [], False
    async for ev in events(c, f"{B}/api/chats/{chat_id}/stream"):
        seen.append(ev)
        if ev["type"] == "snapshot" and not started:
            started = True
            r = await c.post(f"{B}/api/chats/{chat_id}/run", json={"content": content})
            assert r.status_code == 200, r.text
        if ev["type"] == "done":
            return seen


async def new_chat(c, agent):
    return (await c.post(f"{B}/api/chats", json={"agent_id": agent})).json()["id"]


async def get_chat(c, chat_id):
    return (await c.get(f"{B}/api/chats/{chat_id}")).json()


async def test_continues(c):
    print("\n== cut-off reply continues (Coder, cut off twice)")
    await settings(c, base_url="http://127.0.0.1:8766/v1", auto_compact=True, bypass_approvals=False)
    chat = await new_chat(c, "coder")
    evs = await run_turn(c, chat, "cutoff 2")
    notices = [e["text"] for e in evs if e["type"] == "notice"]
    errors = [e["message"] for e in evs if e["type"] == "error"]
    msgs = (await get_chat(c, chat))["messages"]
    print("  notices:", len(notices), "| errors:", errors)
    print("  messages:", [(m["role"], bool(m.get("_truncated")), bool(m.get("tool_calls"))) for m in msgs])
    print("  final:", msgs[-1]["content"])
    assert not errors
    assert sum(NOTICE in n for n in notices) == 2
    assert [m["role"] for m in msgs] == ["user", "assistant", "assistant", "assistant"]
    assert msgs[1]["_truncated"] and msgs[2]["_truncated"] and not msgs[3].get("_truncated")
    assert not any(m.get("tool_calls") for m in msgs), "a call cut off mid-way must not be kept or run"
    # the model got exactly one continuation note and never the raw cut-off replies
    assert "notes_in_payload=1 leaked=0" in msgs[-1]["content"]


async def test_delegated(c):
    print("\n== cut-off reply in a delegated sub-chat still reports back")
    chat = await new_chat(c, "orchestrator")
    evs = await run_turn(c, chat, "delegate cutoff")
    msgs = (await get_chat(c, chat))["messages"]
    result = next(m["content"] for m in msgs if m["role"] == "tool")
    print("  tool result:", result[:160].replace("\n", " / "))
    assert "partial answer 1." in result and "partial answer 2." in result
    assert "Final answer after 2 cut-offs" in result
    assert not [e for e in evs if e["type"] == "error"]


async def test_crowded(c):
    print("\n== cut off while the prompt fills most of the window: compact, then continue")
    await settings(c, base_url="http://127.0.0.1:8768/v1", auto_compact=False)
    chat = await new_chat(c, "coder")
    for i in (1, 2):
        await run_turn(c, chat, f"big {i}")
    evs = await run_turn(c, chat, "cutoff 1 crowded")
    reasons = [e["compaction"]["reason"] for e in evs if e["type"] == "compact_end"]
    data = await get_chat(c, chat)
    print("  compactions this turn:", reasons, "| final:", data["messages"][-1]["content"])
    assert reasons == ["cutoff"]
    assert data["compactions"][-1]["request"] == "cutoff 1 crowded"
    assert data["messages"][-1]["content"].startswith("Final answer after 1 cut-offs")


async def test_full(c):
    print("\n== cut off with a prompt that fills the window and can't be compacted: clear error")
    await settings(c, base_url="http://127.0.0.1:8766/v1")
    chat = await new_chat(c, "coder")
    evs = await run_turn(c, chat, "cutoff 1 full")
    errors = [e["message"] for e in evs if e["type"] == "error"]
    print("  errors:", errors)
    assert len(errors) == 1 and "could not be compacted" in errors[0]


async def main():
    async with httpx.AsyncClient(timeout=None, headers={"X-Agent-Chat": "1"}) as c:
        try:
            await test_continues(c)
            await test_delegated(c)
            await test_crowded(c)
            await test_full(c)
        finally:
            await settings(c, base_url="http://127.0.0.1:8766/v1", auto_compact=True)
    print("\nall cut-off tests passed")

asyncio.run(asyncio.wait_for(main(), 120))
