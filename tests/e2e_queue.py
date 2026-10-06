"""Messages sent while a chat is running are queued and reach the agent at its next step.

Needs the app on AC_URL (default :8767) wired to tests/mock_llm.py.
"""
import asyncio
import json
import os

import httpx

from app.history import QUEUED_NOTE, to_llm

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")


async def events(c, url):
    async with c.stream("GET", url) as r:
        async for line in r.aiter_lines():
            if line.startswith("data:"):
                yield json.loads(line[5:])


async def drive(c, chat_id, content, on_event):
    """Send `content`, call on_event(ev, seen) for every root-stream event, return the events at done."""
    seen, started = [], False
    async for ev in events(c, f"{B}/api/chats/{chat_id}/stream"):
        seen.append(ev)
        if ev["type"] == "snapshot" and not started:
            started = True
            r = await c.post(f"{B}/api/chats/{chat_id}/run", json={"content": content})
            assert r.status_code == 200 and not r.json().get("queued"), r.text
        else:
            await on_event(ev, seen)
        if ev["type"] == "done":
            return seen


async def queue(c, chat_id, content):
    r = await c.post(f"{B}/api/chats/{chat_id}/run", json={"content": content})
    assert r.status_code == 200, r.text
    item = r.json().get("queued")
    assert item and item["content"] == content, r.text
    return item


def deltas(seen):
    return sum(e["type"] == "delta" for e in seen)


async def new_chat(c, agent):
    return (await c.post(f"{B}/api/chats", json={"agent_id": agent})).json()["id"]


async def test_midturn(c):
    print("\n== queued during a tool call: delivered after the result, before the next request")
    chat = await new_chat(c, "coder")
    queues = []

    async def on(ev, seen):
        if ev["type"] == "approval":
            await queue(c, chat, "also mention bananas")
            await c.post(f"{B}/api/chats/{chat}/approvals/{ev['approval_id']}", json={"approve": True})
        elif ev["type"] == "queue":
            queues.append([i["content"] for i in ev["items"]])

    seen = await drive(c, chat, "shell: echo hi", on)
    msgs = seen[-1]["chat"]["messages"]
    print("  messages:", [(m["role"], m.get("_queued", False), (m.get("content") or "")[:40]) for m in msgs])
    print("  queue events:", queues)
    assert [m["role"] for m in msgs] == ["user", "assistant", "tool", "user", "assistant"]
    assert msgs[3]["content"] == "also mention bananas" and msgs[3]["_queued"]
    assert msgs[4]["content"] == "Noted your aside: also mention bananas", "the model must see the queued note"
    assert queues == [["also mention bananas"], []]
    delivered = [e for e in seen if e["type"] == "user_message"]
    assert len(delivered) == 1 and delivered[0]["index"] == 3 and delivered[0]["path"] == []
    assert sum(e["type"] == "run_start" for e in seen) == 1


async def test_after_answer(c):
    print("\n== queued while it answers: the next turn starts in the same run, joined into one message")
    chat = await new_chat(c, "assistant")

    async def on(ev, seen):
        if ev["type"] == "delta" and deltas(seen) == 20:
            await queue(c, chat, "first follow-up")
            await queue(c, chat, "second follow-up")

    seen = await drive(c, chat, "slow please", on)
    msgs = seen[-1]["chat"]["messages"]
    print("  messages:", [(m["role"], m.get("_queued", False), (m.get("content") or "")[:40]) for m in msgs])
    assert [m["role"] for m in msgs] == ["user", "assistant", "user", "assistant"]
    assert msgs[2]["content"] == "first follow-up\n\nsecond follow-up"
    assert not msgs[2].get("_queued"), "a message that starts a new turn is an ordinary one"
    assert msgs[3]["content"].startswith("Here is an answer")
    assert sum(e["type"] == "done" for e in seen) == 1


async def test_edit_and_remove(c):
    print("\n== take queued messages back: one, a gone one (404), all of them; viewers see the queue")
    chat = await new_chat(c, "assistant")
    state = {}

    async def on(ev, seen):
        if ev["type"] == "delta" and deltas(seen) == 20:
            one, two, three = [await queue(c, chat, t) for t in ("one", "two", "three")]
            r = await c.delete(f"{B}/api/chats/{chat}/queue/{two['id']}")
            state["removed"] = r.json()["content"]
            state["again"] = (await c.delete(f"{B}/api/chats/{chat}/queue/{two['id']}")).status_code
            async for e in events(c, f"{B}/api/chats/{chat}/stream"):  # a page opening now
                state["snapshot"] = [i["content"] for i in e["queue"]]
                break
            state["all"] = [i["content"] for i in (await c.delete(f"{B}/api/chats/{chat}/queue")).json()["items"]]
            await queue(c, chat, "four")

    seen = await drive(c, chat, "slow please", on)
    msgs = seen[-1]["chat"]["messages"]
    print("  ", state, "| delivered:", [m["content"] for m in msgs if m["role"] == "user"][1:])
    assert state == {"removed": "two", "again": 404, "snapshot": ["one", "three"], "all": ["one", "three"]}
    assert [m["content"] for m in msgs if m["role"] == "user"] == ["slow please", "four"]


async def test_stop_returns(c):
    print("\n== stopping hands queued messages back unsent")
    chat = await new_chat(c, "assistant")
    returned = []

    async def watch():
        async for ev in events(c, f"{B}/api/events"):
            if ev["type"] == "queue_returned":
                returned.append(ev)
                return

    watcher = asyncio.create_task(watch())

    async def on(ev, seen):
        if ev["type"] == "delta" and deltas(seen) == 20:
            await queue(c, chat, "come back")
            await c.post(f"{B}/api/chats/{chat}/stop")

    seen = await drive(c, chat, "slow please", on)
    await asyncio.wait_for(watcher, 5)
    msgs = seen[-1]["chat"]["messages"]
    print("  returned:", [(e["chat_id"] == chat, e["text"]) for e in returned])
    assert returned and returned[0]["chat_id"] == chat and returned[0]["text"] == "come back"
    assert not any(m.get("content") == "come back" for m in msgs)
    assert (await c.delete(f"{B}/api/chats/{chat}/queue")).json() == {"items": []}


async def test_during_delegation(c):
    print("\n== queued while a sub-agent works: the chat's own agent gets it, the sub-agent doesn't")
    chat = await new_chat(c, "orchestrator")

    async def on(ev, seen):
        if ev["type"] == "approval":  # Coder's shell command, deep in the delegation
            assert ev["path"], "approval should come from the sub-agent"
            await queue(c, chat, "and say thanks")
            await c.post(f"{B}/api/chats/{chat}/approvals/{ev['approval_id']}", json={"approve": True})

    seen = await drive(c, chat, "please delegate this", on)
    root = seen[-1]["chat"]
    sub = (await c.get(f"{B}/api/chats/{root['subchats'][0]}")).json()["messages"]
    print("  root:", [(m["role"], (m.get("content") or "")[:30]) for m in root["messages"]])
    print("  sub: ", [(m["role"], (m.get("content") or "")[:30]) for m in sub])
    assert [m["role"] for m in root["messages"]] == ["user", "assistant", "tool", "user", "assistant"]
    assert root["messages"][3]["_queued"] and root["messages"][4]["content"] == "Noted your aside: and say thanks"
    assert not any("thanks" in (m.get("content") or "") for m in sub)


async def test_busy_rules(c):
    print("\n== edits and empty sends still need an idle chat")
    chat = await new_chat(c, "assistant")
    codes = {}

    async def on(ev, seen):
        if ev["type"] == "delta" and deltas(seen) == 5:
            codes["edit"] = (await c.post(f"{B}/api/chats/{chat}/run", json={"content": "x", "from_index": 0})).status_code
            codes["empty"] = (await c.post(f"{B}/api/chats/{chat}/run", json={"content": "  "})).status_code
            await c.post(f"{B}/api/chats/{chat}/stop")

    await drive(c, chat, "slow please", on)
    print("  ", codes)
    assert codes == {"edit": 409, "empty": 409}


def test_payload():
    print("\n== model payload labels mid-turn messages only")
    msgs = [{"role": "user", "content": "task"}, {"role": "assistant", "content": "", "tool_calls": []},
            {"role": "user", "content": "aside", "_queued": True, "_recall": "- [1] fact"},
            {"role": "user", "content": "new turn"}]
    out = to_llm(msgs)
    assert out[0]["content"] == "task" and out[3]["content"] == "new turn"
    assert out[2]["content"].startswith(QUEUED_NOTE + "\naside\n\n[From your memory")
    assert all(not k.startswith("_") for m in out for k in m)


async def main():
    test_payload()
    async with httpx.AsyncClient(timeout=None, headers={"X-Agent-Chat": "1"}) as c:
        await c.put(f"{B}/api/settings", json={"bypass_approvals": False})
        await test_midturn(c)
        await test_after_answer(c)
        await test_edit_and_remove(c)
        await test_stop_returns(c)
        await test_during_delegation(c)
        await test_busy_rules(c)
    print("\nall queue tests passed")

asyncio.run(asyncio.wait_for(main(), 180))
