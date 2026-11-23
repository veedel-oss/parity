#!/usr/bin/env python3
"""Runs a command on a pseudo terminal and types passwords when asked.

Stands in for a person at the terminal in the end-to-end tests: the password
goes to the terminal, as it would from a keyboard, never to Parity.

    drive.py [--password PW]... [--interrupt-after SECONDS] -- COMMAND...

Each password prompt gets the next password; the last one is repeated.
Prints everything the command wrote and exits with its exit code (130 if it
was interrupted).
"""

import os
import pty
import select
import signal
import sys
import time


def main():
    args = sys.argv[1:]
    passwords = []
    interrupt = None
    while args and args[0] != "--":
        if args[0] == "--password":
            passwords.append(args[1].encode() + b"\n")
            args = args[2:]
        elif args[0] == "--interrupt-after":
            interrupt = float(args[1])
            args = args[2:]
        else:
            sys.exit(f"unknown option {args[0]}")
    cmd = args[1:]
    pid, fd = pty.fork()
    if pid == 0:
        os.execvp(cmd[0], cmd)
    start = time.monotonic()
    out = b""
    seen = 0
    asked = 0
    while True:
        if interrupt is not None and time.monotonic() - start > interrupt:
            # Ctrl-C on the terminal: SIGINT to the foreground process group.
            os.write(fd, b"\x03")
            interrupt = None
        try:
            r, _, _ = select.select([fd], [], [], 0.1)
        except InterruptedError:
            continue
        if r:
            try:
                data = os.read(fd, 4096)
            except OSError:
                break
            if not data:
                break
            out += data
            prompt = out[seen:].lower()
            if b"password" in prompt and prompt.rstrip().endswith(b":"):
                pw = passwords[min(asked, len(passwords) - 1)] if passwords else b"\n"
                asked += 1
                seen = len(out)
                time.sleep(0.2)
                os.write(fd, pw)
        elif time.monotonic() - start > 300:
            os.kill(pid, signal.SIGKILL)
            break
    _, status = os.waitpid(pid, 0)
    sys.stdout.write(out.decode(errors="replace").replace("\r\n", "\n"))
    sys.stdout.flush()
    if os.WIFSIGNALED(status):
        sys.exit(128 + os.WTERMSIG(status))
    sys.exit(os.WEXITSTATUS(status))


main()
