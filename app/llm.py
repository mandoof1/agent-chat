"""Streaming client for OpenAI-compatible /chat/completions servers.

Works with llama.cpp's llama-server (including its `reasoning_content` thinking
stream and `timings` stats), Ollama, LM Studio, vLLM, etc.
"""

import json
import time
from typing import Awaitable, Callable

import httpx


class LLMError(Exception):
    pass


class Completion:
    """Accumulates one streamed reply. Readable mid-stream, so a stopped reply can still be saved."""

    def __init__(self):
        self.content = ""
        self.reasoning = ""
        self.calls: dict[int, dict] = {}
        self.finish_reason: str | None = None
        self.usage: dict | None = None
        self.timings: dict | None = None
        self.reason_t0: float | None = None  # when the first / last piece of thinking arrived
        self.reason_t1: float | None = None

    def message(self) -> dict:
        msg = {"role": "assistant", "content": self.content}
        if self.reasoning:
            msg["reasoning_content"] = self.reasoning
        if self.calls:
            msg["tool_calls"] = [
                {
                    "id": c["id"] or f"call_{i}",
                    "type": "function",
                    "function": {"name": c["name"], "arguments": c["args"] or "{}"},
                }
                for i, c in sorted(self.calls.items())
            ]
        return msg

    def stats(self) -> dict:
        stats = {}
        if self.usage:
            stats["prompt_tokens"] = self.usage.get("prompt_tokens")
            stats["completion_tokens"] = self.usage.get("completion_tokens")
        if self.timings:
            stats["tok_per_s"] = self.timings.get("predicted_per_second")
            stats["prompt_per_s"] = self.timings.get("prompt_per_second")
            stats["cached_tokens"] = self.timings.get("cache_n")
        if self.reasoning and self.reason_t0 is not None:
            stats["think_s"] = round(max(0.0, (self.reason_t1 or self.reason_t0) - self.reason_t0), 1)
        return {k: v for k, v in stats.items() if v is not None}


def _headers(settings: dict) -> dict:
    return {"Authorization": f"Bearer {settings.get('api_key') or 'none'}"}


def _base(settings: dict) -> str:
    return settings["base_url"].rstrip("/")


async def stream_chat(
    client: httpx.AsyncClient,
    settings: dict,
    *,
    model: str,
    messages: list[dict],
    tools: list[dict] | None,
    temperature: float | None,
    out: Completion,
    on_delta: Callable[..., Awaitable[None]],  # (kind, text) or (kind, text, meta) for tool calls
    llama: bool = True,  # the server is llama-server: ask for its per-token timings (other servers may reject unknown fields)
) -> Completion:
    body = {
        "model": model or settings.get("model") or "local",
        "messages": messages,
        "stream": True,
        "stream_options": {"include_usage": True},
    }
    if llama:
        body["timings_per_token"] = True  # token counts on every chunk, for the live context meter
    if tools:
        body["tools"] = tools
    if temperature is not None:
        body["temperature"] = temperature

    url = _base(settings) + "/chat/completions"
    try:
        # read=None: prompt processing of a long context can take minutes before the first token.
        async with client.stream("POST", url, json=body, headers=_headers(settings),
                                 timeout=httpx.Timeout(None, connect=10.0)) as resp:
            if resp.status_code >= 400:
                text = (await resp.aread()).decode(errors="replace")
                raise LLMError(f"Model server returned HTTP {resp.status_code}: {text[:800]}")
            async for line in resp.aiter_lines():
                if not line.startswith("data:"):
                    continue
                data = line[5:].strip()
                if data == "[DONE]":
                    break
                try:
                    chunk = json.loads(data)
                except json.JSONDecodeError:
                    continue
                if chunk.get("error"):
                    err = chunk["error"]
                    raise LLMError(err.get("message") if isinstance(err, dict) else str(err))
                out.usage = chunk.get("usage") or out.usage
                out.timings = chunk.get("timings") or out.timings
                for choice in chunk.get("choices") or []:
                    delta = choice.get("delta") or {}
                    if delta.get("reasoning_content"):
                        now = time.monotonic()
                        out.reason_t0 = out.reason_t0 if out.reason_t0 is not None else now
                        out.reason_t1 = now
                        out.reasoning += delta["reasoning_content"]
                        await on_delta("reasoning", delta["reasoning_content"])
                    if delta.get("content"):
                        out.content += delta["content"]
                        await on_delta("content", delta["content"])
                    for tc in delta.get("tool_calls") or []:
                        index = tc.get("index", 0)
                        slot = out.calls.setdefault(index, {"id": "", "name": "", "args": ""})
                        if tc.get("id"):
                            slot["id"] = tc["id"]
                        fn = tc.get("function") or {}
                        if fn.get("name"):
                            slot["name"] += fn["name"]
                        piece = fn.get("arguments") or ""
                        slot["args"] += piece
                        if piece or fn.get("name"):  # lets the UI show the call while it is being written
                            await on_delta("tool_args", piece, {"index": index, "name": slot["name"]})
                    if choice.get("finish_reason"):
                        out.finish_reason = choice["finish_reason"]
    except httpx.ConnectError:
        raise LLMError(f"Can't reach the model server at {_base(settings)}. Is llama-server running?")
    except httpx.HTTPError as e:
        raise LLMError(f"Connection to the model server failed: {type(e).__name__}: {e}")
    return out


async def server_info(client: httpx.AsyncClient, settings: dict) -> dict:
    """Model list plus, for llama-server, the context size from /props (`kind` says which server it is)."""
    info = {"ok": False, "models": [], "n_ctx": None, "error": None, "kind": "other", "base_url": _base(settings)}
    try:
        r = await client.get(_base(settings) + "/models", headers=_headers(settings), timeout=3)
        r.raise_for_status()
        info["models"] = [m.get("id") for m in r.json().get("data", []) if m.get("id")]
        info["ok"] = True
    except Exception as e:
        info["error"] = f"{type(e).__name__}: {e}" if str(e) else type(e).__name__
        return info
    try:
        root = _base(settings).removesuffix("/v1")
        r = await client.get(root + "/props", headers=_headers(settings), timeout=3)
        if r.status_code == 200:
            props = r.json()
            info["n_ctx"] = (props.get("default_generation_settings") or {}).get("n_ctx") or props.get("n_ctx")
            info["kind"] = "llama"
    except Exception:
        pass
    return info
