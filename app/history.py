"""What gets sent to the model, and automatic compaction of long conversations.

When a conversation nears the model's context window, the older messages are replaced (only in
what is sent to the model) by a summary that the same model writes. The chat file and the UI
keep the full history; chat["compactions"] records each summary and the message index where the
verbatim part resumes.
"""

import json
import time

import httpx

from . import llm

DEFAULT_CHARS_PER_TOKEN = 3.0  # until a real reply calibrates it (code and JSON are dense)
KEEP_FRACTION = 0.2            # recent context kept word for word after an automatic compaction
TOOL_RESULT_CLIP = 1500        # chars of each tool result the summarizer sees
SUMMARY_INPUT_FRACTION = 0.6   # max share of the context the summarizer's input may use
CONTINUE_TAIL = 24000          # chars of a cut-off reply's thinking handed back so it can carry on
QUEUED_NOTE = ("[The user sent this while you were working. Take it into account, then carry on with "
               "your task unless it says otherwise.]")

SUMMARY_PROMPT = (
    "You compress conversations so an AI agent can keep working after older messages are removed "
    "from its context. Write a dense, factual summary of the transcript you are given. Keep: the "
    "goals and every request, decisions and their reasons, facts learned, file names and paths, "
    "commands that were run and what they returned, errors and how they were fixed, what is "
    "finished, what is still open, and the next steps. Keep exact names, numbers and code "
    "identifiers. Use short bullet points under clear headings. No preamble, and no comments about "
    "the summarizing task itself."
)


# ---------------------------------------------------------------- message prep

def repair(messages: list[dict]) -> None:
    """Give every tool call a result (a stopped run can leave some unanswered)."""
    answered = {m.get("tool_call_id") for m in messages if m.get("role") == "tool"}
    i = 0
    while i < len(messages):
        m = messages[i]
        i += 1
        if m.get("role") != "assistant":
            continue
        missing = [tc for tc in m.get("tool_calls") or [] if tc["id"] not in answered]
        # results go right after the assistant message and its existing tool results
        while i < len(messages) and messages[i].get("role") == "tool":
            i += 1
        for tc in missing:
            messages.insert(i, {"role": "tool", "tool_call_id": tc["id"], "content": "[cancelled: the user stopped the run]"})
            answered.add(tc["id"])
            i += 1


def to_llm(messages: list[dict], memory: bool = True, images: bool = False) -> list[dict]:
    """Strip UI-only fields (leading underscore) and UI-only messages. A user message's recalled
    memories (`_recall`, fixed when the message was sent) are appended to it.

    Thinking from earlier turns is dropped: reasoning chat templates (Qwen, DeepSeek...) only
    render the reasoning of the turn in progress, so sending it would just inflate the request
    and our size estimate. Thinking within the current turn is kept (interleaved tool use).

    A reply of the current turn that the context window cut off is replaced by a continuation
    note (see `continuation`); when several were cut off in a row, only the newest is sent.

    A message the user queued while the agent was working (`_queued`) is labelled as such, so the
    model treats it as an aside to the task in progress rather than a fresh request.

    With images=True, a user message with attached images (`_images`, workspace paths) becomes a list
    of content parts with `attach://<path>` placeholders that runner.inline_images turns into data URLs
    right before the request (so the base64 never counts towards the context-size estimate)."""
    live = [i for i, m in enumerate(messages) if not m.get("_error")]
    last_user = max((i for i in live if messages[i].get("role") == "user"), default=-1)
    cut_off = {i for i in live if i > last_user and messages[i].get("_truncated")}
    if cut_off:
        last_user = max(cut_off)  # the note acts as a new user turn, so older thinking is dropped
    out = []
    for k, i in enumerate(live):
        m = messages[i]
        if i in cut_off:
            if k + 1 == len(live) or live[k + 1] not in cut_off:  # a run of them resumes from the newest
                out.append(continuation(m))
            continue
        clean = {k: v for k, v in m.items() if not k.startswith("_")}
        if i < last_user:
            clean.pop("reasoning_content", None)
        if m.get("_queued") and m.get("role") == "user":
            clean["content"] = f"{QUEUED_NOTE}\n{m['content']}"
        if memory and m.get("_recall") and m.get("role") == "user":
            clean["content"] = f"{clean['content']}\n\n[From your memory of the user, possibly relevant]\n{m['_recall']}"
        if images and m.get("_images") and m.get("role") == "user":
            clean["content"] = [{"type": "text", "text": clean["content"]}] + [
                {"type": "image_url", "image_url": {"url": f"attach://{path}"}} for path in m["_images"]]
        out.append(clean)
    return out


def continuation(m: dict) -> dict:
    """What the model sees in place of a reply the context window cut off: the end of its thinking
    and any answer it had started, so the next request has room to pick up where it stopped."""
    reasoning = m.get("reasoning_content") or ""
    parts = ["[Your previous reply was cut off because the context window filled up.]"]
    if reasoning:
        label = "The end of your reasoning so far" if len(reasoning) > CONTINUE_TAIL else "Your reasoning so far"
        parts.append(f"{label}:\n{reasoning[-CONTINUE_TAIL:]}")
    if m.get("content"):
        parts.append(f"Your answer so far:\n{m['content'][-CONTINUE_TAIL:]}")
    parts.append("Continue from where you stopped.")
    return {"role": "user", "content": "\n\n".join(parts)}


def latest(chat: dict) -> dict | None:
    comps = chat.get("compactions") or []
    return comps[-1] if comps else None


def payload(chat: dict, memory: bool = True, images: bool = False) -> list[dict]:
    """The conversation as the model sees it: latest summary (if any), then the verbatim tail."""
    c = latest(chat)
    if not c:
        return to_llm(chat["messages"], memory, images)
    head = "[Earlier conversation, summarized to save context]\n\n" + c["summary"]
    if c.get("request"):
        head += "\n\n[The request currently being worked on, verbatim]\n" + c["request"]
    return [{"role": "user", "content": head}] + to_llm(chat["messages"][c["upto"]:], memory, images)


def size(messages: list[dict], schemas: list[dict] | None) -> int:
    return len(json.dumps(messages, ensure_ascii=False)) + len(json.dumps(schemas or [], ensure_ascii=False))


def chars_per_token(chat: dict) -> float:
    return (chat.get("stats") or {}).get("chars_per_token") or DEFAULT_CHARS_PER_TOKEN


def calibrate(chat: dict, chars: int, prompt_tokens: int | None) -> None:
    if prompt_tokens and prompt_tokens > 200:
        chat.setdefault("stats", {})["chars_per_token"] = round(chars / prompt_tokens, 3)


def estimate(chat: dict, chars: int) -> int:
    return int(chars / chars_per_token(chat))


def is_overflow(err: Exception) -> bool:
    text = str(err).lower()
    return "context" in text and any(w in text for w in ("exceed", "too long", "too many tokens", "context size"))


# ------------------------------------------------------------------ compaction

def choose_cut(msgs: list[dict], start: int, keep_chars: float | None) -> int | None:
    """Index where the verbatim tail begins.

    keep_chars=None (manual): keep from the last user message on. Otherwise keep roughly
    keep_chars of the most recent messages. Never starts the tail on a tool result, so tool
    calls and their results stay together.
    """
    if keep_chars is None:
        cut = next((i for i in range(len(msgs) - 1, start, -1) if msgs[i].get("role") == "user"), None)
        return cut if cut and cut - start >= 2 else None
    acc, cut = 0, len(msgs)
    for i in range(len(msgs) - 1, start, -1):
        acc += len(json.dumps(msgs[i], ensure_ascii=False))
        if acc > keep_chars:
            break
        cut = i
    while cut < len(msgs) and msgs[cut].get("role") == "tool":
        cut += 1
    if cut >= len(msgs):  # the newest message alone is over budget: keep its smallest whole unit
        cut = len(msgs) - 1
        while cut > start and msgs[cut].get("role") == "tool":
            cut -= 1
    return cut if cut - start >= 2 else None


def _clip(text: str, limit: int) -> str:
    text = str(text or "")
    return text if len(text) <= limit else text[:limit] + f" …[{len(text) - limit} more chars]"


def transcript(msgs: list[dict], user_label: str) -> str:
    out = []
    for m in msgs:
        if m.get("_error"):
            continue
        if m["role"] == "user":
            out.append(f"{user_label}:\n{m.get('content') or ''}")
        elif m["role"] == "assistant":
            parts = [m["content"]] if m.get("content") else []
            for tc in m.get("tool_calls") or []:
                parts.append(f"→ called {tc['function']['name']}({_clip(tc['function']['arguments'], TOOL_RESULT_CLIP)})")
            if parts:
                out.append("ASSISTANT:\n" + "\n".join(parts))
        elif m["role"] == "tool":
            out.append(f"TOOL RESULT:\n{_clip(m.get('content'), TOOL_RESULT_CLIP)}")
    return "\n\n".join(out)


async def compact(run, agent: dict, chat: dict, *, path: list, n_ctx: int, reason: str,
                  client: httpx.AsyncClient, settings: dict, user_label: str = "USER") -> bool:
    """Summarize older messages of `chat`. Returns False when there is nothing worth compacting."""
    msgs = chat["messages"]
    prev = latest(chat)
    start = prev["upto"] if prev else 0
    cpt = chars_per_token(chat)
    current = size(payload(chat), None)
    # keep at most ~40% of the trigger point verbatim, so compaction always frees real space
    keep_fraction = min(KEEP_FRACTION, int(settings.get("compact_at") or 70) / 100 * 0.4)
    keep = None if reason == "manual" else n_ctx * keep_fraction * cpt
    if reason == "overflow":  # the server says it doesn't fit, whatever our estimate thinks: cut harder
        keep = min(keep, current * 0.25)
    cut = choose_cut(msgs, start, keep)
    if cut is None:
        return False

    before = estimate(chat, current)
    run.emit("compact_start", path=path, reason=reason, before_tokens=before)
    body = transcript(msgs[start:cut], user_label)
    limit = int(n_ctx * SUMMARY_INPUT_FRACTION * cpt)
    if len(body) > limit:
        body = "[…the oldest part of this transcript was omitted…]\n\n" + body[-limit:]
    async def on_delta(kind: str, text: str, meta: dict | None = None) -> None:
        run.emit("delta", path=path, kind="compact", text=text)

    for attempt in range(3):  # if even the summary request is too long, drop the older half and retry
        request = ""
        if prev:
            request += f"Summary of the conversation before this transcript:\n\n{prev['summary']}\n\n"
        request += f"Transcript to summarize:\n\n{body}\n\nWrite the updated summary now."
        comp = llm.Completion()
        try:
            await llm.stream_chat(client, settings, model=agent["model"], temperature=agent["temperature"], tools=None,
                                  messages=[{"role": "system", "content": SUMMARY_PROMPT}, {"role": "user", "content": request}],
                                  out=comp, on_delta=on_delta, llama=run.llama)
            break
        except llm.LLMError as e:
            if attempt == 2 or not is_overflow(e):
                raise
            body = "[…the oldest part of this transcript was omitted…]\n\n" + body[len(body) // 2:]
    summary = comp.content.strip()
    if not summary:
        run.emit("notice", path=path, text="Compaction failed: the model returned an empty summary.")
        return False

    # the request being worked on: the newest message that started a turn, plus what the user
    # queued while the agent worked on it
    last_user = next((i for i in range(len(msgs) - 1, -1, -1)
                      if msgs[i].get("role") == "user" and not msgs[i].get("_queued")), None)
    request = None
    if last_user is not None and last_user < cut:
        request = "\n\n".join([msgs[last_user]["content"]] + [
            f"{QUEUED_NOTE}\n{m['content']}" for m in msgs[last_user + 1:cut] if m.get("role") == "user"])
    entry = {"upto": cut, "summary": summary, "at": time.time(), "reason": reason, "request": request,
             "before_tokens": before}
    chat.setdefault("compactions", []).append(entry)
    entry["after_tokens"] = estimate(chat, size(payload(chat), None))
    run.emit("compact_end", path=path, compaction=entry)
    return True


def prune(chat: dict, length: int) -> None:
    """Drop compactions that cover messages beyond `length` (after an edit / regenerate)."""
    comps = chat.get("compactions") or []
    while comps and comps[-1]["upto"] > length:
        comps.pop()
