"""Tests for the 0.2 features: pinning, branching, JSON export, bulk delete, approve-all, model-written
titles, the workspace browser, image attachments (with the text-only fallback), live shell output,
agent ordering and prompt preview, message timestamps. Also text in any shape: exports of chats titled in
any script, file names that aren't UTF-8, lone UTF-16 surrogates in request bodies; and the agents_changed
event every client reloads its agents on.

Needs the app on AC_URL (default :8767), the normal mock on :8766 and one started with
MOCK_NO_VISION=1 on :8770.
"""
import asyncio
import io
import json
import os
import struct
import zlib
from urllib.parse import quote

import httpx

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")
MOCK = os.environ.get("MOCK_URL", "http://127.0.0.1:8766/v1")
MOCK_NO_VISION = os.environ.get("MOCK_NO_VISION_URL", "http://127.0.0.1:8770/v1")


async def events(c, url):
    async with c.stream("GET", url) as r:
        async for line in r.aiter_lines():
            if line.startswith("data:"):
                yield json.loads(line[5:])


async def run_turn(c, chat_id, content=None, body=None, on_event=None):
    seen, started = [], False
    async for ev in events(c, f"{B}/api/chats/{chat_id}/stream"):
        seen.append(ev)
        if ev["type"] == "snapshot" and not started:
            started = True
            r = await c.post(f"{B}/api/chats/{chat_id}/run", json=body or {"content": content})
            assert r.status_code == 200, r.text
        elif on_event:
            await on_event(ev)
        if ev["type"] == "done":
            return seen


async def new_chat(c, agent):
    return (await c.post(f"{B}/api/chats", json={"agent_id": agent})).json()["id"]


async def get_chat(c, chat_id):
    return (await c.get(f"{B}/api/chats/{chat_id}")).json()


def png_bytes(w=4, h=4):
    """A tiny valid PNG (so the upload is recognised as an image)."""
    raw = b"".join(b"\x00" + b"\xff\x00\x00" * w for _ in range(h))
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xffffffff)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


async def test_health_and_settings(c):
    print("\n== health + settings")
    h = (await c.get(f"{B}/api/health")).json()
    assert h["ok"] and h["version"], h
    s = (await c.put(f"{B}/api/settings", json={"base_url": MOCK, "auto_title": True, "auto_memory": False})).json()
    assert s["auto_title"] is True and s["auto_memory"] is False
    print("  version", h["version"], "| auto_title on")


async def test_title_and_timestamps(c):
    print("\n== the model names the chat after the first reply; messages carry timestamps")
    chat = await new_chat(c, "assistant")
    titles = []

    async def watch():
        async for ev in events(c, f"{B}/api/events"):
            if ev["type"] == "chat_title" and ev["chat_id"] == chat:
                titles.append(ev["title"])
                return

    watcher = asyncio.create_task(watch())
    seen = await run_turn(c, chat, "tell me about the Roman aqueducts please")
    msgs = seen[-1]["chat"]["messages"]
    assert all(m.get("_ts") for m in msgs if m["role"] in ("user", "assistant")), "every message gets _ts"
    assert seen[-1]["chat"]["title_auto"] is True
    await asyncio.wait_for(watcher, 10)
    data = await get_chat(c, chat)
    print("  title:", data["title"], "| event:", titles)
    assert data["title"] == "Titled: tell me about" and titles == [data["title"]]
    assert data["title_auto"] is False
    # a user-chosen title is never replaced
    chat2 = await new_chat(c, "assistant")
    await c.patch(f"{B}/api/chats/{chat2}", json={"title": "My own name"})
    await run_turn(c, chat2, "hello")
    await asyncio.sleep(2.5)
    assert (await get_chat(c, chat2))["title"] == "My own name"
    print("  renamed chat keeps its name")
    return chat


async def test_pin_fork_export_delete(c, chat):
    print("\n== pin, branch, export, bulk delete")
    r = (await c.patch(f"{B}/api/chats/{chat}", json={"pinned": True})).json()
    assert r["pinned"] is True
    listing = (await c.get(f"{B}/api/chats")).json()
    assert listing[0]["id"] == chat, "pinned chats come first"
    assert listing[0]["messages"] == 2 and listing[0]["last_role"] == "assistant"
    await run_turn(c, chat, "second question")
    fork = (await c.post(f"{B}/api/chats/{chat}/fork", json={"upto": 2})).json()
    fdata = await get_chat(c, fork["id"])
    print("  fork:", fdata["title"], "| messages:", len(fdata["messages"]), "| pinned:", fdata.get("pinned"))
    assert len(fdata["messages"]) == 2 and fdata["title"].endswith("(branch)") and not fdata.get("pinned")
    assert (await c.post(f"{B}/api/chats/{chat}/fork", json={"upto": 99})).status_code == 400
    # the branch continues on its own
    await run_turn(c, fork["id"], "different follow-up")
    assert len((await get_chat(c, fork["id"]))["messages"]) == 4 and len((await get_chat(c, chat))["messages"]) == 4
    r = await c.get(f"{B}/api/chats/{chat}/export.json")
    assert r.status_code == 200 and r.json()["chat"]["id"] == chat and r.json()["agent"]["id"] == "assistant"
    assert "attachment" in r.headers["content-disposition"]
    md = (await c.get(f"{B}/api/chats/{chat}/export.md")).text
    assert md.startswith("# ") and "## You" in md
    # a title in any script: the file name travels as filename*= (a header value must be Latin-1)
    await c.patch(f"{B}/api/chats/{chat}", json={"title": "محادثة 日本語 Ελληνικά"})
    for kind in ("md", "json"):
        r = await c.get(f"{B}/api/chats/{chat}/export.{kind}")
        cd = r.headers.get("content-disposition", "")
        assert r.status_code == 200 and cd.isascii() and cd.endswith(f"filename*=UTF-8''{quote('محادثة-日本語-Ελληνικά')}.{kind}"), (kind, r.status_code, cd)
    assert (await c.get(f"{B}/api/chats/{chat}/export.md")).text.startswith("# محادثة 日本語 Ελληνικά\n")
    r = (await c.post(f"{B}/api/chats/delete", json={"ids": [chat, fork["id"], "nope"]})).json()
    assert r["ok"]
    assert (await c.get(f"{B}/api/chats/{chat}")).status_code == 404
    assert (await c.get(f"{B}/api/chats/{fork['id']}")).status_code == 404
    print("  exports ok, both deleted")


async def test_approve_all(c):
    print("\n== approve all for the rest of the run")
    await c.put(f"{B}/api/settings", json={"bypass_approvals": False})
    chat = await new_chat(c, "coder")
    approvals, bypassed = [], []

    async def on(ev):
        if ev["type"] == "approval":
            approvals.append(ev["name"])
            await c.post(f"{B}/api/chats/{chat}/approvals/{ev['approval_id']}", json={"approve": True, "all": True})
        elif ev["type"] == "approval_bypassed":
            bypassed.append(ev.get("reason"))

    seen = await run_turn(c, chat, "shell twice: echo one", on_event=on)
    done = [e for e in seen if e["type"] == "approval_done"]
    results = [e["content"] for e in seen if e["type"] == "tool_result"]
    assert approvals == ["run_shell"] and done[0].get("all") is True, (approvals, done)
    assert bypassed == ["run"] and len(results) == 2 and "one again" in results[1], (bypassed, results)
    saved = seen[-1]["chat"]["messages"][4]
    assert saved.get("_bypassed") is True, "the auto-approved call is marked"
    assert saved.get("_bypass_reason") == "run", "and why, so a saved chat still says it was this run's approve-all"
    assert "_bypass_reason" not in seen[-1]["chat"]["messages"][2], "the call that asked has none"
    seen = await run_turn(c, chat, "shell: echo two", on_event=on)
    assert approvals == ["run_shell", "run_shell"], "a new run asks again"
    await c.put(f"{B}/api/settings", json={"bypass_approvals": True})
    try:
        seen = await run_turn(c, chat, "shell: echo three", on_event=on)
    finally:
        await c.put(f"{B}/api/settings", json={"bypass_approvals": False})
    assert approvals == ["run_shell", "run_shell"] and bypassed == ["run", "settings"], (approvals, bypassed)
    saved = [m for m in seen[-1]["chat"]["messages"] if m["role"] == "tool"][-1]
    assert saved.get("_bypassed") is True and saved.get("_bypass_reason") == "settings", saved
    print("  first call asked (approved for the run), later run asked again:", approvals, bypassed)


async def test_shell_progress(c):
    print("\n== live output while a shell command runs")
    chat = await new_chat(c, "coder")
    progress = []

    async def on(ev):
        if ev["type"] == "approval":
            await c.post(f"{B}/api/chats/{chat}/approvals/{ev['approval_id']}", json={"approve": True})
        elif ev["type"] == "tool_progress":
            progress.append(ev["content"])

    seen = await run_turn(c, chat, "slow shell", on_event=on)
    result = next(e["content"] for e in seen if e["type"] == "tool_result")
    print(f"  {len(progress)} progress updates; last: {progress[-1].strip().splitlines() if progress else None}")
    assert len(progress) >= 2 and "tick1" in progress[-1]
    assert result.startswith("exit code 0") and "tick4" in result


async def test_workspace(c):
    print("\n== workspace browser")
    chat = await new_chat(c, "coder")
    await run_turn(c, chat, "write: workspace browser test")
    ls = (await c.get(f"{B}/api/workspace", params={"agent_id": "coder"})).json()
    names = [e["name"] for e in ls["entries"]]
    assert "notes.txt" in names, names
    r = await c.get(f"{B}/api/workspace/file", params={"agent_id": "coder", "path": "notes.txt"})
    assert r.status_code == 200 and r.text == "workspace browser test" and r.headers["content-security-policy"] == "sandbox"
    r = await c.get(f"{B}/api/workspace", params={"agent_id": "coder", "path": "../"})
    assert r.status_code == 400, r.text
    r = await c.get(f"{B}/api/workspace/file", params={"agent_id": "coder", "path": "../../etc/passwd"})
    assert r.status_code == 400
    # an html file is served as plain text (never as a page on this origin)
    await c.post(f"{B}/api/chats/{chat}/upload", params={"name": "evil.html"}, content=b"<script>alert(1)</script>")
    r = await c.get(f"{B}/api/workspace/file", params={"agent_id": "coder", "path": "uploads/evil.html"})
    assert r.headers["content-type"].startswith("text/plain"), r.headers["content-type"]
    r = await c.get(f"{B}/api/workspace/file", params={"agent_id": "coder", "path": "uploads/evil.html", "download": "true"})
    assert "attachment" in r.headers["content-disposition"]
    # file names in any script, and ones that aren't UTF-8 at all: listed (not a 500), downloadable
    sub = f"names-{os.getpid()}"
    folder = os.path.join(ls["workspace"], sub)
    os.makedirs(folder, exist_ok=True)
    named = 'résumé 日本語 "q".txt'
    for raw, text in ((b"ok.txt", "ok"), (b"bad\xff\xfe name.txt", "bad"), (named.encode(), "named")):
        with open(os.path.join(folder.encode(), raw), "w") as f:
            f.write(text)
    r = await c.get(f"{B}/api/workspace", params={"agent_id": "coder", "path": sub})
    assert r.status_code == 200, (r.status_code, r.text[:200])
    assert sorted(e["name"] for e in r.json()["entries"]) == sorted(["ok.txt", "bad\ufffd\ufffd name.txt", named]), r.json()
    r = await c.get(f"{B}/api/workspace/file", params={"agent_id": "coder", "path": f"{sub}/{named}", "download": "true"})
    assert r.status_code == 200 and r.text == "named" and r.headers["content-disposition"].endswith(f"filename*=UTF-8''{quote(named, safe='')}"), r.headers
    print("  listing, file, sandbox, html-as-text, names in any script or none all ok")


async def test_images(c):
    print("\n== image attachments reach a vision model as data URLs")
    chat = await new_chat(c, "assistant")
    up = (await c.post(f"{B}/api/chats/{chat}/upload", params={"name": "pic.png"}, content=png_bytes())).json()
    assert up["image"] is True and up["text"] is None, up
    seen = await run_turn(c, chat, body={"content": "describe the image", "images": [up["path"]]})
    msgs = seen[-1]["chat"]["messages"]
    print("  user _images:", msgs[0].get("_images"), "| reply:", msgs[1]["content"])
    assert msgs[0]["_images"] == [up["path"]] and msgs[1]["content"] == "I see 1 image(s)."
    assert seen[-1]["chat"]["stats"]["context_tokens"] < 2000, "base64 must not count as context"

    print("== a model without vision gets the text only, with a notice")
    await c.put(f"{B}/api/settings", json={"base_url": MOCK_NO_VISION})
    try:
        chat = await new_chat(c, "assistant")
        up = (await c.post(f"{B}/api/chats/{chat}/upload", params={"name": "pic2.png"}, content=png_bytes())).json()
        seen = await run_turn(c, chat, body={"content": "describe the image", "images": [up["path"]]})
        notices = [e["text"] for e in seen if e["type"] == "notice"]
        reply = seen[-1]["chat"]["messages"][-1]["content"]
        print("  notice:", notices[0][:60] if notices else None, "| reply:", reply)
        assert notices and "can't take images" in notices[0] and reply == "I see no image(s)."
        assert not [e for e in seen if e["type"] == "error"]
    finally:
        await c.put(f"{B}/api/settings", json={"base_url": MOCK})


async def test_agents(c):
    print("\n== agent order + prompt preview")
    before = [a["id"] for a in (await c.get(f"{B}/api/state")).json()["agents"]]
    r = (await c.put(f"{B}/api/agents/order", json={"ids": ["coder", "assistant", "ghost"]})).json()
    after = [a["id"] for a in r]
    assert after[:2] == ["coder", "assistant"] and set(after) == set(before), after
    # editing an agent keeps its position
    coder = next(a for a in r if a["id"] == "coder")
    body = {k: v for k, v in coder.items() if k not in ("id", "order")}
    saved = (await c.put(f"{B}/api/agents/coder", json=body | {"purpose": "Edited"})).json()
    assert saved["order"] == 0 and saved["purpose"] == "Edited"
    await c.put(f"{B}/api/agents/coder", json=body)  # put it back
    r = (await c.put(f"{B}/api/agents/order", json={"ids": before})).json()
    assert [a["id"] for a in r] == before
    p = (await c.get(f"{B}/api/agents/coder/prompt")).json()
    assert "You are the Coder agent" in p["system_prompt"] and "run_shell" in p["tools"] and "Orchestrator" in p["contacts"]
    assert (await c.get(f"{B}/api/agents/nobody/prompt")).status_code == 404
    print("  reorder ok, edit keeps position, preview has", len(p["tools"]), "tools")


async def test_agents_changed(c):
    print("\n== making, editing, reordering or deleting an agent tells every client (agents_changed)")
    got, ready = [], asyncio.Event()

    async def watch():
        async for ev in events(c, f"{B}/api/events"):
            if ev["type"] == "hello":
                ready.set()
            elif ev["type"] == "agents_changed":
                got.append(ev)
                if len(got) == 4:
                    return

    watcher = asyncio.create_task(watch())
    await asyncio.wait_for(ready.wait(), 10)
    a = (await c.post(f"{B}/api/agents", json={"name": "Broadcast test", "tools": [], "memory": False})).json()
    body = {k: v for k, v in a.items() if k not in ("id", "order")}
    assert (await c.put(f"{B}/api/agents/{a['id']}", json=body | {"name": "Broadcast test 2"})).status_code == 200
    order = [x["id"] for x in (await c.get(f"{B}/api/state")).json()["agents"]]
    assert (await c.put(f"{B}/api/agents/order", json={"ids": order})).status_code == 200
    assert (await c.delete(f"{B}/api/agents/{a['id']}")).status_code == 200
    await asyncio.wait_for(watcher, 10)
    print("  create, edit, reorder, delete:", len(got), "events")


async def test_lone_surrogates(c):
    print("\n== a lone UTF-16 surrogate in a request (valid JSON) becomes U+FFFD, not a 500")
    chat = await new_chat(c, "assistant")
    raw = {"content-type": "application/json"}
    r = await c.patch(f"{B}/api/chats/{chat}", content=b'{"title": "half \\udc00 title"}', headers=raw)
    assert r.status_code == 200 and r.json()["title"] == "half \ufffd title", (r.status_code, r.text[:200])
    seen, started = [], False
    async for ev in events(c, f"{B}/api/chats/{chat}/stream"):
        seen.append(ev)
        if ev["type"] == "snapshot" and not started:
            started = True
            r = await c.post(f"{B}/api/chats/{chat}/run", content=b'{"content": "lone \\ud800 surrogate \\ud83d\\ude00"}', headers=raw)
            assert r.status_code == 200, (r.status_code, r.text[:200])
        if ev["type"] == "done":
            break
    msgs = seen[-1]["chat"]["messages"]
    assert msgs[0]["content"] == "lone \ufffd surrogate 😀" and msgs[1]["role"] == "assistant", msgs[:2]
    assert (await get_chat(c, chat))["messages"][0]["content"] == "lone \ufffd surrogate 😀"
    print("  title and message saved with U+FFFD; the reply came")


async def main():
    async with httpx.AsyncClient(timeout=None, headers={"X-Agent-Chat": "1"}) as c:
        await test_health_and_settings(c)
        chat = await test_title_and_timestamps(c)
        await test_pin_fork_export_delete(c, chat)
        await test_approve_all(c)
        await test_shell_progress(c)
        await test_workspace(c)
        await test_images(c)
        await test_agents(c)
        await test_agents_changed(c)
        await test_lone_surrogates(c)
        await c.put(f"{B}/api/settings", json={"auto_memory": True})
    print("\nall v2 tests passed")


asyncio.run(asyncio.wait_for(main(), 180))
