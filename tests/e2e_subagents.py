"""Subagents (the subagent tool, runner.spawn): a fresh copy of the calling agent that does one
task and reports back.

Needs the app on AC_URL (default :8767) and the mock model on MOCK_URL (default :8766), whose
/props reports one slot. Covers: the subagent's tools (no subagent of its own), results added in
call order with their sub-chats, several running side by side when allowed and one at a time
when not, chat statuses while they run, Stop part way, a subagent's model error, a subagent
started by an agent working for another, and an edit erasing the subagents' chats.
"""
import asyncio
import json
import os

import httpx

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")
MOCK = os.environ.get("MOCK_URL", "http://127.0.0.1:8766/v1")


async def events(c, url):
    async with c.stream("GET", url) as r:
        async for line in r.aiter_lines():
            if line.startswith("data:"):
                yield json.loads(line[5:])


async def turn(c, chat_id, body, on_event=None):
    """Start a run; return all its events once it is done."""
    seen, started = [], False
    async for ev in events(c, f"{B}/api/chats/{chat_id}/stream"):
        seen.append(ev)
        if ev["type"] == "snapshot" and not started:
            started = True
            r = await c.post(f"{B}/api/chats/{chat_id}/run", json=body)
            assert r.status_code == 200, r.text
        elif on_event:
            await on_event(ev)
        if ev["type"] == "done":
            return seen


async def chat(c, chat_id):
    return (await c.get(f"{B}/api/chats/{chat_id}")).json()


async def new_chat(c, agent_id):
    return (await c.post(f"{B}/api/chats", json={"agent_id": agent_id})).json()["id"]


def overlap(a, b):
    """Did two subagent chats run at the same time? (each: its task's time to its last message's)"""
    return a[0] < b[1] and b[0] < a[1]


def span(sub):
    return sub["messages"][0]["_ts"], sub["messages"][-1]["_ts"]


async def test_basics(c):
    print("\n== two subagents: their tools, results in call order, their chats")
    cid = await new_chat(c, "coder")
    evs = await turn(c, cid, {"content": "subagents 2"})
    assert not [e for e in evs if e["type"] == "error"], evs
    starts = [e for e in evs if e["type"] == "subagent_start"]
    assert [(e["subagent"], e["description"], e["agent_id"], e["continued"]) for e in starts] == \
        [(True, "Part 1", "coder", False), (True, "Part 2", "coder", False)], starts
    results = [e["content"] for e in evs if e["type"] == "tool_result" and not e["path"]]
    assert len(results) == 2 and all(r.startswith("Tools: ") for r in results), results
    offered = results[0][len("Tools: "):].split(", ")
    assert "subagent" not in offered and "run_shell" in offered and "ask_agent" in offered, offered
    print(f"  a subagent is offered: {', '.join(offered)}")

    ch = await chat(c, cid)
    roles = [m["role"] for m in ch["messages"]]
    assert roles == ["user", "assistant", "tool", "tool", "assistant"], roles
    calls = [tc["id"] for tc in ch["messages"][1]["tool_calls"]]
    assert [m["tool_call_id"] for m in ch["messages"][2:4]] == calls, "results in call order"
    subs = []
    for m, label in zip(ch["messages"][2:4], ["Part 1", "Part 2"]):
        info = m["_sub"]
        assert info["subagent"] is True and info["description"] == label and info["agent_id"] == "coder", info
        sub = await chat(c, info["chat_id"])
        assert sub["parent"] == {"root_chat_id": cid, "caller_id": "coder", "subagent": True}, sub["parent"]
        assert sub["title"] == label and sub["agent_id"] == "coder", sub["title"]
        assert sub["messages"][0]["content"].startswith("which tools"), sub["messages"][0]
        assert sub["messages"][-1]["content"] == m["content"], "its last reply is the result"
        subs.append(sub["id"])
    assert ch["subchats"] == subs, (ch["subchats"], subs)
    listed = {x["id"]: x for x in (await c.get(f"{B}/api/chats")).json()}
    assert all(listed[s]["parent"]["subagent"] for s in subs), "the chat list says which chats are subagents'"
    r = await c.post(f"{B}/api/chats/{subs[0]}/run", json={"content": "hi"})
    assert r.status_code == 400, "a subagent's chat can't be written in"
    print("  results in order, two sub-chats titled by their descriptions, read-only")
    return cid, subs


async def test_parallel(c):
    print("\n== side by side when allowed, one at a time when not")
    for setting, together in [(2, True), (1, False), (0, False)]:  # (0: the mock's one slot)
        await c.put(f"{B}/api/settings", json={"parallel_subagents": setting})
        cid = await new_chat(c, "coder")
        statuses = []

        async def watch(ev):
            if ev["type"] == "subagent_start" and setting == 2 and len(statuses) == 0:
                await asyncio.sleep(0.4)  # both started by now
                statuses.append({x["id"]: x["status"] for x in (await c.get(f"{B}/api/chats")).json()})

        await turn(c, cid, {"content": "subagents 2: nap"}, watch)
        ch = await chat(c, cid)
        a, b = [await chat(c, m["_sub"]["chat_id"]) for m in ch["messages"][2:4]]
        assert [a["messages"][-1]["content"], b["messages"][-1]["content"]] == ["Part 1 done.", "Part 2 done."]
        assert overlap(span(a), span(b)) == together, (setting, span(a), span(b))
        if statuses:
            st = statuses[0]
            assert (st[cid], st[a["id"]], st[b["id"]]) == ("delegated", "running", "running"), st
            print("  while both work: the chat is delegated, both subagents running")
        print(f"  parallel_subagents={setting}: {'side by side' if together else 'one after the other'}")
    await c.put(f"{B}/api/settings", json={"parallel_subagents": 1})


async def test_stop(c):
    print("\n== Stop after the first of two: its result kept, the other cancelled, in order")
    cid = await new_chat(c, "coder")

    async def stop_after_first(ev):
        if ev["type"] == "tool_result" and not ev["path"]:
            await c.post(f"{B}/api/chats/{cid}/stop")

    await turn(c, cid, {"content": "subagents 2: nap"}, stop_after_first)
    ch = await chat(c, cid)
    calls = [tc["id"] for tc in ch["messages"][1]["tool_calls"]]
    tools = [m for m in ch["messages"] if m["role"] == "tool"]
    assert [m["tool_call_id"] for m in tools] == calls, tools
    assert tools[0]["content"] == "Part 1 done." and tools[1]["content"].startswith("[cancelled"), tools
    print("  " + " · ".join(m["content"][:30] for m in tools))


async def test_error(c):
    print("\n== a subagent's model error ends the subagent, not the turn")
    cid = await new_chat(c, "coder")
    evs = await turn(c, cid, {"content": "subagents 1: explode"})
    assert not [e for e in evs if e["type"] == "error"], "no error for the turn"
    res = [e["content"] for e in evs if e["type"] == "tool_result" and not e["path"]]
    assert res and res[0].startswith("Error: the subagent stopped with an error") and "mock exploded" in res[0], res
    notes = [e["text"] for e in evs if e["type"] == "notice" and len(e["path"]) == 1]
    assert any("mock exploded" in n for n in notes), notes
    ch = await chat(c, cid)
    assert ch["messages"][-1]["role"] == "assistant" and ch["messages"][-1]["content"].startswith("Finished."), ch["messages"][-1]
    sub = await chat(c, ch["messages"][2]["_sub"]["chat_id"])
    assert "mock exploded" in sub["messages"][-1]["_error"], sub["messages"][-1]
    print(f"  {res[0][:90]}")


async def test_nested(c):
    print("\n== a subagent of an agent working for another")
    cid = await new_chat(c, "orchestrator")
    evs = await turn(c, cid, {"content": "delegate: subagents 1"})
    assert not [e for e in evs if e["type"] == "error"], evs
    start = next(e for e in evs if e["type"] == "subagent_start" and e.get("subagent"))
    assert len(start["path"]) == 2 and start["agent_id"] == "coder", start
    deep = [e for e in evs if e["type"] == "delta" and len(e.get("path", [])) == 2]
    assert deep, "the subagent's reply streams under the handoff"
    ch = await chat(c, cid)
    thread = await chat(c, ch["messages"][2]["_sub"]["chat_id"])
    info = next(m["_sub"] for m in thread["messages"] if m["role"] == "tool")
    sub = await chat(c, info["chat_id"])
    assert sub["parent"]["root_chat_id"] == cid and sub["id"] in ch["subchats"], "kept with the chat the user started"
    print("  Orchestrator → Coder → subagent; its chat belongs to the Orchestrator's chat")


async def test_edit_erases(c, cid, subs):
    print("\n== an edit from the start erases the subagents' chats")
    await turn(c, cid, {"content": "hello", "from_index": 0})
    ch = await chat(c, cid)
    assert not ch.get("subchats"), ch.get("subchats")
    for s in subs:
        assert (await c.get(f"{B}/api/chats/{s}")).status_code == 404, s
    print("  gone")


async def test_prompt(c):
    print("\n== the agent editor's preview lists the tool; an agent without it isn't offered it")
    st = (await c.get(f"{B}/api/state")).json()
    assert any(t["name"] == "subagent" and t["group"] == "Agents" for t in st["tools"]), st["tools"]
    cid = await new_chat(c, "assistant")
    evs = await turn(c, cid, {"content": "which tools"})
    res = next(e["message"]["content"] for e in evs if e["type"] == "assistant_end")
    assert "subagent" not in res, res


async def main():
    async with httpx.AsyncClient(timeout=60, headers={"X-Agent-Chat": "1"}) as c:
        await c.put(f"{B}/api/settings", json={"base_url": MOCK, "bypass_approvals": False})
        cid, subs = await test_basics(c)
        await test_parallel(c)
        await test_stop(c)
        await test_error(c)
        await test_nested(c)
        await test_edit_erases(c, cid, subs)
        await test_prompt(c)
    print("\nall subagent tests passed")


asyncio.run(main())
