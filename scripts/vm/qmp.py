#!/usr/bin/env python3
"""Drives the test VM over QMP (never the host's own input).

  qmp.py shot <out.png>          screendump (PPM, converted with magick), or vncshot.py under virgl
  qmp.py key <combo> [...]       HMP sendkey, e.g. meta_l-shift-3, ctrl-alt-f2, ret
  qmp.py type <text>             types ASCII text (sendkey per character)
  qmp.py move <x> <y>            absolute pointer, in pixels of a 1920x1080 screen
  qmp.py click <x> <y> [right]   move + click
  qmp.py rel <dx> <dy> [steps]   relative motion (the PS/2 mouse: crosses onto a second display, the tablet can't)
  qmp.py button <down|up>        left button only, e.g. down, rel …, up to drag
  qmp.py burst <prefix> <n> [keys...]  keys, then n PPM screendumps (no virgl only)
  qmp.py hmp <command>           any HMP command
"""
import json
import os
import socket
import subprocess
import sys
import time

SOCK = os.path.expanduser("~/.cache/noobsbot-vm/qmp.sock")
W, H = 1920, 1080
NAMES = {" ": "spc", "\n": "ret", "-": "minus", "=": "equal", ".": "dot", ",": "comma", "/": "slash",
         ";": "semicolon", "'": "apostrophe", "[": "bracket_left", "]": "bracket_right", "\\": "backslash",
         "`": "grave_accent"}
SHIFTED = {"!": "1", "@": "2", "#": "3", "$": "4", "%": "5", "^": "6", "&": "7", "*": "8", "(": "9", ")": "0",
           "_": "minus", "+": "equal", ":": "semicolon", '"': "apostrophe", "?": "slash", "<": "comma", ">": "dot",
           "{": "bracket_left", "}": "bracket_right", "|": "backslash", "~": "grave_accent"}


class Qmp:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.connect(SOCK)
        self.f = self.s.makefile("rw")
        self.f.readline()
        self.cmd("qmp_capabilities")

    def cmd(self, name, **args):
        self.f.write(json.dumps({"execute": name, "arguments": args}) + "\n")
        self.f.flush()
        while True:
            r = json.loads(self.f.readline())
            if "return" in r:
                return r["return"]
            if "error" in r:
                raise SystemExit(f"{name}: {r['error']}")

    def hmp(self, line):
        return self.cmd("human-monitor-command", **{"command-line": line})

    def abs(self, x, y):
        self.cmd("input-send-event", events=[
            {"type": "abs", "data": {"axis": "x", "value": int(x * 32767 / W)}},
            {"type": "abs", "data": {"axis": "y", "value": int(y * 32767 / H)}}])

    def button(self, btn, down):
        self.cmd("input-send-event", events=[{"type": "btn", "data": {"down": down, "button": btn}}])


def char_key(c):
    if c in SHIFTED:
        return "shift-" + SHIFTED[c]
    if c.isupper():
        return "shift-" + c.lower()
    return NAMES.get(c, c)


def main():
    q, a = Qmp(), sys.argv[1:]
    if a[0] == "shot":
        ppm = a[1] + ".ppm"
        try:
            q.cmd("screendump", filename=ppm)
        except SystemExit:
            # virgl scanout (egl-headless): no console surface, but the VNC server gets the frames.
            subprocess.run([sys.executable, os.path.join(os.path.dirname(__file__), "vncshot.py"), a[1]], check=True)
            return
        subprocess.run(["magick", ppm, a[1]], check=True)
        os.remove(ppm)
    elif a[0] == "key":
        for k in a[1:]:
            q.hmp(f"sendkey {k}")
            time.sleep(0.15)
    elif a[0] == "type":
        for c in a[1]:
            q.hmp(f"sendkey {char_key(c)}")
            time.sleep(0.06)
    elif a[0] == "move":
        q.abs(float(a[1]), float(a[2]))
    elif a[0] == "click":
        q.abs(float(a[1]), float(a[2]))
        time.sleep(0.1)
        btn = "right" if a[3:] == ["right"] else "left"
        q.button(btn, True)
        time.sleep(0.08)
        q.button(btn, False)
    elif a[0] == "rel":
        steps = int(a[3]) if a[3:] else 1
        for _ in range(steps):
            q.cmd("input-send-event", events=[
                {"type": "rel", "data": {"axis": "x", "value": int(float(a[1]) / steps)}},
                {"type": "rel", "data": {"axis": "y", "value": int(float(a[2]) / steps)}}])
            time.sleep(0.02)
    elif a[0] == "button":
        q.button("left", a[1] == "down")
    elif a[0] == "burst":
        # burst <prefix> <n> <keys...>: sends the keys, then n screendumps back to back (catches animations).
        for k in a[3:]:
            q.hmp(f"sendkey {k}")
        for i in range(int(a[2])):
            q.cmd("screendump", filename=f"{a[1]}-{i:02}.ppm")
            time.sleep(0.03)
    elif a[0] == "hmp":
        print(q.hmp(" ".join(a[1:])))


if __name__ == "__main__":
    main()
