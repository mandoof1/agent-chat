"""Agent loop and run management.

A "run" is one turn of a chat: the agent streams a reply, calls tools, maybe asks other
agents (which run their own loops), until it answers without tool calls.

Runs execute as background tasks, independent of any browser connection. Every event is
kept and fanned out to subscribers, so a page that opens mid-run (or reloads) replays the
run so far and then continues live.

Events carry a `path`: the chain of ask_agent tool-call ids leading to the agent that
produced them ([] = the chat's own agent). The UI uses it to nest sub-agent output.

Messages the user sends while a run is going are queued on the run (like Claude Code's
message queue). The chat's own agent gets them at its next step: after the tool calls in
flight finish and before its next request, so it can take them into account mid-task. If it
answers before that, they start the next turn in the same run. When a run ends any other way
(stopped, failed, step limit) queued messages go back to the user's message box, unsent.

When one agent asks another, the conversation between that pair lives in its own chat
(a "sub-chat", owned by the agent being asked, with chat["parent"] pointing back). While
the sub-agent works, that sub-chat is a live *view* of the run: it receives the run's
events for its path, re-based so the sub-chat page renders them like any other chat.
"""

import asyncio
import json
import time
from datetime import date

import httpx

from . import agenda, browser, history, llm, mail, memory, store, tools
from .history import repair

MAX_DEPTH = 3

client = httpx.AsyncClient()
runs: dict[str, "Run"] = {}       # root chat id -> run
views: dict[str, "View"] = {}     # chat id -> live view (root chats and active sub-chats)
chat_subs: dict[str, set[asyncio.Queue]] = {}
global_subs: set[asyncio.Queue] = set()
memory_task: asyncio.Task | None = None  # background memory extraction (yields to real requests)


def broadcast(event: dict) -> None:
    for q in list(global_subs):
        q.put_nowait(event)


def chat_status(chat_id: str) -> str:
    """idle | running | delegated (waiting on another agent) | waiting (needs the user)."""
    view = views.get(chat_id)
    return view.run.status_of(chat_id) if view else "idle"


class View:
    """A chat the run is currently writing: the root chat, or a sub-agent's chat."""

    def __init__(self, run: "Run", chat: dict, path: list, base: int):
        self.run, self.chat, self.path, self.base = run, chat, tuple(path), base
        self.events: list[dict] = []

    def send(self, event: dict) -> None:
        self.events.append(event)
        for q in list(chat_subs.get(self.chat["id"], ())):
            q.put_nowait(event)


class Run:
    def __init__(self, chat: dict):
        self.chat = chat
        self.base = len(chat["messages"])  # messages before this index existed when the run started
        self.approvals: dict[str, asyncio.Future] = {}
        self.task: asyncio.Task | None = None
        self.stack = [View(self, chat, [], self.base)]  # active views, root first, deepest last
        self.n_ctx: int | None = None
        self.bypassed: set[str] = set()  # tool calls that ran without asking (approvals bypassed)
        self.queue: list[dict] = []  # messages the user sent during this run, not delivered yet
        self.turn = self.base  # like base, for the newest turn (queued messages can start one)
        views[chat["id"]] = self.stack[0]

    def emit(self, type_: str, **data) -> None:
        event = {"type": type_, **data}
        self.stack[0].send(event)
        if "path" not in data:
            return
        path = tuple(data["path"])
        for view in self.stack[1:]:
            n = len(view.path)
            if path[:n] == view.path:
                view.send(event | {"path": list(path[n:])})

    def push(self, view: View) -> None:
        self.stack.append(view)
        views[view.chat["id"]] = view
        view.send({"type": "run_start", "agent_id": view.chat["agent_id"], "chat": view.chat, "run_base": view.base})
        self.publish()

    def pop(self, view: View) -> None:
        self.stack.remove(view)
        views.pop(view.chat["id"], None)
        for q in list(chat_subs.get(view.chat["id"], ())):
            q.put_nowait({"type": "done", "chat": view.chat, "ts": time.time()})
        broadcast({"type": "chat_status", "chat_id": view.chat["id"], "status": "idle"})
        self.publish()

    def enqueue(self, content: str) -> dict:
        item = {"id": store.new_id(), "content": content, "ts": time.time()}
        self.queue.append(item)
        self.publish_queue()
        return item

    def dequeue(self, item_id: str | None = None) -> list[dict]:
        """Take one queued message (or all of them) back out before the agent gets it."""
        taken = [i for i in self.queue if item_id in (None, i["id"])]
        if taken:
            self.queue = [i for i in self.queue if i not in taken]
            self.publish_queue()
        return taken

    def publish_queue(self) -> None:
        # Sent live only, not kept in the replayed events: the snapshot carries the current queue.
        event = {"type": "queue", "items": self.queue}
        for q in list(chat_subs.get(self.chat["id"], ())):
            q.put_nowait(event)

    def deliver(self, agent: dict, midturn: bool) -> bool:
        """Add the queued messages to the chat as one user message. Returns False if there were none.

        midturn: the agent is in the middle of its work (it hasn't answered yet), so the model is
        told the message arrived while it was working (history.to_llm)."""
        if not self.queue:
            return False
        content = "\n\n".join(i["content"] for i in self.queue)
        self.queue = []
        add_user_message(self.chat, agent, content)
        msg = self.chat["messages"][-1]
        if midturn:
            msg["_queued"] = True
        self.turn = len(self.chat["messages"])
        store.save_chat(self.chat)
        self.emit("user_message", path=[], message=msg, index=len(self.chat["messages"]) - 1)
        self.publish_queue()
        return True

    def status_of(self, chat_id: str) -> str:
        for i, view in enumerate(self.stack):
            if view.chat["id"] == chat_id:
                if i < len(self.stack) - 1:
                    return "delegated"
                return "waiting" if self.approvals else "running"
        return "idle"

    def publish(self) -> None:
        for view in self.stack:
            broadcast({"type": "chat_status", "chat_id": view.chat["id"], "status": self.status_of(view.chat["id"])})

    async def context_size(self) -> int | None:
        if self.n_ctx is None:
            settings = store.get_settings()
            info = await llm.server_info(client, settings)
            self.n_ctx = info.get("n_ctx") or int(settings.get("context_size") or 0) or None
        return self.n_ctx

    async def ask_approval(self, path: list, call_id: str, name: str, args: dict) -> bool:
        if store.get_settings().get("bypass_approvals"):  # the user switched approvals off
            self.bypassed.add(call_id)
            self.emit("approval_bypassed", path=path, call_id=call_id, name=name)
            return True
        approval_id = store.new_id()
        fut = asyncio.get_running_loop().create_future()
        self.approvals[approval_id] = fut
        self.emit("approval", path=path, approval_id=approval_id, call_id=call_id, name=name, args=args)
        self.publish()
        try:
            approved = await fut
        finally:
            self.approvals.pop(approval_id, None)
        self.emit("approval_done", path=path, approval_id=approval_id, call_id=call_id, approved=approved)
        self.publish()
        return approved


# ------------------------------------------------------------ message prep

def add_user_message(chat: dict, agent: dict, content: str) -> None:
    if agent["memory"] and "memory_profile" not in chat:
        chat["memory_profile"] = memory.profile()  # fixed for this chat, so the prompt cache stays valid
    if content:
        recall = memory.recall_block(content, chat["memory_profile"]["ids"]) if agent["memory"] else ""
        chat["messages"].append({"role": "user", "content": content, "_recall": recall})
        if chat["title"] == "New chat":
            first_line = content.splitlines()[0]
            chat["title"] = first_line[:60] + ("…" if len(first_line) > 60 else "")


def contactable(agent: dict, chain: list[str]) -> list[dict]:
    """Agents this one may talk to right now.

    `chain` is the ids of the agents in the current call chain, ending with this one. They are
    excluded: they are waiting on this agent, and it answers them by ending its turn, not by
    calling them (a call would reach a copy without their context, and could loop forever).
    """
    if "ask_agent" not in agent["tools"] or len(chain) > MAX_DEPTH:
        return []
    others = [a for a in store.list_agents() if a["id"] not in chain]
    if not agent["delegate_all"]:
        others = [a for a in others if a["id"] in agent["delegates"]]
    return others


def system_prompt(agent: dict, caller: dict | None, contacts: list[dict], chat: dict) -> str:
    parts = [agent["system_prompt"].strip()]
    # Date only (not time): the system prompt must stay byte-identical across turns so
    # llama-server can reuse its KV cache for the whole conversation prefix.
    purpose = agent["purpose"].strip().rstrip(".")
    ctx = [f"You are the {agent['name']} agent." + (f" Your job: {purpose}." if purpose else ""),
           f"Today's date: {date.today().isoformat()}."]
    if set(agent["tools"]) & {"list_dir", "read_file", "write_file", "edit_file", "run_shell",
                              "browser_screenshot"}:
        ctx.append(f"Your workspace folder is {tools.workspace_for(agent)}. File paths are relative to it.")
    team = [a for a in store.list_agents() if a["id"] != agent["id"]]
    if team:
        ctx.append("Your team (the other agents):\n" +
                   "\n".join(f"- {a['name']}: {a['purpose'] or 'no description'}" for a in team))
    if contacts:
        ctx.append("You can give a teammate a task or ask them a question with the ask_agent tool when they are "
                   "better suited for it. Each conversation with a teammate is remembered, so you can follow up, "
                   "or answer their questions, with another ask_agent call. Do work that fits your own job "
                   "yourself; don't pass work around needlessly.")
    if agent["memory"]:
        known = (chat.get("memory_profile") or {}).get("text")
        ctx.append("You have long-term memory about the user, shared with the other agents. " +
                   (f"What you remember about them:\n{known}" if known else "You don't know anything about them yet.") +
                   "\n\nUse what you know naturally to personalize your help; don't recite it unprompted. When the "
                   "user shares something durable about themselves (preferences, setup, projects, people, goals) or "
                   "asks you to remember something, save it with the remember tool. If a memory is wrong or out of "
                   "date, forget it and remember the corrected fact. Messages may carry extra memories that look "
                   "relevant, marked [From your memory of the user].")
    if caller:
        ctx.append(f"You are working on a request from the {caller['name']} agent. It only sees your final "
                   "message, so make that a complete answer or report. If you need information from it to do "
                   "the job well, don't guess: end your turn with a clear question. It will reply, and you "
                   "will continue this conversation where you left off.")
    parts.append("\n\n".join(ctx))
    return "\n\n".join(p for p in parts if p)


def truncate(chat: dict, from_index: int) -> None:
    """Cut the chat back to from_index (edit / regenerate). Agent-to-agent conversation from the
    removed turns is erased too: sub-chat messages carry `_turn` = the root run's base."""
    chat["messages"] = chat["messages"][:from_index]
    history.prune(chat, from_index)
    for sub_id in list(chat.get("subchats") or []):
        sub = store.get_chat(sub_id)
        if not sub:
            chat["subchats"].remove(sub_id)
            continue
        msgs = sub["messages"]
        cut = next((i for i, m in enumerate(msgs) if m.get("role") == "user" and m.get("_turn", 0) >= from_index), None)
        if cut is None:
            continue
        del msgs[cut:]
        history.prune(sub, cut)
        if msgs:
            store.save_chat(sub)
        else:
            store.delete_chat(sub_id)
            chat["subchats"].remove(sub_id)
            for key, ref in list((chat.get("threads") or {}).items()):
                if ref == sub_id:
                    del chat["threads"][key]


CONTEXT_EVERY = 0.5  # seconds between live context-meter updates while a reply streams


def estimate_context(agent: dict, chat: dict, caller: dict | None, contacts: list[dict], schemas: list) -> int:
    """Estimated size in tokens of the next request for this chat."""
    prompt = [{"role": "system", "content": system_prompt(agent, caller, contacts, chat)}]
    return history.estimate(chat, history.size(prompt + history.payload(chat, agent["memory"]), schemas))


def show_context(run: "Run", chat: dict, path: list, tokens: int) -> None:
    """Update the chat's context meter now, instead of waiting for the reply to finish."""
    chat.setdefault("stats", {})["context_tokens"] = tokens
    run.emit("ctx", path=path, tokens=tokens)


def agent_tools(agent: dict, contacts: list[dict]) -> list[dict]:
    schemas = [tools.SCHEMAS[t] for t in agent["tools"] if t in tools.SCHEMAS]
    schemas += [mail.SCHEMAS[t] for t in agent["tools"] if t in mail.SCHEMAS]
    schemas += [agenda.SCHEMAS[t] for t in agent["tools"] if t in agenda.SCHEMAS]
    schemas += [browser.SCHEMAS[t] for t in agent["tools"] if t in browser.SCHEMAS]
    if agent["memory"]:
        schemas += list(memory.SCHEMAS.values())
    if contacts:
        schemas.append(tools.ask_agent_schema([a["name"] for a in contacts]))
    return schemas


# --------------------------------------------------------------- the loop

async def agent_loop(run: Run, agent: dict, chat: dict, *, depth: int, path: list,
                     chain: list[str], caller: dict | None = None) -> str:
    """Run `agent` on `chat` until it answers without calling tools. Returns that answer."""
    settings = store.get_settings()
    messages = chat["messages"]
    contacts = contactable(agent, chain)
    schemas = agent_tools(agent, contacts)
    system = lambda: {"role": "system", "content": system_prompt(agent, caller, contacts, chat)}
    user_label = f"REQUEST FROM {caller['name'].upper()}" if caller else "USER"
    save = lambda: store.save_chat(chat)

    async def compact(reason: str) -> bool:
        n_ctx = await run.context_size()
        if not n_ctx:
            return False
        try:
            done = await history.compact(run, agent, chat, path=path, n_ctx=n_ctx, reason=reason,
                                         client=client, settings=settings, user_label=user_label)
        except llm.LLMError as e:
            run.emit("notice", path=path, text=f"Compaction failed: {e}")
            return False
        if done:
            if agent["memory"]:  # the prompt prefix changes anyway, so refresh what the agent knows
                chat["memory_profile"] = memory.profile()
            report_context()
            save()
        return done

    def report_context() -> int:
        tokens = estimate_context(agent, chat, caller, contacts, schemas)
        show_context(run, chat, path, tokens)
        return tokens

    max_steps = max(0, int(settings.get("max_steps", 30) or 0))  # 0 = no limit
    step = 0
    partial: list[str] = []  # answer text from replies that were cut off, joined into the final answer
    while not max_steps or step < max_steps:
        step += 1
        repair(messages)
        # Messages the user queued meanwhile go in now, between the tool results and the next
        # request. Not right after a cut-off reply: its continuation note must come next.
        if not depth and step > 1 and not messages[-1].get("_truncated"):
            run.deliver(agent, midturn=True)
        tokens = report_context()  # tool results and queued messages have grown the prompt
        if settings.get("auto_compact", True):
            n_ctx = await run.context_size()
            limit = (n_ctx or 0) * int(settings.get("compact_at") or 70) / 100
            if n_ctx and tokens > limit:
                await compact("auto")

        comp = await _complete(run, agent, chat, system(), schemas, path, save, settings)
        if comp is None:  # the server said the context is full: compact and retry once
            if not await compact("overflow"):
                raise llm.LLMError("The conversation no longer fits in the model's context window and could not "
                                   "be compacted. Start a new chat, or raise --ctx-size.")
            comp = await _complete(run, agent, chat, system(), schemas, path, save, settings, retry=False)

        msg = comp.message()
        stats = comp.stats()
        if stats:
            msg["_stats"] = stats
        cut_off = comp.finish_reason == "length"
        if cut_off:
            msg["_truncated"] = True
            msg.pop("tool_calls", None)  # a call cut off mid-way has incomplete arguments
        messages.append(msg)
        if stats.get("prompt_tokens"):
            chat.setdefault("stats", {}).update(
                context_tokens=stats["prompt_tokens"] + stats.get("completion_tokens", 0),
                **({"tok_per_s": stats["tok_per_s"]} if stats.get("tok_per_s") else {}))
        save()
        run.emit("assistant_end", path=path, message=msg)

        if cut_off:
            # The window filled up mid-reply. Keep going: the next request carries the end of this
            # reply's thinking instead of the whole thing (history.continuation).
            n_ctx = await run.context_size()
            prompt_tokens = stats.get("prompt_tokens") or 0
            if n_ctx and prompt_tokens > n_ctx / 2 and not await compact("cutoff") and prompt_tokens > n_ctx * 0.9:
                raise llm.LLMError("The conversation fills the model's context window, so replies get cut off, "
                                   "and it could not be compacted. Start a new chat, or raise --ctx-size.")
            run.emit("notice", path=path, text="The reply filled the context window before it finished. "
                     "Continuing from where it stopped…")
            if msg.get("content"):
                partial.append(msg["content"])
            continue
        if not msg.get("tool_calls"):
            answer = "\n\n".join(partial + [msg["content"]]) if partial else msg["content"]
            partial.clear()
            if not depth and run.deliver(agent, midturn=False):  # queued during the reply: answer it next
                step = 0
                continue
            return answer
        partial.clear()

        for tc in msg["tool_calls"]:
            result, sub = await execute_tool(run, agent, tc, depth=depth, path=path, chain=chain)
            tool_msg = {"role": "tool", "tool_call_id": tc["id"], "content": result}
            if sub:
                tool_msg["_sub"] = sub
            if tc["id"] in run.bypassed:
                tool_msg["_bypassed"] = True
            messages.append(tool_msg)
            save()
            run.emit("tool_result", path=path, call_id=tc["id"], content=result)

    run.emit("notice", path=path, text=f"Stopped after {settings.get('max_steps')} steps without a final answer. "
                                        "Raise “Max steps per turn” in Settings to let agents work longer.")
    return "(stopped: step limit reached before a final answer)"


async def _complete(run: Run, agent: dict, chat: dict, sys_msg: dict, schemas: list, path: list, save,
                    settings: dict, retry: bool = True) -> llm.Completion | None:
    """Stream one reply. Returns None if the server rejected the prompt as too long (and retry is allowed)."""
    payload = [sys_msg] + history.payload(chat, agent["memory"])
    chars = history.size(payload, schemas)
    comp = llm.Completion()
    shown = time.monotonic()

    def live_tokens() -> int:
        t = comp.timings or {}
        if "predicted_n" in t:  # llama-server's exact counts (timings_per_token)
            return (t.get("cache_n") or 0) + (t.get("prompt_n") or 0) + t["predicted_n"]
        written = len(comp.reasoning) + len(comp.content) + sum(len(c["args"]) for c in comp.calls.values())
        return history.estimate(chat, chars + written)

    async def on_delta(kind: str, text: str, meta: dict | None = None) -> None:
        nonlocal shown
        run.emit("delta", path=path, kind=kind, text=text, **(meta or {}))
        if time.monotonic() - shown >= CONTEXT_EVERY:
            shown = time.monotonic()
            show_context(run, chat, path, live_tokens())

    run.emit("assistant_start", path=path, agent_id=agent["id"])
    try:
        await llm.stream_chat(client, settings, model=agent["model"], messages=payload, tools=schemas,
                              temperature=agent["temperature"], out=comp, on_delta=on_delta)
    except asyncio.CancelledError:
        if comp.content or comp.reasoning:
            partial = comp.message()
            partial.pop("tool_calls", None)  # half-streamed calls are unusable
            chat["messages"].append(partial | {"_stopped": True})
            save()
        raise
    except llm.LLMError as e:
        if retry and history.is_overflow(e):
            run.emit("assistant_end", path=path, message={"role": "assistant", "content": ""})
            return None
        raise
    history.calibrate(chat, chars, (comp.usage or {}).get("prompt_tokens"))
    return comp


async def execute_tool(run: Run, agent: dict, tc: dict, *, depth: int, path: list, chain: list[str]):
    name = tc["function"]["name"]
    raw = tc["function"]["arguments"]
    try:
        args = json.loads(raw or "{}")
        if not isinstance(args, dict):
            raise ValueError
    except ValueError:
        run.emit("tool_start", path=path, call_id=tc["id"], name=name, args={"_raw": raw})
        return f"Error: tool arguments were not valid JSON: {raw[:300]}", None

    run.emit("tool_start", path=path, call_id=tc["id"], name=name, args=args)
    if name in memory.SCHEMAS:
        if not agent["memory"]:
            return "Error: memory is turned off for you.", None
        result = memory.call(name, args, source=f"agent:{agent['id']}")
        if name != "recall_memory" and not result.startswith("Error"):
            broadcast({"type": "memory_changed", "by": agent["name"], "summary": result})
        return result, None
    if name not in agent["tools"]:
        return f"Error: tool '{name}' is not available to you.", None

    if name == "ask_agent":
        return await delegate(run, agent, args, depth=depth, path=path + [tc["id"]], chain=chain)

    if name == "run_shell" and agent["confirm_shell"]:
        if not await run.ask_approval(path, tc["id"], name, args):
            return "The user denied permission to run this command. Ask them or try another approach.", None

    if name == "calendar_events":
        try:
            return await agenda.events(client, str(args.get("start") or ""), args.get("days") or 0, str(args.get("query") or "")), None
        except (TypeError, ValueError) as e:
            return f"Error: {e}", None

    if name in mail.SCHEMAS:
        if name == "email_send" and not await run.ask_approval(path, tc["id"], name, args):  # always, no opt-out
            return "The user did not approve sending this email. Ask what they want changed, or save a draft.", None
        return await mail.call(name, args), None

    if name in browser.SCHEMAS:
        return await browser.call(name, args, tools.workspace_for(agent)), None

    try:
        timeout = store.get_settings().get("shell_timeout", tools.SHELL_TIMEOUT)
        return await tools.call(name, args, tools.workspace_for(agent), client, shell_timeout=timeout), None
    except tools.ToolError as e:
        return f"Error: {e}", None
    except asyncio.CancelledError:
        raise
    except Exception as e:
        return f"Error: {type(e).__name__}: {e}", None


def thread_chat(root: dict, caller: dict, target: dict, fresh: bool) -> dict:
    """The sub-chat holding the caller→target conversation for this root chat (created if needed)."""
    threads = root.setdefault("threads", {})
    key = f"{caller['id']}>{target['id']}"
    ref = threads.get(key)
    sub = store.get_chat(ref) if isinstance(ref, str) and not fresh else None
    if sub is None:
        sub = store.create_chat(target["id"])
        sub["parent"] = {"root_chat_id": root["id"], "caller_id": caller["id"]}
        sub["memory_profile"] = memory.profile()
        if isinstance(ref, list) and not fresh:  # older format kept the thread inline
            sub["messages"] = ref
        threads[key] = sub["id"]
        root.setdefault("subchats", []).append(sub["id"])
        store.save_chat(sub)
        store.save_chat(root)
    return sub


async def delegate(run: Run, parent: dict, args: dict, *, depth: int, path: list, chain: list[str]):
    """Send a task or question to another agent and return its reply.

    Each caller→callee pair keeps one conversation (a sub-chat) per root chat, so the callee
    remembers earlier requests and can answer with a question that the caller replies to on
    its next call.
    """
    contacts = contactable(parent, chain)
    names = ", ".join(a["name"] for a in contacts) or "nobody"
    wanted = str(args.get("agent", "")).strip().lower()
    target = next((a for a in store.list_agents() if a["name"].lower() == wanted), None)
    if len(chain) > MAX_DEPTH:
        return "Error: delegation depth limit reached; do the work yourself.", None
    if not target:
        return f"Error: there is no agent called '{args.get('agent')}'. You can contact: {names}", None
    if target["id"] == parent["id"]:
        return "Error: you can't ask yourself.", None
    if target["id"] in chain:
        return (f"Error: {target['name']} is waiting on you right now (this work came from it). To ask it "
                "something, end your turn with your question; it will reply."), None
    if target["id"] not in {a["id"] for a in contacts}:
        return f"Error: you are not allowed to contact {target['name']}. You can contact: {names}", None
    message = str(args.get("message") or args.get("task") or "").strip()
    if not message:
        return "Error: 'message' is required.", None

    sub = thread_chat(run.chat, parent, target, fresh=args.get("new_conversation") in (True, "true"))
    msgs = sub["messages"]
    start = len(msgs)
    msgs.append({"role": "user", "content": message, "_turn": run.turn,
                 "_recall": memory.recall_block(message, (sub.get("memory_profile") or {}).get("ids")) if target["memory"] else ""})
    if sub["title"] == "New chat":
        first = message.splitlines()[0]
        sub["title"] = first[:60] + ("…" if len(first) > 60 else "")
    store.save_chat(sub)
    broadcast({"type": "chats_changed"})

    run.emit("subagent_start", path=path, agent_id=target["id"], chat_id=sub["id"], continued=start > 0)
    view = View(run, sub, path, start)
    run.push(view)
    try:
        answer = await agent_loop(run, target, sub, depth=depth + 1, path=path, caller=parent,
                                  chain=chain + [target["id"]])
    finally:
        repair(msgs)
        store.save_chat(sub)
        run.pop(view)
    run.emit("subagent_end", path=path, agent_id=target["id"])
    info = {"agent_id": target["id"], "chat_id": sub["id"], "messages": msgs[start:], "continued": start > 0}
    return answer or "(the agent returned an empty answer)", info


# ------------------------------------------------------------ run control

def start(chat: dict, agent: dict, mode: str = "reply") -> Run:
    cancel_extraction()  # real requests always win the (single) model slot
    run = Run(chat)
    runs[chat["id"]] = run
    run.task = asyncio.create_task(_run_top(run, agent, mode))
    return run


async def _run_top(run: Run, agent: dict, mode: str) -> None:
    chat = run.chat
    run.publish()
    run.emit("run_start", agent_id=agent["id"], chat=chat, run_base=run.base)
    ok = False
    try:
        if mode == "compact":
            n_ctx = await run.context_size() or 32768
            if await history.compact(run, agent, chat, path=[], n_ctx=n_ctx, reason="manual",
                                     client=client, settings=store.get_settings()):
                contacts = contactable(agent, [agent["id"]])
                show_context(run, chat, [], estimate_context(agent, chat, None, contacts, agent_tools(agent, contacts)))
                store.save_chat(chat)
            else:
                run.emit("notice", path=[], text="Nothing to compact yet: the conversation is still short.")
            if run.deliver(agent, midturn=False):  # sent while compacting: answer it now
                mode = "reply"
        if mode == "reply":
            await agent_loop(run, agent, chat, depth=0, path=[], chain=[agent["id"]])
            ok = True
    except asyncio.CancelledError:
        run.emit("stopped")
    except llm.LLMError as e:
        chat["messages"].append({"role": "assistant", "content": "", "_error": str(e)})
        run.emit("error", message=str(e))
    except Exception as e:  # keep the server alive and tell the user
        msg = f"Internal error: {type(e).__name__}: {e}"
        chat["messages"].append({"role": "assistant", "content": "", "_error": msg})
        run.emit("error", message=msg)
    finally:
        for fut in run.approvals.values():
            fut.cancel()
        if run.queue:  # the run ended before the agent got these: hand them back, unsent
            broadcast({"type": "queue_returned", "chat_id": chat["id"],
                       "text": "\n\n".join(i["content"] for i in run.dequeue())})
        repair(chat["messages"])
        if store.get_chat(chat["id"]) is not None:  # chat may have been deleted mid-run
            store.save_chat(chat)
        runs.pop(chat["id"], None)
        views.pop(chat["id"], None)
        run.emit("done", chat=chat, ts=time.time())
        broadcast({"type": "chat_status", "chat_id": chat["id"], "status": "idle"})
        broadcast({"type": "chats_changed"})
        if ok and agent["memory"] and store.get_settings().get("auto_memory", True):
            schedule_extraction(chat["id"], agent)


# ------------------------------------------------------- memory extraction

def cancel_extraction() -> None:
    if memory_task and not memory_task.done():
        memory_task.cancel()


def schedule_extraction(chat_id: str, agent: dict) -> None:
    global memory_task
    cancel_extraction()
    memory_task = asyncio.create_task(_extract(chat_id, agent))


def _excerpt(messages: list[dict], limit: int = 12000) -> str:
    parts = []
    for m in messages:
        if m.get("role") == "user":
            parts.append(f"USER: {m.get('content', '')}")
        elif m.get("role") == "assistant" and m.get("content") and not m.get("_error"):
            parts.append(f"ASSISTANT: {m['content'][:3000]}")
    text = "\n\n".join(parts)
    return text[-limit:]


async def tidy_memory() -> str:
    """Let the model merge duplicates and fix contradictions in memory (user-triggered)."""
    request = memory.tidy_request()
    if not request:
        return "Nothing to tidy yet."
    broadcast({"type": "memory_status", "state": "working"})
    try:
        comp = llm.Completion()

        async def ignore(kind: str, text: str, meta: dict | None = None) -> None:
            pass

        await llm.stream_chat(client, store.get_settings(), model="", tools=None, temperature=None,
                              messages=request, out=comp, on_delta=ignore)
        changes = memory.apply_extraction(comp.content, source="tidy")
        broadcast({"type": "memory_changed", "by": "tidy", **changes})
        return (f"Tidied: {len(changes['updated'])} updated, {len(changes['deleted'])} removed."
                if any(changes.values()) else "Memory already looks tidy.")
    finally:
        broadcast({"type": "memory_status", "state": "idle"})


async def _extract(chat_id: str, agent: dict) -> None:
    """Review the turns since the last extraction and update memory. Cancelled by any new run."""
    await asyncio.sleep(1.5)
    if runs:
        return  # the model is busy; the next finished turn will pick these messages up too
    chat = store.get_chat(chat_id)
    if not chat:
        return
    start, end = chat.get("memory_upto", 0), len(chat["messages"])
    new = chat["messages"][start:end]
    if not any(m.get("role") == "user" for m in new):
        return
    broadcast({"type": "memory_status", "state": "working"})
    try:
        comp = llm.Completion()

        async def ignore(kind: str, text: str, meta: dict | None = None) -> None:
            pass

        await llm.stream_chat(client, store.get_settings(), model=agent["model"], tools=None, temperature=None,
                              messages=memory.extraction_request(_excerpt(new)), out=comp, on_delta=ignore)
        changes = memory.apply_extraction(comp.content, source=f"auto:{chat_id}")
        chat = store.get_chat(chat_id)
        if chat and chat_id not in views:
            chat["memory_upto"] = end
            store.save_chat(chat)
        if any(changes.values()):
            broadcast({"type": "memory_changed", "by": "auto", **changes})
    except llm.LLMError:
        pass  # memory is best-effort; the chat itself already succeeded
    finally:
        broadcast({"type": "memory_status", "state": "idle"})
