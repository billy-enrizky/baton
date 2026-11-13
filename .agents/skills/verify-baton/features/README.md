# Baton verification feature map

Use these files with `../SKILL.md`. Each journey is written for an agent that has never seen baton.

| Feature | Surface | Proof |
|---|---|---|
| Doctor | CLI | `baton doctor --json` schema, six providers, database count |
| TUI status | TUI | `/status` shows thread, phase, queue, and adapter lifecycle |
| Status next action | CLI | `baton status` and `--json` answer what to do now |
| Provider readiness | CLI and TUI | doctor summarizes ready/missing; pickers mark missing harnesses |
| Default provider | CLI and TUI | new work starts on an installed harness, with an explicit override |
| Fork | CLI | `baton fork --json` creates a new thread without changing the source |
| Native fork | CLI | `baton fork --native` attaches a Codex or OpenCode native session to the fork |
| Native rewind | CLI | `baton rewind --native` truncates Codex or OpenCode history and keeps the session |
| Process registry | CLI | `baton ps --json` returns a stable schema and no false live processes |
| Orchestration | CLI | `baton orchestrate` creates worktrees, runs harnesses, and records runs |
| Exec stream | CLI | `baton exec --stream-json` emits parseable JSONL and a final result row |
| Adapter lifecycle | TUI | `/status` shows one state and owner pair per adapter |
| Missing harness | CLI and TUI | `exec`, `/switch`, and the banner name the binary and the next step |

Start with the feature touched by the change. One real journey is enough for a smoke proof; the map exists so a broader change can cover the rest.
