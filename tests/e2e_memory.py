"""Memory tests against the app on AC_URL (default :8767) wired to the normal mock on :8766."""
import asyncio, json, os
import httpx

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")


async def turn(c, chat_id, content):
    started = False
    async with c.stream("GET", f"{B}/api/chats/{chat_id}/stream") as r:
        async for line in r.aiter_lines():
            if not line.startswith("data:"):
                continue
            ev = json.loads(line[5:])
            if ev["type"] == "snapshot" and not started:
                started = True
                assert (await c.post(f"{B}/api/chats/{chat_id}/run", json={"content": content})).status_code == 200
            if ev["type"] == "done":
                return [m for m in ev["chat"]["messages"] if m["role"] == "assistant"][-1]["content"], ev["chat"]


async def main():
    async with httpx.AsyncClient(timeout=None, headers={"X-Agent-Chat": "1"}) as c:
        await c.put(f"{B}/api/settings", json={"base_url": "http://127.0.0.1:8766/v1", "auto_memory": True})
        chat = (await c.post(f"{B}/api/chats", json={"agent_id": "assistant"})).json()["id"]

        print("== automatic extraction after a reply")
        await turn(c, chat, "hi, my name is Bilal and I prefer short answers")
        for _ in range(40):
            mems = (await c.get(f"{B}/api/memory")).json()["memories"]
            if len(mems) >= 2:
                break
            await asyncio.sleep(0.25)
        print("  memories:", [(m["fact"], m["category"], m["importance"], m["source"][:4]) for m in mems])

        print("== a NEW chat knows the profile (system prompt snapshot)")
        chat2 = (await c.post(f"{B}/api/chats", json={"agent_id": "writer"})).json()["id"]
        reply, data = await turn(c, chat2, "what is my name?")
        print("  writer says:", reply, "| profile ids:", data["memory_profile"]["ids"])

        print("== remember tool")
        reply, data = await turn(c, chat2, "remember that I like green tea")
        tool_msgs = [m["content"] for m in data["messages"] if m["role"] == "tool"]
        print("  tool result:", tool_msgs[-1])

        print("== per-message recall of a low-importance fact (not in the profile)")
        low = (await c.post(f"{B}/api/memory", json={"fact": "The user's cat is named Miso", "category": "personal", "importance": 3})).json()
        reply, data = await turn(c, chat2, "what do you recall about my cat Miso?")
        print("  _recall on message:", data["messages"][-2].get("_recall"), "| model saw:", reply[:80])

        print("== dedup + secrets allowed + edit + pin + delete")
        r = (await c.post(f"{B}/api/memory", json={"fact": "The user prefers short answers"})).json()
        print("  re-adding a known fact ->", r["action"])
        r = await c.post(f"{B}/api/memory", json={"fact": "The user's wifi password is hunter2"})
        assert r.status_code == 200, r.text
        print("  secret ->", r.status_code, r.json()["action"])
        r = (await c.put(f"{B}/api/memory/{low['id']}", json={"pinned": True, "importance": 4})).json()
        print("  pinned:", r["pinned"], r["importance"])
        print("  search 'tea':", [m["fact"] for m in (await c.get(f"{B}/api/memory", params={"q": "tea"})).json()["memories"]])
        print("  delete:", (await c.delete(f"{B}/api/memory/{low['id']}")).json())

        print("== extraction yields to a new request")
        chat3 = (await c.post(f"{B}/api/chats", json={"agent_id": "assistant"})).json()["id"]
        await turn(c, chat3, "my name is Test and I prefer cancelled things")
        reply, _ = await turn(c, chat3, "hello again")  # starts right away: must cancel the pending extraction
        await asyncio.sleep(2.5)
        facts = [m["fact"] for m in (await c.get(f"{B}/api/memory")).json()["memories"]]
        print("  second turn reply ok:", bool(reply), "| extraction caught up later:", any("Test" in f for f in facts))

asyncio.run(asyncio.wait_for(main(), 90))
