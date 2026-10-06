"""Mail agent through the app. Needs the app on AC_URL with email configured for the pymap/aiosmtpd test servers."""
import asyncio, json, os
import httpx

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")


async def turn(c, chat_id, content, approve=None):
    started, approvals = False, []
    async with c.stream("GET", f"{B}/api/chats/{chat_id}/stream") as r:
        async for line in r.aiter_lines():
            if not line.startswith("data:"):
                continue
            ev = json.loads(line[5:])
            if ev["type"] == "snapshot" and not started:
                started = True
                await c.post(f"{B}/api/chats/{chat_id}/run", json={"content": content})
            if ev["type"] == "approval":
                approvals.append((ev["name"], ev["args"].get("to")))
                await c.post(f"{B}/api/chats/{chat_id}/approvals/{ev['approval_id']}", json={"approve": approve})
            if ev["type"] == "done":
                tools = [m["content"] for m in ev["chat"]["messages"] if m["role"] == "tool"]
                return tools[-1] if tools else None, approvals


async def main():
    async with httpx.AsyncClient(timeout=None, headers={"X-Agent-Chat": "1"}) as c:
        await c.put(f"{B}/api/settings", json={"base_url": "http://127.0.0.1:8766/v1", "auto_memory": False})
        agents = {a["id"]: a for a in (await c.get(f"{B}/api/state")).json()["agents"]}
        print("Mail agent installed by migration:", "mail" in agents, agents.get("mail", {}).get("tools"))
        print("email config (password hidden):", {k: v for k, v in (await c.get(f"{B}/api/email")).json().items() if k in ("username", "password", "has_password", "configured")})
        chat = (await c.post(f"{B}/api/chats", json={"agent_id": "mail"})).json()["id"]
        result, _ = await turn(c, chat, "check my email please")
        print("check mail ->", result.splitlines()[:3])
        result, approvals = await turn(c, chat, "send a reply to the hello email", approve=False)
        print("deny ->", approvals, "|", result)
        result, approvals = await turn(c, chat, "send a reply to the hello email", approve=True)
        print("approve ->", approvals, "|", result)

asyncio.run(asyncio.wait_for(main(), 60))
