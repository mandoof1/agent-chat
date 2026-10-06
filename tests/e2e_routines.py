"""Routines: schedule math, API validation, run-now, and the scheduler firing a due routine.
Needs the app on AC_URL started with AGENT_CHAT_ROUTINE_TICK=1 and the mock model on :8766."""
import asyncio, json, os, sys, time
from datetime import datetime
import httpx

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from app import routines  # noqa: E402

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")
DATA = os.environ["AGENT_CHAT_DATA"]


def schedule_math():
    thu = datetime(2026, 10, 1, 9, 0)  # a Thursday
    wk = {"type": "daily", "time": "08:00", "days": [0, 1, 2, 3, 4]}
    print("  weekdays 08:00 after Thu 09:00 ->", datetime.fromtimestamp(routines.next_run(wk, thu)).strftime("%a %d %H:%M"))
    fri = datetime(2026, 10, 2, 9, 0)
    print("  weekdays 08:00 after Fri 09:00 ->", datetime.fromtimestamp(routines.next_run(wk, fri)).strftime("%a %d %H:%M"))
    same = {"type": "daily", "time": "20:00", "days": [3]}
    print("  Thu 20:00 after Thu 09:00 ->", datetime.fromtimestamp(routines.next_run(same, thu)).strftime("%a %d %H:%M"))
    iv = {"type": "interval", "minutes": 90}
    print("  every 90 min after 09:00 ->", datetime.fromtimestamp(routines.next_run(iv, thu)).strftime("%H:%M"))


async def main():
    print("== schedule math")
    schedule_math()
    async with httpx.AsyncClient(timeout=None, headers={"X-Agent-Chat": "1"}) as c:
        await c.put(f"{B}/api/settings", json={"base_url": "http://127.0.0.1:8766/v1", "auto_memory": False})
        print("== validation")
        for bad in [{"name": "x", "agent_id": "assistant", "prompt": "p", "schedule": {"type": "daily", "time": "25:00"}},
                    {"name": "x", "agent_id": "assistant", "prompt": "p", "schedule": {"type": "daily", "time": "08:00", "days": []}},
                    {"name": "x", "agent_id": "assistant", "prompt": "p", "schedule": {"type": "interval", "minutes": 1}},
                    {"name": "x", "agent_id": "nobody", "prompt": "p", "schedule": {"type": "interval", "minutes": 60}}]:
            r = await c.post(f"{B}/api/routines", json=bad)
            print("  ", r.status_code, r.json()["detail"])
        print("== create + run now")
        r = (await c.post(f"{B}/api/routines", json={"name": "Daily hello", "agent_id": "assistant", "prompt": "hello there",
                                                    "schedule": {"type": "daily", "time": "08:00", "days": [0, 1, 2, 3, 4]}})).json()
        print("   created, next run:", datetime.fromtimestamp(r["next_run"]).strftime("%a %H:%M"))
        res = (await c.post(f"{B}/api/routines/{r['id']}/run")).json()
        print("   run now ->", res["status"], "| chat:", res["chat_id"])
        await asyncio.sleep(1.5)
        chat = (await c.get(f"{B}/api/chats/{res['chat_id']}")).json()
        print("   chat title:", chat["title"], "| messages:", [m["role"] for m in chat["messages"]], "| prompt:", chat["messages"][0]["content"][:60].replace("\n", " "))
        print("== scheduler fires a due routine (next_run forced into the past)")
        items = json.loads(open(f"{DATA}/routines.json").read())
        items[0]["next_run"] = time.time() - 30
        open(f"{DATA}/routines.json", "w").write(json.dumps(items))
        await asyncio.sleep(4)
        r2 = (await c.get(f"{B}/api/routines")).json()[0]
        chat = (await c.get(f"{B}/api/chats/{r2['chat_id']}")).json()
        print("   last_status:", r2["last_status"], "| next run moved to:", datetime.fromtimestamp(r2["next_run"]).strftime("%a %H:%M"),
              "| user messages in its chat:", sum(m["role"] == "user" for m in chat["messages"]))
        print("== missed long ago -> skipped, not run")
        items = json.loads(open(f"{DATA}/routines.json").read())
        items[0]["next_run"] = time.time() - 5 * 3600
        open(f"{DATA}/routines.json", "w").write(json.dumps(items))
        await asyncio.sleep(3)
        r3 = (await c.get(f"{B}/api/routines")).json()[0]
        chat = (await c.get(f"{B}/api/chats/{r3['chat_id']}")).json()
        print("   last_status:", r3["last_status"], "| user messages still:", sum(m["role"] == "user" for m in chat["messages"]))
        print("== edit + disable + delete")
        r4 = (await c.put(f"{B}/api/routines/{r['id']}", json={"enabled": False, "schedule": {"type": "interval", "minutes": 120}})).json()
        print("   enabled:", r4["enabled"], "| schedule:", r4["schedule"])
        print("   delete:", (await c.delete(f"{B}/api/routines/{r['id']}")).json(), "| chat kept:", (await c.get(f"{B}/api/chats/{r['chat_id'] or res['chat_id']}")).status_code)

asyncio.run(asyncio.wait_for(main(), 60))
