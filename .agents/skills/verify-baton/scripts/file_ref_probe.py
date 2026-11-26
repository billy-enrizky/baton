#!/usr/bin/env python3
"""Drive an `@` file mention through a PTY and assert the ranked insertion."""

import argparse
import json
import os
import pty
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from slash_probe import screen_text
from tui_probe import binary_identity, pump, set_size, wait_for

FILES = ("src/main.rs", "src/parser/mod.rs", "README.md")


def prepare() -> tuple[str, str, Path]:
    root = Path(tempfile.mkdtemp(prefix="baton-file-ref-"))
    home = root / "home"
    work = root / "work"
    home.mkdir()
    work.mkdir()
    for relative in FILES:
        path = work / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("# fixture\n")
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=work, check=True)
    subprocess.run(["git", "add", "."], cwd=work, check=True)
    subprocess.run(
        ["git", "-c", "user.email=probe@example.com", "-c", "user.name=probe",
         "commit", "-qm", "init"],
        cwd=work,
        check=True,
    )
    (home / "config.json").write_text("{}\n")
    return str(home), str(work), root


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/baton")
    parser.add_argument("--width", type=int, default=120)
    parser.add_argument("--height", type=int, default=34)
    parser.add_argument("--timeout", type=float, default=20.0)
    parser.add_argument("--evidence-dir", default="local_docs/verification/evidence")
    args = parser.parse_args()

    binary = Path(args.binary).resolve()
    if not binary.is_file():
        print(f"missing binary: {binary}", file=sys.stderr)
        return 2

    home, work, root = prepare()
    evidence = Path(args.evidence_dir) / f"file-ref-{int(time.time())}"
    evidence.mkdir(parents=True, exist_ok=True)
    master, slave = pty.openpty()
    set_size(master, args.width, args.height)
    env = dict(os.environ, BATON_HOME=home, TERM="xterm-256color")
    proc = subprocess.Popen(
        [str(binary)], stdin=slave, stdout=slave, stderr=slave,
        cwd=work, env=env, close_fds=True,
    )
    os.close(slave)
    transcript = bytearray()
    missing: list[str] = []
    try:
        if not wait_for(master, "Ask anything", args.timeout, transcript):
            raise RuntimeError("TUI did not show its composer")
        os.write(master, b"explain @src/ma")
        pump(master, 1.6, transcript)
        offered = screen_text(transcript, args.width, args.height)
        (evidence / "mention-list.txt").write_text(offered + "\n")
        for value in ["src/main.rs", "project file", "tab/enter complete"]:
            if value not in offered:
                missing.append(f"mention list: {value}")
        os.write(master, b"\t")
        pump(master, 0.6, transcript)
        inserted = screen_text(transcript, args.width, args.height)
        (evidence / "mention-inserted.txt").write_text(inserted + "\n")
        if "explain @src/main.rs" not in inserted:
            missing.append("mention insertion keeps the prompt and replaces the token")
        if "@src/ma " in inserted:
            missing.append("mention insertion left the partial token behind")

        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)
        for _ in range(len("explain @src/main.rs ")):
            os.write(master, b"\x7f")
        pump(master, 0.4, transcript)

        os.write(master, b"@src")
        pump(master, 1.4, transcript)
        directories = screen_text(transcript, args.width, args.height)
        (evidence / "directory-mentions.txt").write_text(directories + "\n")
        for value in ["src/", "project directory"]:
            if value not in directories:
                missing.append(f"directory mentions: {value}")
        os.write(master, b"\t")
        pump(master, 0.6, transcript)
        directory_inserted = screen_text(transcript, args.width, args.height)
        (evidence / "directory-inserted.txt").write_text(directory_inserted + "\n")
        if "@src/" not in directory_inserted:
            missing.append("directory insertion adds the trailing slash")
        os.write(master, b"ma")
        pump(master, 1.2, transcript)
        narrowed = screen_text(transcript, args.width, args.height)
        (evidence / "directory-narrowed.txt").write_text(narrowed + "\n")
        if "src/main.rs" not in narrowed:
            missing.append("typing after a directory narrows inside it")
        os.write(master, b"\t")
        pump(master, 0.6, transcript)
        narrowed_inserted = screen_text(transcript, args.width, args.height)
        if "@src/main.rs " not in narrowed_inserted:
            missing.append("the narrowed file completes after a directory mention")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)
        for _ in range(len("@src/main.rs ")):
            os.write(master, b"\x7f")
        pump(master, 0.4, transcript)

        os.write(master, b"mail a@b")
        pump(master, 0.8, transcript)
        address = screen_text(transcript, args.width, args.height)
        (evidence / "address-no-mention.txt").write_text(address + "\n")
        if "project file" in address:
            missing.append("an email address opened the mention picker")
        if "loading project files" in address:
            missing.append("an email address started an index load")

        os.write(master, b"\x03")
        pump(master, 0.2, transcript)
        os.write(master, b"\x03")
        pump(master, 0.6, transcript)
    finally:
        try:
            os.close(master)
        except OSError:
            pass
        proc.kill()
        proc.wait(timeout=5)

    summary = {
        "passed": not missing,
        "binary": binary_identity(binary),
        "missing": missing,
        "evidence": str(evidence),
    }
    (evidence / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
