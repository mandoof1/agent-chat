"""Tests for sub-chat live views, chat statuses and compaction.

Needs the app on AC_URL (default :8767) and three mock servers:
  :8766 normal, :8768 MOCK_NCTX=3000 (forces auto-compaction), :8769 MOCK_OVERFLOW_CHARS=6000.
"""
import asyncio, json, os
import httpx

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")
MOCK = os.environ.get("MOCK_URL", "http://127.0.0.1:8766/v1")
MOCK_NCTX = os.environ.get("MOCK_NCTX_URL", "http://127.0.0.1:8768/v1")
MOCK_OVERFLOW = os.environ.get("MOCK_OVERFLOW_URL", "http://127.0.0.1:8769/v1")


async def events(c, url):
    async with c.stream("GET", url) as r:
        async for line in r.aiter_lines():
            if line.startswith("data:"):
                yield json.loads(line[5:])


async def settings(c, **kw):
    await c.put(f"{B}/api/settings", json=kw)


async def run_turn(c, chat_id, content=None, endpoint="run"):
    """Send a message (or compact) and return the root stream's event list once done."""
    seen, started = [], False
    async for ev in events(c, f"{B}/api/chats/{chat_id}/stream"):
        seen.append(ev)
        if ev["type"] == "snapshot" and not started:
            started = True
            r = await c.post(f"{B}/api/chats/{chat_id}/{endpoint}", json={"content": content} if content else {})
            assert r.status_code == 200, r.text
        if ev["type"] == "done":
            return seen


async def test_subchat_view(c):
    print("\n== sub-chat live view + statuses")
    await settings(c, base_url=MOCK)
    root = (await c.post(f"{B}/api/chats", json={"agent_id": "orchestrator"})).json()["id"]
    result = {}

    async def watch_sub(sub_id):
        sub_events = []
        async for ev in events(c, f"{B}/api/chats/{sub_id}/stream"):
            sub_events.append(ev)
            if ev["type"] == "approval":
                statuses = {x["id"]: x["status"] for x in (await c.get(f"{B}/api/chats")).json()}
                result["status_root"], result["status_sub"] = statuses[root], statuses[sub_id]
                # approve through the SUB-chat's endpoint
                r = await c.post(f"{B}/api/chats/{sub_id}/approvals/{ev['approval_id']}", json={"approve": True})
                result["approve_via_sub"] = r.status_code
            if ev["type"] == "done":
                break
        result["sub_events"] = [(e["type"], e.get("path"), e.get("running")) for e in sub_events
                                if e["type"] in ("snapshot", "run_start", "approval", "tool_start", "done")]

    started, task = False, None
    async for ev in events(c, f"{B}/api/chats/{root}/stream"):
        if ev["type"] == "snapshot" and not started:
            started = True
            await c.post(f"{B}/api/chats/{root}/run", json={"content": "please delegate this"})
        if ev["type"] == "subagent_start":
            result["sub_id"] = ev["chat_id"]
            task = asyncio.create_task(watch_sub(ev["chat_id"]))
        if ev["type"] == "done":
            break
    await task
    sub = (await c.get(f"{B}/api/chats/{result['sub_id']}")).json()
    summary = next(x for x in (await c.get(f"{B}/api/chats")).json() if x["id"] == result["sub_id"])
    print("  statuses while sub-agent waits for approval: root =", result["status_root"], "| sub =", result["status_sub"])
    print("  approve via sub-chat endpoint:", result["approve_via_sub"])
    print("  sub-chat stream got:", result["sub_events"])
    print("  sub-chat parent:", sub["parent"], "| title:", sub["title"], "| msgs:", [m["role"] for m in sub["messages"]])
    print("  final statuses idle:", summary["status"], next(x for x in (await c.get(f"{B}/api/chats")).json() if x["id"] == root)["status"])
    r = await c.post(f"{B}/api/chats/{result['sub_id']}/run", json={"content": "hi"})
    print("  typing into a sub-chat ->", r.status_code, r.json()["detail"][:60])
    r = await c.delete(f"{B}/api/chats/{root}")
    print("  delete root also deletes sub-chat:", (await c.get(f"{B}/api/chats/{result['sub_id']}")).status_code)


async def test_auto_compact(c):
    print("\n== auto-compaction (mock n_ctx=3000, compact at 70%)")
    await settings(c, base_url=MOCK_NCTX, auto_compact=True, compact_at=70)
    chat = (await c.post(f"{B}/api/chats", json={"agent_id": "assistant"})).json()["id"]
    for i in range(1, 5):
        evs = await run_turn(c, chat, f"big {i}")
        kinds = [e["type"] for e in evs if e["type"].startswith("compact") or e["type"] in ("error", "notice")]
        print(f"  turn {i}: {kinds or 'no compaction'}")
    data = (await c.get(f"{B}/api/chats/{chat}")).json()
    for comp in data.get("compactions", []):
        print(f"  compaction: reason={comp['reason']} upto={comp['upto']} tokens {comp['before_tokens']}->{comp['after_tokens']} "
              f"request={comp['request']!r} summary={comp['summary'][:50]!r}")
    print("  messages kept in chat file:", len(data["messages"]), "| errors:", [m["_error"] for m in data["messages"] if m.get("_error")])
    evs = await run_turn(c, chat, endpoint="compact")
    print("  manual compact:", [e.get("text") or e["type"] for e in evs if e["type"] in ("compact_end", "notice")])
    # edit back to the start: compactions beyond the cut must disappear
    await run_turn(c, chat, None) if False else None
    r = await c.post(f"{B}/api/chats/{chat}/run", json={"content": "big again", "from_index": 0})
    await asyncio.sleep(0.5)
    data = (await c.get(f"{B}/api/chats/{chat}")).json()
    print("  after editing message 0: compactions =", len(data.get("compactions", [])), "| messages =", len(data["messages"]))


async def test_overflow(c):
    print("\n== overflow retry (mock rejects prompts > 6000 chars, auto-compact off)")
    await settings(c, base_url=MOCK_OVERFLOW, auto_compact=False)
    chat = (await c.post(f"{B}/api/chats", json={"agent_id": "assistant"})).json()["id"]
    for i in range(1, 4):
        evs = await run_turn(c, chat, f"big {i}")
        print(f"  turn {i}:", [e.get("message") or e.get("compaction", {}).get("reason") or e["type"]
                              for e in evs if e["type"] in ("compact_end", "error")] or "ok")
    await settings(c, base_url=MOCK, auto_compact=True)


async def test_compact_instructions(c):
    print("\n== /compact <instructions>: the summary request carries them; no body works as before")
    await settings(c, base_url=MOCK)
    chat = (await c.post(f"{B}/api/chats", json={"agent_id": "assistant"})).json()["id"]
    for i in range(3):
        await run_turn(c, chat, f"hello {i}")
    seen, started = [], False
    async for ev in events(c, f"{B}/api/chats/{chat}/stream"):
        seen.append(ev)
        if ev["type"] == "snapshot" and not started:
            started = True
            r = await c.post(f"{B}/api/chats/{chat}/compact", json={"instructions": "  the API design decisions "})
            assert r.status_code == 200, r.text
        if ev["type"] == "done":
            break
    end = next(e for e in seen if e["type"] == "compact_end")["compaction"]
    print("  compaction:", {k: end[k] for k in ("reason", "upto", "instructions")}, "| summary:", end["summary"].splitlines()[-1])
    assert end["reason"] == "manual" and end["instructions"] == "the API design decisions", end
    assert "Kept as asked: the API design decisions" in end["summary"], end["summary"]
    saved = (await c.get(f"{B}/api/chats/{chat}")).json()["compactions"][-1]
    assert saved["instructions"] == "the API design decisions" and saved["summary"] == end["summary"]
    await run_turn(c, chat, "one more")
    await run_turn(c, chat, "and another")
    async with c.stream("GET", f"{B}/api/chats/{chat}/stream") as r:  # no body at all, like the header button
        async for line in r.aiter_lines():
            if line.startswith("data:") and json.loads(line[5:])["type"] == "snapshot":
                assert (await c.post(f"{B}/api/chats/{chat}/compact")).status_code == 200
            if line.startswith("data:") and json.loads(line[5:])["type"] == "done":
                break
    plain = (await c.get(f"{B}/api/chats/{chat}")).json()["compactions"][-1]
    print("  without instructions:", plain["summary"].splitlines()[-1])
    assert "instructions" not in plain and "Kept as asked" not in plain["summary"] and plain["upto"] > saved["upto"]


async def main():
    async with httpx.AsyncClient(timeout=None, headers={"X-Agent-Chat": "1"}) as c:
        await test_subchat_view(c)
        await test_auto_compact(c)
        await test_overflow(c)
        await test_compact_instructions(c)

asyncio.run(asyncio.wait_for(main(), 120))
