"""Browser control: drive a real Brave browser (headless or visible) with Playwright.

One browser session is shared across tool calls so an agent can navigate, read, click and
type across several steps on the same page. Brave is Chromium-based, so Playwright drives it
directly via `executable_path` — no bundled-chromium download needed. The executable path and
headless mode come from settings (Settings → Browser), defaulting to /usr/bin/brave, headless.

A module-level lock serialises browser operations: the whole app shares one session, so two
agents (or a delegate chain) acting at once take turns rather than racing the same page.

Pages are written by whoever controls the site: an agent treats page text, link text and form
fields as information, never as instructions, exactly like fetched web pages and emails.
"""

import asyncio
import os

from .store import DEFAULT_WORKSPACE, get_settings

MAX_TEXT = 12_000
NAV_TIMEOUT = 30_000  # ms
ACTION_TIMEOUT = 15_000  # ms


class BrowserError(Exception):
    pass


_lock = asyncio.Lock()
_pw = None          # the Playwright driver
_browser = None     # the launched Brave browser
_context = None     # one browsing context (cookies, storage)
_page = None        # the active page
_mode = None        # ("executable", headless) the current session was launched with


# ------------------------------------------------------------------ session

async def _ensure():
    """Launch Brave on first use (and relaunch if the executable/headless setting changed)."""
    global _pw, _browser, _context, _page, _mode
    settings = get_settings()
    executable = (settings.get("browser_executable") or "/usr/bin/brave").strip()
    headless = settings.get("browser_headless")
    headless = True if headless is None else bool(headless)
    want = (executable, headless)

    if _browser is not None and _browser.is_connected():
        if _mode == want and _page is not None and not _page.is_closed():
            return _page
        await _teardown()  # setting changed — restart cleanly

    if not os.path.exists(executable):
        raise BrowserError(
            f"browser not found at {executable}. Install Brave or set the path in "
            "Settings → Browser executable.")

    try:
        from playwright.async_api import async_playwright
    except ImportError:
        raise BrowserError("Playwright isn't installed. Run `uv sync` (or `pip install playwright`) "
                           "in the agent-chat folder.")

    try:
        _pw = await async_playwright().start()
        _browser = await _pw.chromium.launch(
            executable_path=executable, headless=headless,
            args=["--no-sandbox", "--disable-dev-shm-usage"],
        )
        _context = await _browser.new_context()
        _context.set_default_timeout(ACTION_TIMEOUT)
        _page = await _context.new_page()
    except Exception as e:
        await _teardown()
        raise BrowserError(f"couldn't start Brave ({type(e).__name__}: {e}).")
    _mode = want
    return _page


async def _teardown():
    global _pw, _browser, _context, _page, _mode
    for obj, closer in ((_context, "close"), (_browser, "close"), (_pw, "stop")):
        try:
            if obj is not None:
                await getattr(obj, closer)()
        except Exception:
            pass
    _pw = _browser = _context = _page = _mode = None


async def shutdown():
    """Called on app shutdown so a visible/headless Brave doesn't linger."""
    async with _lock:
        await _teardown()


# ------------------------------------------------------------------ helpers

def _clip(text: str, limit: int = MAX_TEXT) -> str:
    text = (text or "").strip()
    if len(text) <= limit:
        return text
    return f"{text[:limit]}\n\n... [{len(text) - limit} more characters; scroll or read a specific part]"


async def _state(page, prefix: str = "") -> str:
    """A compact snapshot the model can act on: url, title, visible text."""
    try:
        title = await page.title()
    except Exception:
        title = ""
    try:
        text = await page.evaluate("document.body ? document.body.innerText : ''")
    except Exception:
        text = ""
    head = f"{prefix}\n" if prefix else ""
    return f"{head}URL: {page.url}\nTitle: {title}\n\n{_clip(text) or '(no visible text)'}"


# ------------------------------------------------------------------ tools

async def _navigate(url: str, **_) -> str:
    if not url or "://" not in url:
        url = "https://" + (url or "").lstrip("/")
    page = await _ensure()
    resp = await page.goto(url, timeout=NAV_TIMEOUT, wait_until="domcontentloaded")
    status = f" (HTTP {resp.status})" if resp else ""
    return await _state(page, f"Opened {page.url}{status}.")


async def _read(**_) -> str:
    page = await _ensure()
    if page.url in ("about:blank", ""):
        return "No page open yet. Use browser_navigate to open a URL first."
    return await _state(page)


async def _links(**_) -> str:
    page = await _ensure()
    links = await page.evaluate(
        """Array.from(document.querySelectorAll('a[href]')).map(a => ({
              t: (a.innerText || a.getAttribute('aria-label') || '').trim(),
              h: a.href })).filter(x => x.t && x.h).slice(0, 60)""")
    seen, lines = set(), []
    for lnk in links:
        key = (lnk["t"], lnk["h"])
        if key in seen:
            continue
        seen.add(key)
        lines.append(f"- {lnk['t'][:80]} → {lnk['h']}")
    return "Links on this page:\n" + ("\n".join(lines) or "(none found)")


async def _click(text: str = "", selector: str = "", **_) -> str:
    page = await _ensure()
    if selector:
        locator = page.locator(selector).first
    elif text:
        # Prefer a clickable element whose accessible name matches; fall back to any text.
        locator = page.get_by_role("link", name=text).or_(
            page.get_by_role("button", name=text)).or_(
            page.get_by_text(text, exact=False)).first
    else:
        raise BrowserError("give either `text` (visible label) or a CSS `selector` to click.")
    try:
        await locator.click(timeout=ACTION_TIMEOUT)
    except Exception as e:
        raise BrowserError(f"couldn't click {selector or repr(text)} ({type(e).__name__}). "
                           "Use browser_read or browser_links to see what's on the page.")
    await page.wait_for_load_state("domcontentloaded", timeout=ACTION_TIMEOUT)
    return await _state(page, f"Clicked {selector or repr(text)}.")


async def _type(text: str = "", selector: str = "", label: str = "",
                submit: bool = False, **_) -> str:
    page = await _ensure()
    if selector:
        locator = page.locator(selector).first
    elif label:
        locator = page.get_by_label(label).or_(
            page.get_by_placeholder(label)).first
    else:
        raise BrowserError("give a CSS `selector` or a field `label`/placeholder to type into.")
    try:
        await locator.fill(text, timeout=ACTION_TIMEOUT)
        if submit:
            await locator.press("Enter")
            await page.wait_for_load_state("domcontentloaded", timeout=ACTION_TIMEOUT)
    except Exception as e:
        raise BrowserError(f"couldn't type into {selector or repr(label)} ({type(e).__name__}).")
    return await _state(page, f"Typed into {selector or repr(label)}{' and submitted' if submit else ''}.")


async def _screenshot(ws, path: str = "screenshot.png", full_page: bool = False, **_) -> str:
    page = await _ensure()
    from .tools import _resolve, ToolError  # reuse the workspace-sandbox check
    try:
        target = _resolve(ws, path)
    except ToolError as e:
        raise BrowserError(str(e))
    target.parent.mkdir(parents=True, exist_ok=True)
    await page.screenshot(path=str(target), full_page=bool(full_page))
    return f"Saved a screenshot of {page.url} to {target.relative_to(ws)}"


async def _close(**_) -> str:
    await _teardown()
    return "Closed the browser session."


# ------------------------------------------------------------------ schemas + dispatch

def _fn(name, description, properties, required):
    return {"type": "function", "function": {"name": name, "description": description,
            "parameters": {"type": "object", "properties": properties, "required": required}}}


SCHEMAS = {
    "browser_navigate": _fn(
        "browser_navigate", "Open a URL in the Brave browser and return the page's text. "
        "Starts the browser on first use (headless or visible per Settings → Browser).",
        {"url": {"type": "string", "description": "The address to open, e.g. https://example.com"}},
        ["url"]),
    "browser_read": _fn(
        "browser_read", "Re-read the current page's URL, title and visible text (after a click, "
        "a form submit, or JavaScript updated it).", {}, []),
    "browser_links": _fn(
        "browser_links", "List the links on the current page as visible-text → URL, so you can "
        "choose what to click or navigate to next.", {}, []),
    "browser_click": _fn(
        "browser_click", "Click something on the current page, by its visible text (a link or "
        "button label) or a CSS selector, then return the resulting page.",
        {"text": {"type": "string", "description": "Visible label of the link/button to click"},
         "selector": {"type": "string", "description": "CSS selector (used instead of text if given)"}},
        []),
    "browser_type": _fn(
        "browser_type", "Type text into an input or textarea, found by its label/placeholder or a "
        "CSS selector. Set submit=true to press Enter afterwards (e.g. to run a search).",
        {"text": {"type": "string", "description": "The text to type"},
         "label": {"type": "string", "description": "The field's visible label or placeholder"},
         "selector": {"type": "string", "description": "CSS selector (used instead of label if given)"},
         "submit": {"type": "boolean", "description": "Press Enter after typing. Default false."}},
        ["text"]),
    "browser_screenshot": _fn(
        "browser_screenshot", "Save a PNG screenshot of the current page into the workspace.",
        {"path": {"type": "string", "description": "File name in the workspace. Default screenshot.png"},
         "full_page": {"type": "boolean", "description": "Capture the whole scrollable page. Default false."}},
        []),
    "browser_close": _fn(
        "browser_close", "Close the browser session and free its memory. It reopens on the next "
        "browser_navigate.", {}, []),
}


async def call(name: str, args: dict, ws) -> str:
    async with _lock:
        try:
            if name == "browser_navigate":
                return await _navigate(url=str(args.get("url") or ""))
            if name == "browser_read":
                return await _read()
            if name == "browser_links":
                return await _links()
            if name == "browser_click":
                return await _click(text=str(args.get("text") or ""),
                                    selector=str(args.get("selector") or ""))
            if name == "browser_type":
                return await _type(text=str(args.get("text") or ""),
                                   selector=str(args.get("selector") or ""),
                                   label=str(args.get("label") or ""),
                                   submit=bool(args.get("submit")))
            if name == "browser_screenshot":
                return await _screenshot(ws, path=str(args.get("path") or "screenshot.png"),
                                         full_page=bool(args.get("full_page")))
            if name == "browser_close":
                return await _close()
        except BrowserError as e:
            return f"Error: {e}"
        except asyncio.CancelledError:
            raise
        except Exception as e:
            return f"Error: {type(e).__name__}: {e}"
        return f"Error: unknown browser tool '{name}'"
