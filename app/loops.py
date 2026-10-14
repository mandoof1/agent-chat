"""Loop guard: notices when an agent is stuck going round in circles, and breaks the circle.

Two kinds of loop, both seen with real local models:

- A reply that keeps writing the same passage over and over (Qwen has thought the same plan 19
  times in a row, 115K characters, until the user stopped it). `Repetition` watches the thinking
  and the answer as they stream: when the newest WINDOW characters already appeared REPEATS times
  and the repeated stretch is at least SPAN long, the stream is stopped and the model gets the
  reply up to the loop (one copy of the passage) with a note to act on it instead of writing it
  again (`note`, sent by history.to_llm in place of the reply).
  Only repetition triggers it, never length: a long reply that keeps saying new things is left be.

- The same tool call, with the same arguments, returning the same result again and again.
  `mark_repeats` tags the CALL_WARN-th such result so the model is told it changes nothing
  (`repeat_note`, appended to the result by history.sent); at CALL_STOP the turn ends.

Settings → `loop_guard` turns both off.
"""

import json

WINDOW = 200          # chars at the end of the stream looked for earlier in it
REPEATS = 3           # ...this many times (the newest included) means it is going round
SPAN = 2000           # ...over at least this many chars (a few repeated short lines are just code)
MIN_DISTINCT = 16     # a window of fewer distinct characters (a rule of dashes, a run of zeros) never counts
CHECK_EVERY = 500     # chars streamed between checks
MAX_LOOPS = 3         # replies in a row stopped for looping before the turn gives up
CALL_WARN = 3         # the same call with the same result this many times: tell the model
CALL_STOP = 5         # ...this many times: end the turn


class Looping(Exception):
    """Raised from the stream's delta callback to stop a reply that is going round in circles."""

    def __init__(self, info: dict):
        super().__init__(f"the {info['channel']} repeated itself {info['repeats']} times")
        self.info = info


class Repetition:
    """Watches one reply as it streams (`feed` each piece of thinking or answer)."""

    def __init__(self):
        self.text = {"reasoning": "", "content": ""}
        self.checked = {"reasoning": 0, "content": 0}

    def feed(self, kind: str, piece: str) -> None:
        """Raises Looping when the stream of `kind` has started repeating itself."""
        if kind not in self.text:
            return
        self.text[kind] += piece
        text = self.text[kind]
        if len(text) - self.checked[kind] < CHECK_EVERY:
            return
        self.checked[kind] = len(text)
        if info := find_loop(text):
            raise Looping(info | {"channel": kind})


def find_loop(text: str) -> dict | None:
    """If the end of `text` repeats what came before: how many times, and `keep`, the length of
    the text up to the second copy (everything before the loop, and the passage once)."""
    if len(text) < SPAN:
        return None
    window = text[-WINDOW:]
    if len(set(window)) < MIN_DISTINCT:
        return None
    repeats = text.count(window)
    if repeats < REPEATS:
        return None
    first = text.find(window)
    if len(text) - first < SPAN:
        return None
    second = text.find(window, first + 1)
    period = second - first
    # the passage starts where the copies stop matching, going back from the first one
    start = first
    while start > 0 and text[start - 1] == text[start - 1 + period]:
        start -= 1
    return {"repeats": max(repeats, (len(text) - start) // period), "keep": start + period, "period": period}


def note(m: dict, tail: int) -> dict:
    """What the model sees in place of a reply that was stopped for looping: the reply up to the
    loop (the repeated passage once), and what to do instead. `tail` caps each part, like a cut-off
    reply's continuation."""
    info = m["_looped"]
    keep = info.get("keep")
    reasoning = m.get("reasoning_content") or ""
    content = m.get("content") or ""
    if info.get("channel") == "content":
        content = content[:keep]
    elif keep is not None:
        reasoning, content = reasoning[:keep], ""
    parts = [f"[Your previous reply was stopped because it got stuck in a loop: it wrote the same passage "
             f"{info.get('repeats', 'several')} times in a row.]"]
    if reasoning:
        label = "The end of your reasoning, up to the loop" if len(reasoning) > tail else "Your reasoning, up to the loop"
        parts.append(f"{label} (the repeated passage once):\n{reasoning[-tail:]}")
    if content:
        parts.append(f"Your answer so far:\n{content[-tail:]}")
    parts.append("Do not write that passage again: you have already worked it out. Act on it now: make the "
                 "tool call it leads to, or carry on with your answer from where it was. If you were weighing "
                 "options, pick one and go.")
    return {"role": "user", "content": "\n\n".join(parts)}


def repeat_note(n: int) -> str:
    return (f"[Loop guard: you have now made this exact call {n} times in this task and got this same result "
            "every time. Calling it again won't change it. Use what you have, try a different approach, or "
            "give your answer.]")


def _key(tc: dict) -> tuple[str, str]:
    fn = tc.get("function") or {}
    args = fn.get("arguments") or "{}"
    try:
        args = json.dumps(json.loads(args), sort_keys=True, ensure_ascii=False)
    except (ValueError, TypeError):
        pass
    return fn.get("name") or "", args


def mark_repeats(messages: list[dict], start: int) -> tuple[int, str]:
    """Tag the newest reply's tool results that repeat an earlier call of this task (messages from
    `start`) with the same arguments and the same result: `_repeat` = how many times it has now
    been made, from CALL_WARN on. Results the model no longer sees (cleared to save context) don't
    count: running the tool again is what it was told to do. Returns the highest count among the
    newest reply's calls and that tool's name."""
    replies = [i for i in range(start, len(messages))
               if messages[i].get("role") == "assistant" and messages[i].get("tool_calls")]
    if not replies:
        return 0, ""
    newest = replies[-1]
    results = {m.get("tool_call_id"): m for m in messages[start:] if m.get("role") == "tool"}
    seen: dict[tuple, int] = {}
    for i in replies[:-1]:
        for tc in messages[i]["tool_calls"]:
            r = results.get(tc["id"])
            if r is not None and not r.get("_cleared"):
                k = (*_key(tc), r.get("content"))
                seen[k] = seen.get(k, 0) + 1
    top, name = 0, ""
    for tc in messages[newest]["tool_calls"]:
        r = results.get(tc["id"])
        if r is None:
            continue
        k = (*_key(tc), r.get("content"))
        n = seen.get(k, 0) + 1
        seen[k] = n  # two identical calls in one reply count as two
        if n >= CALL_WARN:
            r["_repeat"] = n
            if n > top:
                top, name = n, k[0]
    return top, name
