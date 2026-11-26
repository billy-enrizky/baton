#!/usr/bin/env python3
"""Record a real Baton TUI session to MP4, GIF, and a poster image.

Frames come from the terminal bytes the TUI actually wrote, replayed through
pyte's emulator, so the recording matches what a user sees. The harness in the
`review` demo is a script, and the caption on the page says so.
"""

from __future__ import annotations

import argparse
import json
import os
import pty
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

import pyte
from PIL import Image, ImageDraw, ImageFont

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / ".agents/skills/verify-baton/scripts"))

from tui_probe import pump, read_available, reap, set_size  # noqa: E402

THEME = {
    "background": (20, 18, 13),
    "foreground": (232, 226, 210),
    "cursor": (240, 198, 116),
    "selection": (60, 52, 30),
}

ANSI = {
    "black": (0, 0, 0),
    "red": (205, 49, 49),
    "green": (13, 188, 121),
    "brown": (229, 229, 16),
    "blue": (36, 114, 200),
    "magenta": (188, 63, 188),
    "cyan": (17, 168, 205),
    "white": (229, 229, 229),
    "brightblack": (102, 102, 102),
    "brightred": (241, 76, 76),
    "brightgreen": (35, 209, 139),
    "brightbrown": (245, 245, 67),
    "brightblue": (59, 142, 234),
    "brightmagenta": (214, 112, 214),
    "brightcyan": (41, 184, 219),
    "brightwhite": (255, 255, 255),
}

FAKE_CODEX = Path("scripts/demo_fake_codex.py").read_text()


def color(value: str, default: tuple[int, int, int]) -> tuple[int, int, int]:
    if not value or value == "default":
        return default
    if value in ANSI:
        return ANSI[value]
    if len(value) == 6:
        try:
            return tuple(int(value[index : index + 2], 16) for index in (0, 2, 4))
        except ValueError:
            return default
    return default


def load_font(size: int) -> ImageFont.FreeTypeFont:
    for path in ("/System/Library/Fonts/Menlo.ttc", "/System/Library/Fonts/SFNSMono.ttf"):
        if Path(path).is_file():
            return ImageFont.truetype(path, size)
    raise SystemExit("no monospace font found")


class Recorder:
    def __init__(self, width: int, height: int, fps: int, font_size: int):
        self.width = width
        self.height = height
        self.fps = fps
        self.font = load_font(font_size)
        self.advance = self.font.getlength("M")
        self.line_height = round(font_size * 1.25)
        self.screen = pyte.Screen(width, height)
        self.stream = pyte.Stream(self.screen)
        self.frames: list[Image.Image] = []
        self.lock = threading.Lock()
        self.running = True

    def repaint(self, master: int) -> None:
        # A fresh TUI can leave one stale cell from its first layout. One resize
        # pair forces a full repaint before the recording starts.
        set_size(master, self.width, self.height + 1)
        time.sleep(0.25)
        set_size(master, self.width, self.height)
        time.sleep(0.45)

    def clear(self) -> None:
        with self.lock:
            self.frames.clear()

    def text(self) -> str:
        with self.lock:
            return "\n".join(self.screen.display)

    def feed(self, data: bytes) -> None:
        if not data:
            return
        with self.lock:
            self.stream.feed(data.decode("utf-8", "replace"))

    def snapshot(self) -> Image.Image:
        size = (round(self.advance * self.width), self.line_height * self.height)
        image = Image.new("RGB", size, THEME["background"])
        draw = ImageDraw.Draw(image)
        with self.lock:
            for y in range(self.height):
                row = self.screen.buffer[y]
                for x in range(self.width):
                    cell = row[x]
                    fg = color(cell.fg, THEME["foreground"])
                    bg = color(cell.bg, THEME["background"])
                    if cell.reverse:
                        fg, bg = bg, fg
                    if bg != THEME["background"]:
                        draw.rectangle(
                            [
                                (x * self.advance, y * self.line_height),
                                ((x + 1) * self.advance - 1, (y + 1) * self.line_height - 1),
                            ],
                            fill=bg,
                        )
                    if cell.data and cell.data != " ":
                        draw.text(
                            (x * self.advance, y * self.line_height - round(self.line_height * 0.08)),
                            cell.data,
                            font=self.font,
                            fill=fg,
                        )
        return image

    def capture(self, master: int, transcript: bytearray) -> None:
        interval = 1.0 / self.fps
        next_frame = time.time()
        while self.running:
            data = read_available(master)
            if data:
                transcript.extend(data)
                self.feed(data)
            now = time.time()
            if now >= next_frame:
                self.frames.append(self.snapshot())
                next_frame = max(next_frame + interval, now - interval)
            time.sleep(0.01)


def write_fake_codex(directory: Path) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / "codex"
    path.write_text(FAKE_CODEX)
    path.chmod(0o755)


def seed_repo(workdir: Path) -> Path:
    source = workdir / "app.txt"
    source.write_text("".join(f"alpha line {index:03d}\n" for index in range(1, 61)))
    for command in (
        ["git", "init", "-q"],
        ["git", "add", "app.txt"],
        ["git", "-c", "user.name=demo", "-c", "user.email=demo@example.com", "commit", "-q", "-m", "seed"],
    ):
        subprocess.run(command, cwd=workdir, check=True, capture_output=True)
    return source


def demo_review(master: int, recorder: Recorder) -> None:
    wait_for(recorder, "Ask anything", 20.0)
    recorder.repaint(master)
    recorder.clear()
    send(master, b"rewrite app.txt\r")
    wait_for(recorder, "ctrl+t expand", 25.0)
    time.sleep(2.2)
    send(master, b"\x14")
    wait_for(recorder, "ctrl+t collapse", 15.0)
    time.sleep(2.4)
    send(master, b"\x14")
    wait_for(recorder, "ctrl+t expand", 15.0)
    time.sleep(1.2)
    send(master, b"\x14")
    wait_for(recorder, "ctrl+t collapse", 15.0)
    time.sleep(2.4)


def demo_commands(master: int, recorder: Recorder) -> None:
    wait_for(recorder, "Ask anything", 20.0)
    recorder.repaint(master)
    recorder.clear()
    send(master, b"/git\r")
    wait_for(recorder, "Commit all changes", 10.0)
    time.sleep(1.8)
    send(master, b"\x1b")
    time.sleep(0.5)
    send(master, b"/switch\r")
    wait_for(recorder, "switch harness", 10.0)
    time.sleep(1.8)
    send(master, b"\x1b")
    time.sleep(0.4)
    send(master, b"/help\r")
    wait_for(recorder, "Type what you want to do", 10.0)
    time.sleep(1.6)
    send(master, b"\x1b")
    time.sleep(0.4)


def send(master: int, data: bytes) -> None:
    os.write(master, data)


def wait_for(recorder: Recorder, needle: str, timeout: float) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if needle in recorder.text():
            return
        time.sleep(0.05)
    tail = "\n".join(recorder.text().splitlines()[-12:])
    raise RuntimeError(f"timed out waiting for {needle!r}; screen:\n{tail}")


def encode(frames_dir: Path, out_dir: Path, name: str, fps: int, poster: Image.Image) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    pattern = str(frames_dir / "frame-%05d.png")
    mp4 = out_dir / f"{name}.mp4"
    subprocess.run(
        [
            "ffmpeg", "-y", "-loglevel", "error", "-framerate", str(fps), "-i", pattern,
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20",
            "-movflags", "+faststart", str(mp4),
        ],
        check=True,
    )
    gif = out_dir / f"{name}.gif"
    subprocess.run(
        [
            "ffmpeg", "-y", "-loglevel", "error", "-framerate", str(fps), "-i", pattern,
            "-vf", "fps=10,scale=820:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=128[p];[s1][p]paletteuse=dither=bayer",
            "-loop", "0", str(gif),
        ],
        check=True,
    )
    poster.save(out_dir / f"{name}-poster.png")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--demo", choices=["review", "commands"], required=True)
    parser.add_argument("--binary", default="target/debug/baton")
    parser.add_argument("--out", default="site/media")
    parser.add_argument("--width", type=int, default=100)
    parser.add_argument("--height", type=int, default=30)
    parser.add_argument("--fps", type=int, default=12)
    parser.add_argument("--font-size", type=int, default=16)
    parser.add_argument("--keep-frames", action="store_true")
    args = parser.parse_args()

    binary = Path(args.binary).resolve()
    if not binary.is_file():
        print(f"missing binary: {binary}", file=sys.stderr)
        return 2

    root = Path(tempfile.mkdtemp(prefix=f"baton-demo-{args.demo}-"))
    home = root / "home"
    workdir = root / "work"
    fake_bin = root / "bin"
    frames_dir = root / "frames"
    for path in (home, workdir, fake_bin, frames_dir):
        path.mkdir(parents=True, exist_ok=True)
    write_fake_codex(fake_bin)

    demo_file = workdir / "app.txt"
    if args.demo == "review":
        demo_file = seed_repo(workdir)
    else:
        (workdir / "README.md").write_text("# Demo repository\n")
        for command in (
            ["git", "init", "-q"],
            ["git", "add", "README.md"],
            ["git", "-c", "user.name=demo", "-c", "user.email=demo@example.com", "commit", "-q", "-m", "seed"],
        ):
            subprocess.run(command, cwd=workdir, check=True, capture_output=True)
    (home / "config.json").write_text(
        json.dumps({"defaultProvider": "codex", "defaultMode": "single"}) + "\n"
    )

    env = os.environ.copy()
    env["BATON_HOME"] = str(home)
    env["BATON_DEMO_FILE"] = str(demo_file)
    env["PATH"] = f"{fake_bin}:{env.get('PATH', '')}"
    env["TERM"] = "xterm-256color"
    env["COLORTERM"] = "truecolor"
    env["CODEX_HOME"] = str(root / "codex-home")
    env["CLAUDE_CONFIG_DIR"] = str(root / "claude-home")
    env.pop("NO_COLOR", None)
    env.pop("CODEX_ICA_API_KEY", None)

    pid, master = pty.fork()
    if pid == 0:
        os.chdir(workdir)
        os.execvpe(str(binary), [str(binary)], env)

    set_size(master, args.width, args.height)
    recorder = Recorder(args.width, args.height, args.fps, args.font_size)
    transcript = bytearray()
    thread = threading.Thread(target=recorder.capture, args=(master, transcript), daemon=True)
    thread.start()
    error = None
    try:
        if args.demo == "review":
            demo_review(master, recorder)
        else:
            demo_commands(master, recorder)
        send(master, b"\x03\x03")
        pump(master, 0.4, transcript)
    except Exception as caught:  # noqa: BLE001
        error = str(caught)
    finally:
        time.sleep(0.3)
        recorder.running = False
        thread.join(timeout=3)
        try:
            reap(pid, 5)
        except (ChildProcessError, OSError):
            pass
        try:
            os.close(master)
        except OSError:
            pass

    if error:
        print(json.dumps({"passed": False, "error": error}, indent=2))
        return 1
    if not recorder.frames:
        print(json.dumps({"passed": False, "error": "no frames captured"}, indent=2))
        return 1

    for index, frame in enumerate(recorder.frames):
        frame.save(frames_dir / f"frame-{index:05d}.png")
    poster = recorder.frames[int(len(recorder.frames) * 0.82)]
    out_dir = Path(args.out)
    encode(frames_dir, out_dir, args.demo, args.fps, poster)
    summary = {
        "passed": True,
        "demo": args.demo,
        "frames": len(recorder.frames),
        "seconds": round(len(recorder.frames) / args.fps, 2),
        "mp4": str(out_dir / f"{args.demo}.mp4"),
        "gif": str(out_dir / f"{args.demo}.gif"),
        "poster": str(out_dir / f"{args.demo}-poster.png"),
    }
    print(json.dumps(summary, indent=2))
    if not args.keep_frames:
        shutil.rmtree(frames_dir, ignore_errors=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
