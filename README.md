# baton

One terminal CLI for every coding agent harness. Same thread, any model, native prompt caching. The TUI opens with a Codex-style header showing the baton version, working directory, session mode, coordinator or harness model, permissions, and billing.

New sessions orchestrate by default: the prompt is the goal, a coordinator splits it, and independent workers run in separate git worktrees, each on the harness that fits the task. `/switch` moves to a single harness when one agent is the right shape. Hit a wall in that harness, switch to another, and keep going in the same thread with Claude Code or Codex holding the context.

baton is a terminal-first reimagining of [t3code](https://github.com/pingdotgg/t3code). Adapter protocol knowledge and the native session scanner are ported from t3code (MIT). The Rust implementation follows codex-rs patterns (ratatui TUI, sequential session loop, split line transports).

## What it does

- **A clear start.** The banner shows version, cwd, session mode, model, reasoning effort when set, permissions, and billing before the first prompt. New threads inherit the harness config's permission mode, so the banner shows the mode the provider will actually use. Greetings go to one harness; real work goes to the planner.
- **Orchestrated by default.** A first prompt starts a coordinator, which plans independent tasks and picks a harness for each. Workers run in isolated worktrees with live progress, commit their changes to their own branches, and can be merged back with a confirmation. If the coordinator cannot return a usable plan, baton runs one direct worker instead of failing the run. If a Baton process dies mid-run, the next start settles the abandoned run to a terminal verdict while preserving completed workers.
- **One CLI, many harnesses.** Codex, Claude Code, Grok Build, OpenCode, Cursor, and Antigravity, driven through their supported programmatic surfaces (app-server JSON-RPC, headless stream-json, ACP, HTTP).
- **The thread is the product.** Switching harness or model mid-thread is a dropdown: `/switch` opens the provider picker, then the model picker. The first turn on a new harness carries a context handoff capped at 24,000 bytes; after that each harness keeps its own native session.
- **High prompt cache hit rates.** Once a harness has a native session, baton sends only the new message and the harness replays its own history. The first turn on a harness that has no session carries a bounded handoff instead. The cache hit rate is always visible in the status bar.
- **Reads the same context jsonl.** `baton import` discovers existing native sessions (claude transcripts, codex rollouts) and imports them as resumable baton threads.
- **Checkpoints.** Every turn ends with a hidden git ref capturing the working tree. `/rewind` restores the conversation and optionally the code.
- **Review from the terminal.** `/review` shows the turn diff with context, keeps notes on exact changed lines, and sends them back as a steer or the next prompt.
- **Live catalogs.** `/switch` loads the provider's current model list (Codex, OpenCode, and ACP providers), with type-to-filter search.
- **Parallel, then merged.** `/multi` fans one prompt across providers and opens a merged live view; press Enter to inspect any child thread. `/multi --worktrees` runs the same fan-out as isolated orchestration workers, so `i` merges and `p` opens PRs through the existing worker flows.
- **Queued by provider.** If two threads target the same provider at once, the second waits in a durable `queued` phase and starts when the adapter is free instead of failing.
- **Agent-readable state.** `status`, `threads`, `turns`, `queue`, `exec`, and `orchestrate` have versioned JSON output. `exec --stream-json` and `orchestrate --stream-json` emit one JSON event per line and end with a result row. `baton mcp` exposes the same durable state over MCP stdio.
- **Subscription or API billing.** Each provider runs on its harness login or an API key, switchable per provider (`baton auth <provider> api`).
- **codex-style keys.** Esc interrupts, Esc-Esc backtracks to a previous prompt, streaming Markdown with tables and aligned columns, tinted diffs, and per-tool activity rows.

## Install

macOS and Linux:

```
curl -fsSL https://raw.githubusercontent.com/billy-enrizky/baton/main/install.sh | sh
```

Windows PowerShell:

```
irm https://raw.githubusercontent.com/billy-enrizky/baton/main/install.ps1 | iex
```

Homebrew:

```
brew install billy-enrizky/baton/baton
```

From source:

```
cargo install --path .
baton
```

The installer downloads the release for your OS and checks its SHA-256. If no release asset matches, it falls back to a source build when Cargo is installed. The harness CLIs you want to drive must still be on PATH.

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

Antigravity runs as `agy acp`. To use an API key and a custom Gemini endpoint, add this to `~/.baton/config.json`:

```json
{
  "antigravity": {
    "mode": "api",
    "apiKeyEnv": "CODEX_ICA_API_KEY",
    "apiBaseUrl": "https://api.servicesessentials.ibm.com"
  },
  "defaultModels": { "antigravity": "gemini-3.7-flash" }
}
```

`apiKeyEnv` is the variable baton reads; the child receives `GEMINI_API_KEY`. The base URL is the endpoint root because `agy` appends `/v1beta` itself, so do not add `/v1`.

Grok uses the same override shape with the OpenAI-compatible `/v1` base. Baton spawns `grok agent stdio` and maps the base URL to `GROK_XAI_API_BASE_URL` and `GROK_MODELS_BASE_URL`:

```json
{
  "grok": {
    "mode": "api",
    "apiKeyEnv": "CODEX_ICA_API_KEY",
    "apiBaseUrl": "https://api.servicesessentials.ibm.com/v1"
  },
  "defaultModels": { "grok": "gemini-3.7-flash" }
}
```

Cursor is installed as `cursor-agent` and baton spawns `cursor-agent acp`. Cursor validates its own API keys, so the ICA key is rejected; run `cursor-agent login` and use subscription auth instead. Cursor cannot use the ICA endpoint.

## Start here

Run `baton` with no arguments. The workspace opens in orchestrate mode. Type what you want to do in plain language and press Enter. Baton picks a harness or splits the work. `/help` shows the common paths, `/help all` lists every command, and `baton doctor` checks what is installed.

## Usage

```
baton                    # fresh orchestrated session in the TUI
baton --mode single      # fresh session on one harness
baton -t <id>            # open an existing thread and its saved session mode
baton threads [--json]   # list threads with phase, turns, goal, queue
baton inbox [--json]     # failed, unverified, needs-input, queued, and unshipped worker work
baton completions <shell>  # bash, zsh, fish, powershell, or elvish
baton status [-t id] [--json]  # plain-language next step, phase, goal, queue, usage
baton activity [-t id] [--json]  # per-turn provider, tool calls, diff, usage
baton memories [-C path] [--json]  # governed project and global memories
baton turns [-t id] [--json]   # full durable turn history
baton queue [-t id] [--clear] [--json]  # inspect/clear queued follow-ups
baton search <query> [--all] [--json]  # search thread titles and conversation text
baton ps [--json]        # list provider servers tracked by baton
baton doctor [--json]    # next action, ready and missing harnesses, configs, database, and process health
baton resume [-t id] [--fork] [--print] [--force]  # open the native CLI
baton exec -p <provider> [--effort level] [--json | --stream-json] "prompt"  # one non-interactive turn
baton exec -t <worker-thread> [--json] "answer"  # answer an orchestration worker question
baton spawn [-p provider] [--json] "task"  # one isolated worker in a worktree

`--json` prints one final result object. `--stream-json` prints one compact JSON object per line for provider session and turn ids, text deltas, tool start and finish, usage, and turn completion, then a final `type: "result"` row. Use it when an agent needs live progress; stdout stays parseable JSONL and progress text stays off stderr.
baton mcp                # MCP stdio server for agent access to the thread store
baton fork [-t id] [--turn n] [--native] [--json] [title]  # branch a thread; Codex and OpenCode can fork natively
baton import [--dry-run] # import recent native sessions as threads
baton usage              # token usage and cache hit rates
baton auth [provider] [subscription|api]
baton diff <id> [turn]   # checkpoint diff against the working tree
baton restore <id> <turn>
baton rewind [-t id] --turn N [--native] [--json]  # remove a turn and later turns; Codex and OpenCode can revert natively, with durable recovery
baton orchestrate [--max-tasks N] [--plan-file path] [--plan-only] [--progress | --stream-json | --json] "goal"  # coordinator + workers in worktrees
baton orchestrate --list [--all] [--json]
baton orchestrate --run <run-id> [--json]
baton orchestrate --integrate <run-id> [--task N] [--json]  # merge completed worker branches
baton orchestrate --open-prs <run-id> [--task N] [--json]  # push branches and open worker PRs
baton orchestrate --archive <run-id> [--json]  # hide a finished run without deleting it

`orchestrate --stream-json` writes one compact JSON row for the plan, each worker start, each worker finish, then the full final report with `type: "result"`. It conflicts with `--json` and `--progress` and applies only to a live run.
baton worktrees [--json] [--run <id>]  # list managed worktrees
baton worktrees --clean [--force] [--run <id>]  # remove clean, merged worktrees
baton bench startup [--json] [--repeat N]  # per-phase startup timings over the active home
```

In the TUI: `/help /activity /status /switch /model /effort /mode /auth /new /fork /worktree /threads /pin /inbox /open /usage /checkpoints /diff /review /rewind /goal /find /multi /orchestrate /runs /commands /queue /turns /memories /compact /context /init /export /settings /mcp /skills /copy /clear /quit /exit`. Typing a command followed by space lists its valid choices, or shows the argument syntax when the command takes free-form text. Partial text filters the choices, and Tab or Enter completes the selection. New threads start in orchestrate mode; `/orchestrate off` switches the current thread to a single harness, `/orchestrate on` switches back, and `/switch` also returns to single-harness mode. In orchestrate mode a composer prompt launches a coordinator run instead of a single turn. `/orchestrate --plan-only <goal>` opens a plan preview with effective models and waits for Enter to run or Esc to edit. `/runs` lists recent durable runs, `/runs --all` includes archived runs, and `/runs <id>` reopens a stored worker report even after it is archived. `/pin` opens the current project's thread picker; Enter toggles a durable pin and pinned threads sort first. In that picker, Ctrl+Up and Ctrl+Down reorder the selected pinned thread without closing the picker; `/pin move <id> up` and `/pin move <id> down` provide the same order from the command line, and completion lists both directions. Press `i` in a report to merge completed worker branches, `p` to push them and open pull requests, or `a` to archive the run. Approvals answer with y/a/n and remain scoped to the thread that requested them. Ctrl+C interrupts a running turn or cancels an active orchestration, including its worker process groups; press twice to exit when idle. Drag in the transcript to select character ranges across wrapped rows; `/copy` copies the latest assistant reply. While a turn is running, a new composer message is sent as a steer on Codex, Claude, and OpenCode; other providers queue it durably. `/fork [--turn N] [title]` branches the conversation into a new thread; both threads share the same working tree, and the fork starts with a transcript handoff instead of the source provider session. `/worktree [title]` forks the conversation into a new thread whose cwd is an isolated git worktree, binds that thread to a one-task orchestration run, and switches to it. The next successful turn commits there, after which the run report can integrate or ship the work. `/rewind --native` opens the rewind picker and uses Codex or OpenCode native rollback with the durable recovery marker; plain `/rewind` keeps the transcript-only path.

`/inbox` lists threads with failed, unverified, interrupted, needs-input, or queued work across the store; Enter opens the selected thread. `/activity` opens an index of every turn with provider, phase, tool calls, diff, and usage; Enter expands a turn, `y` copies the summary, and `d` opens its diff. `baton activity --json` exposes the same index to agents. `/commands [turn]` opens a deep dive of every command and tool call, grouped by provider; Enter expands and `y` copies it. `/memories` lists, adds (`--global`), removes, and clears governed notes; `$memories` in a prompt injects the project and global notes. `/effort` opens a reasoning-effort picker and the composer footer shows the active effort.

In the composer, Enter sends and Shift+Enter (or Alt+Enter) adds a newline. Pasting a multi-line prompt keeps it in the composer instead of submitting the first line, and Up recalls this project's recent prompts. While a turn is running, Enter steers it; a message typed during an orchestration is queued durably. Enter on an empty composer sends the next queued message. A steer the harness does not take falls back to the queue, and the notice names the reason.

For OpenCode, `baton rewind --native` stages and commits a history-only revert at that turn's user message id with `files: false`. Baton truncates its own turns and keeps the provider session; checkpoints still own file restore. A turn recorded before Baton captured OpenCode message ids fails loudly instead of guessing a boundary.

For Codex, `baton fork --native` asks the Codex app-server for a native `thread/fork`, then attaches the new provider session to the baton fork. The source thread is unchanged, and `--turn N` forks through that completed provider turn. For OpenCode, `--native` forks the whole native session through `/session/<id>/fork`; `--turn` is refused until Baton can map a turn to an OpenCode message boundary. Without `--native`, baton copies the transcript and starts the fork with a handoff.

## Orchestration

For one task, `baton spawn "task"` skips the coordinator and runs a single worker in an isolated worktree through the same run, task, commit, and report records. `--json` returns the run, task, thread, worktree, branch, commit, text, and question, so an agent can follow up with `baton exec -t <thread>`.

`baton orchestrate` is a small Orca-style multi-agent loop. A coordinator harness receives the goal, splits it into independent tasks, and chooses the best available harness for each task. Baton then creates a real git worktree per worker under `~/.baton/worktrees/<run>/<task>`, runs each worker through the normal `baton exec` path in parallel, and records the run and task results durably. New interactive sessions use this path by default; `--mode single`, `/orchestrate off`, or `"defaultMode": "single"` in `~/.baton/config.json` restores single-harness startup.

```sh
baton --yolo orchestrate --max-tasks 3 "Audit billing settings, add the missing tests, and update the docs"
baton orchestrate --list
baton orchestrate --run <run-id> --json
```

Workers are ordinary baton threads in their own worktrees, so they keep the same turn phases, adapter lifecycle, process tracking, and native-session behavior as single-thread runs. The global `--yolo`, `--full-auto`, and `--plan` flags apply to the coordinator and worker processes. Use `--plan-file` to supply a plan directly, which is useful for agents and repeatable tests. Add `--plan-only` to resolve the coordinator or plan file, print the effective provider/model per task, and exit without creating a run or starting workers. A plan task may include an optional `model` and an optional `effort`; otherwise the worker uses its harness config defaults. The coordinator sets either field only when the goal explicitly names one. Effort is supported by Codex and Claude in this release; another harness rejects an explicit effort with a clear worker error. `/effort <level>` sets the current thread effort, `/effort default` clears it, and the banner shows it as `provider/model@effort`. The coordinator retries once if its first plan is not valid JSON.

If the source checkout has uncommitted changes, baton asks before starting: Enter captures a source snapshot into every worker worktree, `s` runs a single harness in the live checkout, and Esc cancels and restores the prompt. Worker commits contain only files the worker touched relative to that snapshot; untouched dirty source files stay out of the branch. Worktrees are kept after the run for review. A successful worker commits its changes to its branch as `baton worker: <title>`; `baton orchestrate --integrate <run-id>` merges completed branches in plan order, refuses a dirty source checkout, refuses any run with a worker waiting for input, reports conflicts with the affected files, and skips workers that are already merged. For a dirty-source run, integration uses the captured snapshot as the merge ancestor, so committing those source edits first does not create a false conflict, and untouched dirty files keep the source content. `baton orchestrate --open-prs <run-id>` pushes each eligible worker branch to `origin` (or the only configured remote) and opens one pull request against the source checkout's current branch with `gh`. Open a worker from the report, send a follow-up, and the successful turn commits that worktree and refreshes the run. If a worker ends its reply with `BATON_QUESTION: <question>`, the task and run become `needs_input`, the report shows the question with a `?` marker, and integration and PR shipping are refused until the worker thread is answered. A new commit clears the task's merge and PR markers so the refresh must be integrated or shipped again. A failed follow-up marks an uncommitted task failed; if the task already has a successful commit, the last result and its markers are preserved. Re-running either command is idempotent: merged tasks and tasks with a recorded PR are skipped, and an existing PR for the same head branch is linked instead of duplicated. `baton orchestrate --archive <run-id>` hides a finished run from the default lists without deleting its history or worktrees; use `--list --all` or `/runs --all` to show it again. `baton worktrees --clean` removes clean worktrees whose branch is merged or whose task has an opened PR; dirty or unmerged work without a PR needs the explicit `--force`. A dirty source checkout is branched from `HEAD` and then receives the source snapshot, so workers see tracked and untracked non-ignored edits without those edits entering an untouched worker commit.

### Shared worktree directories

Dependencies and caches that live in gitignored paths can be shared from the primary checkout. Add `baton.json` at the repository root:

```json
{
  "worktree": {
    "sharedDirectories": ["node_modules", "target"]
  }
}
```

Only entries that exist as directories in the primary checkout and pass `git check-ignore` are shared. Each becomes a symlink in the new worktree; missing, tracked, absolute, or `..` paths are skipped. Shared directories are the same files, so edits inside them affect the primary checkout. `baton worktrees --clean` removes the symlink with the worktree and leaves the source directory intact. `/worktree` creates a worktree for a conversation fork and records the binding as an orchestration task, so integrate, PR shipping, archive, and cleanup use the same durable flows.

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
- `baton_status`: provider, model, reasoning effort, mode, goal, latest turn, queue, usage, stale warning
- `baton_threads`: durable threads for the current project, or every project with `all=true`
- `baton_turns`: turn phases, timestamps, provider turn ids, errors
- `baton_messages`: newest conversation rows, including tool and diff rows
- `baton_queue`: inspect queued follow-ups, or clear them explicitly
- `baton_search`: search thread titles and conversation text in the project, or every project with `all=true`
- `baton_processes`: provider server processes tracked by baton, including PID, owner, port, command-line, and process-start verification
- `baton_orchestration_runs`: durable orchestration runs with status, coordinator, task counts, and timestamps; `all=true` includes archived runs
- `baton_orchestration_report`: one run's plan, worker tasks, commits, worktree paths, PR links, text, and errors
- `baton_fork`: create a new thread from a conversation, optionally through a completed turn, without changing the source; set `native=true` for a Codex or OpenCode native fork
- `baton_activity`: recent activity across projects with turn and worker status
- `baton_memories`: durable memories for the project, or every project with `all=true`
- `baton_latest_diff`: newest stored turn diff

The server is read-only except for the explicit `baton_queue` clear option and `baton_fork`, which creates a new thread and leaves the source unchanged. It opens the same `~/.baton/baton.sqlite` database and honors `BATON_HOME`.

## How context and caching work

Each provider keeps its own native session per baton thread. On every turn after the first, only the new user message is sent; the harness resumes its own session, which keeps provider prompt caches warm. The first turn on a harness with no session sends a handoff built from prior messages plus the working-tree summary, capped at 24,000 bytes. Provider caches are separate systems: switching harnesses means a cold cache for the new one, and switching back re-warms within that provider's cache TTL.

## Storage

`~/.baton/baton.sqlite` (threads, provider sessions, message mirror, durable turns, queued follow-ups, review notes, checkpoints, usage, and verified provider process records) and `~/.baton/config.json` (explicit overrides; harness configs are read from `~/.claude`, `~/.codex`, and their project-level equivalents). `BATON_HOME` relocates everything. A provider child left behind by a killed baton process is reaped on the next start only when its dead owner, PID, command line, and process start time still match the recorded process. Each provider child also carries a small watchdog that terminates it within about two seconds when baton disappears, including after SIGKILL.

## Development

```
cargo test     # unit + protocol tests
cargo build --release
```

Planning and verification records:

- [Terminal parity ledger](docs/parity.md)
- [CI design](docs/ci.md)
- [CI acceptance record](docs/ci-acceptance.md)

## License

MIT.
