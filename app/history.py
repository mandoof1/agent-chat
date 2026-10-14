"""What gets sent to the model, and automatic compaction of long conversations.

When a conversation nears the model's context window, the older messages are replaced (only in
what is sent to the model) by a summary that the same model writes. The chat file and the UI
keep the full history; chat["compactions"] records each summary and the message index where the
verbatim part resumes.

Before that, old tool outputs are cleared: the model sees a short placeholder in their place
(`_cleared` on the message; the chat keeps the output), and so are the contents of old
write_file calls, which the file holds (`_cleared_args` on the assistant message, by call id).
It costs no model call, and a tool can always be run again, so it comes first and makes
compaction rarer.
"""

import json
import time

import httpx

from . import llm

DEFAULT_CHARS_PER_TOKEN = 3.0  # until a real reply calibrates it (code and JSON are dense)
KEEP_FRACTION = 0.2            # recent context kept word for word after an automatic compaction (at most)
KEEP_MIN = 0.05                # ...and at least, even when what can't be summarized fills the room
MIN_GAIN = 0.1                 # share of the context an automatic compaction must free to be worth a summary
SUMMARY_FRACTION = 0.06        # length asked of a summary, as a share of the context
SUMMARY_MAX = 4000             # tokens; a longer summary only crowds out the conversation
TOOL_RESULT_CLIP = 1500        # chars of each tool result the summarizer sees
SUMMARY_INPUT_FRACTION = 0.6   # max share of the context the summarizer's input may use
CONTINUE_TAIL = 24000          # chars of a cut-off reply's thinking handed back so it can carry on
CONTINUE_FRACTION = 0.1        # ...but no more than this share of the context (each, thinking and answer)
CLEAR_AT = 0.7                 # old tool outputs are cleared past this share of the compaction trigger...
CLEAR_TO = 0.4                 # ...down to this share of it, in one batch (each batch breaks the prompt cache once)
KEEP_OUTPUTS = 3               # the newest tool outputs are never cleared (as many as fit in KEEP_FRACTION
                               # of the window: three whole-file reads fill an 8k one), nor the current step's
CLEAR_MIN_CHARS = 1000         # shorter outputs aren't worth a placeholder
KEPT_TOOLS = {"ask_agent", "subagent", "remember", "recall_memory", "forget_memory"}  # reports, memory: kept
OUTPUT_FRACTION = 0.25         # most of the context one tool output may take
CANCELLED = "[cancelled: the user stopped the run]"  # the result of a call a stopped run never finished
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
            messages.insert(i, {"role": "tool", "tool_call_id": tc["id"], "content": CANCELLED})
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
        clean = {k: v for k, v in sent(m).items() if not k.startswith("_")}
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


def cleared_note(m: dict) -> str:
    """What the model sees in place of a cleared tool output (fixed, so the prompt cache holds)."""
    return (f"[Output cleared to save context: it was {len(m.get('content') or ''):,} characters. "
            "Run the tool again if you need it.]")


def cleared_args(tc: dict) -> str:
    """A write_file call's arguments with the content left out (the file holds it); fixed, like
    cleared_note."""
    try:
        args = json.loads(tc["function"]["arguments"] or "{}")
        n = len(args["content"])
    except (ValueError, KeyError, TypeError):
        return tc["function"]["arguments"]
    args["content"] = f"[{n:,} characters, cleared to save context: read {args.get('path') or 'the file'} for what it holds now]"
    return json.dumps(args, ensure_ascii=False)


def sent(m: dict) -> dict:
    """`m` as clearing leaves it for the model (its private fields still on)."""
    if m.get("_cleared") and m.get("role") == "tool":
        m = m | {"content": cleared_note(m)}
    if m.get("_cleared_args") and m.get("tool_calls"):
        m = m | {"tool_calls": [tc | {"function": tc["function"] | {"arguments": cleared_args(tc)}}
                                if tc["id"] in m["_cleared_args"] else tc for tc in m["tool_calls"]]}
    return m


def continuation(m: dict) -> dict:
    """What the model sees in place of a reply the context window cut off: the end of its thinking
    and any answer it had started, so the next request has room to pick up where it stopped.
    `_carry` (set when the reply was cut off) is how many chars of each to hand back."""
    tail = m.get("_carry") or CONTINUE_TAIL
    reasoning = m.get("reasoning_content") or ""
    parts = ["[Your previous reply was cut off because the context window filled up.]"]
    if reasoning:
        label = "The end of your reasoning so far" if len(reasoning) > tail else "Your reasoning so far"
        parts.append(f"{label}:\n{reasoning[-tail:]}")
    if m.get("content"):
        parts.append(f"Your answer so far:\n{m['content'][-tail:]}")
    parts.append("Continue from where you stopped.")
    return {"role": "user", "content": "\n\n".join(parts)}


def latest(chat: dict) -> dict | None:
    comps = chat.get("compactions") or []
    return comps[-1] if comps else None


def payload(chat: dict, memory: bool = True, images: bool = False, comp: dict | None = None) -> list[dict]:
    """The conversation as the model sees it: latest summary (if any), then the verbatim tail.
    `comp`: a compaction to use instead of the latest one (to measure what it would leave)."""
    c = comp or latest(chat)
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
        acc += len(json.dumps(sent(msgs[i]), ensure_ascii=False))
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


def fit_output(text: str, chat: dict, n_ctx: int | None) -> str:
    """A tool output cut to OUTPUT_FRACTION of the context (start and end kept): one output must
    not fill a small window. The tools have their own, fixed limits too."""
    limit = int(n_ctx * OUTPUT_FRACTION * chars_per_token(chat)) if n_ctx else 0
    if not limit or len(text) <= limit:
        return text
    half = limit // 2
    return f"{text[:half]}\n\n... [{len(text) - limit:,} characters omitted to fit the context window] ...\n\n{text[-half:]}"


def clear_outputs(chat: dict, *, n_ctx: int, trigger: float, fixed_chars: int = 0, everything: bool = False) -> list[str]:
    """Clear the oldest tool outputs (see the module docstring) until the context is down to
    CLEAR_TO of `trigger` (the compaction point, in tokens). Returns the ids of the calls whose
    outputs were cleared: none when that would free less than MIN_GAIN of the window, which isn't
    worth re-processing the prompt from the first cleared output on. `everything` (the server
    rejected the prompt): clear all that may be cleared, whatever it frees."""
    msgs = chat["messages"]
    prev = latest(chat)
    start = prev["upto"] if prev else 0
    names = {tc["id"]: tc["function"]["name"] for m in msgs[start:] for tc in m.get("tool_calls") or []}
    last_call = next((m for m in reversed(msgs) if m.get("tool_calls")), None)
    step = {tc["id"] for tc in (last_call or {}).get("tool_calls") or []}  # the current step's calls
    keep = set(step)  # ...and the newest outputs
    budget = n_ctx * KEEP_FRACTION * chars_per_token(chat)
    for m in [m for m in msgs[start:] if m.get("role") == "tool" and not m.get("_cleared")][::-1][:KEEP_OUTPUTS]:
        budget -= len(m.get("content") or "")
        if budget < 0 and m["tool_call_id"] not in keep:
            break
        keep.add(m["tool_call_id"])
    wire = lambda text: len(json.dumps(text, ensure_ascii=False))
    candidates = []  # (message, call id, chars it frees), oldest first
    for m in msgs[start:]:
        if (m.get("role") == "tool" and not m.get("_cleared") and m["tool_call_id"] not in keep
                and names.get(m["tool_call_id"]) not in KEPT_TOOLS and len(m.get("content") or "") >= CLEAR_MIN_CHARS):
            candidates.append((m, m["tool_call_id"], wire(m["content"]) - wire(cleared_note(m))))
        for tc in m.get("tool_calls") or []:  # a whole file in write_file's arguments: the file holds it
            if (tc["function"]["name"] == "write_file" and tc["id"] not in step and tc["id"] not in (m.get("_cleared_args") or {})
                    and len(tc["function"]["arguments"] or "") >= CLEAR_MIN_CHARS):
                candidates.append((m, tc["id"], wire(tc["function"]["arguments"]) - wire(cleared_args(tc))))
    current = size(payload(chat), None) + fixed_chars
    target = 0 if everything else trigger * CLEAR_TO * chars_per_token(chat)
    chosen, freed = [], 0
    for c in candidates:  # oldest first: the prompt cache holds up to the first one cleared
        if current - freed <= target:
            break
        freed += c[2]
        chosen.append(c)
    if not chosen or estimate(chat, freed) < (0 if everything else n_ctx * MIN_GAIN):
        return []
    for m, call_id, _ in chosen:
        mark = {"at": time.time(), "upto": len(msgs)}
        if m.get("role") == "tool":
            m["_cleared"] = mark
        else:
            m.setdefault("_cleared_args", {})[call_id] = mark
    return list(dict.fromkeys(call_id for _, call_id, _ in chosen))


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


def summary_budget(n_ctx: int) -> int:
    """Tokens a summary may take: it is sent with every request after it, and folded into the next one."""
    return int(min(SUMMARY_MAX, max(300, n_ctx * SUMMARY_FRACTION)))


def carry(chat: dict, n_ctx: int | None) -> int:
    """Chars of a cut-off reply to hand back (see `continuation`): enough to carry on, while leaving
    the window room for the rest of the reply."""
    return int(min(CONTINUE_TAIL, n_ctx * CONTINUE_FRACTION * chars_per_token(chat))) if n_ctx else CONTINUE_TAIL


def request_for(msgs: list[dict], cut: int) -> str | None:
    """The request being worked on, if compacting up to `cut` would summarize it away: the newest
    message that started a turn, plus what the user queued while the agent worked on it."""
    last_user = next((i for i in range(len(msgs) - 1, -1, -1)
                      if msgs[i].get("role") == "user" and not msgs[i].get("_queued")), None)
    if last_user is None or last_user >= cut:
        return None
    return "\n\n".join([msgs[last_user]["content"]] + [
        f"{QUEUED_NOTE}\n{m['content']}" for m in msgs[last_user + 1:cut] if m.get("role") == "user"])


async def compact(run, agent: dict, chat: dict, *, path: list, n_ctx: int, reason: str,
                  client: httpx.AsyncClient, settings: dict, user_label: str = "USER", instructions: str = "",
                  fixed_chars: int = 0) -> bool:
    """Summarize older messages of `chat`. Returns False when there is nothing worth compacting:
    unless the user asked for it, a compaction must free real space, or it would only be repeated at
    the next step. `fixed_chars` is the part of every request compaction can't shrink (system prompt
    and tool schemas). `instructions` (from /compact) say what the summary must keep."""
    msgs = chat["messages"]
    prev = latest(chat)
    start = prev["upto"] if prev else 0
    cpt = chars_per_token(chat)
    current = size(payload(chat), None) + fixed_chars
    summary_tokens = summary_budget(n_ctx)
    keep = None
    if reason != "manual":
        # Aim for half the trigger point, so the chat has room to grow before the next compaction.
        # What stays whatever is cut (system prompt, tools, the request, the new summary) comes off
        # the verbatim tail's share; with the tail at its minimum, MIN_GAIN decides.
        trigger = int(settings.get("compact_at") or 70) / 100
        unsummarizable = fixed_chars + len(request_for(msgs, len(msgs)) or "") + summary_tokens * cpt
        room = n_ctx * trigger / 2 * cpt - unsummarizable
        keep = min(n_ctx * min(KEEP_FRACTION, trigger * 0.4) * cpt, max(n_ctx * KEEP_MIN * cpt, room))
    if reason == "overflow":  # the server says it doesn't fit, whatever our estimate thinks: cut harder
        keep = min(keep, current * 0.25)
    cut = choose_cut(msgs, start, keep)
    if cut is None:
        return False
    request = request_for(msgs, cut)
    if reason != "manual":
        # what goes (older messages, the previous summary) minus what comes (request, new summary).
        # On overflow the server's limit is what counts, not n_ctx (which the summary budget comes
        # from and which may be wrong): anything removed may be enough, and the check after the
        # summary still drops one that grew the context.
        left = size(payload(chat, comp={"upto": cut, "summary": "", "request": request}), None) + fixed_chars
        freed = estimate(chat, current - left)
        if freed < (1 if reason == "overflow" else summary_tokens + n_ctx * MIN_GAIN):
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
        prompt = ""
        if prev:
            prompt += f"Summary of the conversation before this transcript:\n\n{prev['summary']}\n\n"
        prompt += f"Transcript to summarize:\n\n{body}\n\n"
        if instructions:
            prompt += f"When summarizing, make sure to keep: {instructions}\n\n"
        prompt += f"Write the updated summary now, in at most about {summary_tokens * 3 // 4} words."
        comp = llm.Completion()
        try:
            # no thinking first: a reasoning model can spend the whole reply on it and leave no
            # summary (Qwen3.5-4B in an 8k window did), and it takes minutes on a small GPU
            await llm.stream_chat(client, settings, model=agent["model"], temperature=agent["temperature"], tools=None,
                                  messages=[{"role": "system", "content": SUMMARY_PROMPT}, {"role": "user", "content": prompt}],
                                  out=comp, on_delta=on_delta, llama=run.llama, thinking=False)
            break
        except llm.LLMError as e:
            if attempt == 2 or not is_overflow(e):
                raise
            body = "[…the oldest part of this transcript was omitted…]\n\n" + body[len(body) // 2:]
    summary = comp.content.strip()
    if not summary:
        run.emit("notice", path=path, text="Compaction failed: the model returned an empty summary.")
        return False

    entry = {"upto": cut, "summary": summary, "at": time.time(), "reason": reason, "request": request,
             "before_tokens": before}
    if instructions:
        entry["instructions"] = instructions
    entry["after_tokens"] = estimate(chat, size(payload(chat, comp=entry), None) + fixed_chars)
    if reason != "manual" and entry["after_tokens"] >= before:  # a summary far over its budget
        run.emit("notice", path=path, text="Compaction freed no space (the summary came out too long), so it was not kept.")
        return False
    chat.setdefault("compactions", []).append(entry)
    run.emit("compact_end", path=path, compaction=entry)
    return True


def prune(chat: dict, length: int) -> None:
    """Drop compactions that cover messages beyond `length` (after an edit / regenerate), and
    restore the tool outputs cleared after that point."""
    comps = chat.get("compactions") or []
    while comps and comps[-1]["upto"] > length:
        comps.pop()
    for m in chat["messages"][:length]:
        if (m.get("_cleared") or {}).get("upto", 0) > length:
            del m["_cleared"]
        for call_id, mark in list((m.get("_cleared_args") or {}).items()):
            if mark.get("upto", 0) > length:
                del m["_cleared_args"][call_id]
        if m.get("_cleared_args") == {}:
            del m["_cleared_args"]
