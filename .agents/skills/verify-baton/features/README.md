# Baton verification feature map

Use these files with `../SKILL.md`. Each journey is written for an agent that has never seen baton.

| Feature | Surface | Proof |
|---|---|---|
| Doctor | CLI | `baton doctor` next action, six providers, readiness, database count |
| TUI status | TUI | `/status` shows thread, phase, queue, and adapter lifecycle |
| Queued follow-ups | TUI | `/queue` opens the queue; Enter sends, `e` edits, `d` deletes, `c` twice clears, and Ctrl+Up/Ctrl+Down reorder; every action persists through the store |
| Slash choices | TUI | typing a command argument lists valid choices or its syntax, filters them, and completes with Tab or Enter; `slash_sweep.py` runs every command and requires its stable surface |
| Settings | TUI | `/settings` writes global default mode and bell choices, shows the selected category's options, and labels harness rows read-only |
| Auth billing | TUI | `/auth` lists every harness with subscription and API rows, marks the current mode, names the key variable or `missing`, applies a keyed choice, and keeps the picker open on a missing key or malformed config |
| Roles | TUI and CLI | `/roles` shows the four resolved role rows and detail; `baton roles --json` also returns effort, prompt source/path, and parsed hook declarations |
| Skills | TUI | `/skills` lists Claude and Codex skills, scrolls past eight rows, and Enter inserts the source-specific invocation into the composer |
| Status next action | CLI | `baton status` and `--json` answer what to do now |
| Model catalog | TUI | `/model` merges the live list, the last recorded full list, and the known catalog; a short or failed probe keeps the older models |
| Provider readiness | CLI and TUI | doctor summarizes ready/missing; pickers mark missing harnesses |
| Default provider | CLI and TUI | new work starts on an installed harness, with an explicit override |
| Recent thread hint | TUI | a new session points at existing threads in the folder |
| Attention inbox | TUI | `/inbox` lists failed, unverified, interrupted, needs-input, and queued work and opens the selected thread |
| Thread pins | TUI; CLI read | `/pin` toggles a durable pin, Ctrl+Up/Ctrl+Down or `/pin move` orders pinned threads, pinned rows sort first, and `baton threads --json` exposes `pinned` and `pin_order_key` |
| File navigation | TUI; CLI | `/files [--ignored]` inserts a selected project path or `path:line` and warns when ignored files are included; Ctrl+O launches that target or a selected `/grep` match in `VISUAL` or `EDITOR`; `/grep` and `baton grep` search file contents with sorted previews; `baton files` ranks project paths with ripgrep or git; and `baton open` launches the editor with `-C`-directory containment |
| Composer file mentions | TUI | typing `@path` offers ranked project files and Tab or Enter replaces only the mention token; ordinary text and addresses never start an index load |
| Shell completions | CLI | `baton completions <shell>` generates from the clap command graph and handles a broken pipe |
| Issue tasks | CLI and TUI | `/task [number]` or `baton task <number>` reads an open issue from `origin` and creates a linked worktree; no number opens an issue picker |
| Fork | CLI | `baton fork --json` creates a new thread without changing the source |
| Native fork | CLI | `baton fork --native` attaches a Codex or OpenCode native session to the fork |
| Native rewind | CLI | `baton rewind --native` truncates Codex or OpenCode history and keeps the session |
| Review | TUI | `/review` shows the turn diff, saves a line note, and leaves it unsent |
| TUI native rewind | TUI | `/rewind --native` truncates the provider and store history and prefills the message |
| Retry | TUI | `/retry` lists failed or unverified turns and submits the chosen prompt as a new turn |
| Greeting | TUI | `hi` in default orchestrate mode answers directly, with no planner run |
| MCP stdio | CLI | `baton mcp` handshakes, lists 15 tools including agents, and answers status, turns, threads, agents, and doctor |
| Antigravity | CLI | `agy acp` completes one turn with the ICA key and custom base URL |
| Harness matrix | CLI | Codex, Claude, Cursor, Grok, OpenCode, and Antigravity are checked for a live ICA turn or an explicit external account block |
| Output sanitization | CLI and TUI | Control sequences are replaced before `exec` stdout and the transcript; unit proof only |
| Activity | TUI and CLI | `/activity` and `baton activity --json` index every turn's tools, diff, and usage |
| Subagent calls | TUI and CLI | `/agents` lists live and persisted subagent calls with running, done, failed, or unknown status; `baton agents -t <id>` and `baton_agents` expose the persisted rows |
| Memory files | TUI | `/memory` lists Claude and Codex instruction files, opens or creates them in the editor, and toggles Claude auto-memory |
| Git status, commit, push, checks, pull request, review, and merge | TUI | `/git` inspects the repository, confirms exact commit plans, pushes a chosen remote without force, lists the current pull request's `gh` checks, opens a pull request after a title and body preview, approves, requests changes on, or comments on the current pull request after rechecking the head, and merges it with squash, merge commit, or rebase after the same recheck |
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
