"""Drive the Rust TUI in a pseudo-terminal and read its screen (pyte emulates the terminal).

Used by tests/e2e_tui.py; also handy by hand:
  uv run --with pyte python tests/tui_driver.py --bin tui/target/debug/agent-chat-tui --url http://127.0.0.1:8767
"""
import os
import pty
import re
import select
import signal
import struct
import sys
import termios
import time
import fcntl

import pyte


class Tui:
    def __init__(self, binary, url, cols=120, rows=40, extra=(), cwd=None, env=None, argv=None):
        """`cwd`: the folder the TUI runs in (where /export writes); `env`: variables to set for
        it (XDG_CONFIG_HOME, say, for a state.json of its own); `argv`: its whole command line
        after the binary, instead of `--url URL --quiet` plus `extra` (to leave either out)."""
        self.cols, self.rows = cols, rows
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.ByteStream(self.screen)
        binary = os.path.abspath(binary)
        pid, fd = pty.fork()
        if pid == 0:
            if cwd:
                os.chdir(cwd)
            os.environ.update(env or {})
            os.environ["TERM"] = "xterm-256color"
            os.environ["COLUMNS"], os.environ["LINES"] = str(cols), str(rows)
            os.execv(binary, [binary, *(["--url", url, "--quiet", *extra] if argv is None else argv)])
        self.pid, self.fd = pid, fd
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        self.log = b""

    def pump(self, seconds=0.3):
        end = time.time() + seconds
        while True:
            left = end - time.time()
            if left <= 0:
                break
            r, _, _ = select.select([self.fd], [], [], left)
            if not r:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                break
            if not data:
                break
            self.log += data
            self.stream.feed(data)

    def send(self, data, wait=0.3):
        os.write(self.fd, data.encode() if isinstance(data, str) else data)
        self.pump(wait)

    def send_chunked(self, data, size=1024, gap=0.005, wait=0.3):
        """Write a large input in pieces, reading the screen in between: the pty holds only ~4 KB
        each way, so one big write deadlocks against the TUI's own output."""
        data = data.encode() if isinstance(data, str) else data
        for i in range(0, len(data), size):
            os.write(self.fd, data[i:i + size])
            self.pump(gap)
        self.pump(wait)

    def type(self, text, gap=0.06, wait=0.3):
        """Type like a person: one key per write, further apart than a keystroke paste's keys,
        so each one acts as a key (a single write of several keys is taken as a paste)."""
        for ch in text:
            self.send(ch, gap)
        self.pump(wait)

    def click(self, col, row, wait=0.5):
        """A left click at a 0-based cell (SGR mouse reporting, which the app turns on)."""
        self.send(f"\x1b[<0;{col + 1};{row + 1}M\x1b[<0;{col + 1};{row + 1}m", wait)

    def paste(self, text, wait=0.5):
        """A bracketed paste, the way a terminal delivers it when the app asked for that mode."""
        self.send_chunked("\x1b[200~" + text + "\x1b[201~", wait=wait)

    def resize(self, rows, cols, wait=0.5):
        self.rows, self.cols = rows, cols
        self.screen.resize(rows, cols)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        try:
            os.kill(self.pid, signal.SIGWINCH)
        except ProcessLookupError:
            pass
        self.pump(wait)

    def repaint_diff(self, wait=0.5):
        """Rows that change when the TUI is made to repaint everything (a resize away and back
        makes ratatui clear the screen and draw in full). Anything listed means the incremental
        output had drifted from the frame the app meant to show, i.e. the screen was corrupted.
        It grows by a row rather than shrinking, so nothing scrolls to keep a selection in view.
        Clock times are masked, since a minute can tick over in between; toasts expiring or
        server events landing during the check would show up too, so check a settled screen."""
        def mask(lines):
            return [re.sub(r"\d\d:\d\d", "##:##", l) for l in lines]

        self.settle()
        before = mask(self.lines())
        rows, cols = self.rows, self.cols
        self.resize(rows + 1, cols, wait)
        self.resize(rows, cols, wait)
        self.settle()
        after = mask(self.lines())
        return [(y, b, a) for y, (b, a) in enumerate(zip(before, after)) if b != a]

    def key(self, name, wait=0.3):
        keys = {
            "enter": "\r", "esc": "\x1b", "tab": "\t", "backtab": "\x1b[Z", "up": "\x1b[A", "down": "\x1b[B", "left": "\x1b[D", "right": "\x1b[C",
            "pgup": "\x1b[5~", "pgdn": "\x1b[6~", "home": "\x1b[H", "end": "\x1b[F", "f1": "\x1bOP", "f2": "\x1bOQ", "backspace": "\x7f", "delete": "\x1b[3~",
            "ctrl-k": "\x0b", "ctrl-s": "\x13", "ctrl-u": "\x15", "ctrl-r": "\x12", "ctrl-e": "\x05", "ctrl-g": "\x07", "ctrl-l": "\x0c", "ctrl-y": "\x19",
            "ctrl-q": "\x11", "ctrl-n": "\x0e", "ctrl-c": "\x03", "ctrl-j": "\n", "alt-enter": "\x1b\r",
        }
        if name.startswith("alt-") and name not in keys:
            data = "\x1b" + name[4:]
        elif name in keys:
            data = keys[name]
        else:
            data = name  # literal text
        self.send(data, wait)

    def display(self):
        """Like screen.display, but tolerant of empty cells (pyte trips over zero-width chars)."""
        out = []
        for y in range(self.rows):
            row = self.screen.buffer[y]
            out.append("".join((row[x].data or " ") if x in row else " " for x in range(self.cols)))
        return out

    def text(self):
        return "\n".join(self.display())

    def lines(self):
        return [l.rstrip() for l in self.display()]

    def find(self, needle):
        return any(needle in l for l in self.display())

    def settle(self, quiet=0.05, limit=0.5):
        """Read until the TUI has been silent for `quiet` seconds (at most `limit`). A frame
        reaches the pty in pieces, so a screen read the moment a needle shows can be half drawn,
        more so on a loaded machine."""
        end = time.time() + limit
        while time.time() < end:
            r, _, _ = select.select([self.fd], [], [], quiet)
            if not r:
                return
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return
            if not data:
                return
            self.log += data
            self.stream.feed(data)

    def wait_for(self, needle, timeout=10):
        end = time.time() + timeout
        while time.time() < end:
            self.pump(0.2)
            if self.find(needle):
                self.settle()  # the rest of the frame that drew it
                return True
        return False

    def wait_exit(self, timeout=10):
        """The exit status once the TUI has ended (reading its output meanwhile, so a last write
        can't block it), or None if it is still running after `timeout`. Clears `pid` once it has
        ended, the way callers mark a TUI that needs no close()."""
        end = time.time() + timeout
        while time.time() < end:
            done, status = os.waitpid(self.pid, os.WNOHANG)
            if done:
                self.pump(0.1)
                self.pid = None
                return status
            self.pump(0.1)
        return None

    def close(self):
        try:
            os.kill(self.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(self.pid, 0)
        except ChildProcessError:
            pass


if __name__ == "__main__":
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--url", default="http://127.0.0.1:8765")
    a = ap.parse_args()
    t = Tui(a.bin, a.url)
    t.pump(2)
    print(t.text())
    t.close()
