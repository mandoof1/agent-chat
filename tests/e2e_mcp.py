"""MCP servers as agent tools (app/mcp.py), against tests/fake_mcp.py.

Needs the app on AC_URL (default :8767) and the mock model on MOCK_URL (default :8766).
Covers: settings validation, the agent editor's tool list, direct tools (<server>__<tool>),
structured and image results, tool errors, approval for destructive tools, progress, Stop
cancelling the call on the server, timeouts, a crashed server restarting, and a big catalog
offered through find_tools + call_tool instead.
"""
import asyncio
import json
import os
import sys
from pathlib import Path

import httpx

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")
MOCK = os.environ.get("MOCK_URL", "http://127.0.0.1:8766/v1")
FAKE = str(Path(__file__).resolve().parent / "fake_mcp.py")


def fake(**extra):
    return {"mcpServers": {"fake": {"command": sys.executable, "args": [FAKE], **extra}}}


async def events(c, url):
    async with c.stream("GET", url) as r:
        async for line in r.aiter_lines():
            if line.startswith("data:"):
                yield json.loads(line[5:])


async def turn(c, chat_id, content, approve=None, stop_on_progress=False):
    """Send a message; return (tool results, all events) once the run is done."""
    seen, started = [], False
    async for ev in events(c, f"{B}/api/chats/{chat_id}/stream"):
        seen.append(ev)
        if ev["type"] == "snapshot" and not started:
            started = True
            r = await c.post(f"{B}/api/chats/{chat_id}/run", json={"content": content})
            assert r.status_code == 200, r.text
        elif ev["type"] == "approval":
            await c.post(f"{B}/api/chats/{chat_id}/approvals/{ev['approval_id']}", json={"approve": approve})
        elif ev["type"] == "tool_progress" and stop_on_progress:
            stop_on_progress = False
            await c.post(f"{B}/api/chats/{chat_id}/stop")
        elif ev["type"] == "done":
            return [e["content"] for e in seen if e["type"] == "tool_result"], seen


async def put(c, **kw):
    return await c.put(f"{B}/api/settings", json=kw)


async def new_chat(c, agent_id):
    return (await c.post(f"{B}/api/chats", json={"agent_id": agent_id})).json()["id"]


async def one(c, chat, content, **kw):
    results, evs = await turn(c, chat, content, **kw)
    errors = [e for e in evs if e["type"] == "error"]
    assert not errors, errors
    assert len(results) == 1, (results, [e["type"] for e in evs])
    print(f"  {content[:60]!r} -> {results[0][:120]!r}")
    return results[0], evs


async def test_settings(c):
    print("\n== settings validation")
    r = await put(c, mcp_servers={"mcpServers": {"bad name!": {"command": "x"}}})
    assert r.status_code == 400 and "letters, digits" in r.text, r.text
    r = await put(c, mcp_servers={"mcpServers": {"remote": {"url": "https://example.com/mcp"}}})
    assert r.status_code == 400 and "only local servers" in r.text, r.text
    r = await put(c, mcp_servers={"mcpServers": {"x": {"command": "x", "confirm": "sometimes"}}})
    assert r.status_code == 400 and "confirm must be" in r.text, r.text
    r = await put(c, mcp_servers=fake())
    assert r.status_code == 200, r.text
    assert r.json()["mcp_servers"] == {"fake": {"command": sys.executable, "args": [FAKE], "env": {}}}, r.json()
    st = (await c.get(f"{B}/api/state")).json()
    assert any(t["name"] == "mcp:fake" and t["group"] == "MCP servers" for t in st["tools"]), st["tools"]
    status = (await c.get(f"{B}/api/mcp")).json()
    assert status == [{"name": "fake", "command": f"{sys.executable} {FAKE}", "running": False, "error": "",
                       "tools": 0, "listed": False, "disabled": False}], status
    t = (await c.post(f"{B}/api/mcp/fake/test")).json()
    print(f"  test: {t['tools']} tools, {t['mode']}, ~{t['tokens']} tokens, server {t['server']}")
    assert t["running"] and t["tools"] == 9 and t["mode"] == "direct" and t["server"]["name"] == "fake-mcp", t
    assert (await c.post(f"{B}/api/mcp/nope/test")).status_code == 404


async def test_direct(c, agent_id):
    print("\n== a small server's tools go to the model directly")
    prompt = (await c.get(f"{B}/api/agents/{agent_id}/prompt")).json()
    assert "fake__echo" in prompt["tools"] and "find_tools" not in prompt["tools"], prompt["tools"]
    assert "fake (9 tools; its tools are in your tool list): Fake server for tests." in prompt["system_prompt"], prompt["system_prompt"]
    chat = await new_chat(c, agent_id)
    out, _ = await one(c, chat, "mcp: fake__echo {\"text\": \"hi there\"}")
    assert out == "echo: hi there", out
    out, _ = await one(c, chat, "mcp: fake__add {\"a\": 2, \"b\": 3}")
    assert json.loads(out) == {"sum": 5}, out
    out, _ = await one(c, chat, "mcp: fake__broken {}")
    assert out == "Error: it broke", out
    out, _ = await one(c, chat, "mcp: fake__picture {}")
    assert out.startswith("here it is\n\n[image/png saved to ") and "/mcp/fake-" in out, out
    saved = Path(out.split("saved to ")[1].split(" (")[0])
    assert saved.exists() and saved.read_bytes()[:4] == b"\x89PNG", saved
    out, _ = await one(c, chat, "mcp: fake__client_answers {}")
    assert json.loads(out) == {"s-ping": "result", "s-roots": -32601}, out  # we answer ping, refuse the rest

    print("\n== destructive tools ask first")
    out, evs = await one(c, chat, "mcp: fake__wipe {\"target\": \"disk\"}", approve=False)
    appr = [e for e in evs if e["type"] == "approval"]
    assert len(appr) == 1 and appr[0]["name"] == "fake/wipe" and appr[0]["args"] == {"target": "disk"}, appr
    assert out.startswith("The user did not allow this tool call"), out
    out, _ = await one(c, chat, "mcp: fake__wipe {\"target\": \"disk\"}", approve=True)
    assert out == "wiped disk", out

    print("\n== progress shows on the tool card")
    out, evs = await one(c, chat, "mcp: fake__slow {\"steps\": 3}")
    prog = [e["content"] for e in evs if e["type"] == "tool_progress"]
    assert out == "slow done" and prog and prog[-1] == "1/3 step 1\n2/3 step 2\n3/3 step 3", (out, prog)

    print("\n== Stop cancels the call on the server")
    _, evs = await turn(c, chat, "mcp: fake__slow {\"steps\": 30}", stop_on_progress=True)
    assert not [e for e in evs if e["type"] == "tool_result"], "stopped call still returned"
    await asyncio.sleep(0.5)
    out, _ = await one(c, chat, "mcp: fake__last_cancel {}")
    assert out == "stopped by the user", out
    return chat


async def test_crash(c, chat):
    print("\n== a crashed server reports why and starts again on the next call")
    out, _ = await one(c, chat, "mcp: fake__crash {}")
    assert out.startswith("Error from MCP server fake: the server exited (code 3):") and "fatal: crashed on purpose" in out, out
    status = (await c.get(f"{B}/api/mcp")).json()[0]
    assert not status["running"] and "code 3" in status["error"], status
    out, evs = await one(c, chat, "mcp: fake__echo {\"text\": \"back\"}")
    assert out == "echo: back", out
    assert any(e["type"] == "notice" and "Starting MCP server fake" in e["text"] for e in evs), "no start notice"


async def test_timeout(c, agent_id):
    print("\n== a per-server timeout cancels a slow call")
    assert (await put(c, mcp_servers=fake(timeout=1))).status_code == 200
    chat = await new_chat(c, agent_id)
    out, _ = await one(c, chat, "mcp: fake__slow {\"steps\": 10}")
    assert out == "Error from MCP server fake: no answer within 1 s (the call was cancelled)", out
    out, _ = await one(c, chat, "mcp: fake__last_cancel {}")
    assert out == "timed out", out


async def test_search(c, agent_id):
    print("\n== a big catalog is searched with find_tools and run with call_tool")
    assert (await put(c, mcp_servers=fake(env={"FAKE_MCP_PAD": "60"}, confirm="never"))).status_code == 200
    status = (await c.get(f"{B}/api/mcp")).json()[0]
    assert not status["running"], status  # changed settings stop the old process
    chat = await new_chat(c, agent_id)
    _, evs = await turn(c, chat, "which tools")
    answer = next(e for e in reversed(evs) if e["type"] == "done")["chat"]["messages"][-1]["content"]
    print(f"  {answer}")
    assert "call_tool" in answer and "find_tools" in answer and "fake__echo" not in answer, answer
    prompt = (await c.get(f"{B}/api/agents/{agent_id}/prompt")).json()
    assert "fake (69 tools; find its tools with find_tools and run them with call_tool)" in prompt["system_prompt"]
    out, _ = await one(c, chat, "mcp: find_tools {\"query\": \"echo text\"}")
    assert out.startswith("## fake/echo\nEcho the given text back.\nArguments: {\"type\":\"object\"") and "$schema" not in out, out
    out, _ = await one(c, chat, "mcp: find_tools {\"query\": \"\"}")
    assert out.startswith("fake (69 tools): echo, add, wipe,") and "pad_059" in out, out
    out, _ = await one(c, chat, "mcp: find_tools {\"query\": \"padding\"}")
    assert out.count("## fake/pad_") == 8 and "52 more match" in out, out
    out, _ = await one(c, chat, "mcp: find_tools {\"query\": \"zebra\"}")
    assert out.startswith("No tool matches 'zebra'"), out
    out, _ = await one(c, chat, "mcp: call_tool {\"tool\": \"fake/echo\", \"arguments\": {\"text\": \"found\"}}")
    assert out == "echo: found", out
    out, _ = await one(c, chat, "mcp: call_tool {\"tool\": \"echo\", \"arguments\": \"{\\\"text\\\": \\\"as a string\\\"}\"}")
    assert out == "echo: as a string", out
    out, evs = await one(c, chat, "mcp: call_tool {\"tool\": \"fake/wipe\", \"arguments\": {\"target\": \"x\"}}")
    assert out == "wiped x" and not [e for e in evs if e["type"] == "approval"], out  # confirm: never
    out, _ = await one(c, chat, "mcp: call_tool {\"tool\": \"fake/nothing\", \"arguments\": {}}")
    assert out.startswith("Error: no MCP tool 'fake/nothing'"), out


async def test_broken_command(c, agent_id):
    print("\n== a server that can't start: the turn goes on without it")
    assert (await put(c, mcp_servers={"mcpServers": {"fake": {"command": "/nonexistent/mcp-server"}}})).status_code == 200
    chat = await new_chat(c, agent_id)
    _, evs = await turn(c, chat, "which tools")
    notes = [e["text"] for e in evs if e["type"] == "notice"]
    assert any(n.startswith("MCP server fake didn't start: can't run /nonexistent/mcp-server") for n in notes), notes
    assert not [e for e in evs if e["type"] == "error"], evs


async def main():
    async with httpx.AsyncClient(timeout=60, headers={"X-Agent-Chat": "1"}) as c:
        await put(c, base_url=MOCK)
        agent = (await c.post(f"{B}/api/agents", json={"name": "MCP tester", "tools": ["mcp:fake"], "memory": False,
                                                        "delegate_all": False})).json()
        try:
            await test_settings(c)
            chat = await test_direct(c, agent["id"])
            await test_crash(c, chat)
            await test_timeout(c, agent["id"])
            await test_search(c, agent["id"])
            await test_broken_command(c, agent["id"])
        finally:
            await put(c, mcp_servers={})
            await c.delete(f"{B}/api/agents/{agent['id']}")
        assert (await c.get(f"{B}/api/mcp")).json() == []
    print("\nall MCP tests passed")

asyncio.run(asyncio.wait_for(main(), 180))
