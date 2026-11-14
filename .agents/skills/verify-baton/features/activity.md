# Activity index

## Sub-features
- `/activity` full-screen view, one row per turn
- Provider, model, phase, tool-call count, diff presence, usage
- Enter expands the selected turn in place
- `y` copies the summary; `d` opens `/review` for the turn; Esc closes
- Imported/message-only turns are marked `imported`
- Running turns are marked `running`; details are recorded when the turn ends
- `baton activity [-t thread] [--json]` uses the same builder
- MCP `baton_activity` is read-only

## Driving it with the PTY/CLI harness
```sh
.agents/skills/verify-baton/scripts/tui_probe.py \
  --binary target/debug/baton --command /activity \
  --expect "activity" --after-escape
```
For a populated view, run one turn in a temp repo, then send `/activity`.

## Gotchas
- Activity is settled activity. Live tool rows are in the transcript until the
  turn ends.
- Imported conversations have no native tool records, usage, or diff.
- `d` keeps the activity view behind review; Esc from review returns to it.
