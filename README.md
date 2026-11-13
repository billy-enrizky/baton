# baton

One terminal CLI for every coding agent harness. Same thread, any model, native prompt caching. The TUI opens with a Codex-style header showing the baton version, working directory, session mode, coordinator or harness model, permissions, and billing.

New sessions orchestrate by default: the prompt is the goal, a coordinator splits it, and independent workers run in separate git worktrees, each on the harness that fits the task. `/switch` moves to a single harness when one agent is the right shape. Hit a wall in that harness, switch to another, and keep going in the same thread with Claude Code or Codex holding the context.

baton is a terminal-first reimagining of [t3code](https://github.com/pingdotgg/t3code). Adapter protocol knowledge and the native session scanner are ported from t3code (MIT). The Rust implementation follows codex-rs patterns (ratatui TUI, sequential session loop, split line transports).

## What it does

- **A clear start.** The banner shows version, cwd, session mode, model, permissions, and billing before the first prompt. Greetings go to one harness; real work goes to the planner.
- **Orchestrated by default.** A first prompt starts a coordinator, which plans independent tasks and picks a harness for each. Workers run in isolated worktrees with live progress, commit their changes to their own branches, and can be merged back with a confirmation.
- **One CLI, many harnesses.** Codex, Claude Code, Grok Build, OpenCode, Cursor, and Antigravity, driven through their supported programmatic surfaces (app-server JSON-RPC, headless stream-json, ACP, HTTP).
- **The thread is the product.** Switching harness or model mid-thread is a dropdown: `/switch` opens the provider picker, then the model picker. First turn on a new harness carries a token-budgeted context handoff; after that each harness keeps its own native session.
- **High prompt cache hit rates.** baton never replays history into a harness. The cache hit rate is always visible in the status bar.
- **Reads the same context jsonl.** `baton import` discovers existing native sessions (claude transcripts, codex rollouts) and imports them as resumable baton threads.
- **Checkpoints.** Every turn ends with a hidden git ref capturing the working tree. `/rewind` restores the conversation and optionally the code.
- **Review from the terminal.** `/review` shows the turn diff with context, keeps notes on exact changed lines, and sends them back as a steer or the next prompt.
- **Live catalogs.** `/switch` loads the provider's current model list (Codex, OpenCode, and ACP providers), with type-to-filter search.
- **Parallel, then merged.** `/multi` fans one prompt across providers and opens a merged live view; press Enter to inspect any child thread.
- **Queued by provider.** If two threads target the same provider at once, the second waits in a durable `queued` phase and starts when the adapter is free instead of failing.
- **Agent-readable state.** `status`, `threads`, `turns`, `queue`, and `exec` have versioned JSON output. `baton mcp` exposes the same durable state over MCP stdio.
- **Subscription or API billing.** Each provider runs on its harness login or an API key, switchable per provider (`baton auth <provider> api`).
- **codex-style keys.** Esc interrupts, Esc-Esc backtracks to a previous prompt, streaming Markdown, tinted diffs, and per-tool activity rows.

## Install

Rust toolchain required. The harness CLIs you want to drive must be on PATH.

```
cargo install --path .
baton
```

Provider login (one time each):

```
baton login codex        # codex login
baton login claude       # claude auth login
baton login cursor       # cursor-agent login
baton login grok         # grok login
baton login opencode     # opencode auth login
baton login antigravity  # prints the Google sign-in instructions
```

API-rate billing: set the provider's key (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `CURSOR_API_KEY`, `XAI_API_KEY`, `OPENCODE_API_KEY`, `GEMINI_API_KEY`) and run `baton auth <provider> api`.

## Usage

```
baton                    # fresh orchestrated session in the TUI
baton --mode single      # fresh session on one harness
baton -t <id>            # open an existing thread and its saved session mode
baton threads [--json]   # list threads with phase, turns, goal, queue
baton status [-t id] [--json]  # phase, goal, queue, usage
baton turns [-t id] [--json]   # full durable turn history
baton queue [-t id] [--clear] [--json]  # inspect/clear queued follow-ups
baton search <query> [--all] [--json]  # search thread titles and conversation text
baton ps [--json]        # list provider servers tracked by baton
baton doctor [--json]    # provider binaries, configs, database, and process health
baton resume [-t id] [--fork] [--print] [--force]  # open the native CLI
baton exec -p <provider> [--json] "prompt"  # one non-interactive turn
baton mcp                # MCP stdio server for agent access to the thread store
baton fork [-t id] [--turn n] [--native] [--json] [title]  # branch a thread; Codex can fork natively
baton import [--dry-run] # import recent native sessions as threads
baton usage              # token usage and cache hit rates
baton auth [provider] [subscription|api]
baton diff <id> [turn]   # checkpoint diff against the working tree
baton restore <id> <turn>
baton rewind [-t id] --turn N [--native] [--json]  # remove a turn and later turns; Codex can revert natively
baton orchestrate [--max-tasks N] [--plan-file path] [--progress] [--json] "goal"  # coordinator + workers in worktrees
baton orchestrate --list [--all] [--json]
baton orchestrate --run <run-id> [--json]
baton orchestrate --integrate <run-id> [--task N] [--json]  # merge completed worker branches
baton orchestrate --open-prs <run-id> [--task N] [--json]  # push branches and open worker PRs
baton orchestrate --archive <run-id> [--json]  # hide a finished run without deleting it
baton worktrees [--json] [--run <id>]  # list managed worktrees
baton worktrees --clean [--force] [--run <id>]  # remove clean, merged worktrees
```

In the TUI: `/help /status /switch /model /mode /auth /new /fork /threads /open /usage /checkpoints /diff /review /rewind /goal /find /multi /orchestrate /runs /queue /turns /copy /clear /quit`. New threads start in orchestrate mode; `/orchestrate off` switches the current thread to a single harness, `/orchestrate on` switches back, and `/switch` also returns to single-harness mode. In orchestrate mode a composer prompt launches a coordinator run instead of a single turn. `/runs` lists recent durable runs, `/runs --all` includes archived runs, and `/runs <id>` reopens a stored worker report even after it is archived. Press `i` in a report to merge completed worker branches, `p` to push them and open pull requests, or `a` to archive the run. Approvals answer with y/a/n and remain scoped to the thread that requested them. Ctrl+C interrupts a running turn or cancels an active orchestration, including its worker process groups; press twice to exit when idle. Drag in the transcript to select character ranges across wrapped rows; `/copy` copies the latest assistant reply. While a turn is running, a new composer message is sent as a steer on Codex, Claude, and OpenCode; other providers queue it durably. `/fork [--turn N] [title]` branches the conversation into a new thread; both threads share the same working tree, and the fork starts with a transcript handoff instead of the source provider session.

For Codex, `baton fork --native` asks the Codex app-server for a native `thread/fork`, then attaches the new provider session to the baton fork. The source thread is unchanged, and `--turn N` forks through that completed provider turn. Without `--native`, baton copies the transcript and starts the fork with a handoff.

## Orchestration

`baton orchestrate` is a small Orca-style multi-agent loop. A coordinator harness receives the goal, splits it into independent tasks, and chooses the best available harness for each task. Baton then creates a real git worktree per worker under `~/.baton/worktrees/<run>/<task>`, runs each worker through the normal `baton exec` path in parallel, and records the run and task results durably. New interactive sessions use this path by default; `--mode single`, `/orchestrate off`, or `"defaultMode": "single"` in `~/.baton/config.json` restores single-harness startup.

```sh
baton --yolo orchestrate --max-tasks 3 "Audit billing settings, add the missing tests, and update the docs"
baton orchestrate --list
baton orchestrate --run <run-id> --json
```

Workers are ordinary baton threads in their own worktrees, so they keep the same turn phases, adapter lifecycle, process tracking, and native-session behavior as single-thread runs. The global `--yolo`, `--full-auto`, and `--plan` flags apply to the coordinator and worker processes. Use `--plan-file` to supply a plan directly, which is useful for agents and repeatable tests. The coordinator retries once if its first plan is not valid JSON.

If the source checkout has uncommitted changes, baton asks before starting: Enter orchestrates anyway from HEAD, `s` runs a single harness so it sees the edits, and Esc cancels and restores the prompt. Worktrees are kept after the run for review. A successful worker commits its changes to its branch as `baton worker: <title>`; `baton orchestrate --integrate <run-id>` merges completed branches in plan order, refuses a dirty source checkout, aborts a conflicting merge with the affected files, and skips workers that are already merged. `baton orchestrate --open-prs <run-id>` pushes each eligible worker branch to `origin` (or the only configured remote) and opens one pull request against the source checkout's current branch with `gh`. Re-running either command is idempotent: merged tasks and tasks with a recorded PR are skipped, and an existing PR for the same head branch is linked instead of duplicated. `baton orchestrate --archive <run-id>` hides a finished run from the default lists without deleting its history or worktrees; use `--list --all` or `/runs --all` to show it again. `baton worktrees --clean` removes clean worktrees whose branch is merged or whose task has an opened PR; dirty or unmerged work without a PR needs the explicit `--force`. A dirty source checkout is branched from `HEAD`, so uncommitted source changes are not present in worker worktrees.

### Shared worktree directories

Dependencies and caches that live in gitignored paths can be shared from the primary checkout. Add `baton.json` at the repository root:

```json
{
  "worktree": {
    "sharedDirectories": ["node_modules", "target"]
  }
}
```

Only entries that exist as directories in the primary checkout and pass `git check-ignore` are shared. Each becomes a symlink in the new worktree; missing, tracked, absolute, or `..` paths are skipped. Shared directories are the same files, so edits inside them affect the primary checkout. `baton worktrees --clean` removes the symlink with the worktree and leaves the source directory intact.

## MCP

`baton mcp` serves the local baton store over MCP stdio. It is intended for coding agents that need to inspect the threads they are working alongside.

```json
{
  "mcpServers": {
    "baton": {
      "command": "baton",
      "args": ["mcp"]
    }
  }
}
```

Tools:

- `baton_doctor`: provider binaries, effective models/auth/modes, config files, database size, thread count, and tracked process health
- `baton_status`: provider, model, mode, goal, latest turn, queue, usage, stale warning
- `baton_threads`: durable threads for the current project, or every project with `all=true`
- `baton_turns`: turn phases, timestamps, provider turn ids, errors
- `baton_messages`: newest conversation rows, including tool and diff rows
- `baton_queue`: inspect queued follow-ups, or clear them explicitly
- `baton_search`: search thread titles and conversation text in the project, or every project with `all=true`
- `baton_processes`: provider server processes tracked by baton, including PID, owner, port, command-line, and process-start verification
- `baton_fork`: create a new thread from a conversation, optionally through a completed turn, without changing the source; set `native=true` for a Codex native fork
- `baton_latest_diff`: newest stored turn diff

The server is read-only except for the explicit `baton_queue` clear option and `baton_fork`, which creates a new thread and leaves the source unchanged. It opens the same `~/.baton/baton.sqlite` database and honors `BATON_HOME`.

## How context and caching work

Each provider keeps its own native session per baton thread. On every turn only the new user message is sent; the harness resumes its own session, which keeps provider prompt caches warm. Provider caches are separate systems: switching harnesses means a cold cache for the new one, and switching back re-warms within that provider's cache TTL.

## Storage

`~/.baton/baton.sqlite` (threads, provider sessions, message mirror, durable turns, queued follow-ups, review notes, checkpoints, usage, and verified provider process records) and `~/.baton/config.json` (explicit overrides; harness configs are read from `~/.claude`, `~/.codex`, and their project-level equivalents). `BATON_HOME` relocates everything. A provider child left behind by a killed baton process is reaped on the next start only when its dead owner, PID, command line, and process start time still match the recorded process.

## Development

```
cargo test     # unit + protocol tests
cargo build --release
```

## License

MIT.
