"""Routines: agent tasks that run on a schedule (e.g. a morning email briefing).

Each routine owns one chat. A run appends the routine's prompt to that chat as a user
message and starts the agent, exactly like typing it yourself, so you can read every run and
reply to follow up. Routines only run while the app is running. If the app was off at the
scheduled time, a run missed by less than two hours still happens on start-up; older ones are
skipped.
"""

import asyncio
import json
import os
import re
import time
from datetime import datetime, timedelta

from . import store

FILE = store.DATA / "routines.json"
TICK = int(os.environ.get("AGENT_CHAT_ROUTINE_TICK", "20"))  # seconds between schedule checks
GRACE = 2 * 3600        # run a missed routine if we're at most this late
MIN_INTERVAL = 5        # minutes


def load() -> list[dict]:
    try:
        return json.loads(FILE.read_text())
    except FileNotFoundError:
        return []


def save(items: list[dict]) -> None:
    store._write(FILE, items)


def get(routine_id: str) -> dict | None:
    return next((r for r in load() if r["id"] == routine_id), None)


def validate(data: dict) -> dict:
    """Normalize a routine from the API. Raises ValueError with a readable message."""
    name = str(data.get("name") or "").strip()
    prompt = str(data.get("prompt") or "").strip()
    if not name or not prompt:
        raise ValueError("a routine needs a name and a prompt")
    if not store.get_agent(str(data.get("agent_id") or "")):
        raise ValueError("unknown agent")
    s = data.get("schedule") or {}
    if s.get("type") == "daily":
        if not re.fullmatch(r"([01]\d|2[0-3]):[0-5]\d", str(s.get("time", ""))):
            raise ValueError("time must look like 08:30")
        raw_days = s.get("days")
        days = sorted({int(d) for d in (range(7) if raw_days is None else raw_days) if 0 <= int(d) <= 6})
        if not days:
            raise ValueError("pick at least one day")
        schedule = {"type": "daily", "time": s["time"], "days": days}
    elif s.get("type") == "interval":
        minutes = int(s.get("minutes") or 0)
        if minutes < MIN_INTERVAL:
            raise ValueError(f"the interval must be at least {MIN_INTERVAL} minutes")
        schedule = {"type": "interval", "minutes": minutes}
    else:
        raise ValueError("schedule type must be daily or interval")
    return {"name": name[:80], "prompt": prompt, "agent_id": data["agent_id"], "schedule": schedule,
            "enabled": bool(data.get("enabled", True))}


def next_run(schedule: dict, after: datetime) -> float:
    if schedule["type"] == "interval":
        return (after + timedelta(minutes=schedule["minutes"])).timestamp()
    hh, mm = map(int, schedule["time"].split(":"))
    for i in range(8):
        day = (after + timedelta(days=i)).date()
        candidate = datetime(day.year, day.month, day.day, hh, mm)
        if candidate > after and candidate.weekday() in schedule["days"]:
            return candidate.timestamp()
    raise ValueError("no upcoming day")  # unreachable: days is non-empty


def create(data: dict) -> dict:
    routine = validate(data) | {"id": store.new_id(), "created": time.time(), "last_run": None,
                                "last_status": None, "chat_id": None}
    routine["next_run"] = next_run(routine["schedule"], datetime.now())
    save(load() + [routine])
    return routine


def update(routine_id: str, data: dict) -> dict:
    items = load()
    for i, r in enumerate(items):
        if r["id"] == routine_id:
            fresh = validate(r | data)
            r.update(fresh)
            r["next_run"] = next_run(r["schedule"], datetime.now())
            items[i] = r
            save(items)
            return r
    raise KeyError(routine_id)


def delete(routine_id: str) -> bool:
    items = load()
    kept = [r for r in items if r["id"] != routine_id]
    save(kept)
    return len(kept) != len(items)


def record(routine_id: str, **fields) -> None:
    items = load()
    for r in items:
        if r["id"] == routine_id:
            r.update(fields)
    save(items)


async def loop(trigger) -> None:
    """Run due routines forever. `trigger(routine)` starts one and returns a status string."""
    while True:
        try:
            now = time.time()
            for r in load():
                if not r.get("enabled") or not r.get("next_run") or r["next_run"] > now:
                    continue
                if now - r["next_run"] > GRACE:  # the app was off at the time: skip to the next slot
                    record(r["id"], next_run=next_run(r["schedule"], datetime.now()), last_status="skipped (app was off)")
                    continue
                try:
                    status = await trigger(r)
                except Exception as e:  # a broken routine must not stop the others
                    status = f"failed: {e}"
                record(r["id"], last_run=now, last_status=status,
                       next_run=next_run(r["schedule"], datetime.now()))
        except Exception:
            pass
        await asyncio.sleep(TICK)
