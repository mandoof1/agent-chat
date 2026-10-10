"""Calendar through the app: connect a local .ics, ask the Planner (mock model on :8766)."""
import asyncio, json, os, sys
from datetime import date, datetime, timedelta, timezone
import httpx, icalendar

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")
MOCK = os.environ.get("MOCK_URL", "http://127.0.0.1:8766/v1")
ICS = os.environ.get("TEST_ICS", "/tmp/agent-chat-test.ics")


def make_ics():
    cal = icalendar.Calendar(); cal.add("prodid", "-//test//"); cal.add("version", "2.0")
    t = date.today() + timedelta(days=1)
    e = icalendar.Event(); e.add("uid", "1"); e.add("summary", "Capstone demo")
    e.add("dtstart", datetime(t.year, t.month, t.day, 14, 0, tzinfo=timezone.utc)); e.add("dtend", datetime(t.year, t.month, t.day, 15, 0, tzinfo=timezone.utc))
    cal.add_component(e)
    open(ICS, "wb").write(cal.to_ical())


async def main():
    make_ics()
    async with httpx.AsyncClient(timeout=None, headers={"X-Agent-Chat": "1"}) as c:
        await c.put(f"{B}/api/settings", json={"base_url": MOCK, "auto_memory": False})
        agents = {a["id"]: a for a in (await c.get(f"{B}/api/state")).json()["agents"]}
        print("Planner installed:", "planner" in agents, agents.get("planner", {}).get("tools"))
        print("bad link ->", (await c.post(f"{B}/api/calendars", json={"name": "x", "url": "nope"})).json()["detail"][:50])
        print("add ->", (await c.post(f"{B}/api/calendars", json={"name": "Uni", "url": ICS})).json()["name"])
        print("test ->", (await c.post(f"{B}/api/calendars/test")).json()["message"])
        chat = (await c.post(f"{B}/api/chats", json={"agent_id": "planner"})).json()["id"]
        started = False
        async with c.stream("GET", f"{B}/api/chats/{chat}/stream") as r:
            async for line in r.aiter_lines():
                if not line.startswith("data:"):
                    continue
                ev = json.loads(line[5:])
                if ev["type"] == "snapshot" and not started:
                    started = True
                    await c.post(f"{B}/api/chats/{chat}/run", json={"content": "what's on my calendar?"})
                if ev["type"] == "tool_result":
                    print("tool result:", ev["content"].splitlines()[:2])
                if ev["type"] == "done":
                    break

asyncio.run(asyncio.wait_for(main(), 60))
