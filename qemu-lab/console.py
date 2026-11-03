#!/usr/bin/env python3
"""Drive the lab VM's QEMU serial unix socket, one connection per invocation."""
import socket, sys, time, os, re

SOCK = "/home/c08r4d0r/worktrees/tc-qemu-buy-verify/qemu-lab/serial.sock"

def run(cmds, wait_s=3.0, timeout=90):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(SOCK)
    time.sleep(1.0)
    out = b""
    try:
        while True:  # drain banner
            chunk = s.recv(65536)
            if not chunk:
                break
            out += chunk
            if out.endswith(b"# ") or out.endswith(b"$ "):
                break
    except socket.timeout:
        pass
    for c in cmds:
        s.sendall(c.encode() + b"\n")
        end = time.time() + wait_s
        got = b""
        while time.time() < end:
            try:
                chunk = s.recv(65536)
                if not chunk:
                    break
                got += chunk
                end = time.time() + 0.4
            except socket.timeout:
                break
        out += got
    s.sendall(b"echo __MARK_$?__\n")
    end = time.time() + 10
    tail = b""
    while time.time() < end:
        try:
            chunk = s.recv(65536)
            if not chunk:
                break
            tail += chunk
            if b"__MARK_" in tail:
                break
        except socket.timeout:
            break
    s.close()
    text = (out + tail).decode("utf-8", "replace")
    text = re.sub(r"\x1b\[[0-9;?]*[a-zA-Z]", "", text).replace("\r", "")
    return text

if __name__ == "__main__":
    wait = float(os.environ.get("WAIT", "3"))
    print(run(sys.argv[1:], wait))
