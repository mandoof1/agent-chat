"""A small MCP server (stdio) for testing app/mcp.py without Node or network.

Run by the app as: python tests/fake_mcp.py   (Settings → MCP; tests/e2e_mcp.py sets it up)
Tools: echo, add (structured result only), wipe (no annotations: may be destructive, so the app
asks first), slow (progress notifications; notes a cancellation), picture (an image), broken
(isError), crash (exits with code 3), last_cancel, client_answers (what the app replied to the
ping and roots/list this server sent it). FAKE_MCP_PAD=N adds N padding tools with long
descriptions, so the catalog is too big to hand the model directly. tools/list pages 40 at a time.
"""

import base64
import json
import os
import sys
import threading
import time

PNG = base64.b64encode(bytes.fromhex(
    "89504e470d0a1a0a0000000d4948445200000001000000010806000000"
    "1f15c4890000000d49444154789c6360f8cfc0f01f0005000201e2a3b1c90000000049454e44ae426082")).decode()
RO = {"readOnlyHint": True}
TOOLS = [
    {"name": "echo", "description": "Echo the given text back.", "annotations": RO,
     "inputSchema": {"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object",
                     "properties": {"text": {"type": "string"}}, "required": ["text"]}},
    {"name": "add", "description": "Add two numbers.", "annotations": RO,
     "inputSchema": {"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}}}},
    {"name": "wipe", "description": "Wipe a target (pretend).", "inputSchema": {"type": "object", "properties": {"target": {"type": "string"}}}},
    {"name": "slow", "description": "Work slowly in steps, reporting progress.", "annotations": RO,
     "inputSchema": {"type": "object", "properties": {"steps": {"type": "integer"}}}},
    {"name": "picture", "description": "Return a tiny picture.", "annotations": RO, "inputSchema": {"type": "object"}},
    {"name": "broken", "description": "Always fails.", "annotations": RO, "inputSchema": {"type": "object"}},
    {"name": "crash", "description": "Exit the server.", "annotations": {"readOnlyHint": False, "destructiveHint": False}},
    {"name": "last_cancel", "description": "The reason of the last cancellation.", "annotations": RO},
    {"name": "client_answers", "description": "What the client answered to our requests.", "annotations": RO},
]
for i in range(int(os.environ.get("FAKE_MCP_PAD", "0"))):
    TOOLS.append({"name": f"pad_{i:03d}", "annotations": RO, "inputSchema": {"type": "object"},
                  "description": f"Padding tool number {i}. " + "It does nothing useful at all. " * 12})

out_lock = threading.Lock()
cancelled: dict = {}
state = {"last_cancel": "", "answers": {}}


def send(msg):
    with out_lock:
        sys.stdout.write(json.dumps(msg) + "\n")
        sys.stdout.flush()


def text(t, error=False):
    return {"content": [{"type": "text", "text": t}], "isError": error}


def call(id_, params):
    name, args = params.get("name"), params.get("arguments") or {}
    token = (params.get("_meta") or {}).get("progressToken")
    if name == "echo":
        result = text(f"echo: {args.get('text')}")
    elif name == "add":
        result = {"content": [], "structuredContent": {"sum": args.get("a", 0) + args.get("b", 0)}}
    elif name == "wipe":
        result = text(f"wiped {args.get('target')}")
    elif name == "slow":
        steps = int(args.get("steps", 3))
        for i in range(steps):
            if id_ in cancelled:
                return  # a cancelled request gets no reply
            time.sleep(0.3)
            if token is not None:
                send({"jsonrpc": "2.0", "method": "notifications/progress",
                      "params": {"progressToken": token, "progress": i + 1, "total": steps, "message": f"step {i + 1}"}})
        result = text("slow done")
    elif name == "picture":
        result = {"content": [{"type": "text", "text": "here it is"}, {"type": "image", "data": PNG, "mimeType": "image/png"}]}
    elif name == "broken":
        result = text("it broke", error=True)
    elif name == "crash":
        sys.stderr.write("fatal: crashed on purpose\n")
        sys.stderr.flush()
        os._exit(3)
    elif name == "last_cancel":
        result = text(state["last_cancel"] or "none")
    elif name == "client_answers":
        result = text(json.dumps(state["answers"], sort_keys=True))
    else:
        send({"jsonrpc": "2.0", "id": id_, "error": {"code": -32602, "message": f"Unknown tool: {name}"}})
        return
    send({"jsonrpc": "2.0", "id": id_, "result": result})


def main():
    for line in sys.stdin:
        msg = json.loads(line)
        method, id_ = msg.get("method"), msg.get("id")
        if method is None:  # the client answering one of our requests
            state["answers"][id_] = "result" if "result" in msg else msg["error"]["code"]
        elif method == "initialize":
            send({"jsonrpc": "2.0", "id": id_, "result": {
                "protocolVersion": msg["params"]["protocolVersion"], "capabilities": {"tools": {"listChanged": True}},
                "serverInfo": {"name": "fake-mcp", "version": "1.0"}, "instructions": "Fake server for tests."}})
        elif method == "notifications/initialized":
            send({"jsonrpc": "2.0", "id": "s-ping", "method": "ping"})
            send({"jsonrpc": "2.0", "id": "s-roots", "method": "roots/list"})
        elif method == "tools/list":
            start = int((msg.get("params") or {}).get("cursor") or 0)
            page = {"tools": TOOLS[start:start + 40]}
            if start + 40 < len(TOOLS):
                page["nextCursor"] = str(start + 40)
            send({"jsonrpc": "2.0", "id": id_, "result": page})
        elif method == "tools/call":
            threading.Thread(target=call, args=(id_, msg["params"]), daemon=True).start()
        elif method == "notifications/cancelled":
            p = msg.get("params") or {}
            cancelled[p.get("requestId")] = True
            state["last_cancel"] = p.get("reason", "")
        elif id_ is not None:
            send({"jsonrpc": "2.0", "id": id_, "error": {"code": -32601, "message": "Method not found"}})


main()
