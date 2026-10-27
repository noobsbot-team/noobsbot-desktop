#!/usr/bin/env python3
"""Lock screen checks against a nested desktop: a client can't unlock our screen, and can lock and unlock its own.

Starts the desktop nested twice under dbus-run-session with scratch XDG dirs (the real config is never touched):
1. `--show lock` (our own screen up): an ext-session-lock client's lock gets `finished`; its unlock_and_destroy
   changes nothing, so a second and third client's lock gets `finished` too.
2. unlocked: the client's lock gets `locked`; after unlock_and_destroy a second client can lock again, and so on.
3. unlocked, and the first locker dies without unlocking: our screen takes over, so the second client's lock gets
   `finished`, and its unlock_and_destroy changes nothing for the third.

usage: scripts/lock-test.py [path/to/noobsbot-desktop]   (default: target/debug/noobsbot-desktop)
"""

import importlib.util
import os
import re
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("fuzz_client", os.path.join(HERE, "fuzz-client.py"))
fuzz = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fuzz)
message, string, read_globals = fuzz.message, fuzz.string, fuzz.read_globals

MANAGER, LOCK = 4, 5  # object ids after read_globals' registry (2) and callback (3)


def events(sock, seconds):
    """(object, opcode) of every event for `seconds`, or until the server closes the connection."""
    buf, deadline = b"", time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            chunk = sock.recv(65536)
        except socket.timeout:
            continue
        if not chunk:
            return
        buf += chunk
        while len(buf) >= 8:
            obj, word = struct.unpack_from("<II", buf)
            size = word >> 16
            if len(buf) < size:
                break
            buf = buf[size:]
            yield obj, word & 0xFFFF


def try_lock(path):
    """A fresh connection that binds the lock manager and asks for a lock: ("locked"|"finished"|"error"|"timeout", sock)."""
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(0.5)
    sock.connect(path)
    name = next(n for n, i, _ in read_globals(sock) if i == "ext_session_lock_manager_v1")
    sock.sendall(message(2, 0, struct.pack("<I", name) + string("ext_session_lock_manager_v1") + struct.pack("<II", 1, MANAGER)))
    sock.sendall(message(MANAGER, 1, struct.pack("<I", LOCK)))
    for obj, opcode in events(sock, 5):
        if obj == LOCK:
            return ("locked", "finished")[opcode], sock
        if obj == 1 and opcode == 0:
            return "error", sock
    return "timeout", sock


def unlock_and_destroy(sock):
    sock.sendall(message(LOCK, 2, b""))
    for _ in events(sock, 1):  # a protocol error or the close, if any
        pass
    sock.close()


def run_desktop(binary, show, env):
    log = tempfile.NamedTemporaryFile("w", prefix="lock-test-", suffix=".log", delete=False)
    args = ["dbus-run-session", "--", binary, "--nested"] + (["--show", show] if show else [])
    proc = subprocess.Popen(args, stdout=log, stderr=subprocess.STDOUT, env=env, start_new_session=True)
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline and proc.poll() is None:
        with open(log.name) as f:
            found = re.search(r"listening on WAYLAND_DISPLAY=(\S+)", f.read())
        if found:
            return proc, log.name, os.path.join(env["XDG_RUNTIME_DIR"], found.group(1))
        time.sleep(0.2)
    os.killpg(proc.pid, signal.SIGKILL)
    sys.exit(f"the desktop didn't start; log: {log.name}")


def stop(proc):
    os.killpg(proc.pid, signal.SIGTERM)
    try:
        return proc.wait(10)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        return "killed"


def check(binary, show, want, why, die=False):
    env = dict(os.environ, RUST_BACKTRACE="1")
    scratch = tempfile.mkdtemp(prefix="lock-test-xdg-")
    for var in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"):
        env[var] = os.path.join(scratch, var.lower())
    proc, log, path = run_desktop(binary, show, env)
    try:
        time.sleep(1)  # the first frame, so the lock screen (if shown) is really up
        got = []
        for n in range(3):
            result, sock = try_lock(path)
            got.append(result)
            if die and n == 0:
                sock.close()
            else:
                unlock_and_destroy(sock)
            time.sleep(0.5)  # the unlock is deferred a loop turn
    finally:
        exit_code = stop(proc)
    ok = got == want
    print(f"{'ok' if ok else 'FAIL'}: {why}: locks {got} (want {want}); exit {exit_code}; log {log}")
    return ok


def main():
    binary = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "..", "target", "debug", "noobsbot-desktop")
    results = [
        check(binary, "lock", ["finished"] * 3, "our lock screen up: a client's lock is refused and its unlock changes nothing"),
        check(binary, None, ["locked"] * 3, "unlocked: a client locks, unlocks, and another can lock again"),
        check(binary, None, ["locked", "finished", "finished"], "a locker died: ours takes over, no client unlocks it", die=True),
    ]
    sys.exit(0 if all(results) else 1)


if __name__ == "__main__":
    main()
