#!/usr/bin/env python3
"""Plugs or unplugs the VM's second display (boot with HEADS=2): asks QEMU's VNC server on that head for a desktop
size, which QEMU hands the guest as the monitor's preferred mode (0 0 = unplugged). Usage: head.py <w> <h> | head.py off"""
import os
import struct
import sys

from vncshot import connect, recv

SOCK = os.path.expanduser("~/.cache/noobsbot-vm/vnc1.sock")


def main(args):
    w, h = (0, 0) if args == ["off"] else map(int, args)
    s, *_ = connect(SOCK)
    s.sendall(struct.pack(">BxHi", 2, 1, -308))  # SetEncodings: ExtendedDesktopSize, so QEMU takes SetDesktopSize
    s.sendall(struct.pack(">BxHHBxIHHHHI", 251, w, h, 1, 0, 0, 0, w, h, 0))
    s.settimeout(2)
    try:
        while recv(s, 1):
            pass
    except (TimeoutError, SystemExit):
        pass
    s.close()


if __name__ == "__main__":
    main(sys.argv[1:])
