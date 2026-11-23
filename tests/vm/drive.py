#!/usr/bin/env python3
"""Runs a command on a pseudo terminal and types passwords when asked.

Stands in for a person at the terminal in the VM tests: the password goes
to the terminal, as it would from a keyboard, never to Parity.

    drive.py [--password PW]... [--interrupt-when SHELL-COMMAND] -- COMMAND...

Each password prompt gets the next password; the last one is repeated.
--interrupt-when types Ctrl-C as soon as SHELL-COMMAND succeeds. It is run
every 0.2 seconds once every given password has been typed, so the test
decides when Ctrl-C comes (for example once collect mode has a file open),
not a delay that depends on how fast the machines are.
Prints everything the command wrote and exits with its exit code (130 if it
was interrupted).
"""

import os
import pty
import select
import signal
import subprocess
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
        elif args[0] == "--interrupt-when":
            interrupt = args[1]
            args = args[2:]
        else:
            sys.exit(f"unknown option {args[0]}")
    cmd = args[1:]
    pid, fd = pty.fork()
    if pid == 0:
        # As at a terminal: default signal handling, even if this runs in the
        # background of a shell (which ignores SIGINT and SIGQUIT there) or
        # Python ignored SIGPIPE. Ignored signals are inherited across exec.
        for s in (signal.SIGINT, signal.SIGQUIT, signal.SIGPIPE):
            signal.signal(s, signal.SIG_DFL)
        os.execvp(cmd[0], cmd)
    start = time.monotonic()
    out = b""
    seen = 0
    asked = 0
    next_poll = 0.0
    while True:
        if interrupt is not None and asked >= len(passwords) and time.monotonic() >= next_poll:
            next_poll = time.monotonic() + 0.2
            if ready(interrupt):
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


def ready(command):
    try:
        r = subprocess.run(
            command,
            shell=True,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=10,
        )
    except subprocess.TimeoutExpired:
        return False
    return r.returncode == 0


main()
