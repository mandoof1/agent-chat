"""Browser agent tools against a real Brave browser, headless.

Drives app.browser.call directly (no model/server needed), on a local HTML page served over
file://, so it exercises the real Playwright+Brave path: navigate, read, links, click, type,
screenshot and the workspace sandbox. Run: PYTHONPATH=. tests/e2e_browser.py
Set BROWSER_EXECUTABLE to override the Brave path (default /usr/bin/brave).
"""
import asyncio
import os
import tempfile
from pathlib import Path

os.environ.setdefault("AGENT_CHAT_WORKSPACE", tempfile.mkdtemp(prefix="ac-browser-ws-"))

from app import browser, store  # noqa: E402

PAGE1 = """<!doctype html><title>Start Page</title>
<h1>Welcome</h1><p>This is the start page for the browser test.</p>
<input aria-label="Search box" placeholder="Type here">
<p><a href="page2.html">Go to page two</a></p>"""
PAGE2 = """<!doctype html><title>Second Page</title>
<h1>Page Two</h1><p>You clicked through to the second page. Marker: SECOND_OK.</p>"""


async def main():
    store.save_settings({
        "browser_headless": True,
        "browser_executable": os.environ.get("BROWSER_EXECUTABLE", "/usr/bin/brave"),
    })
    ws = Path(os.environ["AGENT_CHAT_WORKSPACE"]).resolve()
    tmp = Path(tempfile.mkdtemp(prefix="ac-browser-site-"))
    (tmp / "page1.html").write_text(PAGE1)
    (tmp / "page2.html").write_text(PAGE2)
    url1 = (tmp / "page1.html").as_uri()  # file://... keeps its scheme

    async def call(name, **args):
        out = await browser.call(name, args, ws)
        assert not out.startswith("Error:"), f"{name} -> {out}"
        return out

    try:
        print("\n== browser_navigate")
        out = await call("browser_navigate", url=url1)
        assert "Start page for the browser test".lower() in out.lower(), out
        assert "Title: Start Page" in out, out
        print("  " + out.splitlines()[0])

        print("== browser_read")
        out = await call("browser_read")
        assert "Welcome" in out, out

        print("== browser_links")
        out = await call("browser_links")
        assert "page2.html" in out and "Go to page two" in out, out
        print("  " + out.splitlines()[1])

        print("== browser_type (fill, no submit)")
        out = await call("browser_type", label="Search box", text="hello world")
        assert "Typed into" in out, out

        print("== browser_click (follow link by text)")
        out = await call("browser_click", text="Go to page two")
        assert "SECOND_OK" in out and "Page Two" in out, out
        print("  " + out.splitlines()[0])

        print("== browser_screenshot")
        out = await call("browser_screenshot", path="shot.png")
        shot = ws / "shot.png"
        assert shot.is_file() and shot.stat().st_size > 0, out
        assert shot.read_bytes()[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
        print(f"  {out} ({shot.stat().st_size} bytes)")

        print("== screenshot path stays in the workspace")
        esc = await browser.call("browser_screenshot", {"path": "../escape.png"}, ws)
        assert esc.startswith("Error:") and "outside the workspace" in esc, esc
        print("  blocked: " + esc)

        print("== browser_close")
        out = await call("browser_close")
        assert "Closed" in out, out

        print("== schemas wired")
        assert set(browser.SCHEMAS) == {
            "browser_navigate", "browser_read", "browser_links", "browser_click",
            "browser_type", "browser_screenshot", "browser_close"}, list(browser.SCHEMAS)

        print("\nALL BROWSER TESTS PASSED")
    finally:
        await browser.shutdown()


if __name__ == "__main__":
    asyncio.run(main())
