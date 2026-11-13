# baton

One terminal CLI for every coding agent harness. Same thread, any model, native prompt caching.

Start a thread with codex. Hit a wall. `/switch`, pick claude, and keep going in the same thread with Claude Code holding the context. Switch back later and the Codex session, and its provider-side prompt cache, resumes where you left off.

baton is a terminal-first reimagining of [t3code](https://github.com/pingdotgg/t3code). Adapter protocol knowledge and the native session scanner are ported from t3code (MIT). The Rust implementation follows codex-rs patterns (ratatui TUI, sequential session loop, split line transports).

## What it does

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
baton                    # fresh thread in the TUI (unsaved until the first message)
baton -t <id>            # open an existing thread
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
```

In the TUI: `/help /status /switch /model /mode /auth /new /fork /threads /open /usage /checkpoints /diff /review /rewind /goal /find /multi /queue /turns /copy /clear /quit`. Approvals answer with y/a/n and remain scoped to the thread that requested them. Ctrl+C interrupts a running turn, press twice to exit. Drag in the transcript to select and copy rendered lines; `/copy` copies the latest assistant reply. While a turn is running, a new composer message is sent as a steer on Codex, Claude, and OpenCode; other providers queue it durably. `/fork [--turn N] [title]` branches the conversation into a new thread; both threads share the same working tree, and the fork starts with a transcript handoff instead of the source provider session.

For Codex, `baton fork --native` asks the Codex app-server for a native `thread/fork`, then attaches the new provider session to the baton fork. The source thread is unchanged, and `--turn N` forks through that completed provider turn. Without `--native`, baton copies the transcript and starts the fork with a handoff.

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
- `baton_fork`: create a new thread from a conversation, optionally through a completed turn, without changing the source
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
