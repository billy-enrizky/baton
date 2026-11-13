---
name: verify-baton
description: "Drive baton's Rust CLI and TUI through an isolated PTY and prove user-visible behavior. Use after changing slash commands, adapter lifecycle/status, doctor, fork, queue, turn phases, or any TUI rendering. Also use when a provider readiness or process-leak claim needs live evidence."
---

# Verify baton

Baton is a terminal app, so the proof is a terminal transcript, not a unit test alone. This skill builds the debug binary once, starts each drive in its own PTY with a temporary `BATON_HOME` and working directory, captures a full screen after forcing a redraw, and preserves the evidence.

## Launch

From the repository root:

```sh
cargo build
```

Use `target/debug/baton` for verification. Do not reinstall `~/.cargo/bin/baton` just to test a change.

Every drive gets isolated state:

```sh
BATON_HOME=$(mktemp -d)
WORKDIR=$(mktemp -d)
```

The TUI is usable when the composer prompt (`Ask anything...`) is visible. A short CLI command (`baton doctor --json`, `baton fork --json`, `baton ps --json`) is usable when it exits 0 and prints its JSON object.

## Doctor

Run this first when a drive looks wrong:

```sh
target/debug/baton doctor --json
```

It reports the effective provider binaries, models, auth modes, config files, database state, and tracked process health. In a temporary home the thread count may be zero; that is a valid isolated instance.

## Drive

The helper is `scripts/tui_probe.py`. It opens a PTY, sets the terminal size, waits for the composer, sends one slash command, forces a full redraw, dumps the parsed screen, then sends `/quit` and verifies a clean exit.

```sh
.agents/skills/verify-baton/scripts/tui_probe.py \
  --binary target/debug/baton \
  --command /status \
  --expect "adapters:" \
  --expect "codex=" \
  --evidence-dir local_docs/verification/evidence
```

For a CLI proof, use the real binary directly and keep the JSON:

```sh
tmp=$(mktemp -d)
BATON_HOME="$tmp" target/debug/baton doctor --json > /tmp/baton-doctor.json
jq -e '.schema_version == 1 and (.providers | length == 6)' /tmp/baton-doctor.json
BATON_HOME="$tmp" target/debug/baton ps --json | jq -e '.processes == []'
```

The helper writes a timestamped folder under `--evidence-dir` with:

- `screen.txt`: parsed visible screen after the forced redraw
- `transcript.raw`: raw PTY bytes
- `summary.json`: command, expectations, exit code, and pass/fail

## Evidence standards

- Exercise the user path: type the slash command a user types, or run the CLI a user runs. Do not call internal test helpers.
- Capture the result and the side effect. For `/status`, assert the adapter lifecycle line. For `doctor`, assert the schema and provider list. For `ps`, assert the process list.
- A screen assertion must run after a forced redraw. Ratatui sends differential updates, so grepping one keypress frame is not a screen.
- Preserve evidence when cleanup runs. Cleanup removes the temporary home/workdir, never the evidence folder.
- Never kill by process name. The helper kills the exact child PID it started.

## Cleanup

The helper waits for `/quit` and reaps its child. If it times out, it sends SIGTERM then SIGKILL to that exact PID. Remove only the temporary directories the drive created:

```sh
rm -rf "$BATON_HOME" "$WORKDIR"
```

Do not remove `local_docs/verification/evidence/`.

## Feature map

Start with `features/README.md`. Each file names the user path, the drive command, and the observable end state. Add a feature file when a new user-facing command or state is added; do not weaken a journey to make it pass.

## Gotchas

- The first TUI frame can race the PTY setup. Wait for `Ask anything...`, then keep draining the PTY during the warmup. A bare sleep lets the redraw loop fill the output buffer and block the app before it reads the command.
- Do not set the PTY master non-blocking. On macOS that can leave the child's stdin effectively non-blocking, and crossterm consumes only the first byte of a command.
- The screen parser consumes the whole ANSI stream from startup, so a forced resize is not required. Resizing immediately after typing can race the command.
- `/status` renders as a notice above the composer. It wraps at the terminal width, so assert a short stable substring.
- A temporary home starts with no threads. Use `--expect "thread:"` as well as `--expect "adapters:"` when you need both.
- `baton doctor` reads the real harness config directories for provider readiness even with a temporary `BATON_HOME`; that is intentional. The baton database and thread count are isolated.
- Do not point the PTY at a live baton session the user is already using.
