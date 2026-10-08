"""Drive the Rust TUI in a pseudo-terminal and read its screen (pyte emulates the terminal).

Used by tests/e2e_tui.py; also handy by hand:
  uv run --with pyte python tests/tui_driver.py --bin tui/target/debug/agent-chat-tui --url http://127.0.0.1:8767
"""
import os
import pty
import select
import signal
import struct
import sys
import termios
import time
import fcntl

import pyte


class Tui:
    def __init__(self, binary, url, cols=120, rows=40, extra=()):
        self.cols, self.rows = cols, rows
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.ByteStream(self.screen)
        pid, fd = pty.fork()
        if pid == 0:
            os.environ["TERM"] = "xterm-256color"
            os.environ["COLUMNS"], os.environ["LINES"] = str(cols), str(rows)
            os.execv(binary, [binary, "--url", url, "--quiet", *extra])
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

    def key(self, name, wait=0.3):
        keys = {
            "enter": "\r", "esc": "\x1b", "tab": "\t", "backtab": "\x1b[Z", "up": "\x1b[A", "down": "\x1b[B", "left": "\x1b[D", "right": "\x1b[C",
            "pgup": "\x1b[5~", "pgdn": "\x1b[6~", "home": "\x1b[H", "end": "\x1b[F", "f1": "\x1bOP", "f2": "\x1bOQ", "backspace": "\x7f", "delete": "\x1b[3~",
            "ctrl-k": "\x0b", "ctrl-s": "\x13", "ctrl-u": "\x15", "ctrl-r": "\x12", "ctrl-e": "\x05", "ctrl-g": "\x07", "ctrl-l": "\x0c", "ctrl-y": "\x19",
            "ctrl-q": "\x11", "ctrl-n": "\x0e", "ctrl-j": "\n", "alt-enter": "\x1b\r",
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

    def wait_for(self, needle, timeout=10):
        end = time.time() + timeout
        while time.time() < end:
            self.pump(0.2)
            if self.find(needle):
                return True
        return False

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
