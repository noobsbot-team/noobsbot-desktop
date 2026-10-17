#!/usr/bin/env python3
"""Fuzzing client (E1.11 "a broken app can't crash the desktop").

Connects to $WAYLAND_DISPLAY over and over, binds every global, then sends random requests to the objects it knows:
random opcodes, random argument words (numbers, known and fresh object ids, strings) and sometimes a file descriptor.
The compositor must answer with protocol errors and disconnects, never die. Exits 1 if the socket stops accepting.

usage: WAYLAND_DISPLAY=wayland-1 scripts/fuzz-client.py [seconds] [seed]
"""

import os
import random
import socket
import struct
import sys
import time


def message(obj, opcode, payload):
    return struct.pack("<II", obj, (8 + len(payload)) << 16 | opcode) + payload


def string(s):
    data = s.encode() + b"\0"
    return struct.pack("<I", len(data)) + data + b"\0" * (-len(data) % 4)


def read_globals(sock):
    """wl_display.get_registry + sync, then collect wl_registry.global events until the callback fires."""
    sock.sendall(message(1, 1, struct.pack("<I", 2)) + message(1, 0, struct.pack("<I", 3)))
    buf, globals = b"", []
    deadline = time.monotonic() + 2
    while time.monotonic() < deadline:
        chunk = sock.recv(65536)
        if not chunk:
            break
        buf += chunk
        while len(buf) >= 8:
            obj, word = struct.unpack_from("<II", buf)
            size, opcode = word >> 16, word & 0xFFFF
            if len(buf) < size:
                break
            body, buf = buf[8:size], buf[size:]
            if obj == 2 and opcode == 0:
                name, length = struct.unpack_from("<II", body)
                interface = body[8 : 8 + length - 1].decode()
                (version,) = struct.unpack_from("<I", body, 8 + length + (-length % 4))
                globals.append((name, interface, version))
            elif obj == 3:
                return globals
    return globals


def fuzz_connection(path, rng):
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(2)
    sock.connect(path)
    try:
        objects, next_id = [1, 2], 4
        for name, interface, version in read_globals(sock):
            sock.sendall(message(2, 0, struct.pack("<I", name) + string(interface) + struct.pack("<II", rng.randint(1, version), next_id)))
            objects.append(next_id)
            next_id += 1
        sock.setblocking(False)
        for _ in range(rng.randint(1, 200)):
            args = b""
            for _ in range(rng.randint(0, 6)):
                kind = rng.random()
                if kind < 0.3:
                    args += struct.pack("<I", next_id)
                    objects.append(next_id)
                    next_id += 1
                elif kind < 0.5:
                    args += struct.pack("<I", rng.choice(objects))
                elif kind < 0.6:
                    args += string(rng.choice(["", "x", "wl_surface", "é" * 300]))
                elif kind < 0.8:
                    args += struct.pack("<i", rng.randint(-5, 5000))
                else:
                    args += struct.pack("<I", rng.getrandbits(32))
            data = message(rng.choice(objects), rng.randint(0, 12), args)
            if rng.random() < 0.05:
                data = data[: rng.randint(1, len(data))]
            if rng.random() < 0.1:
                fd = os.memfd_create("fuzz")
                os.ftruncate(fd, rng.choice([0, 1, 4096, 1 << 20]))
                socket.send_fds(sock, [data], [fd])
                os.close(fd)
            else:
                sock.send(data)
            try:
                while sock.recv(65536):
                    pass
            except BlockingIOError:
                pass
    except (BrokenPipeError, ConnectionResetError, socket.timeout, OSError):
        pass  # The compositor disconnected us: the expected answer to garbage.
    finally:
        sock.close()


def main():
    seconds = float(sys.argv[1]) if len(sys.argv) > 1 else 30
    seed = int(sys.argv[2]) if len(sys.argv) > 2 else random.randrange(1 << 32)
    path = os.path.join(os.environ["XDG_RUNTIME_DIR"], os.environ["WAYLAND_DISPLAY"])
    rng = random.Random(seed)
    connections, end = 0, time.monotonic() + seconds
    while time.monotonic() < end:
        try:
            fuzz_connection(path, rng)
        except (ConnectionRefusedError, FileNotFoundError):
            print(f"seed {seed}: compositor gone after {connections} connections")
            return 1
        connections += 1
    print(f"seed {seed}: {connections} connections, compositor alive")
    return 0


if __name__ == "__main__":
    sys.exit(main())
