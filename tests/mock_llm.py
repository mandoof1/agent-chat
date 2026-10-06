"""Fake llama-server for testing the harness without a GPU.

Run: uv run uvicorn tests.mock_llm:app --port 8766
Then point Settings → Model server URL at http://127.0.0.1:8766/v1

Behaviour is keyed off the last user message:
  "shell: <cmd>"   -> calls run_shell
  "write: <text>"  -> calls write_file notes.txt
  "delegate"       -> calls ask_agent(Coder, "shell: echo hello from coder")
  "qa"             -> Orchestrator asks Coder to build something; Coder answers with a
                      QUESTION; Orchestrator replies; Coder must remember the original task
  "boss test"      -> Orchestrator asks Coder, which tries to call Orchestrator back (refused)
  "slow"           -> streams a long reply slowly (for testing Stop and the live context meter)
                      ("notimings" in it: no per-token timings, like servers other than llama-server)
  "cutoff N [crowded|full]" -> the reply fills the context window (finish_reason "length") N
                      times, then answers; "crowded"/"full" report the prompt at 80%/95% of n_ctx
  "delegate cutoff" -> Orchestrator asks Coder "cutoff 2"
  a message queued mid-turn -> "Noted your aside: <message>"
  anything else    -> thinks, then answers with Markdown and a code block
After a tool result it answers with a summary of that result.
"""

import asyncio
import json
import os
import re
import time

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse, StreamingResponse

app = FastAPI()
NCTX = int(os.environ.get("MOCK_NCTX", "85000"))
OVERFLOW_CHARS = int(os.environ.get("MOCK_OVERFLOW_CHARS", "0"))  # reject prompts longer than this


def chunk(delta=None, finish=None, **extra):
    body = {"id": "mock", "object": "chat.completion.chunk", "created": int(time.time()), "model": "mock-model",
            "choices": [{"index": 0, "delta": delta or {}, "finish_reason": finish}] if delta is not None or finish else []}
    body.update(extra)
    return f"data: {json.dumps(body)}\n\n"


def pieces(text, n=6):
    return [text[i:i + n] for i in range(0, len(text), n)]


@app.get("/v1/models")
async def models():
    return {"object": "list", "data": [{"id": "mock-model", "object": "model"}]}


@app.get("/props")
async def props():
    return {"default_generation_settings": {"n_ctx": NCTX}}


@app.post("/v1/chat/completions")
async def completions(request: Request):
    body = await request.json()
    msgs = body["messages"]
    last = msgs[-1]
    tools = {t["function"]["name"] for t in body.get("tools") or []}
    agent = (re.search(r"You are the (\w+) agent", msgs[0]["content"]) or [None, "?"])[1]
    history = " | ".join(str(m.get("content")) for m in msgs[1:] if m["role"] == "user")
    prompt_tokens = len(json.dumps(msgs)) // 3
    if OVERFLOW_CHARS and len(json.dumps(msgs)) > OVERFLOW_CHARS:
        return JSONResponse({"error": {"code": 400, "type": "exceed_context_size_error",
                             "message": "the request exceeds the available context size, try increasing it"}}, 400)
    summarizing = msgs[0]["content"].startswith("You compress conversations")
    extracting = msgs[0]["content"].startswith("You maintain long-term memory")

    async def gen():
        delay = 0.004
        yield chunk({"role": "assistant", "content": None})
        tool = None
        finish, reported, completion = None, prompt_tokens, 50
        n_gen = 0
        per_token = body.get("timings_per_token") and "notimings" not in str(last.get("content"))

        def token(delta):  # one streamed piece; with timings_per_token it carries llama-server's counts
            nonlocal n_gen
            n_gen += 1
            if not per_token:
                return chunk(delta)
            return chunk(delta, timings={"cache_n": 10, "prompt_n": reported - 10, "predicted_n": n_gen})
        if extracting and "Tidy it up" in msgs[0]["content"]:  # memory tidy: delete exact duplicates, reword "yours"
            seen, delete, update = {}, [], []
            for line in last["content"].splitlines():
                m = re.match(r"\[(\d+)\] \(([^)]*)\) (.*)", line)
                if not m:
                    continue
                key = m[3].lower().strip(". ")
                if key in seen:
                    delete.append(int(m[1]))
                else:
                    seen[key] = m[1]
                if "yours" in m[2]:
                    update.append({"id": int(m[1]), "fact": "REWORDED " + m[3]})
            reasoning, text = "Find duplicates.", json.dumps({"update": update, "delete": delete, "add": []})
        elif extracting:  # memory extraction: turn "my name is X" / "I prefer Y" into facts
            excerpt = last["content"].split("Conversation excerpt:", 1)[-1]
            add = []
            if m := re.search(r"my name is (\w+)", excerpt, re.I):
                add.append({"fact": f"The user's name is {m[1]}", "category": "personal", "importance": 9})
            if m := re.search(r"I prefer ([\w ]+)", excerpt):
                add.append({"fact": f"The user prefers {m[1].strip()}", "category": "preference", "importance": 7})
            reasoning, text = "Look for durable facts.", json.dumps({"add": add, "update": [], "delete": []})
            await asyncio.sleep(0.3)
        elif summarizing:
            reasoning = "Summarize the transcript."
            text = f"## Summary\n- Earlier transcript had {last['content'].count('ASSISTANT:')} assistant turns.\n- The user wants big replies."
        elif (last["role"] == "tool" and "list_dir" in tools
              and (loop := re.match(r"loop (\d+)", next(m["content"] for m in msgs if m["role"] == "user")))
              and sum(m["role"] == "tool" for m in msgs) < int(loop[1])):
            reasoning, text = "Keep going.", ""
            tool = ("list_dir", {})
        elif last["role"] == "tool" and agent == "Orchestrator" and "QUESTION" in last["content"]:
            reasoning, text = "Coder asked which language. Answer it.", ""
            tool = ("ask_agent", {"agent": "Coder", "message": "Use Python."})
        elif last["role"] == "tool":
            text = f"Finished. The tool returned:\n\n```\n{last['content'][:200]}\n```"
            reasoning = "The tool ran; summarise the result."
        else:
            user = last["content"]
            reasoning = f"The user said: {user[:60]}. Let me decide what to do."
            text = ""
            if user.startswith("[The user sent this while you were working"):
                text = "Noted your aside: " + user.split("\n", 1)[1].split("\n\n[From your memory")[0]
            elif user.startswith("loop ") and "list_dir" in tools:
                tool = ("list_dir", {})
            elif user.startswith("write code") and "write_file" in tools:
                code = "\n".join(['"""Tiny CLI that greets people."""', "import argparse", "", "",
                                  "def greet(name: str, shout: bool = False) -> str:",
                                  '    msg = f"Hello, {name}!"', "    return msg.upper() if shout else msg", "", "",
                                  "def main() -> None:", "    parser = argparse.ArgumentParser()",
                                  '    parser.add_argument("name")', '    parser.add_argument("--shout", action="store_true")',
                                  "    args = parser.parse_args()", "    print(greet(args.name, args.shout))", "", "",
                                  'if __name__ == "__main__":', "    main()"])
                tool = ("write_file", {"path": "greet.py", "content": code})
                delay = 0.03
            elif user.startswith("what's on my calendar") and "calendar_events" in tools:
                tool = ("calendar_events", {"days": 10})
            elif user.startswith("check my email") and "email_list" in tools:
                tool = ("email_list", {"unread_only": True})
            elif user.startswith("send a reply") and "email_send" in tools:
                tool = ("email_send", {"to": "friend@example.com", "subject": "Re: Hello, World!",
                                       "body": "Hi! Thanks for the message.", "reply_to_uid": "104"})
            elif user.startswith("remember that") and "remember" in tools:
                tool = ("remember", {"fact": "The user " + user[len("remember that I "):].strip(), "category": "preference", "importance": 6})
            elif user.startswith("what is my name"):
                m = re.search(r"The user's name is (\w+)", msgs[0]["content"])
                text = f"Your name is {m[1]}." if m else "I don't know your name."
            elif user.startswith("what do you recall"):
                text = "Recall block: " + (user.split("[From your memory of the user, possibly relevant]")[-1].strip()
                                           if "[From your memory" in user else "none")
            elif user == "Build a hello script.":
                text = "QUESTION: which language should I use?"
            elif user == "Use Python.":
                text = ("Building it in Python. I remember the task: Build a hello script."
                        if "Build a hello script." in history else "I have no memory of any task.")
            elif user == "qa" and "ask_agent" in tools:
                tool = ("ask_agent", {"agent": "Coder", "message": "Build a hello script."})
            elif user == "boss test" and "ask_agent" in tools:
                tool = ("ask_agent", {"agent": "Coder", "message": "call your boss"})
            elif user == "call your boss" and "ask_agent" in tools:
                tool = ("ask_agent", {"agent": "Orchestrator", "message": "hi boss"})
            elif user.startswith("shell:") and "run_shell" in tools:
                tool = ("run_shell", {"command": user[6:].strip()})
            elif user.startswith("write:") and "write_file" in tools:
                tool = ("write_file", {"path": "notes.txt", "content": user[6:].strip()})
            elif user.startswith("search:") and "web_search" in tools:
                tool = ("web_search", {"query": user[7:].strip()})
            elif user == "delegate cutoff" and "ask_agent" in tools:
                tool = ("ask_agent", {"agent": "Coder", "message": "cutoff 2"})
            elif user.startswith(("cutoff", "[Your previous reply was cut off")):
                n, mode = re.search(r"cutoff (\d+) ?(\w*)", history).groups()
                done = [int(k) for k in re.findall(r"Round (\d+):", user)]
                k = max(done, default=0) + 1
                if k <= int(n):
                    reasoning = f"Round {k}: " + "pondering the design. " * 300
                    text = f"partial answer {k}."
                    tool = ("list_dir", {}) if "list_dir" in tools else None  # cut off mid-call: must not run
                    finish = "length"
                    if k == 1 and mode in ("crowded", "full"):
                        reported = int(NCTX * (0.8 if mode == "crowded" else 0.95))
                    completion = max(1, NCTX - reported)
                else:
                    notes = sum(m["role"] == "user" and str(m["content"]).startswith("[Your previous reply") for m in msgs)
                    leaked = sum("Round " in (m.get("reasoning_content") or "") for m in msgs if m["role"] == "assistant")
                    reasoning = "Done now."
                    text = f"Final answer after {n} cut-offs. notes_in_payload={notes} leaked={leaked}"
            elif "delegate" in user and "ask_agent" in tools:
                tool = ("ask_agent", {"agent": "Coder", "message": "shell: echo hello from coder"})
            elif user.startswith("think long"):
                reasoning = "\n".join(f"Step {i}: considering option {i} carefully before answering." for i in range(120))
                text = "Thought it through. Final answer: 42."
                delay = 0.012
            elif user.startswith("big"):
                text = "Big reply. " + " ".join(f"token{i}" for i in range(500))
            elif "slow" in user:
                text = " ".join(f"word{i}" for i in range(400))
                delay = 0.03
            else:
                text = ("Here is an answer with **Markdown**.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n"
                        "```python\ndef hello():\n    print('hi')\n```\n\nDone.")
        for p in pieces(reasoning):
            yield token({"reasoning_content": p})
            await asyncio.sleep(delay)
        for p in pieces(text):
            yield token({"content": p})
            await asyncio.sleep(delay)
        if tool:
            args = json.dumps(tool[1])
            yield chunk({"tool_calls": [{"index": 0, "id": f"call_{int(time.time()*1000)}", "type": "function",
                                         "function": {"name": tool[0], "arguments": ""}}]})
            for p in pieces(args, 10):
                yield token({"tool_calls": [{"index": 0, "function": {"arguments": p}}]})
                await asyncio.sleep(delay)
        yield chunk({}, finish=finish or ("tool_calls" if tool else "stop"))
        yield chunk(None, usage={"prompt_tokens": reported, "completion_tokens": completion, "total_tokens": reported + completion},
                    timings={"predicted_per_second": 33.3, "prompt_per_second": 900.0, "cache_n": 10})
        yield "data: [DONE]\n\n"

    return StreamingResponse(gen(), media_type="text/event-stream")
