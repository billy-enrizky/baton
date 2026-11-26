#!/usr/bin/env python3
"""Drive the real TUI under three terminal color advertisements and inspect SGR."""

from __future__ import annotations

import argparse
import json
import os
import pty
import re
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / ".agents/skills/verify-baton/scripts"))

from tui_probe import binary_identity, pump, reap, set_size, wait_for  # noqa: E402

SGR = re.compile(rb"\x1b\[([0-9;]*)m")
NAMED_FG = [(30, 37), (90, 97)]
NAMED_BG = [(40, 47), (100, 107)]
BASIC_NAMES = [name for low, high in NAMED_FG + NAMED_BG for name in range(low, high + 1)]


def sgr_parameters(transcript: bytes) -> list[list[int]]:
    return [
        [int(part) for part in match.split(b";") if part]
        for match in (m.group(1) for m in SGR.finditer(transcript))
    ]


def color_usage(transcript: bytes) -> dict[str, list[int]]:
    """Split SGR into named basic colors and extended colors, skipping `38;5;n`."""
    named: list[int] = []
    indexed: list[int] = []
    rgb: list[int] = []
    for parameters in sgr_parameters(transcript):
        index = 0
        while index < len(parameters):
            value = parameters[index]
            if value in (38, 48, 58) and index + 1 < len(parameters):
                kind = parameters[index + 1]
                if kind == 5 and index + 2 < len(parameters):
                    indexed.append(parameters[index + 2])
                    index += 3
                    continue
                if kind == 2 and index + 4 < len(parameters):
                    rgb.extend(parameters[index + 2 : index + 5])
                    index += 5
                    continue
                index += 1
                continue
            if value in BASIC_NAMES:
                named.append(value)
            index += 1
    return {"named": named, "indexed": indexed, "rgb": rgb}


def run_mode(binary: Path, term: str, colorterm: str | None, width: int, height: int) -> bytes:
    home = tempfile.mkdtemp(prefix="baton-color-home-")
    work = tempfile.mkdtemp(prefix="baton-color-cwd-")
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(work)
        env = os.environ.copy()
        env.pop("NO_COLOR", None)
        env["BATON_HOME"] = home
        env["TERM"] = term
        if colorterm:
            env["COLORTERM"] = colorterm
        else:
            env.pop("COLORTERM", None)
        os.execvpe(str(binary), [str(binary)], env)
    set_size(master, width, height)
    transcript = bytearray()
    try:
        if not wait_for(master, "Ask anything", 20.0, transcript):
            raise RuntimeError(f"TUI did not start with TERM={term}")
        pump(master, 1.5, transcript)
        os.write(master, b"\x03")
        pump(master, 0.2, transcript)
        os.write(master, b"\x03")
        pump(master, 0.3, transcript)
        reap(pid, 5)
    finally:
        try:
            os.close(master)
        except OSError:
            pass
    return bytes(transcript)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/baton")
    parser.add_argument("--width", type=int, default=120)
    parser.add_argument("--height", type=int, default=34)
    parser.add_argument("--evidence-dir", default="local_docs/verification/evidence")
    args = parser.parse_args()

    binary = Path(args.binary).resolve()
    if not binary.is_file():
        print(f"missing binary: {binary}", file=sys.stderr)
        return 2
    evidence = Path(args.evidence_dir) / f"color-{int(time.time())}"
    evidence.mkdir(parents=True, exist_ok=True)

    cases = [
        ("ansi16", "xterm", None),
        ("ansi256", "xterm-256color", None),
        ("truecolor", "xterm-256color", "truecolor"),
    ]
    missing: list[str] = []
    for name, term, colorterm in cases:
        transcript = run_mode(binary, term, colorterm, args.width, args.height)
        (evidence / f"{name}.raw").write_bytes(transcript)
        usage = color_usage(transcript)
        named, indexed, rgb = usage["named"], usage["indexed"], usage["rgb"]
        if name == "ansi16":
            if rgb:
                missing.append("ansi16 emitted RGB")
            if any(value > 15 for value in indexed):
                missing.append(
                    f"ansi16 emitted an index above 15: {sorted(set(indexed))}"
                )
            if not indexed and not named:
                missing.append("ansi16 emitted no palette colors")
            # 11 is bright yellow. A nearest-RGB fallback reads the cream
            # accent as gray (7), so this proves the role table is wired.
            if 11 not in indexed and 93 not in named:
                missing.append("ansi16 did not render the accent as bright yellow")
        elif name == "ansi256":
            if rgb:
                missing.append("ansi256 emitted RGB")
            if not any(value >= 16 for value in indexed):
                missing.append("ansi256 emitted no cube or grayscale colors")
        elif not rgb:
            missing.append("truecolor emitted no RGB")

    summary = {
        "passed": not missing,
        "binary": binary_identity(binary),
        "missing": missing,
        "evidence": str(evidence),
    }
    (evidence / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
    return 0 if not missing else 1


if __name__ == "__main__":
    raise SystemExit(main())
