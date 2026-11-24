# baton

One terminal CLI for every coding agent harness. Same thread, any model, native prompt caching. The TUI opens with a status header showing the baton version, working directory, session mode, coordinator or harness model, permissions, and billing.

New sessions orchestrate by default: the prompt is the goal, a coordinator splits it, and independent workers run in separate git worktrees, each on the harness that fits the task. `/switch` moves to a single harness when one agent is the right shape. Hit a wall in that harness, switch to another, and keep going in the same thread with Claude Code or Codex holding the context.

## What it does

- **A clear start.** The banner shows version, cwd, session mode, model, reasoning effort when set, permissions, and billing before the first prompt. New threads inherit the harness config's permission mode, so the banner shows the mode the provider will actually use. Greetings go to one harness; real work goes to the planner.
- **Orchestrated by default.** A first prompt starts a coordinator, which plans independent tasks and picks a harness for each. Workers run in isolated worktrees with live progress, commit their changes to their own branches, and can be merged back with a confirmation. If the coordinator cannot return a usable plan, baton runs one direct worker instead of failing the run. If a Baton process dies mid-run, the next start settles the abandoned run to a terminal verdict while preserving completed workers.
- **One CLI, many harnesses.** Codex, Claude Code, Grok Build, OpenCode, Cursor, and Antigravity, driven through their supported programmatic surfaces (app-server JSON-RPC, headless stream-json, ACP, HTTP).
- **The thread is the product.** Switching harness or model mid-thread is a dropdown: `/switch` opens the provider picker, then the model picker. The first turn on a new harness carries a context handoff capped at 24,000 bytes; after that each harness keeps its own native session.
- **High prompt cache hit rates.** Once a harness has a native session, baton sends only the new message and the harness replays its own history. The first turn on a harness that has no session carries a bounded handoff instead. The cache hit rate is always visible in the status bar.
- **Reads the same context jsonl.** `baton import` discovers existing native sessions (claude transcripts, codex rollouts) and imports them as resumable baton threads.
- **Checkpoints.** Every turn ends with a hidden git ref capturing the working tree. `/rewind` restores the conversation and optionally the code.
- **Review from the terminal.** `/review` shows the turn diff with context, keeps notes on exact changed lines, and sends them back as a steer or the next prompt.
- **Live catalogs.** `/switch` loads the provider's current model list (Codex, OpenCode, and ACP providers), with type-to-filter search. A short or degraded live probe keeps the known catalog instead of hiding models Baton already ships.
- **Parallel, then merged.** `/multi` fans one prompt across providers and opens a merged live view; press Enter to inspect any child thread. `/multi --worktrees` runs the same fan-out as isolated orchestration workers, so `i` merges and `p` opens PRs through the existing worker flows.
- **Queued by provider.** If two threads target the same provider at once, the second waits in a durable `queued` phase and starts when the adapter is free instead of failing.
- **Agent-readable state.** `status`, `threads`, `turns`, `queue`, `exec`, and `orchestrate` have versioned JSON output. `exec --stream-json` and `orchestrate --stream-json` emit one JSON event per line and end with a result row. `baton mcp` exposes the same durable state over MCP stdio.
- **Subscription or API billing.** Each provider runs on its harness login or an API key. `/auth` opens a billing picker for every harness and shows which key variable is set or missing; `baton auth <provider> api` does the same from the command line.
- **Familiar terminal keys.** Esc interrupts, Esc-Esc backtracks to a previous prompt, streaming Markdown with tables and aligned columns, tinted diffs, and per-tool activity rows.

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

See [Install Baton](install.md) for supported platforms, checksum verification, upgrades, uninstall, and editor and search prerequisites.

## Set up a harness

Run `baton login <provider>` once for each harness you want to drive, or set the provider's API key and choose API billing with `/auth`. The setup guide covers every key variable, custom endpoints, roles, MCP, skills, memory, and troubleshooting: [Set up Baton](setup.md).

## Start here

Run `baton` with no arguments. The workspace opens in the configured default session mode, which is orchestrate until `/settings` or `config.json` changes it. Type what you want to do in plain language and press Enter. Baton picks a harness or splits the work. `/help` shows the common paths, `/help all` lists every command, and `baton doctor` checks what is installed.

## Usage

```
baton                    # fresh session in the configured default mode
baton --mode single      # fresh session on one harness
baton -t <id>            # open an existing thread and its saved session mode
baton threads [--json]   # list threads with phase, turns, goal, queue
baton inbox [--json]     # failed, unverified, needs-input, queued, and unshipped worker work
baton completions <shell>  # bash, zsh, fish, powershell, or elvish
baton status [-t id] [--json]  # plain-language next step, phase, goal, queue, usage
baton activity [-t id] [--json]  # per-turn provider, tool calls, diff, usage
baton memories [-C path] [--json]  # governed project and global memories for prompt expansion
baton turns [-t id] [--json]   # full durable turn history
baton queue [-t id] [--clear] [--json]  # inspect/clear queued follow-ups
baton search <query> [--all] [--json]  # search thread titles and conversation text
baton files [query...] [--ignored] [--json]  # list project files for terminal quick-open
baton open <path[:line]> [--print]  # open a project file, optionally at a line
baton grep <query> [--case-sensitive] [--word] [--regex] [--json]  # search project file contents
baton ps [--json]        # list provider servers tracked by baton
baton doctor [--json]    # next action, ready and missing harnesses, configs, database, and process health
baton resume [-t id] [--fork] [--print] [--force]  # open the native CLI
baton exec -p <provider> [--effort level] [--json | --stream-json] "prompt"  # one non-interactive turn
baton exec -t <worker-thread> [--json] "answer"  # answer an orchestration worker question
baton spawn [-p provider] [--json] "task"  # one isolated worker in a worktree
baton roles [--json]                       # orchestrator, planner, implementor, reviewer

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

`baton files` uses `rg --files` when ripgrep is installed and falls back to `git ls-files` inside a repository. Discovery and `baton open` are scoped to the current `-C` directory. Hidden project files such as `.github`, `.vscode`, and `.env.example` stay visible; tool caches and build output such as `.git`, `node_modules`, `.next`, `.venv`, the `target` directory directly under `-C`, and package-manager stores are pruned. Query tokens filter during discovery, so unmatched files do not consume the cap, and a filename that contains every token sorts ahead of a path-only match. Each query token also matches as an in-order subsequence, so `pmod` finds `src/parser/mod.rs` and `pmod rs` finds the same file even when the tokens are typed in a different order; matched characters are highlighted in the TUI picker. Subsequence tokens are independent, may cross path separators, and never outrank a substring match. Add a trailing `:line` on the `/files` command or the live filter, such as `/files pmod rs:12`, to insert `path:line` instead of a bare path. The suffix is 1-based; spaces around the colon are ignored; `:0` stays part of the filter; leading zeros are normalized; and `app\:12` filters a literal colon. The inserted text is composer content. `baton open path:line` parses the same suffix and launches the editor at that line: VS Code-family editors get `--goto path:line`, Vim/Emacs/nano get `+line path`, and Helix/Sublime/Zed plus unknown editors get `path:line`. The merged matches are capped at 20,001 across both discovery passes, and the global `--limit` controls output. Recently picked or opened files break ranking ties before length and path order; `/files` or `/grep` Enter, Ctrl+O in either picker, and `baton open` record them, capped at 200 per canonical `-C` directory. Worktrees do not share recent files. Add `--ignored` to rank gitignored files in the same pool; a short `.env` is then visible at the default limit, though ignored rows beyond a full 20,001-match cap are dropped. `baton open` refuses paths outside the current `-C` directory, prefers `VISUAL` over `EDITOR`, supports quoted editor commands including Windows paths, and waits for the editor. `--print` shows the launch command with host-shell quoting without starting the editor; quoting is best effort on Windows, while the run path passes argv directly. `baton grep` searches file contents with `rg` (falling back to `git grep`), sorts matches by path and line, searches hidden project files such as `.github`, prunes VCS and cache directories, skips binary files, and uses the global `--limit`. Add `--case-sensitive`, `--word`, or `--regex` for the matching mode; `--json` returns `schema_version`, `cwd`, `query`, the flags, `limit`, `truncated`, `count`, and `matches` with `path`, `line`, and a 300-character `text` preview. The query is trimmed, multiple words form one phrase, and no matches exits 0.

In the TUI, `/grep [--case|--case-sensitive] [--word] [--regex] <query>` searches in the background and opens a picker of `path:line` matches: typing narrows the loaded results, Enter inserts `path:line ` into the composer, Ctrl+O launches the selected match at its line in `VISUAL` or `EDITOR`, and Esc clears the filter before closing. Results are capped at 200; when the cap is hit, the footer says to rerun `/grep` for a fresh search. Binary and cache paths are skipped. Esc hides the panel, and a search already running finishes in the background. `/files [--ignored] [query]` opens a picker over the capped inventory for the current thread's project directory and loads it in the background. Gitignored files stay out unless `--ignored` is explicit; that mode shows a warning because build output and secrets such as `.env` can appear. Recently picked files sort first within a match tier: typing narrows the loaded rows, Enter inserts the selected path and a trailing space into the composer, Esc clears the filter before closing, and Esc closes immediately while the scan is still running. Ctrl+O launches `VISUAL` or `EDITOR` at the selected file, including the parsed line target. Both pickers restore the TUI and show an opened notice; a missing editor or invalid command leaves the picker open. `/open` still selects threads, and the CLI `baton open path:line` launches the editor at that line.

In the TUI: `/help /activity /status /git /switch /model /effort /mode /auth /roles /new /fork /worktree /threads /pin /inbox /open /files /grep /usage /checkpoints /diff /review /rewind /retry /goal /find /multi /orchestrate /runs /commands /queue /turns /memory /compact /context /init /export /settings /mcp /skills /copy /clear /quit /exit`. Typing a command followed by space lists its valid choices, or shows the argument syntax when the command takes free-form text. Partial text filters the choices, and Tab or Enter completes the selection. A `--` prefix offers flag completions for `/files` and `/grep` without intercepting a bare `/files` or `/grep` Enter. `/roles` opens the role registry with the resolved orchestrator, planner, implementor, and reviewer models and prompt sources; Enter shows the selected role's detail. `/settings` opens global default session mode and terminal-bell choices above the current harness's read-only settings; Enter writes a global choice to `config.json`, applies the bell immediately, and shows the selected category's full option list. `/skills` opens the discovered Claude Code and Codex skill picker; Enter inserts `$name` and reports the source. A Claude thread rewrites the final provider payload to `/name`; the stored composer and transcript keep `$name`. The notice warns when the source harness differs or when `/orchestrate off` is needed. New sessions and `/new` use the configured default session mode, which is orchestrate until changed; `/fork` keeps the source mode and `/worktree` starts single. `/orchestrate off` switches the current thread to a single harness, `/orchestrate on` switches back, and `/switch` also returns to single-harness mode. In orchestrate mode a composer prompt launches a coordinator run instead of a single turn. `/orchestrate --plan-only <goal>` opens a plan preview with effective models and waits for Enter to run or Esc to edit. `/runs` lists recent durable runs, `/runs --all` includes archived runs, and `/runs <id>` reopens a stored worker report even after it is archived. `/pin` opens the current project's thread picker; Enter toggles a durable pin and pinned threads sort first. In that picker, Ctrl+Up and Ctrl+Down reorder the selected pinned thread without closing the picker; `/pin move <id> up` and `/pin move <id> down` provide the same order from the command line, and completion lists both directions. Press `i` in a report to merge completed worker branches, `p` to push them and open pull requests, or `a` to archive the run. Approvals answer with y/a/n, and `c` copies the approval prompt. Pickers and previews copy their selected text with `c`; the model picker uses `ctrl+y` so `c` remains a filter character. Approvals remain scoped to the thread that requested them. Ctrl+C interrupts a running turn or cancels an active orchestration, including its worker process groups; press twice to exit when idle. Drag in the transcript to select character ranges across wrapped rows; `/copy` copies the latest assistant reply. While a turn is running, a new composer message is sent as a steer on Codex, Claude, and OpenCode; other providers queue it durably. `/fork [--turn N] [title]` branches the conversation into a new thread; both threads share the same working tree, and the fork starts with a transcript handoff instead of the source provider session. `/worktree [title]` forks the conversation into a new thread whose cwd is an isolated git worktree, binds that thread to a one-task orchestration run, and switches to it. The next successful turn commits there, after which the run report can integrate or ship the work. `/rewind --native` opens the rewind picker and uses Codex or OpenCode native rollback with the durable recovery marker; plain `/rewind` keeps the transcript-only path. `/retry` lists failed or unverified turns with stored prompt previews. Same-harness rows show native-session coverage; cross-harness rows show the switch instruction. The selected-turn detail shows the bounded error and full prompt length. The picker names the durable current harness, effective model, permission mode, and session coverage, and `/retry <turn>` opens it with that row selected. Enter submits the first user message as one new turn when the failed turn used that same harness. A turn from another harness is refused with a switch instruction, so a stored prompt cannot bypass the redacted handoff preview. Open, queued, native-rewind, or live orchestration work blocks the retry, and an engine-rejected retry is shown without entering the queue.

`/inbox` lists threads with failed, unverified, interrupted, needs-input, or queued work across the store; Enter opens the selected thread. `/activity` opens an index of every turn with provider, phase, tool calls, diff, and usage; Enter expands a turn, `y` copies the summary, and `d` opens its diff. `baton activity --json` exposes the same index to agents. `/commands [turn]` opens a deep dive of every command and tool call, grouped by provider; Enter expands and `y` copies it. `/memory` lists Claude user, project, local, and rule files, Codex user instructions and existing user override, project `AGENTS.md` and an existing root `AGENTS.override.md`, and the project auto-memory index and topic files. Enter opens the selected file or creates it first, `Open auto-memory folder` opens the directory in the editor, and Auto-memory toggles Claude's user `autoMemoryEnabled` setting. `/memories` is gone; `$memories` still expands governed notes in a prompt, and `baton memories` remains for scripts. `/git` opens Status, Commit all changes, Commit staged changes, Push current branch, and Cancel. Status scrolls every changed path with its code; the commit confirmation scrolls every planned path and names the branch and message before Enter commits the whole repository. Push picks a remote, names the exact ref and ahead/behind counts, never forces, and does not change the upstream. `/effort` opens a reasoning-effort picker and the composer footer shows the active effort. `/auth` opens a scrollable billing picker for every harness, marks the current mode, shows the API key variable or `missing`, applies a keyed choice, and stays open with the exact variable name when the key is absent.

In the composer, Enter sends and Shift+Enter (or Alt+Enter) adds a newline. Pasting a multi-line prompt keeps it in the composer instead of submitting the first line, and Up recalls this project's recent prompts. While a turn is running, Enter steers it; a message typed during an orchestration is queued durably. Enter on an empty composer sends the next queued message. A steer the harness does not take falls back to the queue, and the notice names the reason.

For OpenCode, `baton rewind --native` stages and commits a history-only revert at that turn's user message id with `files: false`. Baton truncates its own turns and keeps the provider session; checkpoints still own file restore. A turn recorded before Baton captured OpenCode message ids fails loudly instead of guessing a boundary.

For Codex, `baton fork --native` asks the Codex app-server for a native `thread/fork`, then attaches the new provider session to the baton fork. The source thread is unchanged, and `--turn N` forks through that completed provider turn. For OpenCode, `--native` forks the whole native session through `/session/<id>/fork`; `--turn` is refused until Baton can map a turn to an OpenCode message boundary. Without `--native`, baton copies the transcript and starts the fork with a handoff.

## Orchestration

For one task, `baton spawn "task"` skips the coordinator and runs a single worker in an isolated worktree through the same run, task, commit, and report records. `--json` returns the run, task, thread, worktree, branch, commit, text, and question, so an agent can follow up with `baton exec -t <thread>`.

`baton orchestrate` is a small Orca-style multi-agent loop. A coordinator harness receives the goal, splits it into independent tasks, and chooses the best available harness for each task. Baton then creates a real git worktree per worker under `~/.baton/worktrees/<run>/<task>`, runs each worker through the normal `baton exec` path in parallel, and records the run and task results durably. New interactive sessions use this path by default; `--mode single`, `/orchestrate off`, or `"defaultMode": "single"` in `~/.baton/config.json` restores single-harness startup.

The planner role supplies the coordinator provider, model, and effort. The orchestrator and planner markdown prompts are embedded in the planning request. Matching implementor-provider tasks inherit the implementor model and effort when the plan leaves them unset, and every worker receives the implementor prompt after Baton's fixed worktree preamble. `/roles` shows the reviewer role and global hook declarations, but reviewer gating and hook execution are not active yet. A conversational greeting in the TUI still uses the current thread's single harness, which the banner labels as `thread`. Worker-starting orchestration requires a git repository; `--plan-only` and previews work outside git. `orchestrate` rejects `--provider`, `--model`, and `--effort`; set those values on the roles in `roles.json`.

```sh
baton --yolo orchestrate --max-tasks 3 "Audit billing settings, add the missing tests, and update the docs"
baton orchestrate --list
baton orchestrate --run <run-id> --json
```

Workers are ordinary baton threads in their own worktrees, so they keep the same turn phases, adapter lifecycle, process tracking, and native-session behavior as single-thread runs. The global `--yolo`, `--full-auto`, and `--plan` flags apply to the coordinator and worker processes. Use `--plan-file` to supply a plan directly, which is useful for agents and repeatable tests. Add `--plan-only` to resolve the coordinator or plan file, print the effective provider/model per task, and exit without creating a run or starting workers. A plan task may include an optional `model` and an optional `effort`. An implementor-provider task without either field inherits the implementor role values; other provider tasks use the harness config defaults. The coordinator sets either field only when the goal explicitly names one. A resolved plan records that its role defaults were applied, so running a saved preview cannot change the model or effort that the preview showed. Effort is supported by Codex and Claude in this release; another harness rejects an explicit effort with a clear worker error. `/effort <level>` sets the current thread effort, `/effort default` clears it, and the banner shows it as `provider/model@effort`. The coordinator retries once if its first plan is not valid JSON. `baton spawn` resolves the provider model from config before writing its one-task plan, so an implementor role default does not silently replace the configured model.

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

User guides:

- [Install Baton](install.md)
- [Set up Baton](setup.md)

Contributor and release-engineering records live under `docs/` and `.agents/`.

## License

MIT. See [NOTICE](NOTICE) for upstream attribution.
