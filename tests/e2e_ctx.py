"""The context meter updates live: at each step, while a reply streams, and after compaction.

Needs the app on AC_URL (default :8767) wired to tests/mock_llm.py on :8766.
"""
import asyncio
import json
import os
import time

import httpx

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")
EVERY = 0.5  # runner.CONTEXT_EVERY


async def events(c, url):
    async with c.stream("GET", url) as r:
        async for line in r.aiter_lines():
            if line.startswith("data:"):
                yield json.loads(line[5:])


async def run_turn(c, chat_id, content=None, endpoint="run"):
    """Send a message (or compact). Returns [(seconds since start, event)] for the root stream once done."""
    seen, started, t0 = [], False, time.monotonic()
    async for ev in events(c, f"{B}/api/chats/{chat_id}/stream"):
        seen.append((time.monotonic() - t0, ev))
        if ev["type"] == "snapshot" and not started:
            started = True
            r = await c.post(f"{B}/api/chats/{chat_id}/{endpoint}", json={"content": content} if content else {})
            assert r.status_code == 200, r.text
        if ev["type"] == "done":
            return seen


def streaming(seen):
    """The ctx events sent while the reply streamed, the reply's time span, and its final message."""
    start = next(i for i, (_, e) in enumerate(seen) if e["type"] == "assistant_start")
    end = next(i for i, (_, e) in enumerate(seen) if e["type"] == "assistant_end")
    ctx = [e["tokens"] for _, e in seen[start:end] if e["type"] == "ctx"]
    deltas = sum(e["type"] == "delta" for _, e in seen[start:end])
    return ctx, seen[end][0] - seen[start][0], seen[end][1]["message"], deltas


def increasing(xs):
    return all(a < b for a, b in zip(xs, xs[1:]))


async def test_exact(c, chat):
    print("\n== llama-server timings: exact counts while the reply streams")
    seen = await run_turn(c, chat, "slow reply please")
    before = [e for _, e in seen if e["type"] in ("ctx", "assistant_start")]
    assert before[0]["type"] == "ctx", "the step's estimate should come before the request"
    ctx, span, msg, deltas = streaming(seen)
    prompt = msg["_stats"]["prompt_tokens"]
    print(f"  {len(ctx)} live updates over {span:.1f}s, {deltas} deltas; prompt {prompt}; first {ctx[:3]} last {ctx[-1]}")
    assert len(ctx) >= 5, ctx
    assert len(ctx) <= span / EVERY + 2, f"throttle broken: {len(ctx)} updates in {span:.1f}s"
    assert increasing(ctx), ctx
    assert all(0 < t - prompt <= deltas for t in ctx), "live count should be prompt + tokens generated so far"
    chat_now = (await c.get(f"{B}/api/chats/{chat}")).json()
    want = prompt + msg["_stats"]["completion_tokens"]
    assert chat_now["stats"]["context_tokens"] == want, (chat_now["stats"], want)
    print("  saved context after the reply is the exact usage:", want)


async def test_estimate(c, chat):
    print("\n== no per-token timings (other servers): estimated from the streamed text")
    seen = await run_turn(c, chat, "slow notimings")
    step = next(e["tokens"] for _, e in seen if e["type"] == "ctx")
    ctx, span, _, _ = streaming(seen)
    print(f"  step estimate {step}; {len(ctx)} live updates over {span:.1f}s: {ctx[:3]} … {ctx[-1]}")
    assert len(ctx) >= 5 and increasing(ctx), ctx
    assert ctx[0] > step, "the estimate should grow from the prompt estimate as text streams"


async def test_compact(c, chat):
    print("\n== manual compaction shows the smaller context right away")
    before = (await c.get(f"{B}/api/chats/{chat}")).json()["stats"]["context_tokens"]
    seen = await run_turn(c, chat, endpoint="compact")
    types = [e["type"] for _, e in seen]
    assert "compact_end" in types, [e.get("text") or e["type"] for _, e in seen]
    after = [e["tokens"] for _, e in seen[types.index("compact_end"):] if e["type"] == "ctx"]
    saved = (await c.get(f"{B}/api/chats/{chat}")).json()["stats"]["context_tokens"]
    print(f"  {before} -> {after} (saved {saved})")
    assert after and after[-1] < before and saved == after[-1]


async def main():
    async with httpx.AsyncClient(timeout=120, headers={"X-Agent-Chat": "1"}) as c:
        await c.put(f"{B}/api/settings", json={"base_url": "http://127.0.0.1:8766/v1", "auto_compact": True})
        chat = (await c.post(f"{B}/api/chats", json={"agent_id": "assistant"})).json()["id"]
        await test_exact(c, chat)
        await test_estimate(c, chat)
        await test_compact(c, chat)
    print("\nall context-meter tests passed")


asyncio.run(main())
