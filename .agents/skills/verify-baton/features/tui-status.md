# TUI status

## Sub-features
- Thread id and title
- Provider and model
- Effective permissions, the selected permission mode, and the provider default
- Goal and paused state
- Turn count and latest phase
- Queue count
- Adapter lifecycle states
- Cached rate-limit plan, primary and secondary window percentages, credits, and a `/limits` refresh hint

## How to get to it (user POV)
Start `baton` and type `/status`, or run the PTY helper with `--command /status`.

## Driving it with the PTY/CLI harness
```sh
.agents/skills/verify-baton/scripts/tui_probe.py \
  --binary target/debug/baton \
  --command /status \
  --expect "thread:" \
  --expect "permissions:" \
  --expect "adapters:" \
  --expect "codex=" \
  --evidence-dir local_docs/verification/evidence
```

## Gotchas
- The notice wraps at the terminal width. Assert short substrings, not a whole line.
- A fresh temporary home has no history and no running turns, so the phase is `none`.
- Adapter states are live handles. A fresh TUI should show `unloaded` for adapters that have not run.
- `/status` reads the stored snapshot and never starts a provider read. The line says `cached; /limits refreshes`; run `/limits` for a live refresh and reset times.
