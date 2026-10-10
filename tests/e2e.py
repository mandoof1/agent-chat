"""End-to-end test against a running app (port 8765) wired to tests/mock_llm.py."""
import asyncio, json, os, sys
from collections import Counter
import httpx

B = os.environ.get("AC_URL", "http://127.0.0.1:8765")
MOCK = os.environ.get("MOCK_URL", "http://127.0.0.1:8766/v1")


async def scenario(agent, content, approve=None, stop_after=None, peek=False, run_body=None, chat_id=None):
    async with httpx.AsyncClient(timeout=None, headers={"X-Agent-Chat": "1"}) as c:
        if not chat_id:
            chat_id = (await c.post(f"{B}/api/chats", json={"agent_id": agent})).json()["id"]
        types, paths, peeked = [], set(), None
        started = False
        async with c.stream("GET", f"{B}/api/chats/{chat_id}/stream") as r:
            async for line in r.aiter_lines():
                if not line.startswith("data:"):
                    continue
                ev = json.loads(line[5:])
                types.append(ev["type"])
                if "path" in ev:
                    paths.add(tuple(ev["path"]))
                if ev["type"] == "snapshot" and not started:
                    started = True
                    resp = await c.post(f"{B}/api/chats/{chat_id}/run", json=run_body or {"content": content})
                    assert resp.status_code == 200, resp.text
                elif ev["type"] == "approval":
                    await c.post(f"{B}/api/chats/{chat_id}/approvals/{ev['approval_id']}", json={"approve": approve})
                elif ev["type"] == "delta" and types.count("delta") == 20 and peek:
                    # a second viewer joining mid-run must get snapshot + full replay
                    async with c.stream("GET", f"{B}/api/chats/{chat_id}/stream") as r2:
                        first = []
                        async for l2 in r2.aiter_lines():
                            if l2.startswith("data:"):
                                first.append(json.loads(l2[5:]))
                                if len(first) == 3:
                                    break
                        peeked = [(e["type"], e.get("running")) for e in first]
                elif stop_after and ev["type"] == "delta" and types.count("delta") == stop_after:
                    await c.post(f"{B}/api/chats/{chat_id}/stop")
                elif ev["type"] == "done":
                    msgs = ev["chat"]["messages"]
                    return chat_id, Counter(types), paths, msgs, peeked, ev["chat"]


def show(name, res):
    chat_id, types, paths, msgs, peeked, chat = res
    print(f"\n== {name}")
    print("  events:", dict(types))
    print("  paths:", sorted(paths))
    for m in msgs:
        extra = {k: (v if k != "_sub" else f"<{len(v['messages'])} sub msgs, agent={v['agent_id']}>") for k, v in m.items() if k.startswith("_") and k != "_stats"}
        tc = [t["function"]["name"] for t in m.get("tool_calls") or []]
        print(f"  {m['role']:9} {repr((m.get('content') or '')[:70]):74} {tc or ''} {extra or ''}")
    if peeked:
        print("  mid-run second viewer got:", peeked)
    print("  stats:", chat.get("stats"))
    for key, ref in (chat.get("threads") or {}).items():
        thread = httpx.get(f"{B}/api/chats/{ref}").json()["messages"]
        print(f"  thread {key} (sub-chat {ref}): " + " / ".join(f"{m['role']}:{(m.get('content') or '')[:45]!r}" for m in thread if m["role"] != "tool"))


async def main():
    show("plain chat", await scenario("assistant", "hello there"))
    show("shell approved", await scenario("coder", "shell: echo hi && pwd", approve=True))
    show("shell denied", await scenario("coder", "shell: echo should-not-run", approve=False))
    show("write file", await scenario("coder", "write: hello from the test"))
    show("delegation", await scenario("orchestrator", "please delegate this", approve=True))
    qa = await scenario("orchestrator", "qa")
    show("question round-trip", qa)
    show("loop guard (callee calls its caller)", await scenario("orchestrator", "boss test"))
    show("regenerate drops that turn's agent threads", await scenario("orchestrator", None, run_body={"from_index": 0, "content": "hello"}, chat_id=qa[0]))
    show("stop + mid-run viewer", await scenario("assistant", "slow please", stop_after=60, peek=True))
    cid = (await scenario("assistant", "first question"))[0]
    show("regenerate", await scenario("assistant", None, run_body={"from_index": 1}, chat_id=cid))
    show("edit", await scenario("assistant", None, run_body={"content": "edited question", "from_index": 0}, chat_id=cid))
    async with httpx.AsyncClient(headers={"X-Agent-Chat": "1"}) as c:
        await c.put(f"{B}/api/settings", json={"base_url": "http://127.0.0.1:8799/v1"})
    show("server down", await scenario("assistant", "hi"))
    async with httpx.AsyncClient(headers={"X-Agent-Chat": "1"}) as c:
        await c.put(f"{B}/api/settings", json={"base_url": MOCK})
        r = await c.post(f"{B}/api/chats/{cid}/run", json={})
        print("\nrun with nothing to answer ->", r.status_code, r.text)

asyncio.run(asyncio.wait_for(main(), 120))
