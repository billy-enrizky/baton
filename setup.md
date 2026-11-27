# Set up Baton

Baton drives harness CLIs that you install and authenticate yourself. A harness is the coding agent CLI you already use, such as Codex, Claude Code, Cursor, Grok, OpenCode, or Antigravity. This guide covers the first run, provider login, API billing, custom endpoints, roles, MCP, skills, memory, and common failures.

## What Baton needs from you

Baton needs no account of its own and sends no telemetry. It runs your harness CLIs with your credentials.

Required for a turn: one harness CLI on `PATH`, either signed in with `baton login <provider>` (or `/login` inside the workspace) or pointed at an API key with `/auth <provider> api`. The tables below list the login command and the key variable for each harness.

Optional, only for the features that use them:

| Tool or setting | Needed for |
|---|---|
| Authenticated `gh` | `/git checks`, `/git pr`, `/git review`, `/git merge`, `/task`, and worker pull requests from an orchestration report. The rest of `/git` works without it. |
| `rg` | `/files` and `/grep`. Without it, `/files` falls back to `git ls-files` and `/grep` falls back to `git grep` inside a repository. |
| `VISUAL` or `EDITOR` | Opening a file from `/files`, `/grep`, or `/memory`. |
| An MCP server | Whatever you add with `/mcp`; nothing is configured for you. |

Costs belong to the harness provider: a subscription for the harness login, or usage billed to the API key. A custom endpoint, including an internal gateway, carries that provider's terms. Baton adds no fee, no hosted service, and no background account.

## First run

```sh
baton
```

The TUI opens in the configured default session mode, which is orchestrate until you change the global default with `/settings` or `"defaultMode": "single"` in `config.json`. `/orchestrate off` changes only the current thread. Run `baton doctor` to see every harness binary, its auth mode, the Baton config files, database size, thread count, and tracked provider processes. For subscription billing, run `baton login <provider>` once for each harness you want to use, or type `/login` inside the workspace. API-key setups can skip the login.

## Provider login

Inside the workspace, `/login` opens the same choice as a picker. It runs the selected harness login in the terminal, then returns to Baton with the result.

| Provider | Command | Notes |
|---|---|---|
| Codex | `baton login codex` | Runs `codex login`. |
| Claude Code | `baton login claude` | Runs `claude auth login`. |
| Cursor | `baton login cursor` | Runs `cursor-agent login`. Cursor validates its own keys. |
| Grok | `baton login grok` | Runs `grok login`. |
| OpenCode | `baton login opencode` | Runs `opencode auth login`. |
| Antigravity | `baton login antigravity` | Prints the Google sign-in steps for `agy`. |

## API billing

Each provider can run on its harness login or an API key. `/auth` opens a scrollable picker with a subscription row and an API row for every harness. The API row names the key variable and says `key` or `missing`, so you can see what Baton will read before you switch.

| Provider | Key variable |
|---|---|
| Claude Code | `ANTHROPIC_API_KEY` |
| Codex | `OPENAI_API_KEY` |
| Cursor | `CURSOR_API_KEY` |
| Grok | `XAI_API_KEY` |
| OpenCode | `OPENCODE_API_KEY` |
| Antigravity | `GEMINI_API_KEY` |

Set the key in the environment or `~/.baton/config.json`; Baton also reads Claude and Codex settings. A config-file key wins over a harness key, which wins over the environment variable. Run `baton secret check` to see which source supplied each key without printing it; `--json` emits one row per provider. Then choose the API row. A missing key keeps the picker open with the exact variable name. The CLI form is `baton auth <provider> api`; `/auth <provider>` opens a two-row picker for that harness with the current mode marked, and it writes nothing until you choose a row. `/auth` and `/auth <provider>` remain readable while work is running; changing a mode waits until the turn or orchestration ends.

Baton reads a provider-neutral variable when you set `apiKeyEnv`. The child still receives the harness-native variable:

```json
{
  "antigravity": {
    "mode": "api",
    "apiKeyEnv": "MY_GATEWAY_KEY",
    "apiBaseUrl": "https://your-gateway.example.com"
  },
  "defaultModels": { "antigravity": "gemini-3.7-flash" }
}
```

Antigravity runs as `agy acp`. Its base URL is the endpoint root because `agy` appends `/v1beta`; do not add `/v1`.

Grok uses the same shape with the OpenAI-compatible `/v1` base. Baton spawns `grok agent stdio` and maps the base URL to `GROK_XAI_API_BASE_URL` and `GROK_MODELS_BASE_URL`:

```json
{
  "grok": {
    "mode": "api",
    "apiKeyEnv": "MY_GATEWAY_KEY",
    "apiBaseUrl": "https://your-gateway.example.com/v1"
  },
  "defaultModels": { "grok": "gemini-3.7-flash" }
}
```

Cursor runs as `cursor-agent acp` and rejects a custom endpoint. Use `cursor-agent login`.

OpenCode reads its own `opencode.json` for a custom provider. Baton does not map an OpenCode base URL. Keep the CLIs current with `grok update`, `cursor-agent update`, and `opencode upgrade`.

## Config file

`~/.baton/config.json` stores Baton overrides. Set `BATON_HOME` to use a different directory. The file is written with owner-only permissions on Unix. `/auth` and `/settings` refuse a malformed file instead of replacing it, so a hand-edit error never deletes your keys.

Useful top-level keys:

```json
{
  "defaultProvider": "claude",
  "defaultMode": "single",
  "notifications": { "bell": false },
  "defaultModels": { "claude": "claude-sonnet-5-5" },
  "codex": { "apiKeyEnv": "OPENAI_API_KEY" },
  "antigravity": {
    "mode": "api",
    "apiKeyEnv": "MY_GATEWAY_KEY",
    "apiBaseUrl": "https://your-gateway.example.com"
  }
}
```

`defaultMode` accepts `orchestrate` or `single`. Provider keys use the provider name as the key. Unknown provider and notification fields survive a write. With `notifications.bell` on, Baton rings on turn end, an approval, and a provider question; a two-second limiter mutes a burst.

## Roles

Baton has four roles: orchestrator, planner, implementor, and reviewer. Defaults are Claude `claude-opus-5-5` for orchestrator and planner, Codex `gpt-6.2-sol` for implementor, and Codex `gpt-6.2-astra` for reviewer. The coordinator turn uses the planner role's provider and model, with both the orchestrator and planner prompts; matching worker tasks use the implementor role. After every completed worker that has a commit, the reviewer role reads that commit and answers `APPROVE` or `REQUEST_CHANGES`; only an approved commit can be integrated or shipped. `/roles` shows the resolved rows and Enter shows the provider, model, effort, prompt source, config source, and parsed hook events. `baton roles --json` exposes the same registry.

Override models in `~/.baton/roles.json`:

```json
{
  "schema_version": 1,
  "review": true,
  "roles": {
    "orchestrator": { "provider": "claude", "model": "claude-opus-5-5" },
    "planner": { "provider": "claude", "model": "claude-opus-5-5" },
    "implementor": { "provider": "codex", "model": "gpt-6.2-sol" },
    "reviewer": { "provider": "codex", "model": "gpt-6.2-astra" }
  }
}
```

`review` defaults to `true`. Set it to `false` to skip the reviewer turn; a skipped review does not block integration or shipping. The run reports each worker's review status as `approve`, `request_changes`, `pending`, `failed`, or `skipped`, and a run with a blocking review finishes as `needs_review`. A follow-up commit marks the review `pending` until the reviewer reads the new commit.

A role must set `provider` and `model` together. `effort` is accepted only for a provider that supports reasoning effort. Malformed JSON, a partial provider/model pair, an unknown provider, an unknown hook event, an empty hook command, or a timeout outside 1 to 300 seconds fails closed. Declared `before_plan`, `after_plan`, `before_implement`, and `after_implement` hooks run in that order with the declared timeout: a failing plan hook aborts the run, a failing `before_implement` hook fails the task before the worker starts, and a failing `after_implement` hook fails the task before its commit. `before_review` runs before the reviewer turn and `after_review` runs after the verdict; a failing review hook records `failed` for that worker and blocks integration and shipping.

To run every role on Codex with DeepSeek V4.1 Flash:

```json
{
  "schema_version": 1,
  "roles": {
    "orchestrator": { "provider": "codex", "model": "rits/deepseek-v4.1-flash" },
    "planner": { "provider": "codex", "model": "rits/deepseek-v4.1-flash" },
    "implementor": { "provider": "codex", "model": "rits/deepseek-v4.1-flash" },
    "reviewer": { "provider": "codex", "model": "rits/deepseek-v4.1-flash" }
  }
}
```

`/roles` and `baton roles --json` show the resolved rows, so a partial or malformed override is visible before a run.

Override a prompt by creating `~/.baton/roles/orchestrator.md`, `planner.md`, `implementor.md`, or `reviewer.md`. Prompts are capped at 6 KiB. The built-in worker preamble stays first; user role text cannot move it.

Hooks are declared in the same file:

```json
{
  "hooks": {
    "before_plan": [{ "argv": ["./scripts/prepare-plan.sh"], "timeout_secs": 30 }]
  }
}
```

Known events are `before_plan`, `after_plan`, `before_implement`, `after_implement`, `before_review`, and `after_review`. Hook declarations are validated and shown by `/roles`, and all six events run with the declared timeout.

## MCP, skills, and memory

- `/mcp` lists, adds, removes, or syncs MCP servers. `baton mcp` exposes Baton state over stdio. `baton serve` claims the store, runs recovery, and listens on `BATON_HOME/serve.sock` (0600, same-user only) until SIGINT or SIGTERM. `--stdio` serves the spawner pipe and answers newline-delimited `ping`, `claim-info`, and `status` requests instead; `--socket <path>` overrides the socket path. Socket requests need a live grant id and secret with the scope their command needs: `read` for status and messages, `operate` for submit. `ping` is the only request without one. `baton grant list` shows grants without their secrets, `baton grant renew <id> [--days N]` extends one without shortening its window or rotating its secret, and `baton grant revoke <id>` deletes one. `baton audit list [--limit N] [--json]` shows the append-only audit events, including the target that names what changed. Grant mint, renewal, or revoke commits with its audit row in one transaction, every accepted turn records a `turn.submit` row without its prompt text, every approval answer records an `approval.decide` row and every question answer records a `question.answer` row before they are sent, an answer that cannot be recorded is denied or sent empty, successful git actions record a `git.*` row with their sha or pull request number, a saved global setting records a `settings.write` row named by its key, orchestration start and settle record `orchestration.*` rows named by the run id, governed memory writes record `memory.*` rows named by the memory id, and MCP fork or queue clear records the stdio spawner as the confirming authority. A renewal records a `grant.renew` audit row. At the host terminal, `pair [--label <name>] [--scope <s>]... [--ceiling <mode>] [--expires-days N]` mints a grant with `read` and `operate` by default and prints a `baton://local#...` URL whose fragment carries the secret; add `--scope approve` to answer approval requests; the ceiling is `default` unless you pass `--ceiling plan`, `default`, or `acceptEdits`, and bypass is never grantable; `renew <id> [--days N]` extends one live grant there. `baton attach --url <url> [--json]` uses that URL to read the host scope, thread count, and running turn count over the socket without starting another engine. Add `--thread <id>` for that thread's status, `--messages` to page its stored messages from a resume cursor, `--follow` to poll until it is idle, or `--submit <text>` to start a turn on that thread. The host runs submits in its own engine for the project directory it was started in. `--idempotency-key <key>` reuses a retry key, and attach makes a fresh one when the flag is absent. A repeat with the same key returns the stored result with `replayed` set and starts no second turn, and a stored result is replayed for seven days. A raw socket submit must send `args.idempotency_key` as a non-empty string of at most 128 characters. `baton attach --url <url> --approvals [--thread <id>] [--json]` lists pending approvals, and `--respond <request> --choice <choice> [--idempotency-key <key>] [--json]` answers one with the `approve` scope and writes an `approval.decide` row named by the grant. The local socket and attach require macOS or Linux; Windows uses `--stdio`. On stop it stops its provider children, cancels the work it owns with the `host shut down` reason, reports any worktrees it leaves in place, settles work owned by processes that already stopped, and releases the claim. While that claim is live, CLI and MCP clients exit 4 with the owner and takeover hint, and the TUI opens read-only with the same line. `baton host takeover --pid <pid>` asks for confirmation, replaces the claim, stops provider children recorded under the old host, settles open work, and releases the store for a new host.
- `/skills` lists skills from the Claude and Codex skill directories. Enter inserts `$name`; a Claude thread rewrites the final payload to `/name`.
- `/memory` lists Claude user, project, local, and rule files, Codex user and project instructions, and the auto-memory index and topics. Enter opens or creates a file in `VISUAL` or `EDITOR`. Auto-memory edits the user `autoMemoryEnabled` setting. The governed-memory CLI is `baton memories`; the `$memories` prompt token still expands governed notes.

## Verify every harness

From a source checkout, build Baton and run the harness matrix probe. It probes every advertised harness, records account limits as `blocked`, and checks Cursor's capability. With `--include-claude` it also runs a live Claude turn; by default it skips that turn:

```sh
cargo build --locked
uv run --no-project python .agents/skills/verify-baton/scripts/harness_matrix_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The probe needs an API key in the variable named by `--key-env`, and Python 3.10 or newer. `uv run --no-project python` supplies Python; a bare `python3` on macOS 3.9 exits with a version message. Add `--include-claude` only when you approve a live Claude subscription turn.

## Troubleshooting

- `baton doctor` names a missing harness binary and the next step. Install the CLI or put it on `PATH`.
- `/auth` says `missing <VARIABLE>`. Set that variable in the environment or `config.json`; Claude and Codex settings also work. Then reopen `/auth`.
- `/auth` says `cannot read config`. Fix `~/.baton/config.json`; the picker did not overwrite it.
- Cursor rejects a custom endpoint. Remove the cursor `apiBaseUrl` and use `cursor-agent login`.
- A turn fails auth after the banner looks ready. The harness can still reject an unentitled model or expired login. Run the provider's login command again.
- `/worktree` refuses a dirty checkout. Commit or stash first, or use `/fork` to stay in the current checkout.
- A run stays `running` after its process stopped. `baton runs settle <run-id> --status failed` records a final status. `completed` is refused while a task or turn is open, and `--force` settles a run whose recorded owner still looks alive.

## Next

Return to the [README](README.md) for the command surface, or read [Install Baton](install.md) for upgrades and uninstall.
