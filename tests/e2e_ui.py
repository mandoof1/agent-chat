"""The web UI, driven in real headless Brave with Playwright, against the app on AC_URL (default
:8767) wired to tests/mock_llm.py on :8766.

Covers: loading without console errors, streaming a reply, approvals (including "approve all for
this run"), live shell output, the files drawer and viewer, the command palette, branching, image
attachments, model-written titles, unread dots, the error box's retry, keyboard shortcuts, and the
mobile layout. Run: uv run python tests/e2e_ui.py   (BROWSER_EXECUTABLE overrides the Brave path)
"""
import asyncio
import json
import os
import struct
import sys
import tempfile
import time
import zlib

import httpx
from playwright.async_api import async_playwright, expect

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")
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
    await page.click("#history .tool-card .t-label .btn:has-text('Open')", force=True) if await page.locator("#history .tool-card .t-label .btn:has-text('Open')").count() else None
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
    await api("PUT", "/api/settings", {"base_url": "http://127.0.0.1:8766/v1"})
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


async def main():
    await api("PUT", "/api/settings", {"base_url": "http://127.0.0.1:8766/v1", "bypass_approvals": False, "auto_title": True, "auto_memory": False})
    errors = ERRORS
    async with async_playwright() as p:
        browser = await p.chromium.launch(executable_path=BRAVE, headless=True, args=["--no-sandbox", "--disable-dev-shm-usage"])
        ctx = await browser.new_context(viewport={"width": 1440, "height": 900}, color_scheme="dark")
        page = await ctx.new_page()
        page.on("pageerror", lambda e: errors.append(f"pageerror: {e}"))
        page.on("console", lambda m: errors.append(f"console.{m.type}: {m.text}") if m.type == "error" else None)

        try:
            await desktop(page)
        except BaseException:
            if SHOTS:
                await page.screenshot(path=os.path.join(SHOTS, "ui-failure.png"))
                print("screenshot of the failure:", os.path.join(SHOTS, "ui-failure.png"), file=sys.stderr)
            raise
        await ctx.close()

        print("== mobile layout: menu opens the chat list, composer fits")
        ctx = await browser.new_context(viewport={"width": 390, "height": 844}, color_scheme="dark", is_mobile=True, has_touch=True)
        page = await ctx.new_page()
        page.on("pageerror", lambda e: errors.append(f"pageerror: {e}"))
        await page.goto(B)
        await page.click(".agent-card:has-text('Assistant')")
        await expect(page.locator("#composer")).to_be_visible()
        scroll_w = await page.evaluate("document.documentElement.scrollWidth")
        assert scroll_w <= 390, f"horizontal overflow: {scroll_w}"
        await page.click("#menu-btn")
        await expect(page.locator("#sidebar")).to_be_in_viewport()
        await shot(page, "ui-mobile")
        await ctx.close()
        await browser.close()

    assert not errors, "\n".join(errors)
    print("\nall UI tests passed")


async def run():
    try:
        await asyncio.wait_for(main(), 300)
    except BaseException:
        if ERRORS:
            print("browser errors so far:\n  " + "\n  ".join(ERRORS), file=sys.stderr)
        raise


ERRORS = []
asyncio.run(run())
