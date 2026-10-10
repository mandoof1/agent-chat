"""The web UI, driven in real headless Brave with Playwright, against the app on AC_URL (default
:8767) wired to tests/mock_llm.py on :8766.

Covers: loading without console errors, streaming a reply, approvals (including "approve all for
this run"), live shell output, the files drawer and viewer, the command palette, branching, image
attachments, model-written titles, unread dots, the error box's retry, keyboard shortcuts, slash
commands (the menu and every command's effect), the chat footer (times compacted, how long it has
been going), pasting very large text into the message box, and the mobile layout.
Run: uv run python tests/e2e_ui.py   (BROWSER_EXECUTABLE overrides the Brave path)
"""
import asyncio
import json
import os
import re
import struct
import sys
import tempfile
import time
import zlib

import httpx
from playwright.async_api import async_playwright, expect

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")
MOCK = os.environ.get("MOCK_URL", "http://127.0.0.1:8766/v1")
BRAVE = os.environ.get("BROWSER_EXECUTABLE", "/usr/bin/brave")
SHOTS = os.environ.get("UI_SHOTS")  # a folder: save screenshots there too


def png_bytes(w=6, h=6):
    raw = b"".join(b"\x00" + b"\x20\xa0\xff" * w for _ in range(h))
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xffffffff)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


async def shot(page, name):
    if SHOTS:
        await page.screenshot(path=os.path.join(SHOTS, f"{name}.png"))


async def api(method, path, body=None):
    async with httpx.AsyncClient(headers={"X-Agent-Chat": "1"}, timeout=30) as c:
        r = await c.request(method, B + path, json=body)
        r.raise_for_status()
        return r.json()


async def desktop(page):
    errors = ERRORS
    print("== loads, lists the starter agents")
    await page.goto(B)
    await expect(page.locator("#welcome h1")).to_contain_text("Pick an agent")
    await expect(page.locator(".agent-row[data-agent]:not(.all)")).to_have_count(9)
    await expect(page.locator("#server-status")).to_have_class("server-status ok", timeout=5000)
    assert not errors, errors

    print("== a reply streams in and renders as Markdown; the model names the chat")
    await page.click(".agent-card:has-text('Assistant')")
    await expect(page.locator("#chat-header")).to_be_visible()
    await page.fill("#input", "hello there, show me a table")
    await page.keyboard.press("Enter")
    await expect(page.locator("#stop-btn")).to_be_visible(timeout=5000)                       # a run is going
    await expect(page.locator("#history .md table")).to_be_visible(timeout=15000)            # finished, re-rendered from history
    await expect(page.locator("#history details.thinking summary")).to_contain_text("Thought for")
    await expect(page.locator("#history .md pre code.hljs")).to_be_visible()
    await expect(page.locator("#history .turn-head .stat")).to_contain_text("tok/s")
    await expect(page.locator("#hdr-title")).to_have_text("Titled: hello there, show", timeout=8000)
    await expect(page.locator("#chat-list .chat-item.active .title")).to_contain_text("Titled: hello there")
    await shot(page, "ui-chat")

    print("== approval card: approve all for this run auto-approves the second command")
    await page.click(".agent-row[data-agent='coder']")
    await page.click("#new-chat-btn")
    await page.fill("#input", "shell twice: echo one")
    await page.keyboard.press("Enter")
    await expect(page.locator(".approval-box")).to_be_visible(timeout=10000)
    await expect(page.locator("#chat-list .chat-item.active .status.waiting")).to_be_visible()
    await shot(page, "ui-approval")
    await page.click(".approval-box button:has-text('Approve all for this run')")
    await expect(page.locator(".tool-card .t-auto")).to_be_visible(timeout=10000)
    await expect(page.locator("#history .tool-card .t-state:has-text('exit 0')")).to_have_count(2, timeout=15000)
    await expect(page.locator("#history .tool-card")).to_have_count(2)
    # the saved chat keeps why it ran without asking, so the label doesn't change when the run ends
    await expect(page.locator("#history .tool-card .t-auto")).to_have_attribute("title", re.compile("you approved everything for this run"))

    print("== live output while a shell command runs")
    await page.fill("#input", "slow shell")
    await page.keyboard.press("Enter")
    await expect(page.locator(".approval-box")).to_be_visible(timeout=10000)
    await page.click(".approval-box button:has-text('Approve')")
    await expect(page.locator("#live pre.live-out")).to_contain_text("tick1", timeout=6000)
    await expect(page.locator("#stop-btn")).to_be_hidden(timeout=20000)  # the run ended
    await expect(page.locator("#history .tool-card .t-state").last).to_have_text("exit 0")
    await expect(page.locator("#history .tool-card pre").last).to_contain_text("tick4")
    assert await page.locator("pre.live-out").count() == 0, "live output is replaced by the result"

    print("== files drawer lists the workspace, the viewer opens a file")
    await page.keyboard.press("Control+.")
    await expect(page.locator("#files")).to_be_visible()
    await page.fill("#input", "write code please")
    await page.keyboard.press("Enter")
    await expect(page.locator("#history .tool-card .t-name:has-text('write_file')")).to_be_visible(timeout=15000)
    await expect(page.locator("#files-list .file-row:has-text('greet.py')")).to_be_visible(timeout=5000)
    await page.click("#files-list .file-row:has-text('greet.py')")
    await expect(page.locator("#viewer-dialog")).to_be_visible()
    await expect(page.locator("#viewer-body pre")).to_contain_text("def greet")
    await shot(page, "ui-viewer")
    await page.keyboard.press("Escape")
    card = page.locator("#history details.tool-card", has=page.locator(".t-label .btn:has-text('Open')")).first
    if await card.count():  # a collapsed card shows nothing to click (display: none): open it, then its file
        await card.locator(":scope > summary").click()
        await card.locator(".t-label .btn:has-text('Open')").first.click()
        await expect(page.locator("#viewer-dialog")).to_be_visible()
    await page.keyboard.press("Escape")
    await page.keyboard.press("Control+.")
    await expect(page.locator("#files")).to_be_hidden()

    print("== command palette finds chats by content and runs commands")
    await page.keyboard.press("Control+k")
    await expect(page.locator("#palette")).to_be_visible()
    await page.keyboard.type("show me a table")
    await expect(page.locator("#palette-list .palette-item .p-title:has-text('Titled: hello there')").first).to_be_visible(timeout=5000)
    await shot(page, "ui-palette")
    await page.keyboard.press("Enter")
    await expect(page.locator("#hdr-title")).to_have_text("Titled: hello there, show")
    await page.keyboard.press("Control+k")
    await page.keyboard.type("keyboard short")
    await page.keyboard.press("Enter")
    await expect(page.locator("#shortcuts-dialog")).to_be_visible()
    await page.keyboard.press("Escape")

    print("== branching makes a copy up to a message; the original keeps its messages")
    await page.hover("#history .turn")
    await page.click("#history .turn-actions .icon-btn[title^='Branch']")
    await expect(page.locator("#hdr-title")).to_contain_text("(branch)", timeout=5000)
    branch_id = (await page.evaluate("location.hash")).split("/")[-1]
    branch = await api("GET", f"/api/chats/{branch_id}")
    assert len(branch["messages"]) == 2, branch["messages"]

    print("== image attachments show a thumbnail and travel with the message")
    tmp = tempfile.NamedTemporaryFile(suffix=".png", delete=False)
    tmp.write(png_bytes()); tmp.close()
    await page.set_input_files("#file-input", tmp.name)
    await expect(page.locator("#attachments .attach-chip img")).to_be_visible(timeout=5000)
    await page.fill("#input", "describe the image")
    await page.keyboard.press("Enter")
    await expect(page.locator("#history .msg-images img")).to_be_visible(timeout=15000)
    await expect(page.locator("#history .md").last).to_have_text("I see 1 image(s).")

    print("== a chat that finishes while you look elsewhere gets an unread dot")
    other = await api("POST", "/api/chats", {"agent_id": "writer"})
    await api("POST", f"/api/chats/{other['id']}/run", {"content": "hello from elsewhere"})
    await page.click(".agent-row.all")
    await expect(page.locator("#chat-list .chat-item:has-text('hello from elsewhere') .status.unread").first).to_be_visible(timeout=15000)
    await page.locator("#chat-list .chat-item:has-text('hello from elsewhere')").first.click()
    await expect(page.locator("#history .md")).to_contain_text("Markdown", timeout=10000)
    await page.click(".agent-row.all")
    await expect(page.locator("#chat-list .chat-item.active .status.unread")).to_have_count(0)  # seen now

    print("== context menu: rename and pin")
    row = page.locator("#chat-list .chat-item:has-text('hello from elsewhere')").first
    await row.click(button="right")
    await page.click("#context-menu button:has-text('Rename')")
    renamed = f"Renamed by test {int(time.time())}"
    await page.fill("#confirm-input", renamed)
    await page.click("#confirm-ok")
    await expect(page.locator(f"#chat-list .chat-item:has-text({renamed!r})").first).to_be_visible(timeout=5000)
    await page.locator(f"#chat-list .chat-item:has-text({renamed!r})").first.click(button="right")
    await page.click("#context-menu button:has-text('Pin to top')")
    await expect(page.locator("#chat-list .chat-group").first).to_have_text("Pinned", timeout=5000)
    await expect(page.locator("#chat-list .chat-item").first).to_contain_text("Renamed by test")

    print("== a dead model server shows an error with 'Try again'; the retry works once it is back")
    await api("PUT", "/api/settings", {"base_url": "http://127.0.0.1:9/v1"})
    await page.locator(f"#chat-list .chat-item:has-text({renamed!r})").first.click()
    await page.fill("#input", "one more")
    await page.keyboard.press("Enter")
    await expect(page.locator("#history .error-box")).to_be_visible(timeout=10000)
    await expect(page.locator("#history .error-box .e-title")).to_have_text("Couldn't reach the model")
    await expect(page.locator("#offline-bar")).to_be_visible(timeout=15000)
    await shot(page, "ui-error")
    await api("PUT", "/api/settings", {"base_url": MOCK})
    await page.click("#history .error-box button:has-text('Try again')")
    await expect(page.locator("#history .error-box")).to_have_count(0, timeout=15000)
    await expect(page.locator("#history .md").last).to_contain_text("Markdown")

    print("== keyboard: n opens the agent picker, ? the shortcuts, Ctrl+B hides the list")
    await page.keyboard.press("Escape")
    await page.locator("body").click(position={"x": 800, "y": 400})
    await page.keyboard.press("n")
    await expect(page.locator("#pick-dialog")).to_be_visible()
    await page.keyboard.press("Escape")
    await page.keyboard.press("Control+b")
    await expect(page.locator("#sidebar")).to_be_hidden()
    await page.keyboard.press("Control+b")
    await expect(page.locator("#sidebar")).to_be_visible()

    print("== settings dialog: tabs switch, test connection reports the server")
    await page.click("#settings-btn")
    await page.click("#settings-tabs .tab[data-tab='agents']")
    await expect(page.locator(".tab-pane[data-pane='agents']")).to_be_visible()
    await page.click("#settings-tabs .tab[data-tab='model']")
    await page.click("#settings-test-btn")
    await expect(page.locator("#settings-test")).to_contain_text("Connected", timeout=5000)
    await page.keyboard.press("Escape")

    print("== agent editor: prompt preview, duplicate, delete the copy")
    await page.click(".agent-row[data-agent='writer']", button="right")
    await page.click("#context-menu button:has-text('Edit agent')")
    await expect(page.locator("#agent-dialog")).to_be_visible()
    await page.click("#agent-preview")
    await expect(page.locator("#prompt-preview")).to_contain_text("You are the Writer agent")
    await page.keyboard.press("Escape")
    await page.click("#agent-duplicate")
    await expect(page.locator("#agent-dialog-title")).to_have_text("Edit Writer copy", timeout=5000)
    await page.click("#agent-delete")
    await page.click("#confirm-ok")
    await expect(page.locator(".agent-row[data-agent]:not(.all)")).to_have_count(9, timeout=5000)


async def chat_now(page):
    return await api("GET", f"/api/chats/{(await page.evaluate('location.hash')).split('/')[-1]}")


async def cmd(page, text):
    """Type a slash command into the message box and run it with Enter: the box empties."""
    await page.fill("#input", text)
    await page.keyboard.press("Enter")
    await expect(page.locator("#input")).to_have_value("")


async def send(page, text):
    await page.fill("#input", text)
    await page.keyboard.press("Enter")
    await expect(page.locator("#stop-btn")).to_be_visible(timeout=5000)
    await expect(page.locator("#stop-btn")).to_be_hidden(timeout=20000)


GOING = r"going for (\d+s|\d+m)"  # a chat made during this run: under an hour old
SPANS = ((-5, "0s"), (0, "0s"), (45, "45s"), (59.9, "59s"), (60, "1m"), (719, "11m"), (3599, "59m"),  # as in test_unit.py
         (3600, "1h 0m"), (11520, "3h 12m"), (86399, "23h 59m"), (86400, "1d 0h"), (187200, "2d 4h"))


async def slash_commands(page):
    menu, opts, card = page.locator("#slash-menu"), page.locator("#slash-list [role=option]"), page.locator("#cmd-out .cmd-card")
    foot = page.locator("#chat-foot")
    chat = await api("POST", "/api/chats", {"agent_id": "assistant"})
    await page.evaluate("id => location.hash = '#/chat/' + id", chat["id"])
    await expect(page.locator("#hdr-title")).to_have_text("New chat")
    await expect(foot).to_be_hidden()  # no messages yet
    await send(page, "hello for commands")

    print("== the footer under the last turn: not compacted yet, and how long the chat has been going, counting up")
    await expect(foot).to_have_text(re.compile(rf"^not compacted yet · {GOING}$"))
    geo = await page.evaluate("""() => { const f = document.querySelector('#chat-foot').getBoundingClientRect(),
        t = [...document.querySelectorAll('#history .turn')].at(-1).getBoundingClientRect(); return { below: f.top >= t.bottom - 1, h: f.height }; }""")
    assert geo["below"] and geo["h"] < 24, geo  # one line, after the last turn
    was = await foot.inner_text()
    await expect(foot).not_to_have_text(was, timeout=3000)  # live: the seconds tick
    spans = await page.evaluate("async (c) => { const { fmtSpan } = await import('/static/js/util.js'); return c.map(([s]) => fmtSpan(s)); }",
                                [list(c) for c in SPANS])
    assert spans == [want for _, want in SPANS], list(zip(SPANS, spans))

    print("== slash commands: '/' opens the menu above the message box, listing every command")
    await page.click("#input")
    await page.keyboard.type("/")
    await expect(menu).to_be_visible()
    await expect(opts).to_have_count(26)
    await expect(opts.first).to_have_attribute("aria-selected", "true")
    await expect(opts.first).to_contain_text("/help")
    await expect(page.locator("#input")).to_have_attribute("aria-activedescendant", "slash-opt-0")
    geo = await page.evaluate("""() => { const m = document.querySelector('#slash-menu').getBoundingClientRect(),
        box = document.querySelector('.composer-box').getBoundingClientRect(), list = document.querySelector('#slash-list');
        return { above: m.bottom <= box.top, rows: list.clientHeight / list.querySelector('[role=option]').offsetHeight }; }""")
    assert geo["above"] and geo["rows"] <= 8.5, geo
    await shot(page, "ui-slash-menu")

    print("== typing filters it (own names before aliases); arrows move, wrapping; Tab completes; Esc closes")
    await page.keyboard.type("co")
    await expect(opts).to_have_count(5)
    assert [t.split()[0] for t in await opts.all_inner_texts()] == ["/compact", "/copy", "/context", "/settings", "/usage"]
    await expect(opts.nth(3)).to_contain_text("/config")
    await page.keyboard.press("ArrowDown")
    await expect(page.locator("#input")).to_have_attribute("aria-activedescendant", "slash-opt-1")
    await page.keyboard.press("ArrowUp")
    await page.keyboard.press("ArrowUp")
    await expect(opts.nth(4)).to_have_attribute("aria-selected", "true")  # wrapped to the end
    await page.keyboard.press("ArrowDown")  # (not Ctrl+N: off a Mac the browser keeps it for a new window, CDP keys or not)
    await expect(opts.nth(0)).to_have_attribute("aria-selected", "true")
    await page.keyboard.press("Tab")
    await expect(page.locator("#input")).to_have_value("/compact ")  # takes arguments: a space follows
    await expect(menu).to_be_hidden()
    await page.fill("#input", "")
    await page.keyboard.type("/he")
    await page.keyboard.press("Tab")
    await expect(page.locator("#input")).to_have_value("/help")
    await expect(menu).to_be_visible()
    await page.keyboard.press("Escape")
    await expect(menu).to_be_hidden()
    await expect(page.locator("#input")).to_have_value("/help")
    await page.keyboard.press("Backspace")
    await expect(menu).to_be_visible()  # typing reopens it

    print("== /help (Enter on the selection) shows every command in a card, not a chat message")
    await page.keyboard.press("Enter")
    await expect(page.locator("#input")).to_have_value("")
    await expect(card.locator(".cmd-name")).to_have_text("/help")
    await expect(card.locator("dt")).to_have_count(26)
    await expect(card).to_contain_text("/remember <fact>")
    await expect(card).to_contain_text("also /cost, /stats")

    print("== clicking a row runs it; a row that needs an argument completes instead")
    await page.keyboard.type("/")
    await opts.filter(has_text="/context").click()
    await expect(card.locator(".cmd-name")).to_have_text("/context")
    await expect(card.locator(".cmd-line")).to_contain_text("auto-compacts at 70%")
    await expect(card.locator(".cmd-line")).to_contain_text(re.compile(r"ctx [\d.]+k? / 85k \(\d+%\)"))
    await page.keyboard.type("/rem")
    await opts.filter(has_text="/remember").click()
    await expect(page.locator("#input")).to_have_value("/remember ")
    await expect(page.locator("#input")).to_be_focused()

    print("== /remember <fact> saves it to memory")
    await page.keyboard.type("my favourite colour is teal")
    await page.keyboard.press("Enter")
    await expect(page.locator("#toasts .toast", has_text="Remembered: my favourite colour is teal")).to_be_visible()
    assert any(m["fact"] == "my favourite colour is teal" for m in (await api("GET", "/api/memory?q=teal"))["memories"])

    print("== /retry regenerates the last reply in place")
    before = (await chat_now(page))["messages"]
    await cmd(page, "/retry")
    await expect(page.locator("#stop-btn")).to_be_hidden(timeout=20000)
    await expect(page.locator("#history .turn")).to_have_count(1)
    after = (await chat_now(page))["messages"]
    assert len(after) == len(before) == 2 and after[1]["_ts"] > before[1]["_ts"], (before, after)

    print("== /compact with instructions: the summary keeps what was asked")
    await send(page, "second message for commands")
    await cmd(page, "/compact keep the table")
    await expect(page.locator("details.compact-divider summary")).to_contain_text("keeping keep the table", timeout=15000)
    comp = (await chat_now(page))["compactions"][-1]
    assert comp["instructions"] == "keep the table" and "Kept as asked: keep the table" in comp["summary"], comp
    await expect(foot).to_have_text(re.compile(rf"^compacted 1× · {GOING}$"))
    await cmd(page, "/context")
    await expect(card.locator(".cmd-age")).to_have_text(re.compile(rf"^compacted 1× · started [^(]+ \({GOING}\)$"))

    print("== /usage, /status, /model (list, then switch)")
    await cmd(page, "/usage")
    await expect(card.locator(".cmd-line")).to_contain_text(re.compile(r"in · .* out · 2 replies"))
    await expect(card.locator(".cmd-age")).to_have_text(re.compile(rf"^compacted 1× · started [^(]+ \({GOING}\)$"))
    await cmd(page, "/status")
    await expect(card).to_contain_text("reachable")
    await expect(card).to_contain_text("mock-model")
    await expect(card).to_contain_text("Agent Chat 0.")
    await cmd(page, "/model")
    await expect(card.locator(".cmd-line")).to_have_text("Assistant uses the default: mock-model")
    await expect(card.locator(".cmd-opt.on")).to_have_text("default · mock-model")
    await cmd(page, "/model mock")
    await expect(card.locator(".cmd-opt.on")).to_have_text("mock-model")
    assert next(a for a in (await api("GET", "/api/state"))["agents"] if a["id"] == "assistant")["model"] == "mock-model"
    await card.locator(".cmd-opt", has_text="default").click()
    await expect(card.locator(".cmd-opt.on")).to_have_text("default · mock-model")
    assert next(a for a in (await api("GET", "/api/state"))["agents"] if a["id"] == "assistant")["model"] == ""
    await shot(page, "ui-slash-model")

    print("== /copy puts the last reply on the clipboard; /export downloads md and json")
    await cmd(page, "/copy")
    await expect(page.locator("#toasts .toast", has_text="Copied")).to_be_visible()
    assert (await page.evaluate("navigator.clipboard.readText()")).startswith("Here is an answer with **Markdown**")
    async with page.expect_download() as dl:
        await cmd(page, "/export")
    assert (await dl.value).suggested_filename.endswith(".md"), (await dl.value).suggested_filename
    md = open(await (await dl.value).path(), encoding="utf-8").read()
    assert re.search(rf"^Agent: Assistant · compacted 1× · started \d{{4}}-\d\d-\d\d \d\d:\d\d \({GOING}\)$", md, re.M), md[:300]
    async with page.expect_download() as dl:
        await cmd(page, "/export json")
    assert (await dl.value).suggested_filename.endswith(".json")
    await expect(page.locator("#hdr-title")).not_to_have_text("")  # still on the chat

    print("== /rename <title> and /pin change the chat")
    await cmd(page, "/rename Slash renamed")
    await expect(page.locator("#hdr-title")).to_have_text("Slash renamed")
    assert (await chat_now(page))["title"] == "Slash renamed"
    await cmd(page, "/pin")
    await expect(page.locator("#chat-list .chat-item.active .title svg")).to_be_visible()
    assert (await chat_now(page))["pinned"] is True
    await cmd(page, "/pin")
    await expect(page.locator("#toasts .toast", has_text="Unpinned")).to_be_visible()
    assert (await chat_now(page))["pinned"] is False

    print("== unknown commands toast and keep the text; // and paths are sent as messages")
    await page.fill("#input", "/frobnicate now")
    await page.keyboard.press("Enter")
    await expect(page.locator("#toasts .toast", has_text="Unknown command /frobnicate")).to_be_visible()
    await expect(page.locator("#input")).to_have_value("/frobnicate now")
    await send(page, "//hi there")
    await send(page, "/etc/hosts is broken")
    users = [m["content"] for m in (await chat_now(page))["messages"] if m["role"] == "user"]
    assert users[-2:] == ["/hi there", "/etc/hosts is broken"], users
    await expect(page.locator("#history .msg-user .bubble").last).to_have_text("/etc/hosts is broken")

    print("== while the agent works, a command runs at once (never queued); /retry and /pin wait and stay in the box; /stop stops")
    await page.fill("#input", "slow reply please")
    await page.keyboard.press("Enter")
    await expect(page.locator("#live .md")).to_contain_text("word1", timeout=5000)
    geo = await page.evaluate("""() => ({ foot: document.querySelector('#chat-foot').getBoundingClientRect().top,
        live: document.querySelector('#live .turn').getBoundingClientRect().bottom })""")
    assert geo["foot"] >= geo["live"] - 1 and await foot.is_visible(), geo  # under the running turn
    await cmd(page, "/usage")
    await expect(card.locator(".cmd-name")).to_have_text("/usage")
    await page.fill("#input", "/retry")
    await page.keyboard.press("Enter")
    await expect(page.locator("#toasts .toast", has_text="The agent is working").first).to_be_visible()
    await expect(page.locator("#input")).to_have_value("/retry")  # kept, to run once the reply ends
    await page.fill("#input", "")
    await page.keyboard.type("/ret")  # picked from the menu half-typed: the box keeps the whole command
    await expect(opts.first).to_contain_text("/retry")
    await page.keyboard.press("Enter")
    await expect(page.locator("#input")).to_have_value("/retry")
    await expect(menu).to_be_hidden()
    await page.fill("#input", "/pin")
    await page.keyboard.press("Enter")
    await expect(page.locator("#toasts .toast", has_text="then /pin.")).to_be_visible()  # not the server's 409 "chat is busy"
    await expect(page.locator("#input")).to_have_value("/pin")
    assert (await chat_now(page))["pinned"] is False
    await expect(page.locator("#toasts .toast.err")).to_have_count(0)
    await expect(page.locator("#queue")).to_be_hidden()
    await expect(page.locator("#stop-btn")).to_be_visible()
    await cmd(page, "/stop")
    await expect(page.locator("#stop-btn")).to_be_hidden(timeout=10000)
    await expect(page.locator("#history .stopped-tag")).to_be_visible()
    msgs = (await chat_now(page))["messages"]
    assert not any(str(m.get("content", "")).startswith("/") and m["role"] == "user" and m["content"] not in ("/hi there", "/etc/hosts is broken")
                   for m in msgs), [m.get("content") for m in msgs]

    print("== /edit opens your last message for editing; /theme switches; /verbose, /files, dialogs")
    await cmd(page, "/edit")
    await expect(page.locator("#history .edit-box textarea")).to_be_focused()
    await expect(page.locator("#history .edit-box textarea")).to_have_value("slow reply please")
    await page.keyboard.press("Escape")
    await page.click("#input")
    await cmd(page, "/theme light")
    await expect(page.locator("html")).to_have_attribute("data-theme", "light")
    await cmd(page, "/theme")
    await expect(page.locator("html")).to_have_attribute("data-theme", "dark")
    await cmd(page, "/theme auto")
    await expect(page.locator("html")).not_to_have_attribute("data-theme", re.compile("."))
    await cmd(page, "/verbose")
    await expect(page.locator("body")).to_have_class(re.compile(r"\bverbose\b"))
    await cmd(page, "/verbose")
    await cmd(page, "/files")
    await expect(page.locator("#files")).to_be_visible()
    await cmd(page, "/files")
    await expect(page.locator("#files")).to_be_hidden()
    for text, dialog in (("/settings", "#settings-dialog"), ("/config", "#settings-dialog"), ("/memory", "#memory-dialog"),
                         ("/routines", "#routines-dialog"), ("/agents", "#agent-dialog")):
        await cmd(page, text)
        await expect(page.locator(dialog)).to_be_visible()
        await page.keyboard.press("Escape")
        await expect(page.locator(dialog)).to_be_hidden()
        await page.click("#input")
    await cmd(page, "/permissions")
    await expect(page.locator("#toasts .toast", has_text="Agents ask before running shell commands")).to_be_visible()
    await cmd(page, "/resume hello for")
    await expect(page.locator("#palette")).to_be_visible()
    await expect(page.locator("#palette-q")).to_have_value("hello for")
    await expect(page.locator("#palette-list .palette-item", has_text="Slash renamed").first).to_be_visible(timeout=5000)
    await page.keyboard.press("Escape")

    print("== '/' anywhere starts a command; /branch, /delete (asks first), /new [agent]")
    await page.locator("#messages").click(position={"x": 5, "y": 5})
    await page.keyboard.press("/")
    await expect(page.locator("#input")).to_be_focused()
    await expect(menu).to_be_visible()
    await page.keyboard.press("Escape")
    await page.fill("#input", "")
    main_id = chat["id"]
    await cmd(page, "/branch")
    await expect(page.locator("#hdr-title")).to_contain_text("(branch)", timeout=5000)
    branch_id = (await page.evaluate("location.hash")).split("/")[-1]
    assert branch_id != main_id
    await page.click("#input")
    await cmd(page, "/delete")
    await expect(page.locator("#confirm-dialog")).to_be_visible()
    await page.click("#confirm-ok")
    await expect(page.locator("#welcome")).to_be_visible()
    async with httpx.AsyncClient(headers={"X-Agent-Chat": "1"}) as c:
        assert (await c.get(f"{B}/api/chats/{branch_id}")).status_code == 404
    await page.evaluate("id => location.hash = '#/chat/' + id", main_id)
    await expect(page.locator("#hdr-title")).to_have_text("Slash renamed")
    await page.click("#input")
    await cmd(page, "/new")
    await expect(page.locator("#hdr-title")).to_have_text("New chat")
    await expect(foot).to_be_hidden()
    fresh = await chat_now(page)
    assert fresh["id"] != main_id and fresh["agent_id"] == "assistant"
    await page.click("#input")
    await cmd(page, "/clear coder")
    await expect(page.locator("#hdr-sub")).to_contain_text("Coder")
    assert (await chat_now(page))["agent_id"] == "coder"


async def footer_counts_compactions_mid_run(page):
    """An automatic compaction updates the footer's count when it happens, not only when the run ends."""
    foot = page.locator("#chat-foot")
    chat = await api("POST", "/api/chats", {"agent_id": "assistant"})
    await page.evaluate("id => location.hash = '#/chat/' + id", chat["id"])
    await expect(page.locator("#hdr-title")).to_have_text("New chat")
    await send(page, "big reply")
    await expect(foot).to_have_text(re.compile(rf"^not compacted yet · {GOING}$"))
    await api("PUT", "/api/settings", {"compact_at": 1})  # the next step's prompt is over 1% of the window: it compacts first
    try:
        await page.fill("#input", "slow reply please")
        await page.keyboard.press("Enter")
        await expect(page.locator("#live details.compact-divider")).to_be_visible(timeout=10000)
        await expect(foot).to_have_text(re.compile(rf"^compacted 1× · {GOING}$"), timeout=5000)
        assert await page.locator("#stop-btn").is_visible(), "the run should still be going"
        await shot(page, "ui-footer")
        await page.click("#stop-btn")
        await expect(page.locator("#stop-btn")).to_be_hidden(timeout=10000)
    finally:
        await api("PUT", "/api/settings", {"compact_at": 70})
    assert len((await chat_now(page))["compactions"]) == 1
    await expect(foot).to_have_text(re.compile(rf"^compacted 1× · {GOING}$"))
    await api("POST", f"/api/chats/{chat['id']}/run", {"from_index": 1})  # regenerating the first reply drops the summary of it
    await expect(foot).to_have_text(re.compile(rf"^not compacted yet · {GOING}$"), timeout=10000)
    await expect(page.locator("#stop-btn")).to_be_hidden(timeout=20000)
    assert (await chat_now(page)).get("compactions") == []


async def paste(page, text):
    """Paste text into the message box with Ctrl+V; returns the ms from the key to the box's input event."""
    await page.evaluate("t => navigator.clipboard.writeText(t)", text)
    await page.click("#input")
    await page.evaluate("""() => { window.T = {}; if (window.T_on) return; window.T_on = true; const i = document.querySelector('#input');
        i.addEventListener('keydown', () => T.kd = performance.now(), true); i.addEventListener('input', () => T.in = performance.now(), true); }""")
    await page.keyboard.press("Control+V")
    await page.wait_for_function("() => T.in", timeout=30000)
    return await page.evaluate("T.in - T.kd")


async def layout(page):
    return await page.evaluate("""() => { const i = document.querySelector('#input'), box = document.querySelector('.composer-box').getBoundingClientRect(),
        m = document.querySelector('#messages');
        return { h: i.getBoundingClientRect().height, sw: document.documentElement.scrollWidth, w: innerWidth, bottom: box.bottom,
                 vh: innerHeight, msgs: m.getBoundingClientRect().height, msw: m.scrollWidth, mcw: m.clientWidth }; }""")


async def big_paste(page, mobile=False):
    """Very large pastes keep the page usable: the box grows only to its cap, nothing overflows, sending works."""
    chat = await api("POST", "/api/chats", {"agent_id": "assistant"})
    await page.evaluate("id => location.hash = '#/chat/' + id", chat["id"])
    await expect(page.locator("#hdr-title")).to_have_text("New chat")
    lines = "\r\n".join(f"line {i}\twith a tab, {'some text ' * (i % 7)}end" for i in range(3000))
    await paste(page, lines)
    want = lines.replace("\r\n", "\n")
    assert await page.evaluate("document.querySelector('#input').value") == want
    lay = await layout(page)
    print(f"  {len(want):,} chars on 3,000 lines: box {lay['h']:.0f}px, page {lay['sw']}px wide, messages {lay['msgs']:.0f}px tall")
    assert lay["h"] <= 262 and lay["sw"] <= lay["w"] and lay["bottom"] <= lay["vh"] and lay["msgs"] > 150, lay
    await shot(page, f"ui-paste{'-mobile' if mobile else ''}")
    await page.keyboard.press("Enter")
    await expect(page.locator("#stop-btn")).to_be_hidden(timeout=20000)
    await expect(page.locator("#history .msg-user .more-btn")).to_have_text("Show all")
    sent = [m["content"] for m in (await chat_now(page))["messages"] if m["role"] == "user"]
    assert sent == [want.strip()], [len(s) for s in sent]

    one = "x" * 50_000  # one repeated character: the case Chrome's own paste handles in quadratic time
    took = await paste(page, one)
    await page.evaluate("() => { window.T = {}; }")
    await page.keyboard.press("!")
    await page.wait_for_function("() => T.in")
    key = await page.evaluate("T.in - T.kd")
    lay = await layout(page)
    print(f"  one 50,000-char line: pasted in {took:.0f} ms, next keystroke {key:.0f} ms, box {lay['h']:.0f}px")
    assert took < 1500 and key < 300, (took, key)  # Chrome's own paste of it took ~3 s, and every keystroke after as long
    assert lay["h"] <= 262 and lay["sw"] <= lay["w"] and lay["bottom"] <= lay["vh"], lay
    await page.keyboard.press("Enter")
    await expect(page.locator("#stop-btn")).to_be_hidden(timeout=20000)
    await page.locator("#history .msg-user .more-btn").last.click()
    lay = await layout(page)
    assert lay["sw"] <= lay["w"] and lay["msw"] <= lay["mcw"], lay
    sent = [m["content"] for m in (await chat_now(page))["messages"] if m["role"] == "user"]
    assert sent[-1] == one + "!", len(sent[-1])


async def main():
    await api("PUT", "/api/settings", {"base_url": MOCK, "bypass_approvals": False, "auto_title": True, "auto_memory": False})
    errors = ERRORS
    async with async_playwright() as p:
        browser = await p.chromium.launch(executable_path=BRAVE, headless=True, args=["--no-sandbox", "--disable-dev-shm-usage"])
        ctx = await browser.new_context(viewport={"width": 1440, "height": 900}, color_scheme="dark",
                                        permissions=["clipboard-read", "clipboard-write"])
        page = await ctx.new_page()
        page.on("pageerror", lambda e: errors.append(f"pageerror: {e}"))
        page.on("console", lambda m: errors.append(f"console.{m.type}: {m.text}") if m.type == "error" else None)

        try:
            await desktop(page)
            await slash_commands(page)
            print("== the footer counts a compaction as it happens mid-run")
            await footer_counts_compactions_mid_run(page)
            print("== pasting very large text into the message box")
            await big_paste(page)
        except BaseException:
            if SHOTS:
                await page.screenshot(path=os.path.join(SHOTS, "ui-failure.png"))
                print("screenshot of the failure:", os.path.join(SHOTS, "ui-failure.png"), file=sys.stderr)
            raise
        await ctx.close()

        print("== mobile layout: menu opens the chat list, composer fits")
        ctx = await browser.new_context(viewport={"width": 390, "height": 844}, color_scheme="dark", is_mobile=True, has_touch=True,
                                        permissions=["clipboard-read", "clipboard-write"])
        page = await ctx.new_page()
        page.on("pageerror", lambda e: errors.append(f"pageerror: {e}"))
        await page.goto(B)
        await page.click(".agent-card:has-text('Assistant')")
        await expect(page.locator("#composer")).to_be_visible()
        scroll_w = await page.evaluate("document.documentElement.scrollWidth")
        assert scroll_w <= 390, f"horizontal overflow: {scroll_w}"
        print("== mobile: the command menu fits the screen; a tap runs a command")
        await page.click("#input")
        await page.keyboard.type("/")
        await expect(page.locator("#slash-menu")).to_be_visible()
        box = await page.locator("#slash-menu").bounding_box()
        assert box["x"] >= 0 and box["x"] + box["width"] <= 390 and box["y"] >= 0, box
        assert await page.evaluate("document.documentElement.scrollWidth") <= 390
        await shot(page, "ui-mobile-slash")
        await page.locator("#slash-list [role=option]", has_text="/help").tap()
        await expect(page.locator("#cmd-out .cmd-card")).to_be_visible()
        assert await page.evaluate("document.documentElement.scrollWidth") <= 390
        print("== mobile: pasting very large text")
        await big_paste(page, mobile=True)
        await page.click("#menu-btn")
        await expect(page.locator("#sidebar")).to_be_in_viewport()
        await shot(page, "ui-mobile")
        await ctx.close()
        await browser.close()

    assert not errors, "\n".join(errors)
    print("\nall UI tests passed")


async def run():
    try:
        await asyncio.wait_for(main(), 480)
    except BaseException:
        if ERRORS:
            print("browser errors so far:\n  " + "\n  ".join(ERRORS), file=sys.stderr)
        raise


ERRORS = []
asyncio.run(run())
