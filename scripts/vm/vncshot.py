#!/usr/bin/env python3
"""Screenshot of the test VM through QEMU's VNC unix socket: QMP screendump has no surface to read when the
guest scans out through virgl (egl-headless), but egl-headless feeds the VNC server. Usage: vncshot.py <out.png> [socket] (the second head: ~/.cache/noobsbot-vm/vnc1.sock)"""
import os
import socket
import struct
import subprocess
import sys

SOCK = os.path.expanduser("~/.cache/noobsbot-vm/vnc.sock")


def recv(s, n):
    b = b""
    while len(b) < n:
        chunk = s.recv(n - len(b))
        if not chunk:
            raise SystemExit("vnc: closed")
        b += chunk
    return b


def connect(path=SOCK):
    """Handshake (no auth, shared); returns the socket and the framebuffer size."""
    s = socket.socket(socket.AF_UNIX)
    s.connect(path)
    s.sendall(recv(s, 12)[:12])  # echo the server's protocol version (RFB 003.008)
    types = recv(s, recv(s, 1)[0])
    if 1 not in types:
        raise SystemExit(f"vnc: needs auth {list(types)}")
    s.sendall(b"\x01")
    if struct.unpack(">I", recv(s, 4))[0]:
        raise SystemExit("vnc: security failed")
    s.sendall(b"\x01")  # shared
    w, h = struct.unpack(">HH", recv(s, 4))
    recv(s, 16)
    recv(s, struct.unpack(">I", recv(s, 4))[0])
    return s, w, h


def main(out, path=SOCK):
    s, w, h = connect(path)
    # 32 bpp, depth 24, little endian, true colour, BGRX.
    s.sendall(struct.pack(">BxxxBBBBHHHBBBxxx", 0, 32, 24, 0, 1, 255, 255, 255, 16, 8, 0))
    s.sendall(struct.pack(">BxHI", 2, 1, 0))  # raw encoding only
    s.sendall(struct.pack(">BBHHHH", 3, 0, 0, 0, w, h))
    fb = bytearray(w * h * 4)
    got = 0
    while got < w * h:
        kind = recv(s, 1)[0]
        if kind != 0:
            if kind == 2:  # bell
                continue
            if kind == 3:  # cut text
                recv(s, 3)
                recv(s, struct.unpack(">I", recv(s, 4))[0])
                continue
            raise SystemExit(f"vnc: message {kind}")
        recv(s, 1)
        for _ in range(struct.unpack(">H", recv(s, 2))[0]):
            x, y, rw, rh, enc = struct.unpack(">HHHHi", recv(s, 12))
            if enc == -223:  # desktop size
                w, h, fb = rw, rh, bytearray(rw * rh * 4)
                continue
            data = recv(s, rw * rh * 4)
            for row in range(rh):
                o = ((y + row) * w + x) * 4
                fb[o:o + rw * 4] = data[row * rw * 4:(row + 1) * rw * 4]
            got += rw * rh
    s.close()
    rgb = bytearray(w * h * 3)
    rgb[0::3], rgb[1::3], rgb[2::3] = fb[2::4], fb[1::4], fb[0::4]
    subprocess.run(["magick", "-size", f"{w}x{h}", "-depth", "8", "rgb:-", out], input=bytes(rgb), check=True)


if __name__ == "__main__":
    main(*sys.argv[1:3])
