#!/usr/bin/env python3
"""Drive a baton TUI through a PTY, capture a full screen, and preserve evidence."""

import argparse
import fcntl
import json
import os
import pty
import re
import select
import signal
import struct
import sys
import tempfile
import termios
import time
from pathlib import Path


class Screen:
    def __init__(self, width: int, height: int) -> None:
        self.width = width
        self.height = height
        self.grid = [[" "] * width for _ in range(height)]
        self.row = 0
        self.col = 0

    def clear(self) -> None:
        self.grid = [[" "] * self.width for _ in range(self.height)]
        self.row = 0
        self.col = 0

    def scroll(self) -> None:
        self.grid.pop(0)
        self.grid.append([" "] * self.width)
        self.row = self.height - 1

    def put(self, char: str) -> None:
        if char == "\r":
            self.col = 0
            return
        if char == "\n":
            self.row += 1
            if self.row >= self.height:
                self.scroll()
            return
        if char == "\b":
            self.col = max(0, self.col - 1)
            return
        if char == "\t":
            self.col = min(self.width - 1, (self.col // 8 + 1) * 8)
            return
        if ord(char) < 32 or ord(char) == 127:
            return
        if self.col >= self.width:
            self.col = 0
            self.row += 1
            if self.row >= self.height:
                self.scroll()
        self.grid[self.row][self.col] = char
        self.col += 1

    def csi(self, params: str, final: str) -> None:
        values = [int(part) if part.isdigit() else 0 for part in params.replace("?", "").replace(">", "").split(";") if part != ""]
        first = values[0] if values else 0
        if final in ("H", "f"):
            self.row = max(0, min(self.height - 1, (values[0] if values else 1) - 1))
            self.col = max(0, min(self.width - 1, (values[1] if len(values) > 1 else 1) - 1))
        elif final == "A":
            self.row = max(0, self.row - (first or 1))
        elif final == "B":
            self.row = min(self.height - 1, self.row + (first or 1))
        elif final == "C":
            self.col = min(self.width - 1, self.col + (first or 1))
        elif final == "D":
            self.col = max(0, self.col - (first or 1))
        elif final == "J":
            if first in (2, 3):
                self.clear()
            elif first == 0:
                for x in range(self.col, self.width):
                    self.grid[self.row][x] = " "
                for y in range(self.row + 1, self.height):
                    self.grid[y] = [" "] * self.width
        elif final == "K":
            if first in (0, 1, 2):
                start = 0 if first == 2 else self.col
                for x in range(start, self.width):
                    self.grid[self.row][x] = " "
        elif final == "X":
            for x in range(self.col, min(self.width, self.col + (first or 1))):
                self.grid[self.row][x] = " "
        elif final == "G":
            self.col = max(0, min(self.width - 1, (first or 1) - 1))
        elif final == "d":
            self.row = max(0, min(self.height - 1, (first or 1) - 1))

    def feed(self, text: str) -> None:
        index = 0
        while index < len(text):
            char = text[index]
            if char == "\x1b":
                match = re.match(r"\x1b\[([0-9;?<>]*)([@-~])", text[index:])
                if match:
                    self.csi(match.group(1), match.group(2))
                    index += len(match.group(0))
                    continue
                match = re.match(r"\x1b\][^\x07]*\x07", text[index:])
                if match:
                    index += len(match.group(0))
                    continue
                if index + 1 < len(text):
                    index += 2
                    continue
                index += 1
                continue
            self.put(char)
            index += 1

    def lines(self) -> list[str]:
        return ["".join(row).rstrip() for row in self.grid]


def set_size(fd: int, width: int, height: int) -> None:
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))


def read_available(fd: int) -> bytes:
    chunks = []
    while True:
        ready, _, _ = select.select([fd], [], [], 0)
        if not ready:
            break
        try:
            data = os.read(fd, 65536)
        except OSError:
            break
        if not data:
            break
        chunks.append(data)
    return b"".join(chunks)


def pump(fd: int, duration: float, transcript: bytearray) -> None:
    deadline = time.time() + duration
    while time.time() < deadline:
        data = read_available(fd)
        if data:
            transcript.extend(data)
        time.sleep(0.02)


def wait_for(fd: int, needle: str, timeout: float, transcript: bytearray) -> bool:
    deadline = time.time() + timeout
    text = ""
    while time.time() < deadline:
        data = read_available(fd)
        if data:
            transcript.extend(data)
            text += data.decode("utf-8", "replace")
            if needle in text:
                return True
        time.sleep(0.05)
    return False


def debug(message: str) -> None:
    if os.environ.get("BATON_VERIFY_DEBUG"):
        print(f"[probe] {message}", file=sys.stderr, flush=True)


def reap(pid: int, timeout: float) -> int:
    deadline = time.time() + timeout
    while time.time() < deadline:
        done, status = os.waitpid(pid, os.WNOHANG)
        if done:
            return os.waitstatus_to_exitcode(status)
        time.sleep(0.05)
    debug("child did not exit after quit; sending SIGTERM")
    try:
        os.kill(pid, signal.SIGTERM)
    except OSError:
        pass
    deadline = time.time() + 2
    while time.time() < deadline:
        done, status = os.waitpid(pid, os.WNOHANG)
        if done:
            return os.waitstatus_to_exitcode(status)
        time.sleep(0.05)
    debug("child ignored SIGTERM; sending SIGKILL")
    try:
        os.kill(pid, signal.SIGKILL)
    except OSError:
        pass
    deadline = time.time() + 2
    while time.time() < deadline:
        done, status = os.waitpid(pid, os.WNOHANG)
        if done:
            return os.waitstatus_to_exitcode(status)
        time.sleep(0.05)
    return -1


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/baton")
    parser.add_argument("--command", default="/status")
    parser.add_argument("--expect", action="append", default=[])
    parser.add_argument("--reject", action="append", default=[])
    parser.add_argument("--arg", action="append", default=[])
    parser.add_argument("--width", type=int, default=120)
    parser.add_argument("--height", type=int, default=34)
    parser.add_argument("--ready", default="Ask anything")
    parser.add_argument("--warmup", type=float, default=3.0)
    parser.add_argument("--timeout", type=float, default=12.0)
    parser.add_argument("--evidence-dir", default="local_docs/verification/evidence")
    args = parser.parse_args()

    binary = Path(args.binary).resolve()
    if not binary.is_file():
        print(f"missing binary: {binary}", file=sys.stderr)
        return 2

    home = tempfile.mkdtemp(prefix="baton-verify-home-")
    workdir = tempfile.mkdtemp(prefix="baton-verify-cwd-")
    evidence = Path(args.evidence_dir) / f"verify-baton-{int(time.time())}"
    evidence.mkdir(parents=True, exist_ok=True)

    pid, master = pty.fork()
    if pid == 0:
        os.chdir(workdir)
        env = os.environ.copy()
        env["BATON_HOME"] = home
        env["TERM"] = "xterm-256color"
        os.execvpe(str(binary), [str(binary), *args.arg], env)

    set_size(master, args.width, args.height)
    transcript = bytearray()
    exit_code = None
    passed = False
    try:
        if not wait_for(master, args.ready, args.timeout, transcript):
            raise RuntimeError(f"TUI did not show ready text: {args.ready}")
        debug("ready")
        # The ready text is painted before crossterm finishes entering raw
        # mode. Typing immediately loses all but the first byte at the tty
        # layer, which leaves a bare slash suggestion list.
        pump(master, args.warmup, transcript)
        os.write(master, f"{args.command}\r".encode())
        pump(master, 0.8, transcript)
        debug("command sent")
        # The screen parser consumes the whole ANSI stream from startup, so it
        # reconstructs the final screen without a forced resize.
        pump(master, 0.5, transcript)
        screen_text = bytes(transcript).decode("utf-8", "replace")
        screen = Screen(args.width, args.height)
        screen.feed(screen_text)
        lines = screen.lines()
        joined = "\n".join(lines)
        expectations = args.expect or ["thread:", "adapters:"]
        missing = [expect for expect in expectations if expect not in joined]
        rejected = [reject for reject in args.reject if reject in joined]
        passed = not missing and not rejected
        (evidence / "screen.txt").write_text(joined + "\n")
        (evidence / "transcript.raw").write_bytes(bytes(transcript))
        debug("screen captured")
        os.write(master, b"/quit\r")
        pump(master, 0.5, transcript)
        exit_code = reap(pid, 5)
        (evidence / "transcript.raw").write_bytes(bytes(transcript))
        (evidence / "summary.json").write_text(json.dumps({
            "command": args.command,
            "expect": expectations,
            "missing": missing,
            "rejected": rejected,
            "exit_code": exit_code,
            "passed": passed and exit_code == 0,
            "evidence": str(evidence),
        }, indent=2) + "\n")
        print(json.dumps({
            "passed": passed and exit_code == 0,
            "missing": missing,
            "rejected": rejected,
            "exit_code": exit_code,
            "evidence": str(evidence),
        }, indent=2))
        if not passed:
            print(joined, file=sys.stderr)
        return 0 if passed and exit_code == 0 else 1
    except Exception as error:
        try:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
        except OSError:
            pass
        (evidence / "transcript.raw").write_bytes(bytes(transcript))
        print(json.dumps({"passed": False, "error": str(error), "evidence": str(evidence)}, indent=2))
        return 1
    finally:
        try:
            os.close(master)
        except OSError:
            pass
        import shutil
        shutil.rmtree(home, ignore_errors=True)
        shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
