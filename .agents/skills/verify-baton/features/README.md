# Baton verification feature map

Use these files with `../SKILL.md`. Each journey is written for an agent that has never seen baton.

| Feature | Surface | Proof |
|---|---|---|
| Doctor | CLI | `baton doctor --json` schema, six providers, database count |
| TUI status | TUI | `/status` shows thread, phase, queue, and adapter lifecycle |
| Fork | CLI | `baton fork --json` creates a new thread without changing the source |
| Native fork | CLI | `baton fork --native` attaches a Codex native session to the fork |
| Process registry | CLI | `baton ps --json` returns a stable schema and no false live processes |

Start with the feature touched by the change. One real journey is enough for a smoke proof; the map exists so a broader change can cover the rest.
