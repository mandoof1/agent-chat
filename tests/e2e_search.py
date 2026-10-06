"""web_search through a real SearXNG instance, driven by the Researcher agent's tool loop.

Needs the app on AC_URL (default :8767), the mock model on :8766, and SearXNG on SEARXNG_URL
(default http://127.0.0.1:8888, see ~/start-searxng.sh). This hits the live web via SearXNG.
"""
import asyncio
import json
import os
import re

import httpx

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")
SEARX = os.environ.get("SEARXNG_URL", "http://127.0.0.1:8888")


async def events(c, url):
    async with c.stream("GET", url) as r:
        async for line in r.aiter_lines():
            if line.startswith("data:"):
                yield json.loads(line[5:])


async def settings(c, **kw):
    r = await c.put(f"{B}/api/settings", json=kw)
    return r.json()


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


async def search(c, query):
    chat = (await c.post(f"{B}/api/chats", json={"agent_id": "researcher"})).json()["id"]
    evs = await run_turn(c, chat, f"search: {query}")
    results = [e["content"] for e in evs if e["type"] == "tool_result"]
    errors = [e["message"] for e in evs if e["type"] == "error"]
    assert not errors, errors
    assert len(results) == 1, results
    return results[0]


async def test_searxng(c):
    print("\n== web_search via SearXNG")
    saved = await settings(c, base_url="http://127.0.0.1:8766/v1", search_url=SEARX)
    assert saved["search_url"] == SEARX, saved
    out = await search(c, "FastAPI background tasks")
    print("  " + out[:300].replace("\n", "\n  "))
    assert not out.startswith("Error:"), out
    urls = re.findall(r"^   (https?://\S+)$", out, re.M)
    assert len(urls) >= 3, out
    assert any("fastapi.tiangolo.com" in u for u in urls), urls


async def test_unreachable(c):
    print("\n== web_search with SearXNG down gives a clear error")
    await settings(c, search_url="http://127.0.0.1:9")
    out = await search(c, "anything")
    print("  " + out)
    assert out.startswith("Error: can't reach SearXNG at http://127.0.0.1:9"), out


async def main():
    async with httpx.AsyncClient(timeout=None, headers={"X-Agent-Chat": "1"}) as c:
        try:
            await test_searxng(c)
            await test_unreachable(c)
        finally:
            await settings(c, search_url=SEARX)
    print("\nall search tests passed")

asyncio.run(asyncio.wait_for(main(), 120))
