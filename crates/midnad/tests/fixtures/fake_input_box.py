#!/usr/bin/env python3
"""A stand-in for Claude Code's input box, for tests/local_triggers.rs.

Draws `sent: <text>` for each submitted message, then the input box: a `❯` row between two
rules. Typed and pasted text goes in the box, Enter (`\\r` or kitty `CSI 13 u`) submits it,
ctrl-c (`\\x03` or kitty `CSI 99;5 u`) clears it. Like Claude, its title spins (`◐`) while it
works on a submitted message, here for a moment, and then shows it stopped (`✳`).
"""
import os
import re
import sys
import threading
try:
    import tty
except ImportError:  # Windows
    tty = None


def setraw():
    """Raw keys: tty on Unix. Windows has no termios: the console gets VT input without line
    input, echo or ctrl-c handling, and VT output, which is how Claude Code reads keys there."""
    if tty:
        tty.setraw(0)
        return
    import ctypes
    k = ctypes.windll.kernel32
    k.SetConsoleMode(k.GetStdHandle(-10), 0x200)
    k.SetConsoleMode(k.GetStdHandle(-11), 7)

out = sys.stdout
lock = threading.Lock()
sent = []
box = ""


def title(t):
    with lock:
        out.write(f"\x1b]0;{t}\x07")
        out.flush()


def draw():
    with lock:
        redraw()


def redraw():
    cols, _ = os.get_terminal_size()
    rows = ["  fake input box", ""] + [f"sent: {s}" for s in sent] + [""]
    buf = "\x1b[H\x1b[2J" + "\r\n".join(r[:cols] for r in rows)
    buf += "\r\n" + "─" * cols + "\r\n❯ " + box.replace("\n", "\r\n  ") + "\r\n" + "─" * cols
    out.write(buf)
    out.flush()


def main():
    global box
    setraw()
    out.write("\x1b[?1049h\x1b[?2004h")
    draw()
    pending = ""
    while True:
        data = os.read(0, 4096)
        if not data:
            return
        pending += data.decode("utf-8", "replace")
        while pending:
            m = re.match(r"\x1b\[200~(.*?)\x1b\[201~|\x1b\[200~|\x1b\[[0-9;]*[A-Za-z~]|.", pending, re.S)
            if not m or (m.group(0) == "\x1b[200~"):
                break
            tok, pending = m.group(0), pending[m.end():]
            if m.group(1) is not None:
                box += m.group(1)
            elif tok in ("\r", "\x1b[13u"):
                sent.append(box.replace("\n", " / "))
                box = ""
                title("◐ fake")
                threading.Timer(0.8, title, ("✳ fake",)).start()
            elif tok in ("\x03", "\x1b[99;5u"):
                box = ""
            elif tok.isprintable():
                box += tok
        draw()


main()
