# Baton verification feature map

Use these files with `../SKILL.md`. Each journey is written for an agent that has never seen baton.

| Feature | Surface | Proof |
|---|---|---|
| Doctor | CLI | `baton doctor` next action, six providers, readiness, database count |
| TUI status | TUI | `/status` shows thread, phase, queue, and adapter lifecycle |
| Slash choices | TUI | typing a command argument lists valid choices or its syntax, filters them, and completes with Tab or Enter |
| Status next action | CLI | `baton status` and `--json` answer what to do now |
| Provider readiness | CLI and TUI | doctor summarizes ready/missing; pickers mark missing harnesses |
| Default provider | CLI and TUI | new work starts on an installed harness, with an explicit override |
| Recent thread hint | TUI | a new session points at existing threads in the folder |
| Attention inbox | TUI | `/inbox` lists failed, unverified, interrupted, needs-input, and queued work and opens the selected thread |
| Thread pins | TUI; CLI read | `/pin` toggles a durable pin, Ctrl+Up/Ctrl+Down or `/pin move` orders pinned threads, pinned rows sort first, and `baton threads --json` exposes `pinned` and `pin_order_key` |
| File navigation | TUI; CLI | `/files` inserts a selected project path or `path:line`, `/grep` and `baton grep` search file contents with sorted previews, `baton files` ranks project paths with ripgrep or git, and `baton open` launches `VISUAL` or `EDITOR` with `-C`-directory containment |
| Shell completions | CLI | `baton completions <shell>` generates from the clap command graph and handles a broken pipe |
| Fork | CLI | `baton fork --json` creates a new thread without changing the source |
| Native fork | CLI | `baton fork --native` attaches a Codex or OpenCode native session to the fork |
| Native rewind | CLI | `baton rewind --native` truncates Codex or OpenCode history and keeps the session |
| Review | TUI | `/review` shows the turn diff, saves a line note, and leaves it unsent |
| TUI native rewind | TUI | `/rewind --native` truncates the provider and store history and prefills the message |
| Greeting | TUI | `hi` in default orchestrate mode answers directly, with no planner run |
| MCP stdio | CLI | `baton mcp` handshakes, lists 14 tools, and answers status, turns, threads, and doctor |
| Antigravity | CLI | `agy acp` completes one turn with the ICA key and custom base URL |
| Output sanitization | CLI and TUI | Control sequences are replaced before `exec` stdout and the transcript; unit proof only |
| Activity | TUI and CLI | `/activity` and `baton activity --json` index every turn's tools, diff, and usage |
| Memories | TUI and CLI | `/memories`, `$memories`, and `baton memories` share canonical project/global rows |
| Grok | CLI | `grok agent stdio` completes a turn on the ICA endpoint |
| Cursor | CLI | `cursor-agent acp` gives the login hint and rejects a custom endpoint |
| Process registry | CLI | `baton ps --json` returns a stable schema and no false live processes |
| Orchestration | CLI | `baton orchestrate` creates worktrees, runs harnesses, and records runs |
| Orchestrate stream | CLI | `--stream-json` emits plan, worker, and result JSONL rows |
| Spawn | CLI | `baton spawn` runs one worker in a worktree and returns JSON |
| Exec stream | CLI | `baton exec --stream-json` emits parseable JSONL and a final result row |
| Adapter lifecycle | TUI | `/status` shows one state and owner pair per adapter |
| Missing harness | CLI and TUI | `exec`, `/switch`, and the banner name the binary and the next step |
| Installers | Shell and PowerShell | `ci/installer-selftest.sh` selects the Linux asset, verifies its SHA-256, and rejects a corrupted archive |

Start with the feature touched by the change. One real journey is enough for a smoke proof; the map exists so a broader change can cover the rest.
