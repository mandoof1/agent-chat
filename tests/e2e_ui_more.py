"""More of the web UI's newer features, in real headless Brave with Playwright, against the app on AC_URL
(default :8767) wired to tests/mock_llm.py on MOCK_URL (default :8766). Runs after tests/e2e_ui.py in
tests/run_all.sh, against the same app, so it makes its own agent and chats and puts back the settings it
changes.

Covers what e2e_ui.py doesn't, trying to break it: the slash command registry against the spec's table;
the menu for every command and alias (case, prefixes, substrings, odd characters, a 5,000-character word),
keyboard (every row scrolled into view, wrap-around, Ctrl+P, Home/End, Shift+Tab, Alt), mouse (hover,
focus kept on press, click outside), Tab completion with text after the word, ARIA (listbox, options,
aria-activedescendant); IME composition while typing "/"; the menu next to the palette and dialogs;
commands before a chat has loaded; text the app puts in the box under an open menu (queued messages handed
back, a chat switch) and a message sent with // coming back to the box as //; arguments in every shape (a
command that refuses its arguments, or fails on the server, keeps the line; /export of a chat titled in
Arabic and Japanese); agents made, renamed and deleted elsewhere showing at once; every
idle-only command refused mid-run keeping its exact text (also when picked half-typed or clicked); commands
mid-run never touching the queue; the chat footer (two compactions, /retry keeping and Regenerate trimming
the count, switching chats, a late answer for the previous chat, joining a run that just compacted, a
sub-agent's chat, the running time ticking past minutes, hours and days in step with the /usage and
/context cards, on a fake clock); large pastes (multi-MB, CRLF, lone CR,
tabs, control characters, emoji, over a selection, the long-word fast path and what it costs); Enter,
Shift+Enter, the Send button and the queue working as before; and the menu, cards and footer at 390px
with touch.
Run: uv run python tests/e2e_ui_more.py   (BROWSER_EXECUTABLE overrides the Brave path; ONLY=menu,footer
runs some sections)
"""
import asyncio
import datetime
import json
import os
import re
import sys
import time

import httpx
from playwright.async_api import async_playwright, expect

B = os.environ.get("AC_URL", "http://127.0.0.1:8767")
MOCK = os.environ.get("MOCK_URL", "http://127.0.0.1:8766/v1")
BRAVE = os.environ.get("BROWSER_EXECUTABLE", "/usr/bin/brave")
SHOTS = os.environ.get("UI_SHOTS")
ONLY = set(filter(None, os.environ.get("ONLY", "").split(",")))
U = f"{int(time.time()) % 1000000}{os.getpid() % 100:02d}"  # makes this run's agent, chats, facts and titles unique
AGENT = f"UIMore {U}"

# The spec's table (SPEC Part 2) as the web has it: name, aliases, args hint, chat-only, idle-only. /quit is
# the terminal's alone; /pin became idle-only later (the server refuses to pin a running chat).
SPEC = [
    ("help", [], "", False, False), ("new", ["clear"], "[agent]", False, False),
    ("resume", ["chats"], "[search]", False, False), ("rename", [], "[title]", True, True),
    ("pin", [], "", True, True), ("branch", ["fork"], "", True, False),
    ("retry", ["regenerate"], "", True, True), ("edit", [], "", True, True),
    ("stop", [], "", True, False), ("compact", [], "[instructions]", True, True),
    ("export", [], "[md|json]", True, False), ("copy", [], "", True, False),
    ("delete", [], "", True, True), ("agents", [], "", False, False),
    ("model", [], "[name]", False, False), ("memory", [], "", False, False),
    ("remember", [], "<fact>", False, False), ("permissions", [], "[ask|bypass]", False, False),
    ("settings", ["config"], "", False, False), ("routines", [], "", False, False),
    ("files", [], "", False, False), ("verbose", [], "", False, False),
    ("theme", [], "[auto|light|dark]", False, False), ("context", [], "", True, False),
    ("usage", ["cost", "stats"], "", True, False), ("status", [], "", False, False),
]
GOING = r"going for (\d+s|\d+m)"  # a chat made during this run: under an hour old
DEFECTS = []
ERRORS = []


def js_len(s):
    """A string's length as JavaScript counts it (UTF-16 code units: an emoji is 2)."""
    return len(s.encode("utf-16-le")) // 2


def section(name):
    print(f"== {name}", flush=True)


def soft(ok, what):
    """A check whose failure is a defect to report, not a reason to stop the section."""
    if not ok:
        print(f"!! {what}", flush=True)
        DEFECTS.append(what)
    return ok


async def api(method, path, body=None):
    async with httpx.AsyncClient(headers={"X-Agent-Chat": "1"}, timeout=60) as c:
        r = await c.request(method, B + path, json=body)
        r.raise_for_status()
        return r.json()


async def settings():
    return (await api("GET", "/api/state"))["settings"]


async def api_status(method, path):
    async with httpx.AsyncClient(headers={"X-Agent-Chat": "1"}, timeout=30) as c:
        return (await c.request(method, B + path)).status_code


async def shot(page, name):
    if SHOTS:
        await page.screenshot(path=os.path.join(SHOTS, f"{name}.png"))


async def hook(page):
    """Reach the page's state (the same module instance the app uses) so waits can be exact."""
    await page.wait_for_function("() => document.querySelector('#server-status')")
    await page.evaluate("async () => { window.__S = (await import('/static/js/state.js')).S; }")
    await page.wait_for_function("() => __S.agents.length > 0 && __S.version")


async def open_chat(page, cid):
    await page.evaluate("id => location.hash = '#/chat/' + id", cid)
    await page.wait_for_function("id => __S.chat && __S.chat.id === id", arg=cid, timeout=15000)


async def new_chat(page, title=None, agent=None):
    """A chat with this run's agent, opened; a title of our own keeps the model from renaming it."""
    chat = await api("POST", "/api/chats", {"agent_id": agent or AGENT_ID})
    if title:
        await api("PATCH", f"/api/chats/{chat['id']}", {"title": title})
    await open_chat(page, chat["id"])
    await expect(page.locator("#composer")).to_be_visible()
    return chat["id"]


async def api_reply(cid, content):
    """Send a message through the API and wait until that run has ended."""
    await api("POST", f"/api/chats/{cid}/run", {"content": content})
    for _ in range(600):
        c = next((c for c in await api("GET", "/api/chats") if c["id"] == cid), None)
        if c and c["status"] == "idle" and c["last_role"] == "assistant":
            return
        await asyncio.sleep(0.05)
    raise AssertionError(f"the reply to {content!r} never finished")


async def chat_now(page):
    return await api("GET", f"/api/chats/{await page.evaluate('__S.chatId')}")


async def idle(page, timeout=30000):
    await page.wait_for_function("() => __S.chat && !__S.running", timeout=timeout)


async def send(page, text):
    """Send a message and wait for the reply to finish (the app's own state, not a button's flicker)."""
    n = await page.evaluate("__S.chat.messages.length")
    await page.fill("#input", text)
    await page.keyboard.press("Enter")
    await page.wait_for_function("n => __S.chat && !__S.running && __S.chat.messages.length >= n + 2 && __S.chat.messages.at(-1).role === 'assistant'",
                                 arg=n, timeout=30000)


async def cmd(page, text):
    """Run a slash command typed into the message box: the box empties."""
    await page.fill("#input", text)
    await page.keyboard.press("Enter")
    await expect(page.locator("#input")).to_have_value("")


async def kept(page, text, says):
    """A command that refuses its arguments (or that the server refuses): it says why, and the line stays in
    the box to fix."""
    await page.fill("#input", text)
    await page.keyboard.press("Enter")
    await toast(page, says)
    await expect(page.locator("#input")).to_have_value(text)


async def toast(page, text, timeout=5000):
    await expect(page.locator("#toasts .toast", has_text=text).first).to_be_visible(timeout=timeout)


async def rows(page):
    """The menu's rows as [name, alias, args, desc], or None when it is closed."""
    return await page.evaluate("""() => document.querySelector('#slash-menu').hidden ? null :
        [...document.querySelectorAll('#slash-list [role=option]')].map(r => [r.querySelector('.sc-name')?.textContent,
          r.querySelector('.sc-alias')?.textContent?.trim() || null, r.querySelector('.sc-args')?.textContent?.trim() || '',
          r.querySelector('.sc-desc')?.textContent])""")


async def selected(page):
    """Index of the selected row, whether exactly one row is selected and the box points at it, and
    whether that row is wholly inside the list's scrolled view."""
    return await page.evaluate("""() => { const l = document.querySelector('#slash-list'), rs = [...l.children],
        on = rs.filter(r => r.getAttribute('aria-selected') === 'true'), r = on[0], i = rs.indexOf(r);
        const ad = document.querySelector('#input').getAttribute('aria-activedescendant');
        return { i, one: on.length === 1 && r.classList.contains('selected') && rs.filter(x => x.classList.contains('selected')).length === 1,
                 points: !!r && ad === r.id, seen: !!r && r.offsetTop >= l.scrollTop - 1 && r.offsetTop + r.offsetHeight <= l.scrollTop + l.clientHeight + 1 }; }""")


async def menu_closed(page):
    await expect(page.locator("#slash-menu")).to_be_hidden()
    ad = await page.evaluate("document.querySelector('#input').getAttribute('aria-activedescendant')")
    assert ad is None or await page.locator(f"#{ad}").count() == 1, f"aria-activedescendant points at {ad!r}, which is gone"


def expected_rows(q, chat=True):
    """What the spec says the menu lists for q: commands with a name or alias starting with q, then those
    that only contain it. Order inside each group is the client's; an exact name or alias comes first."""
    q = q.lower()
    avail = [s for s in SPEC if chat or not s[3]]
    names = lambda s: [s[0], *s[1]]  # noqa: E731
    starts = [s[0] for s in avail if any(n.startswith(q) for n in names(s))]
    contains = [s[0] for s in avail if s[0] not in starts and any(q in n for n in names(s))]
    exact = next((s[0] for s in avail if q in names(s)), None)
    return starts, contains, exact


# --------------------------------------------------------------------------------------------- registry

async def registry(page):
    section("the web registry is the spec's table: names, aliases, args, chat-only and idle-only flags, in order")
    web = await page.evaluate("""async () => (await import('/static/js/commands.js')).COMMANDS.map(c =>
        [c.name, c.aliases || [], c.args || '', !!c.chat, !!c.idle, typeof c.run, c.desc || ''])""")
    assert [w[:5] for w in web] == [list(s) for s in SPEC], [(w[:5], list(s)) for w, s in zip(web, SPEC) if w[:5] != list(s)] or (len(web), len(SPEC))
    assert all(w[5] == "function" and w[6] for w in web), "every command has a run function and a description"
    words = [n for s in SPEC for n in (s[0], *s[1])]
    assert len(words) == len(set(words)), "no name or alias is used twice"

    section("/help lists every command once, with its args and its aliases")
    await new_chat(page, f"Registry {U}")
    await cmd(page, "/help")
    card = page.locator("#cmd-out .cmd-card")
    dts = await card.locator("dt").all_inner_texts()
    assert dts == [f"/{n}{f' {a}' if a else ''}" for n, _, a, _, _ in SPEC], dts
    dds = await card.locator("dd").all_inner_texts()
    for (n, aliases, *_), dd in zip(SPEC, dds):
        if aliases:
            assert dd.endswith(f"also {', '.join('/' + a for a in aliases)}"), (n, dd)
        else:
            assert "also /" not in dd, (n, dd)
    await expect(page.locator("#cmd-out section.cmd-card")).to_have_attribute("aria-label", "/help")
    await expect(page.locator("#cmd-out")).to_have_attribute("aria-live", "polite")
    await card.locator(".cmd-link").click()
    await expect(page.locator("#shortcuts-dialog")).to_be_visible()
    await expect(page.locator("#shortcuts-list")).to_contain_text("in an empty one, start a command")
    await page.keyboard.press("Escape")
    await card.get_by_role("button", name="Close").click()
    await expect(card).to_have_count(0)


# ------------------------------------------------------------------------------------------------- menu

async def menu_filtering(page):
    section("the menu: every name and alias (any case) finds its command first; prefixes before substrings")
    await new_chat(page, f"Menu {U}")
    await page.click("#input")
    queries = set()
    for n, aliases, *_ in SPEC:
        for w in (n, *aliases):
            queries |= {w, w.upper(), w[:1], w[:2], w[:3], w[1:4], w[-3:], w.capitalize()}
    bad = []
    for q in sorted(queries):
        await page.fill("#input", "/" + q)
        got = await rows(page)
        starts, contains, exact = expected_rows(q)
        if not starts and not contains:
            if got is not None:
                bad.append((q, "menu open for nothing", got))
            continue
        if got is None:
            bad.append((q, "menu closed", starts + contains))
            continue
        names = [r[0] for r in got]
        if sorted(names[:len(starts)]) != sorted("/" + s for s in starts) or sorted(names[len(starts):]) != sorted("/" + s for s in contains):
            bad.append((q, names, starts, contains))
        if exact and names[0] != "/" + exact:
            bad.append((q, "exact match not first", names))
        for name, alias, args, desc in got:  # aliases match, but the row shows the main name (and the alias that matched)
            spec = next(s for s in SPEC if "/" + s[0] == name)
            if alias is not None and (alias.lstrip("/") not in spec[1] or q.lower() not in alias.lower() or q.lower() in spec[0]):
                bad.append((q, "alias shown wrongly", name, alias))
            if args != spec[2] or not desc:
                bad.append((q, "args or description", name, args, desc))
    assert not bad, bad[:8]
    print(f"  {len(queries)} queries checked")
    await page.fill("#input", "/cost")
    assert (await rows(page))[0][:2] == ["/usage", "/cost"], await rows(page)
    await page.fill("#input", "/REGEN")
    assert (await rows(page))[0][:2] == ["/retry", "/regenerate"], await rows(page)
    # a command's own name before another's alias: "/stat" + Enter is /status, not /usage (alias /stats)
    for q, want in (("/stat", ["/status", "/usage"]), ("/st", ["/stop", "/status", "/usage"]), ("/co", ["/compact", "/copy", "/context", "/settings", "/usage"])):
        await page.fill("#input", q)
        got = await rows(page)
        assert [r[0] for r in got] == want, (q, got)
    assert (await rows(page))[-1][:2] == ["/usage", "/cost"], await rows(page)
    await page.fill("#input", "/st")
    assert [r[:2] for r in await rows(page)][-1] == ["/usage", "/stats"], await rows(page)  # the alias that starts with it
    await page.fill("#input", "/stat")
    await page.keyboard.press("Enter")
    await expect(page.locator("#cmd-out .cmd-name")).to_have_text("/status")
    await page.keyboard.press("Escape")

    section("a query that matches nothing (odd characters, a 5,000-character word) closes the menu; Enter says unknown, keeps the text")
    for q in ("/(", "/.*", "/[a", "/\\", "/?", "/+x", "/zzzz", "/" + "a" * 5000, "/héllo", "/😀"):
        await page.fill("#input", q)
        await menu_closed(page)
        await page.keyboard.press("Enter")
        await toast(page, f"Unknown command /{q[1:40]}")
        await toast(page, "begin it with //")
        await expect(page.locator("#input")).to_have_value(q)
    await expect(page.locator("#cmd-out .cmd-card")).to_have_count(0)


async def menu_keys(page):
    section("keys: arrows walk all 26 rows, each scrolled into view, wrapping both ways; Ctrl+P (and Ctrl+N as a Mac sends it); one selected row")
    await page.fill("#input", "")
    await page.keyboard.type("/")
    await expect(page.locator("#slash-list [role=option]")).to_have_count(26)
    await expect(page.locator("#slash-list")).to_have_attribute("role", "listbox")
    await expect(page.locator("#slash-list")).to_have_attribute("aria-label", "Commands")
    await expect(page.locator("#input")).to_have_attribute("aria-controls", "slash-list")
    await expect(page.locator("#input")).to_have_attribute("aria-autocomplete", "list")
    for i in range(1, 27):
        await page.keyboard.press("ArrowDown")
        s = await selected(page)
        assert s["i"] == i % 26 and s["one"] and s["points"] and s["seen"], (i, s)
    await page.keyboard.press("ArrowUp")
    s = await selected(page)
    assert s["i"] == 25 and s["seen"] and s["points"], s
    await page.keyboard.press("Control+p")
    assert (await selected(page))["i"] == 24
    # Off a Mac a real Ctrl+N never reaches the page (Chrome and Firefox open a new window), so this only
    # checks the handler a Mac's Ctrl+N reaches: CDP delivers the key straight to the page.
    await page.keyboard.press("Control+n")
    await page.keyboard.press("Control+n")
    s = await selected(page)
    assert s["i"] == 0 and s["seen"], s
    await page.keyboard.press("Alt+ArrowDown")  # not the menu's: the selection stays
    assert (await selected(page))["i"] == 0
    await expect(page.locator("#input")).to_have_value("/")

    section("Home/End/arrows: the menu follows the cursor in and out of the command word; Tab completes in front of the args")
    await page.fill("#input", "/comp keep the API notes")
    await menu_closed(page)
    await page.keyboard.press("Home")
    await expect(page.locator("#slash-menu")).to_be_visible()
    assert (await rows(page))[0][0] == "/compact"
    await page.keyboard.press("End")
    await menu_closed(page)
    for _ in range(len(" keep the API notes")):
        await page.keyboard.press("ArrowLeft")
    await expect(page.locator("#slash-menu")).to_be_visible()  # the cursor is at the end of the word: still in it
    await page.keyboard.press("Tab")
    await expect(page.locator("#input")).to_have_value("/compact keep the API notes")
    assert await page.evaluate("[document.querySelector('#input').selectionStart, document.querySelector('#input').selectionEnd]") == [9, 9]
    await expect(page.locator("#input")).to_be_focused()
    await page.fill("#input", "/rem")
    await page.keyboard.press("Tab")
    await expect(page.locator("#input")).to_have_value("/remember ")
    await page.fill("#input", "/HE")
    await page.keyboard.press("Tab")
    await expect(page.locator("#input")).to_have_value("/help")
    await expect(page.locator("#slash-menu")).to_be_visible()

    section("Esc closes the menu and keeps the text; a second Esc closes a card, still keeping the text; Shift+Tab leaves")
    await page.keyboard.press("Escape")
    await menu_closed(page)
    await expect(page.locator("#input")).to_have_value("/help")
    await page.keyboard.press("Enter")  # the menu was dismissed: Enter runs what was typed
    await expect(page.locator("#cmd-out .cmd-name")).to_have_text("/help")
    await page.keyboard.type("/sta")
    await page.keyboard.press("Escape")
    await expect(page.locator("#cmd-out .cmd-card")).to_be_visible()  # the first Esc was the menu's
    await page.keyboard.press("Escape")
    await expect(page.locator("#cmd-out .cmd-card")).to_have_count(0)
    await expect(page.locator("#input")).to_have_value("/sta")
    await page.keyboard.press("Backspace")
    await expect(page.locator("#slash-menu")).to_be_visible()  # typing reopens it
    await page.keyboard.press("Shift+Tab")
    await menu_closed(page)
    await expect(page.locator("#input")).not_to_be_focused()
    await expect(page.locator("#input")).to_have_value("/st")


async def menu_mouse(page):
    section("mouse: hovering selects; pressing a row keeps the focus in the box; a click runs it, or completes one needing an argument")
    await page.click("#input")
    await expect(page.locator("#slash-menu")).to_be_visible()  # a click back into the word reopens the menu
    await page.fill("#input", "/")
    row = page.locator("#slash-list [role=option]", has_text="/status")
    await row.hover()
    await expect(row).to_have_attribute("aria-selected", "true")
    box = await row.bounding_box()
    await page.mouse.move(box["x"] + 20, box["y"] + box["height"] / 2)
    await page.mouse.down()
    assert await page.evaluate("document.activeElement.id") == "input", "a press on a row moved the focus out of the message box"
    await expect(page.locator("#slash-menu")).to_be_visible()
    await page.mouse.up()
    await expect(page.locator("#cmd-out .cmd-name")).to_have_text("/status")
    await expect(page.locator("#input")).to_have_value("")
    await expect(page.locator("#input")).to_be_focused()
    await page.keyboard.type("/rememb")
    await page.locator("#slash-list [role=option]", has_text="/remember").click()
    await expect(page.locator("#input")).to_have_value("/remember ")
    assert await page.evaluate("document.querySelector('#input').selectionStart") == len("/remember ")
    await page.fill("#input", "/he")
    await page.locator("#messages").click(position={"x": 5, "y": 5})
    await menu_closed(page)
    await expect(page.locator("#input")).to_have_value("/he")
    await page.click("#input")
    await page.keyboard.press("End")
    await expect(page.locator("#slash-menu")).to_be_visible()
    await page.keyboard.press("Escape")
    await page.fill("#input", "")


# -------------------------------------------------------------------------------------------- arguments

async def arguments(page):
    section("commands on an empty chat say there is nothing to act on, and send nothing")
    cid = await new_chat(page, f"Args {U}")
    for text, said in (("/copy", "No reply to copy yet"), ("/retry", "Nothing to regenerate yet"), ("/branch", "Nothing to branch yet"),
                       ("/edit", "No message of yours to edit yet"), ("/stop", "Nothing is running"), ("/regenerate", "Nothing to regenerate yet")):
        await cmd(page, text)
        await toast(page, said)
    await cmd(page, "/context")
    await expect(page.locator("#cmd-out .cmd-line")).to_have_text(re.compile(r"^ctx 0 / 85k · nothing sent yet$"))
    await cmd(page, "/usage")
    await expect(page.locator("#cmd-out .cmd-line")).to_have_text("0 in · 0 out · 0 replies")
    await cmd(page, "/stats")
    await expect(page.locator("#cmd-out .cmd-name")).to_have_text("/usage")
    assert (await api("GET", f"/api/chats/{cid}"))["messages"] == []

    section("/export: JSON, .json, Markdown, md in any case download that format; anything else is explained")
    await send(page, "hello for arguments")
    await send(page, "second for arguments")
    for arg, ext in (("JSON", ".json"), (".json", ".json"), ("Markdown", ".md"), ("MD", ".md"), ("", ".md")):
        async with page.expect_download() as dl:
            await cmd(page, f"/export {arg}".strip())
        assert (await dl.value).suggested_filename.endswith(ext), (arg, (await dl.value).suggested_filename)
    await kept(page, "/export pdf", "Export as md or json")
    title = f"محادثة 日本語 {U}"  # a model names chats in the user's language
    await cmd(page, f"/rename {title}")
    await expect(page.locator("#hdr-title")).to_have_text(title)
    for fmt in ("md", "json"):
        async with page.expect_download() as dl:
            await cmd(page, f"/export {fmt}")
        failed = await (await dl.value).failure()
        body = "" if failed else open(await (await dl.value).path(), encoding="utf-8").read()
        soft(not failed and title in body, f"/export {fmt} of a chat titled {title!r} failed ({failed or body[:60]!r}): the server puts the "
                                           "title's letters in the Content-Disposition filename, which must be Latin-1, and the request dies")

    section("/theme names in any case; an unknown name changes nothing")
    await cmd(page, "/theme LIGHT")
    await expect(page.locator("html")).to_have_attribute("data-theme", "light")
    await kept(page, "/theme blue", "Themes: auto, light, dark")
    await expect(page.locator("html")).to_have_attribute("data-theme", "light")
    await cmd(page, "/theme Auto")
    await expect(page.locator("html")).not_to_have_attribute("data-theme", re.compile("."))

    section("/rename trims its title; HTML in arguments stays text everywhere it is shown")
    await cmd(page, f"/rename    Spaced title {U}   ")
    await expect(page.locator("#hdr-title")).to_have_text(f"Spaced title {U}")
    hostile = f"<img src=x onerror=window.__pwned=1> {U}"
    await cmd(page, f"/rename {hostile}")
    await expect(page.locator("#hdr-title")).to_have_text(hostile)
    await cmd(page, f"/remember {hostile} is not a fact")
    await toast(page, "<img src=x")
    await kept(page, f"/new {hostile}", "No agent called")
    assert await page.evaluate("window.__pwned") is None
    await expect(page.locator("#chat-list img[src=x], #toasts img, #hdr-title img")).to_have_count(0)

    section("/remember trims the fact; /resume with and without a search; /compact without instructions")
    await cmd(page, f"/remember    spaced fact {U}   ")
    await toast(page, f"spaced fact {U}")
    assert any(m["fact"] == f"spaced fact {U}" for m in (await api("GET", f"/api/memory?q={U}"))["memories"])
    await kept(page, "/remember", "Say what to remember")
    route = re.compile(r".*/api/memory$")  # the server refusing it: the line stays too
    await page.route(route, lambda r: r.fulfill(status=400, content_type="application/json", body='{"detail": "memory is full"}')
                     if r.request.method == "POST" else r.continue_())
    n = len(ERRORS)
    try:
        await kept(page, f"/remember refused {U}", "memory is full")
    finally:
        await page.unroute(route)
    for _ in range(40):  # the browser logs the 400 the route answered: expected, not a page error
        if any("/api/memory" in e for e in ERRORS[n:]):
            break
        await asyncio.sleep(0.05)
    ERRORS[n:] = [e for e in ERRORS[n:] if not ("status of 400" in e and e.endswith("/api/memory"))]
    await cmd(page, "/chats")
    await expect(page.locator("#palette")).to_be_visible()
    await expect(page.locator("#palette-q")).to_have_value("")
    await page.keyboard.press("Escape")
    await cmd(page, "/resume second for arguments")
    await expect(page.locator("#palette-q")).to_have_value("second for arguments")
    await expect(page.locator("#palette-list .palette-item", has_text=hostile).first).to_be_visible(timeout=5000)  # found by message text
    await page.keyboard.press("Escape")
    await page.click("#input")
    await cmd(page, "/compact")
    await page.wait_for_function("() => __S.chat && !__S.running && (__S.chat.compactions || []).length === 1", timeout=20000)
    comp = (await chat_now(page))["compactions"][-1]
    assert "instructions" not in comp and "Kept as asked" not in comp["summary"], comp
    await expect(page.locator("details.compact-divider summary")).not_to_contain_text("keeping")

    section("/model on this run's own agent: list, exact name in any case, padded, unknown (set anyway, said so), default")
    async def model_now():
        return next(a for a in (await api("GET", "/api/state"))["agents"] if a["id"] == AGENT_ID)["model"]
    await cmd(page, "/model")
    await expect(page.locator("#cmd-out .cmd-line")).to_have_text(f"{AGENT} uses the default: mock-model")
    try:
        await cmd(page, "/model MOCK-MODEL")
        await toast(page, f"{AGENT} now uses mock-model")
        assert await model_now() == "mock-model"
        await expect(page.locator("#cmd-out .cmd-opt.on")).to_have_text("mock-model")  # the open card follows
        async def two_models(route):  # the server lists a second model whose name also has "mock" in it
            r = await route.fetch()
            body = await r.json()
            await route.fulfill(response=r, json={**body, "models": [*body["models"], "mock-model-large"]})
        server = re.compile(r".*/api/server$")
        poll = "async () => (await import('/static/js/main.js')).pollServer()"
        await page.route(server, two_models)
        try:
            await page.evaluate(poll)
            await kept(page, "/model mock", "Which one? mock-model, mock-model-large")  # more than one: it asks, the line stays
        finally:
            await page.unroute(server)
            await page.evaluate(poll)
        assert await model_now() == "mock-model"
        await cmd(page, f"/model    nonesuch-{U}   ")
        await toast(page, "the server doesn't list it")
        assert await model_now() == f"nonesuch-{U}"
        await cmd(page, "/model Default")
        await toast(page, "uses the default model again")
        assert await model_now() == ""
    finally:
        a = next(a for a in (await api("GET", "/api/state"))["agents"] if a["id"] == AGENT_ID)
        await api("PUT", f"/api/agents/{AGENT_ID}", {**a, "model": ""})

    section("/permissions: an unknown word is explained; bypass asks first (Cancel changes nothing); ask switches back")
    before = (await settings())["bypass_approvals"]
    assert before is False
    try:
        await kept(page, "/permissions sometimes", "Use /permissions ask or /permissions bypass")
        await cmd(page, "/permissions ask")
        await toast(page, "already ask")
        await cmd(page, "/permissions BYPASS")
        await expect(page.locator("#confirm-dialog")).to_be_visible()
        await page.click("#confirm-cancel")
        assert (await settings())["bypass_approvals"] is False
        await page.click("#input")
        await cmd(page, "/permissions bypass")
        await page.click("#confirm-ok")
        await expect(page.locator("#mode-btn")).to_contain_text("Bypassing permissions")
        assert (await settings())["bypass_approvals"] is True
        await page.click("#input")
        await cmd(page, "/permissions bypass")
        await toast(page, "already bypassed")
        await cmd(page, "/permissions Ask")
        await expect(page.locator("#mode-btn")).to_contain_text("Asks before acting")
        assert (await settings())["bypass_approvals"] is False
    finally:
        await api("PUT", "/api/settings", {"bypass_approvals": before})

    section("the rest run from the box too: /agents edits this chat's agent; /memory, /routines, /config open; /files and /pin toggle")
    await page.click("#input")
    for text, dialog, title in (("/agents", "#agent-dialog", f"Edit {AGENT}"), ("/memory", "#memory-dialog", None),
                                ("/routines", "#routines-dialog", None), ("/config", "#settings-dialog", None)):
        await cmd(page, text)
        await expect(page.locator(dialog)).to_be_visible()
        if title:
            await expect(page.locator("#agent-dialog-title")).to_have_text(title)
        await page.keyboard.press("Escape")
        await expect(page.locator(dialog)).to_be_hidden()
        await page.click("#input")
    drawer = page.locator("#files")
    for _ in range(2):
        was = await drawer.is_visible()
        await cmd(page, "/files")
        await (expect(drawer).to_be_hidden() if was else expect(drawer).to_be_visible())
    await cmd(page, "/pin")
    await toast(page, "Pinned to the top")
    assert (await api("GET", f"/api/chats/{cid}"))["pinned"] is True
    await cmd(page, "/PIN")
    await toast(page, "Unpinned")
    assert (await api("GET", f"/api/chats/{cid}"))["pinned"] is False

    section("/new and /clear find an agent by exact name in any case or by the start of it; an unknown one is explained")
    await page.click("#input")
    await cmd(page, f"/new {AGENT.upper()}")
    await page.wait_for_function("cid => __S.chat && __S.chat.id !== cid && __S.chat.messages.length === 0", arg=cid)
    assert await page.evaluate("__S.chat.agent_id") == AGENT_ID
    first = await page.evaluate("__S.chatId")
    await page.click("#input")
    await cmd(page, f"/clear {AGENT.lower()[:-1]}")
    await page.wait_for_function("cid => __S.chat && __S.chat.id !== cid", arg=first)
    assert await page.evaluate("__S.chat.agent_id") == AGENT_ID
    await page.click("#input")
    await kept(page, f"/new nobody-{U}", f"No agent called “nobody-{U}”")
    await page.fill("#input", "")


# ---------------------------------------------------------------------------------------------- mid-run

async def mid_run(page):
    section("mid-run (an approval holds the run): every idle-only command is refused with its exact text kept")
    cid = await new_chat(page, f"Midrun {U}")
    await send(page, "hello before the run")
    reply = (await chat_now(page))["messages"][-1]["content"]
    await page.fill("#input", f"shell: echo midrun {U}")
    await page.keyboard.press("Enter")
    await expect(page.locator(".approval-box")).to_be_visible(timeout=15000)
    assert await page.evaluate("__S.running") is True
    await expect(page.locator("#send-btn")).to_have_text(re.compile("Queue"))
    geo = await page.evaluate("""() => ({ foot: document.querySelector('#chat-foot').getBoundingClientRect().top,
        live: [...document.querySelectorAll('#live .turn')].at(-1).getBoundingClientRect().bottom })""")
    assert geo["foot"] >= geo["live"] - 1 and await page.locator("#chat-foot").is_visible(), geo
    before = await chat_now(page)
    texts = ["/retry", "/regenerate", "/RETRY", "/edit", "/compact keep the API notes", "/compact keep\nthis too",
             "/delete", "/rename Later title", "/rename", "/pin", "/Pin   "]
    for text in texts:
        await page.fill("#input", text)
        await page.keyboard.press("Enter")
        name = next(s[0] for s in SPEC if text.split()[0][1:].lower() in (s[0], *s[1]))
        await toast(page, f"then /{name}.")
        soft(await page.input_value("#input") == text, f"mid-run {text!r} refused but the box now holds {await page.input_value('#input')!r}")
    for typed, keys, kept in (("/ret", ["Enter"], "/retry"), ("/comp keep x", ["Home", "Enter"], "/compact keep x"),
                              ("/del", ["Tab", "Enter"], "/delete"), ("/ren New name", ["Home", "Enter"], "/rename New name")):
        await page.fill("#input", typed)
        for k in keys:
            await page.keyboard.press(k)
        await toast(page, "The agent is working")
        await expect(page.locator("#input")).to_have_value(kept)
        await menu_closed(page)
    await page.fill("#input", "/reg")
    await page.locator("#slash-list [role=option]", has_text="/retry").click()  # a click on a refused row: spelled out too
    await toast(page, "then /retry.")
    await expect(page.locator("#input")).to_have_value("/retry")
    after = await chat_now(page)
    assert (after["title"], after.get("pinned"), after.get("compactions") or []) == (before["title"], before.get("pinned"), before.get("compactions") or []), after
    await expect(page.locator("#history .edit-box")).to_have_count(0)
    await expect(page.locator("#confirm-dialog")).to_be_hidden()
    await expect(page.locator("#toasts .toast.err")).to_have_count(0)
    await expect(page.locator(".approval-box")).to_be_visible()

    section("mid-run: other commands run at once; queued messages stay queued; //text and paths queue as messages; ↑ takes them back")
    await page.fill("#input", f"queued plain {U}")
    await page.keyboard.press("Enter")
    await expect(page.locator("#queue .queue-item")).to_have_count(1)
    await page.fill("#input", f"//slash queued {U}")
    await page.keyboard.press("Enter")
    await page.fill("#input", f"/etc/hosts queued {U}")
    await page.keyboard.press("Enter")
    await expect(page.locator("#queue .queue-item")).to_have_count(3)
    assert await page.locator("#queue .q-text").all_inner_texts() == [f"queued plain {U}", f"/slash queued {U}", f"/etc/hosts queued {U}"]
    for text, check in (("/usage", "#cmd-out .cmd-name"), ("/context", "#cmd-out .cmd-name"), ("/cost", "#cmd-out .cmd-name"),
                        ("/help", "#cmd-out .cmd-name"), ("/status", "#cmd-out .cmd-name")):
        await cmd(page, text)
        await expect(page.locator(check)).to_have_text("/" + {"/cost": "usage"}.get(text, text[1:]))
    await cmd(page, "/copy")
    await toast(page, "Copied")
    assert await page.evaluate("navigator.clipboard.readText()") == reply
    await cmd(page, "/verbose")
    await expect(page.locator("body")).to_have_class(re.compile(r"\bverbose\b"))
    await cmd(page, "/verbose")
    await cmd(page, f"/remember mid-run fact {U}")
    await toast(page, f"mid-run fact {U}")
    async with page.expect_download() as dl:
        await cmd(page, "/export")
    assert (await dl.value).suggested_filename.endswith(".md")
    await expect(page.locator("#queue .queue-item")).to_have_count(3)
    users = [m["content"] for m in (await chat_now(page))["messages"] if m["role"] == "user"]
    assert not any(u.startswith("/") for u in users), users
    await page.fill("#input", "")
    await page.keyboard.press("ArrowUp")
    await expect(page.locator("#queue")).to_be_hidden()
    await expect(page.locator("#input")).to_have_value(f"queued plain {U}\n\n/slash queued {U}\n\n/etc/hosts queued {U}")
    await page.fill("#input", "")

    section("mid-run: /branch makes a copy up to the running turn and the original keeps running; /stop stops it")
    n = await page.evaluate("__S.chat.messages.length")
    await cmd(page, "/fork")
    await page.wait_for_function("cid => __S.chat && __S.chat.id !== cid", arg=cid, timeout=10000)
    branch = await chat_now(page)
    assert branch["title"].endswith("(branch)") and len(branch["messages"]) == n, (branch["title"], len(branch["messages"]), n)
    await open_chat(page, cid)
    await expect(page.locator(".approval-box")).to_be_visible(timeout=10000)
    await page.click("#input")
    await cmd(page, "/stop")
    await idle(page, 15000)
    await expect(page.locator("#history .tool-card").last).to_contain_text("cancelled")  # stopped while it waited for approval


# ------------------------------------------------------------------------------ text the app puts in the box

async def app_text(page):
    section("text the app puts in the box (queued messages handed back) closes the stale menu; Enter never wipes it with a command")
    cid = await new_chat(page, f"Handback {U}")
    await page.fill("#input", f"shell: echo handback {U}")
    await page.keyboard.press("Enter")
    await expect(page.locator(".approval-box")).to_be_visible(timeout=15000)
    await page.fill("#input", f"handed back {U}")
    await page.keyboard.press("Enter")
    await expect(page.locator("#queue .queue-item")).to_have_count(1)
    await page.keyboard.type("/he")
    await expect(page.locator("#slash-menu")).to_be_visible()
    await api("POST", f"/api/chats/{cid}/stop")  # the run ends before the agent reads it: the server hands it back
    await toast(page, "back in the message box", timeout=10000)
    await idle(page)
    box = f"handed back {U}\n\n/he"
    await expect(page.locator("#input")).to_have_value(box)
    soft(not await page.locator("#slash-menu").is_visible(), "the command menu stayed open (still listing /help) after the app put "
                                                              "the handed-back message in front of the typed '/he'")
    await page.keyboard.press("Enter")
    await page.wait_for_function("() => !__S.running", timeout=20000)
    users = [m["content"] for m in (await api("GET", f"/api/chats/{cid}"))["messages"] if m["role"] == "user"]
    kept = await page.input_value("#input")
    soft(f"handed back {U}" in kept or any(f"handed back {U}" in u for u in users),
         f"Enter ran the stale menu's /help and emptied the box: the handed-back message {f'handed back {U}'!r} is gone "
         f"(box {kept!r}, sent {users[-1:]!r})")
    await idle(page)
    await page.fill("#input", "")

    section("switching chats with the menu open (focus still in the box) closes it; Enter in the new chat's empty box runs nothing")
    other = await api("POST", "/api/chats", {"agent_id": AGENT_ID})
    await page.click("#input")
    await page.keyboard.type("/sta")
    await expect(page.locator("#slash-menu")).to_be_visible()
    await page.evaluate("id => location.hash = '#/chat/' + id", other["id"])  # the address changing (a link, Back) leaves the focus in the box
    await page.wait_for_function("id => __S.chat && __S.chat.id === id", arg=other["id"])
    await expect(page.locator("#input")).to_have_value("")
    soft(not await page.locator("#slash-menu").is_visible(), "after switching chats the command menu still showed the previous chat's '/sta' rows over an empty box")
    await page.keyboard.press("Enter")
    soft(await page.locator("#cmd-out .cmd-card").count() == 0, "Enter in the new chat's empty message box ran /status from the previous chat's menu")
    await open_chat(page, cid)
    await expect(page.locator("#input")).to_have_value("/sta")  # its draft came back with it
    await page.keyboard.press("Escape")
    await page.fill("#input", "")

    section("a message sent with // comes back to the box as // (↑, the queue's pencil, a run that ends first), so Enter sends it again")
    cid = await new_chat(page, f"Slashback {U}")
    await page.fill("#input", f"shell: echo slashback {U}")
    await page.keyboard.press("Enter")
    await expect(page.locator(".approval-box")).to_be_visible(timeout=15000)
    sent = f"//help me fix the login page {U}"  # queued as "/help me fix…", which as typed would be /help
    await page.fill("#input", sent)
    await page.keyboard.press("Enter")
    await expect(page.locator("#queue .q-text")).to_have_text([sent[1:]])
    await page.keyboard.press("ArrowUp")
    await expect(page.locator("#input")).to_have_value(sent)
    await menu_closed(page)
    await page.keyboard.press("Enter")  # queued again, as the same message
    await expect(page.locator("#queue .q-text")).to_have_text([sent[1:]])
    await page.locator("#queue .queue-item .icon-btn[title^='Edit']").click()
    await expect(page.locator("#input")).to_have_value(sent)
    await page.keyboard.press("Enter")
    await expect(page.locator("#queue .q-text")).to_have_text([sent[1:]])
    await page.fill("#input", f"//etc/x and /etc/hosts {U}")  # "//etc/x…" queues "/etc/x…", a path: it comes back as it is
    await page.keyboard.press("Enter")
    await expect(page.locator("#queue .queue-item")).to_have_count(2)
    await page.fill("#input", "")
    await api("POST", f"/api/chats/{cid}/stop")
    await toast(page, "back in the message box", timeout=10000)
    await idle(page)
    await expect(page.locator("#input")).to_have_value(f"{sent}\n\n/etc/x and /etc/hosts {U}")
    await expect(page.locator("#cmd-out .cmd-card")).to_have_count(0)
    await page.keyboard.press("Enter")
    await page.wait_for_function("t => __S.chat.messages.some(m => m.role === 'user' && m.content.startsWith(t))", arg=sent[1:], timeout=20000)
    await idle(page)
    await expect(page.locator("#cmd-out .cmd-card")).to_have_count(0)
    users = [m["content"] for m in (await chat_now(page))["messages"] if m["role"] == "user"]
    assert users[-1] == f"{sent[1:]}\n\n/etc/x and /etc/hosts {U}", users


# --------------------------------------------------------------------------------------------- composing

async def composing(page):
    section("IME: keys pressed while composing don't drive the menu, run a command or send; committing '/' opens it")
    await new_chat(page, f"IME {U}")
    await send(page, "hello before composing")
    cdp = await page.context.new_cdp_session(page)
    await page.click("#input")
    await cmd(page, "/help")
    await expect(page.locator("#cmd-out .cmd-card")).to_be_visible()
    posts = []
    page.on("request", lambda r: posts.append(r.url) if r.method == "POST" and "/run" in r.url else None)
    await cdp.send("Input.imeSetComposition", {"text": "/", "selectionStart": 1, "selectionEnd": 1})
    await expect(page.locator("#input")).to_have_value("/")
    was = await page.evaluate("document.querySelector('#input').getAttribute('aria-activedescendant')")
    await page.keyboard.press("ArrowDown")  # the browser marks it isComposing: the IME's, not the menu's
    assert await page.evaluate("document.querySelector('#input').getAttribute('aria-activedescendant')") == was

    async def composing_key(key):
        return await page.evaluate("""k => { const i = document.querySelector('#input');
            const e = new KeyboardEvent('keydown', { key: k, isComposing: true, bubbles: true, cancelable: true });
            i.dispatchEvent(e); return e.defaultPrevented; }""", key)
    for key in ("Enter", "Tab", "ArrowUp"):
        soft(not await composing_key(key), f"a composing {key} was taken by the app")
    await expect(page.locator("#input")).to_have_value("/")
    await composing_key("Escape")
    soft(await page.locator("#cmd-out .cmd-card").count() == 1, "Esc pressed while composing (the IME's own cancel key) closed the /help card")
    await cdp.send("Input.insertText", {"text": "/"})
    await expect(page.locator("#input")).to_have_value("/")
    await expect(page.locator("#slash-menu")).to_be_visible()
    await page.keyboard.press("ArrowDown")
    await expect(page.locator("#input")).to_have_attribute("aria-activedescendant", "slash-opt-1")  # composed and committed: the menu's again
    await page.keyboard.press("Escape")
    await page.fill("#input", "")
    await cdp.send("Input.imeSetComposition", {"text": "にほん", "selectionStart": 3, "selectionEnd": 3})
    await composing_key("Enter")
    await cdp.send("Input.insertText", {"text": "日本"})
    await expect(page.locator("#input")).to_have_value("日本")
    assert not posts, posts  # nothing was sent while composing
    await page.keyboard.press("Enter")
    await page.wait_for_function("() => !__S.running && __S.chat.messages.some(m => m.role === 'user' && m.content === '日本')", timeout=20000)
    await idle(page)
    await cdp.detach()


# ---------------------------------------------------------------------------------- palette and dialogs

async def dialogs(page):
    section("the '/' key: nothing on the welcome page; with a draft in the box it only focuses it, adding no '/'")
    await page.click("#brand-btn")
    await expect(page.locator("#welcome")).to_be_visible()
    await page.locator("#welcome").click(position={"x": 5, "y": 5})
    await page.keyboard.press("/")
    await menu_closed(page)
    await expect(page.locator("#input")).not_to_be_focused()
    cid = await new_chat(page, f"Dialogs {U}")
    await page.fill("#input", "a draft")
    await page.locator("#messages").click(position={"x": 5, "y": 5})
    await page.keyboard.press("/")
    await expect(page.locator("#input")).to_be_focused()
    await expect(page.locator("#input")).to_have_value("a draft")
    await menu_closed(page)
    await page.fill("#input", "")

    section("the menu and the palette: Ctrl+K closes the menu and keeps the text; '/' in the palette is just a search")
    await page.click("#input")
    await page.keyboard.type("/he")
    await expect(page.locator("#slash-menu")).to_be_visible()
    await page.keyboard.press("Control+k")
    await expect(page.locator("#palette")).to_be_visible()
    await menu_closed(page)
    await expect(page.locator("#input")).to_have_value("/he")
    await page.keyboard.type("/")
    await expect(page.locator("#palette-q")).to_have_value("/")
    await menu_closed(page)
    await page.keyboard.press("Escape")
    await expect(page.locator("#palette")).to_be_hidden()
    await expect(page.locator("#input")).to_have_value("/he")
    await page.click("#input")
    await page.keyboard.press("End")
    await page.keyboard.type("l")
    await expect(page.locator("#slash-menu")).to_be_visible()
    assert (await rows(page))[0][0] == "/help"
    await page.keyboard.press("Escape")
    await page.fill("#input", "")

    section("the menu and dialogs: '/' with the shortcuts open, or typed in a settings field, starts no command")
    await page.locator("#messages").click(position={"x": 5, "y": 5})
    await page.keyboard.press("?")
    await expect(page.locator("#shortcuts-dialog")).to_be_visible()
    await page.keyboard.press("/")
    await menu_closed(page)
    await expect(page.locator("#input")).to_have_value("")
    await page.keyboard.press("Escape")
    await expect(page.locator("#shortcuts-dialog")).to_be_hidden()
    await page.click("#input")
    await cmd(page, "/settings")
    await expect(page.locator("#settings-dialog")).to_be_visible()
    field = page.locator("#settings-dialog input[name=api_key]")
    await field.click()
    await page.keyboard.type("/")
    await menu_closed(page)
    await page.keyboard.press("Escape")
    await expect(page.locator("#settings-dialog")).to_be_hidden()
    assert (await settings())["base_url"] == MOCK, "Esc must not save the field typed into"

    section("the menu and the confirm dialog: /delete then Cancel keeps the chat")
    await page.click("#input")
    await cmd(page, "/delete")
    await expect(page.locator("#confirm-dialog")).to_be_visible()
    await menu_closed(page)
    await page.click("#confirm-cancel")
    await expect(page.locator("#confirm-dialog")).to_be_hidden()
    assert await page.evaluate("__S.chatId") == cid and await api_status("GET", f"/api/chats/{cid}") == 200


async def agents_elsewhere(page):
    section("agents made, renamed and deleted elsewhere (another tab, the terminal client) show at once, in the open chat too")
    a = await api("POST", "/api/agents", {"name": f"Elsewhere {U}", "emoji": "🛰", "purpose": "", "tools": [], "memory": False})
    row = page.locator(f"#app .agent-row[data-agent='{a['id']}']")
    await expect(row.locator(".name")).to_have_text(f"Elsewhere {U}", timeout=5000)
    chat = await api("POST", "/api/chats", {"agent_id": a["id"]})
    await open_chat(page, chat["id"])
    await expect(page.locator("#hdr-sub")).to_have_text(f"Elsewhere {U}")
    await api("PUT", f"/api/agents/{a['id']}", {**{k: v for k, v in a.items() if k != "id"}, "name": f"Renamed {U}"})
    await expect(page.locator("#hdr-sub")).to_have_text(f"Renamed {U}", timeout=5000)
    await expect(row.locator(".name")).to_have_text(f"Renamed {U}")
    await api("DELETE", f"/api/agents/{a['id']}")
    await expect(page.locator("#hdr-sub")).to_have_text("Deleted agent", timeout=5000)
    await expect(row).to_have_count(0)


async def before_load(page):
    section("before the chat has loaded: only commands that need no chat are listed; a chat-only one says so and keeps its text")
    chat = await api("POST", "/api/chats", {"agent_id": AGENT_ID})
    pattern = re.compile(rf".*/api/chats/{chat['id']}/stream$")
    held = []
    await page.route(pattern, lambda route: held.append(route))
    try:
        await page.evaluate("id => location.hash = '#/chat/' + id", chat["id"])
        await page.wait_for_function("id => __S.chatId === id && !__S.chat", arg=chat["id"])
        await expect(page.locator("#composer")).to_be_visible()
        await page.click("#input")
        await page.keyboard.type("/")
        got = [r[0] for r in await rows(page)]
        assert got == ["/" + s[0] for s in SPEC if not s[3]], got
        await page.keyboard.press("Escape")
        await page.fill("#input", "")
        for text in ("/rename later", "/compact keep it", "/usage"):
            await page.fill("#input", text)
            await page.keyboard.press("Enter")
            await toast(page, "Open a chat first")
            await expect(page.locator("#input")).to_have_value(text)
        await page.fill("#input", "")
        await page.keyboard.type("/")  # the menu is open when the chat arrives
    finally:
        for r in held:
            await r.continue_()
        await page.unroute(pattern)
    await page.wait_for_function("id => __S.chat && __S.chat.id === id", arg=chat["id"], timeout=15000)
    got = await rows(page)
    soft(got is not None and len(got) == 26, f"the menu opened before the chat loaded kept listing only the {len(got or [])} commands that need no chat "
                                              "after it loaded (/compact, /usage… appear only after another keystroke)")
    await page.keyboard.press("Escape")
    await page.fill("#input", "")


# ----------------------------------------------------------------------------------------------- footer

async def regenerate_turn(page, i):
    turn = page.locator("#history .turn").nth(i)
    await turn.hover()
    await turn.locator(".icon-btn[title='Regenerate']").click()
    if await page.locator("#history .turn").count() > i + 1:
        await expect(page.locator("#confirm-dialog")).to_be_visible()
        await page.click("#confirm-ok")


async def footer(page):
    foot = page.locator("#chat-foot")
    section("footer: two compactions say 2×; it sits after the turns and before a command's card; its title says when it started")
    a = await new_chat(page, f"Footer A {U}")
    await expect(foot).to_be_hidden()
    await send(page, "footer one")
    await expect(foot).to_have_text(re.compile(rf"^not compacted yet · {GOING}$"))
    await send(page, "footer two")
    await cmd(page, "/compact")
    await page.wait_for_function("() => !__S.running && (__S.chat.compactions || []).length === 1", timeout=20000)
    await expect(foot).to_have_text(re.compile(rf"^compacted 1× · {GOING}$"))
    await send(page, "footer three")
    await cmd(page, "/compact keep the footer")
    await page.wait_for_function("() => !__S.running && (__S.chat.compactions || []).length === 2", timeout=20000)
    await expect(foot).to_have_text(re.compile(rf"^compacted 2× · {GOING}$"))
    chat = await chat_now(page)
    assert [c["upto"] for c in chat["compactions"]] == [2, 4], chat["compactions"]
    since = float(await foot.locator("[data-since]").get_attribute("data-since"))
    assert abs(since - chat["created"]) < 0.01, (since, chat["created"])
    await expect(foot).to_have_attribute("title", re.compile(r"^Started "))
    await cmd(page, "/usage")
    order = await page.evaluate("""() => [...document.querySelector('#messages').children].filter(n => n.offsetParent).map(n => n.id)""")
    assert order[-3:] == ["live", "chat-foot", "cmd-out"] or order[-2:] == ["chat-foot", "cmd-out"], order
    geo = await page.evaluate("""() => ({ f: document.querySelector('#chat-foot').getBoundingClientRect(), c: document.querySelector('#cmd-out').getBoundingClientRect(),
        t: [...document.querySelectorAll('#history .turn')].at(-1).getBoundingClientRect() })""")
    assert geo["t"]["bottom"] <= geo["f"]["top"] + 1 and geo["f"]["bottom"] <= geo["c"]["top"] + 1 and geo["f"]["height"] < 24, geo

    section("footer: /retry keeps the count (the summaries cover what stays); Regenerate on an earlier turn trims it")
    await cmd(page, "/retry")
    await page.wait_for_function("() => !__S.running && __S.chat.messages.length === 6", timeout=20000)
    await expect(foot).to_have_text(re.compile(rf"^compacted 2× · {GOING}$"))
    await regenerate_turn(page, 1)  # from message 3: the summary up to 4 goes
    await page.wait_for_function("() => !__S.running && __S.chat.messages.length === 4", timeout=20000)
    await expect(foot).to_have_text(re.compile(rf"^compacted 1× · {GOING}$"))
    await regenerate_turn(page, 0)
    await page.wait_for_function("() => !__S.running && __S.chat.messages.length === 2", timeout=20000)
    await expect(foot).to_have_text(re.compile(rf"^not compacted yet · {GOING}$"))
    assert (await chat_now(page)).get("compactions") == []

    section("footer: switching chats shows each chat's own count and start; the welcome page has none")
    await cmd(page, "/compact")  # not enough to summarize: one exchange
    await idle(page)
    await send(page, "footer again")
    await cmd(page, "/compact")
    await page.wait_for_function("() => !__S.running && (__S.chat.compactions || []).length === 1", timeout=20000)
    await cmd(page, "/usage")
    b = await new_chat(page, f"Footer B {U}")
    await expect(page.locator("#cmd-out .cmd-card")).to_have_count(0)  # a command's card belongs to the chat it ran in
    await expect(foot).to_be_hidden()
    await send(page, "footer b")
    for cid, want in ((a, "compacted 1×"), (b, "not compacted yet"), (a, "compacted 1×"), (b, "not compacted yet")):
        await page.locator("#chat-list .chat-item", has_text=f"Footer {'A' if cid == a else 'B'} {U}").first.click()
        await page.wait_for_function("id => __S.chat && __S.chat.id === id", arg=cid)
        await expect(foot).to_have_text(re.compile(rf"^{want} · {GOING}$"))
        created = (await api("GET", f"/api/chats/{cid}"))["created"]
        assert abs(float(await foot.locator("[data-since]").get_attribute("data-since")) - created) < 0.01
    await page.click("#brand-btn")
    await expect(page.locator("#welcome")).to_be_visible()
    await expect(foot).to_be_hidden()

    section("footer: a late answer about the previous chat's compactions doesn't land on the chat open now")
    c = await new_chat(page, f"Footer C {U}")
    await send(page, "late one")
    await send(page, "late two")
    release, fetched = asyncio.Event(), []

    async def hold(route):
        resp = await route.fetch()
        fetched.append(len((await resp.json()).get("compactions") or []))
        await release.wait()
        await route.fulfill(response=resp)
    pattern = re.compile(rf".*/api/chats/{c}$")
    await page.route(pattern, hold)
    try:
        await cmd(page, "/compact")
        await page.wait_for_function("() => !__S.running && (__S.chat.compactions || []).length === 1", timeout=20000)
        for _ in range(300):
            if fetched:
                break
            await asyncio.sleep(0.05)
        assert fetched == [1], fetched
        await open_chat(page, b)
        await expect(foot).to_have_text(re.compile(rf"^not compacted yet · {GOING}$"))
        async with page.expect_request_finished(lambda r: pattern.match(r.url)):
            release.set()
        await page.evaluate("() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))")
        await expect(foot).to_have_text(re.compile(rf"^not compacted yet · {GOING}$"))
        assert await page.evaluate("__S.chat.compactions || []") == []
    finally:
        release.set()
        await page.unroute(pattern)

    section("footer: opening a chat whose running reply has just compacted it counts that once (the stream replays the compaction)")
    j = await api("POST", "/api/chats", {"agent_id": AGENT_ID})
    await api("PATCH", f"/api/chats/{j['id']}", {"title": f"Footer J {U}"})
    await api_reply(j["id"], "big reply for joining")
    await api("PUT", "/api/settings", {"compact_at": 1})  # the next step's prompt is over 1% of the window: it compacts first
    try:
        await api("POST", f"/api/chats/{j['id']}/run", {"content": "slow reply please"})
        for _ in range(300):
            if len((await api("GET", f"/api/chats/{j['id']}")).get("compactions") or []) == 1:
                break
            await asyncio.sleep(0.05)
    finally:
        await api("PUT", "/api/settings", {"compact_at": 70})
    assert len((await api("GET", f"/api/chats/{j['id']}"))["compactions"]) == 1
    async with page.expect_response(re.compile(rf".*/api/chats/{j['id']}$")):  # the replayed compact_end asks for the count
        await page.evaluate("id => location.hash = '#/chat/' + id", j["id"])
    await page.wait_for_function("id => __S.chat && __S.chat.id === id && __S.running", arg=j["id"])
    await page.evaluate("() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))")
    await expect(foot).to_have_text(re.compile(rf"^compacted 1× · {GOING}$"))
    dividers = await page.locator("#messages details.compact-divider").count()
    soft(dividers == 1, f"joining the running chat shows {dividers} 'Earlier messages summarized' dividers for its one compaction (the footer says "
                        "compacted 1×): the snapshot's history has it and the replayed compact_end adds it again in the live turn")
    await api_status("POST", f"/api/chats/{j['id']}/stop")  # (it may have just finished by itself)
    await idle(page, 20000)
    await expect(foot).to_have_text(re.compile(rf"^compacted 1× · {GOING}$"))
    await expect(page.locator("#messages details.compact-divider")).to_have_count(1)

    section("footer: a sub-agent's chat shows its own; the chat that started it keeps its own count")
    d = await new_chat(page, f"Footer D {U}")
    await page.fill("#input", f"delegate this {U}")
    await page.keyboard.press("Enter")
    await expect(page.locator(".approval-box")).to_be_visible(timeout=20000)
    await page.click(".approval-box button:has-text('Approve')")
    await page.wait_for_function("() => !__S.running && __S.chat.messages.some(m => m.role === 'tool')", timeout=30000)
    await expect(foot).to_have_text(re.compile(rf"^not compacted yet · {GOING}$"))
    parent = await chat_now(page)
    sub_id = parent["subchats"][0]
    await page.locator("#history details.tool-card:has(.open-sub) > summary").first.click()  # the handoff's card, collapsed
    await page.locator("#history .open-sub").first.click()
    await page.wait_for_function("id => __S.chat && __S.chat.id === id", arg=sub_id)
    sub = await api("GET", f"/api/chats/{sub_id}")
    await expect(page.locator("#composer")).to_be_hidden()
    await expect(foot).to_have_text(re.compile(rf"^not compacted yet · {GOING}$"))
    assert abs(float(await foot.locator("[data-since]").get_attribute("data-since")) - sub["created"]) < 0.01
    await page.locator("#messages").click(position={"x": 5, "y": 5})
    await page.keyboard.press("/")  # no message box in a sub-agent's chat: no command menu either
    await menu_closed(page)
    await page.click("#sub-banner-open")
    await page.wait_for_function("id => __S.chat && __S.chat.id === id", arg=d)
    await expect(foot).to_have_text(re.compile(rf"^not compacted yet · {GOING}$"))


async def footer_clock(browser):
    section("footer on a fake clock: 59s, 1m, 59m, 1h 0m, 1d 0h, in step with the open /usage and /context cards")
    ctx = await browser.new_context(viewport={"width": 1280, "height": 860}, color_scheme="dark")
    page = await ctx.new_page()
    watch(page)
    await page.clock.install()
    chat = await api("POST", "/api/chats", {"agent_id": AGENT_ID})
    await api("PATCH", f"/api/chats/{chat['id']}", {"title": f"Clock {U}"})
    await api_reply(chat["id"], "hello clock")
    await page.goto(f"{B}/#/chat/{chat['id']}")
    await hook(page)
    await page.wait_for_function("id => __S.chat && __S.chat.id === id && !__S.running", arg=chat["id"], timeout=15000)
    created = chat["created"]
    foot = page.locator("#chat-foot")
    await page.clock.pause_at(datetime.datetime.fromtimestamp(created + 58.3))
    await page.clock.run_for(1000)
    await expect(foot).to_have_text("not compacted yet · going for 59s")
    await page.click("#input")
    await cmd(page, "/usage")
    usage = page.locator("#cmd-out .cmd-age [data-since]")
    await expect(usage).to_have_text("59s")
    await page.clock.run_for(1000)
    await expect(foot).to_have_text("not compacted yet · going for 1m")
    await expect(usage).to_have_text("1m")
    await page.clock.fast_forward(3538_000)
    await page.clock.run_for(1000)
    await expect(foot).to_have_text("not compacted yet · going for 59m")
    await page.clock.run_for(1000)
    await expect(foot).to_have_text("not compacted yet · going for 1h 0m")
    await expect(usage).to_have_text("1h 0m")
    await page.click("#input")
    await cmd(page, "/context")
    ctxage = page.locator("#cmd-out .cmd-age")
    await expect(ctxage).to_have_text(re.compile(r"^not compacted yet · started .+ \(going for 1h 0m\)$"))
    await page.clock.fast_forward((86400 - 3601) * 1000)
    await page.clock.run_for(1000)
    await expect(foot).to_have_text("not compacted yet · going for 1d 0h")
    await expect(ctxage).to_have_text(re.compile(r"\(going for 1d 0h\)$"))
    await ctx.close()


# ----------------------------------------------------------------------------------------------- pastes

async def paste(page, text):
    """Paste with Ctrl+V; returns (ms from the key to the box's input event, ms the app's own paste handler took)."""
    await page.evaluate("t => navigator.clipboard.writeText(t)", text)
    await page.evaluate("""() => { window.T = {}; if (window.T_on) return; window.T_on = true; const i = document.querySelector('#input');
        window.addEventListener('paste', () => T.p0 = performance.now(), true);
        i.addEventListener('paste', () => T.p1 = performance.now());  // registered after the app's: runs after it
        i.addEventListener('keydown', () => T.kd = performance.now(), true); i.addEventListener('input', () => T.in = performance.now(), true); }""")
    await page.keyboard.press("Control+V")
    await page.wait_for_function("() => T.in", timeout=120000)
    return await page.evaluate("[T.in - T.kd, T.p1 - T.p0]")


async def keystroke(page):
    await page.evaluate("() => { window.T = {}; }")
    await page.keyboard.press("!")
    await page.wait_for_function("() => T.in", timeout=120000)
    return await page.evaluate("T.in - T.kd")


async def clear(page):
    """Empty the box by setting it, as the app does after sending (keys and Playwright's fill take minutes on megabytes)."""
    await page.evaluate("() => { const i = document.querySelector('#input'); i.value = ''; i.dispatchEvent(new Event('input')); }")


async def layout(page):
    return await page.evaluate("""() => { const i = document.querySelector('#input'), box = document.querySelector('.composer-box').getBoundingClientRect();
        return { h: i.getBoundingClientRect().height, sw: document.documentElement.scrollWidth, w: innerWidth, bottom: box.bottom, vh: innerHeight }; }""")


async def pastes(page):
    section("pastes: control characters, lone CR, CRLF, tabs and emoji reach the server as pasted (CR becomes LF), both paste paths")
    await new_chat(page, f"Paste {U}")
    await page.click("#input")
    small = "start a\x00b\x07c\x1b[31mred\x1b[0m d\x7fe\x0bf\x0cg h\u0085i\tj\rk\r\nl\n\rm 👨‍👩‍👧 🇦🇪 👋🏽 ✍️ 1️⃣ 𝔘 end"
    norm = small.replace("\r\n", "\n").replace("\r", "\n")
    await paste(page, small)
    assert await page.input_value("#input") == norm, repr(await page.input_value("#input"))
    await page.keyboard.press("Enter")
    await page.wait_for_function("t => !__S.running && __S.chat.messages.some(m => m.role === 'user' && m.content === t)", arg=norm, timeout=20000)
    long = "x" * 2500 + "\r\n" + small  # a word over 2,000 characters: the app inserts it itself
    await page.click("#input")
    await paste(page, long)
    want = long.replace("\r\n", "\n").replace("\r", "\n")
    assert await page.input_value("#input") == want
    await page.keyboard.press("Enter")
    await page.wait_for_function("n => !__S.running && __S.chat.messages.some(m => m.role === 'user' && m.content.length === n)", arg=js_len(want), timeout=20000)
    users = [m["content"] for m in (await chat_now(page))["messages"] if m["role"] == "user"]
    assert users == [norm, want], [u[:40] for u in users]
    async with page.expect_download() as dl:
        await cmd(page, "/export json")
    exported = json.loads(open(await (await dl.value).path(), encoding="utf-8").read())
    assert [m["content"] for m in exported["chat"]["messages"] if m["role"] == "user"] == [norm, want]

    section("pastes over a selection replace it, caret after the pasted text, on both paths; '/'+a long word opens no menu")
    for text in ("small\r\npiece", "y" * 2100 + "\rz"):
        await page.fill("#input", "head SEL foot")
        await page.evaluate("() => document.querySelector('#input').setSelectionRange(5, 8)")
        await paste(page, text)
        t = text.replace("\r\n", "\n").replace("\r", "\n")
        assert await page.input_value("#input") == f"head {t} foot"
        assert await page.evaluate("document.querySelector('#input').selectionStart") == 5 + len(t)
    await page.fill("#input", "")
    await paste(page, "/" + "q" * 3000)
    await menu_closed(page)
    await page.fill("#input", "")
    await paste(page, "/hel")
    await expect(page.locator("#slash-menu")).to_be_visible()  # a pasted command word is as good as a typed one
    await page.keyboard.press("Escape")
    await page.fill("#input", "")

    section("pastes: in a chat with a reply, selecting all of a 230 KB paste and deleting it doesn't freeze the page")
    text = "\n".join(f"line {i}\tsome words here and there {i * 7} end" for i in range(5000))
    await paste(page, text)
    await page.keyboard.press("End")
    took = {}
    for key in ("Control+A", "Backspace"):
        t0 = time.monotonic()
        await page.keyboard.press(key)
        took[key] = (time.monotonic() - t0) * 1000
    await expect(page.locator("#input")).to_have_value("")
    print(f"  select all {took['Control+A']:.0f} ms, delete {took['Backspace']:.0f} ms")
    soft(max(took.values()) < 1500, f"select-all then delete of a {js_len(text) // 1000} KB paste took {took['Control+A']:.0f} ms + {took['Backspace']:.0f} ms "
                                    "in a chat with a reply (Chromium makes textarea selection quadratic while any closed <details> is on the page; "
                                    "every reply has one, its 'Thought for' block)")

    section("pastes: 4 MB of CRLF lines keep the box at its cap, the page from scrolling sideways, and typing usable")
    await new_chat(page, f"Paste big {U}")  # no reply yet: nothing on the page slows the box's own editing
    await page.click("#input")
    lines = "\r\n".join(f"line {i}\tsome words here and there {i * 7} end" for i in range(90000))
    took, handler = await paste(page, lines)
    assert await page.evaluate("document.querySelector('#input').value.length") == len(lines) - 89999
    key = await keystroke(page)
    lay = await layout(page)
    print(f"  {len(lines):,} chars: paste {took:.0f} ms (handler {handler:.0f} ms), next key {key:.0f} ms, box {lay['h']:.0f}px")
    assert lay["h"] <= 262 and lay["sw"] <= lay["w"] and lay["bottom"] <= lay["vh"], lay
    soft(handler < 1000, f"the paste handler took {handler:.0f} ms on 4 MB of short words")
    await menu_closed(page)
    await clear(page)

    section("pastes: the long-word check itself must not freeze the page (1 MB of 1,500-char lines, 2 MB of 500-char words)")
    for name, text in (("1 MB of 1,500-char lines", "\n".join("q" * 1500 for _ in range(700))),
                       ("2 MB of 500-char words", ("z" * 500 + " ") * 4000)):
        took, handler = await paste(page, text)
        assert await page.evaluate("document.querySelector('#input').value.length") == len(text)
        print(f"  {name}: paste {took:.0f} ms, the app's paste handler {handler:.0f} ms")
        soft(handler < 300, f"pasting {name} blocked the page for {handler:.0f} ms inside the app's paste handler (its /\\S{{2000}}/ "
                            f"test restarts at every position and rescans up to 2,000 characters: ~L² steps for each word of L < 2,000)")
        await clear(page)

    section("pastes: 1.5 MB sent in one message arrives whole, CR normalised, tabs and emoji kept")
    big = "\r\n".join(f"row {i}\tcells 😀 and\rmore {i}" for i in range(40000))
    await paste(page, big)
    want = big.replace("\r\n", "\n").replace("\r", "\n")
    await page.keyboard.press("Enter")
    await page.wait_for_function("n => !__S.running && __S.chat.messages.some(m => m.role === 'user' && m.content.length === n)", arg=js_len(want), timeout=60000)
    sent = [m["content"] for m in (await chat_now(page))["messages"] if m["role"] == "user"][-1]
    assert sent == want, (len(sent), len(want))
    lay = await layout(page)
    assert lay["sw"] <= lay["w"], lay
    assert await page.input_value("#input") == ""


# ------------------------------------------------------------------------------------- Enter and Send

async def enter_as_before(page):
    section("Enter and Send as before: Shift+Enter is a line, blank text sends nothing, the Send button runs commands too")
    await new_chat(page, f"Enter {U}")
    posts = []
    page.on("request", lambda r: posts.append(r.url) if r.method == "POST" and r.url.endswith("/run") else None)
    await page.click("#input")
    await page.keyboard.type("two")
    await page.keyboard.press("Shift+Enter")
    await page.keyboard.type("lines")
    await expect(page.locator("#input")).to_have_value("two\nlines")
    await page.fill("#input", "   \n\t  ")
    await page.keyboard.press("Enter")
    await expect(page.locator("#input")).to_have_value("   \n\t  ")
    await page.fill("#input", "/help")
    await page.keyboard.press("Escape")
    await page.click("#send-btn")
    await expect(page.locator("#cmd-out .cmd-name")).to_have_text("/help")
    await expect(page.locator("#input")).to_have_value("")
    assert not posts, posts
    await page.fill("#input", "hello / not a command")
    await page.keyboard.press("Enter")
    await page.wait_for_function("() => !__S.running && __S.chat.messages.length >= 2", timeout=20000)
    assert [m["content"] for m in (await chat_now(page))["messages"] if m["role"] == "user"] == ["hello / not a command"]
    assert len(posts) == 1


# ----------------------------------------------------------------------------------------------- mobile

async def mobile(browser):
    section("390px with touch: the menu's rows fit, a tap completes or runs, a tap outside closes it; cards and footer fit")
    ctx = await browser.new_context(viewport={"width": 390, "height": 844}, color_scheme="dark", is_mobile=True, has_touch=True,
                                    permissions=["clipboard-read", "clipboard-write"])
    page = await ctx.new_page()
    watch(page)
    chat = await api("POST", "/api/chats", {"agent_id": AGENT_ID})
    await api("PATCH", f"/api/chats/{chat['id']}", {"title": f"Mobile {U}"})
    await page.goto(f"{B}/#/chat/{chat['id']}")
    await hook(page)
    await page.wait_for_function("id => __S.chat && __S.chat.id === id", arg=chat["id"])
    await page.tap("#input")
    await send(page, "hello from a phone")
    await expect(page.locator("#chat-foot")).to_have_text(re.compile(rf"^not compacted yet · {GOING}$"))
    h = await page.evaluate("document.querySelector('#chat-foot').getBoundingClientRect().height")
    assert h < 24, h
    await page.tap("#input")
    await page.keyboard.type("/")
    await expect(page.locator("#slash-list [role=option]")).to_have_count(26)
    geo = await page.evaluate("""() => { const l = document.querySelector('#slash-list'), m = document.querySelector('#slash-menu').getBoundingClientRect();
        return { rows: [...l.children].map(r => r.getBoundingClientRect().right), lw: l.scrollWidth - l.clientWidth, top: m.top, right: m.right,
                 sw: document.documentElement.scrollWidth }; }""")
    assert max(geo["rows"]) <= 390 and geo["lw"] <= 0 and geo["top"] >= 0 and geo["right"] <= 390 and geo["sw"] <= 390, geo
    for i in range(1, 27):
        await page.keyboard.press("ArrowDown")
        s = await selected(page)
        assert s["i"] == i % 26 and s["seen"], (i, s)
    box = await page.locator("#slash-list").bounding_box()  # a swipe up the list scrolls it: no row runs, the box keeps the focus
    cdp = await ctx.new_cdp_session(page)
    x, y0, y1 = box["x"] + 100, box["y"] + box["height"] - 30, box["y"] + 30
    await cdp.send("Input.dispatchTouchEvent", {"type": "touchStart", "touchPoints": [{"x": x, "y": y0}]})
    for k in range(1, 11):
        await cdp.send("Input.dispatchTouchEvent", {"type": "touchMove", "touchPoints": [{"x": x, "y": y0 + (y1 - y0) * k / 10}]})
    await cdp.send("Input.dispatchTouchEvent", {"type": "touchEnd", "touchPoints": []})
    await page.wait_for_function("() => document.querySelector('#slash-list').scrollTop > 100")
    await expect(page.locator("#slash-menu")).to_be_visible()
    await expect(page.locator("#input")).to_be_focused()
    await expect(page.locator("#input")).to_have_value("/")
    await expect(page.locator("#cmd-out .cmd-card")).to_have_count(0)
    await page.locator("#slash-list [role=option]", has_text="/remember").tap()
    await expect(page.locator("#input")).to_have_value("/remember ")
    await expect(page.locator("#input")).to_be_focused()
    await page.keyboard.type(f"tapped fact {U}")
    await page.keyboard.press("Enter")
    await toast(page, f"tapped fact {U}")
    await page.keyboard.type("/con")
    await expect(page.locator("#slash-menu")).to_be_visible()
    await page.tap("#chat-foot")
    await menu_closed(page)
    await expect(page.locator("#input")).to_have_value("/con")
    await page.tap("#input")
    await page.keyboard.press("End")
    await expect(page.locator("#slash-menu")).to_be_visible()
    await page.locator("#slash-list [role=option]", has_text="/context").tap()
    await expect(page.locator("#cmd-out .cmd-name")).to_have_text("/context")
    await page.tap("#input")
    await cmd(page, "/help")
    w = await page.evaluate("""() => ({ sw: document.documentElement.scrollWidth, card: document.querySelector('#cmd-out .cmd-card').getBoundingClientRect().right,
        m: document.querySelector('#messages').scrollWidth - document.querySelector('#messages').clientWidth })""")
    assert w["sw"] <= 390 and w["card"] <= 390 and w["m"] <= 0, w
    await shot(page, "ui-more-mobile")
    await ctx.close()


# ------------------------------------------------------------------------------------------------- main

def watch(page):
    page.on("pageerror", lambda e: ERRORS.append(f"pageerror: {e}"))
    page.on("console", lambda m: ERRORS.append(f"console.{m.type}: {m.text} {m.location.get('url') or ''}".rstrip()) if m.type == "error" else None)


async def main():
    global AGENT_ID
    keep = await settings()
    await api("PUT", "/api/settings", {"base_url": MOCK, "bypass_approvals": False, "auto_memory": False, "compact_at": 70, "auto_compact": True})
    AGENT_ID = (await api("POST", "/api/agents", {"name": AGENT, "emoji": "🧪", "purpose": "", "tools": ["run_shell", "ask_agent"],
                                                  "confirm_shell": True, "memory": False}))["id"]
    try:
        async with async_playwright() as p:
            browser = await p.chromium.launch(executable_path=BRAVE, headless=True, args=["--no-sandbox", "--disable-dev-shm-usage"])
            ctx = await browser.new_context(viewport={"width": 1440, "height": 900}, color_scheme="dark",
                                            permissions=["clipboard-read", "clipboard-write"])
            page = await ctx.new_page()
            watch(page)
            await page.goto(B)
            await hook(page)
            steps = [("registry", registry), ("menu", menu_filtering), ("menu", menu_keys), ("menu", menu_mouse), ("args", arguments),
                     ("midrun", mid_run), ("midrun", app_text), ("ime", composing), ("dialogs", dialogs), ("dialogs", before_load), ("agents", agents_elsewhere), ("footer", footer),
                     ("enter", enter_as_before), ("paste", pastes)]
            try:
                for key, step in steps:
                    if not ONLY or key in ONLY:
                        await step(page)
            except BaseException:
                if SHOTS:
                    await page.screenshot(path=os.path.join(SHOTS, "ui-more-failure.png"))
                raise
            await ctx.close()
            if not ONLY or "clock" in ONLY:
                await footer_clock(browser)
            if not ONLY or "mobile" in ONLY:
                await mobile(browser)
            await browser.close()
    finally:
        await api("PUT", "/api/settings", {k: keep[k] for k in ("base_url", "bypass_approvals", "auto_memory", "compact_at", "auto_compact")})
    assert not ERRORS, "\n".join(ERRORS)
    if DEFECTS:
        print("\ndefects found:\n  " + "\n  ".join(DEFECTS), file=sys.stderr)
        sys.exit(1)
    print("\nall UI checks passed")


async def run():
    try:
        await asyncio.wait_for(main(), 900)
    except BaseException:
        if ERRORS:
            print("browser errors so far:\n  " + "\n  ".join(ERRORS), file=sys.stderr)
        raise


AGENT_ID = None
asyncio.run(run())
