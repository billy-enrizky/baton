# Set up Baton

Baton drives harness CLIs that you install and authenticate yourself. This guide covers the first run, provider login, API billing, custom endpoints, roles, MCP, skills, memory, and common failures.

## What Baton needs from you

Baton needs no account of its own and sends no telemetry. It runs your harness CLIs with your credentials.

Required for a turn: one harness CLI on `PATH`, either signed in with `baton login <provider>` or pointed at an API key with `/auth <provider> api`. The tables below list the login command and the key variable for each harness.

Optional, only for the features that use them:

| Tool or setting | Needed for |
|---|---|
| Authenticated `gh` | `/git checks`, `/git pr`, `/task`, and pull request creation from an orchestration report. The rest of `/git` works without it. |
| `rg` | `/files` and `/grep`. Without it Baton falls back to `git grep` inside a repository. |
| `VISUAL` or `EDITOR` | Opening a file from `/files`, `/grep`, or `/memory`. |
| An MCP server | Whatever you add with `/mcp`; nothing is configured for you. |

Costs belong to the harness provider: a subscription for the harness login, or usage billed to the API key. A custom endpoint, including an internal gateway, carries that provider's terms. Baton adds no fee, no hosted service, and no background account.

## First run

```sh
baton
```

The TUI opens in the configured default session mode, which is orchestrate until you change it with `/settings` or `/orchestrate off`. Run `baton doctor` to see every harness binary, its auth mode, the Baton config files, database size, thread count, and tracked provider processes. Run `baton login <provider>` once for each harness you want to use.

## Provider login

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

Set the key in the environment, the harness config, or `~/.baton/config.json`, then choose the API row. A missing key keeps the picker open with the exact variable name. The CLI form is `baton auth <provider> api`; `/auth <provider>` prints the resolved mode and key source without changing anything.

Baton reads a provider-neutral variable when you set `apiKeyEnv`. The child still receives the harness-native variable:

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

Antigravity runs as `agy acp`. Its base URL is the endpoint root because `agy` appends `/v1beta`; do not add `/v1`.

Grok uses the same shape with the OpenAI-compatible `/v1` base. Baton spawns `grok agent stdio` and maps the base URL to `GROK_XAI_API_BASE_URL` and `GROK_MODELS_BASE_URL`:

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

Cursor runs as `cursor-agent acp` and rejects a custom endpoint. Use `cursor-agent login`.

OpenCode reads its own `opencode.json` for a custom provider. Baton does not map an OpenCode base URL. Keep the CLIs current with `grok update`, `cursor-agent update`, and `opencode upgrade`.

## Config file

`~/.baton/config.json` stores Baton overrides. Set `BATON_HOME` to use a different directory. The file is written with mode 0600. `/auth` and `/settings` refuse a malformed file instead of replacing it, so a hand-edit error never deletes your keys.

Useful top-level keys:

```json
{
  "defaultProvider": "claude",
  "defaultMode": "single",
  "notifications": { "bell": false },
  "defaultModels": { "claude": "claude-sonnet-5-5" },
  "codex": { "apiKeyEnv": "CODEX_ICA_API_KEY" },
  "antigravity": {
    "mode": "api",
    "apiKeyEnv": "CODEX_ICA_API_KEY",
    "apiBaseUrl": "https://api.servicesessentials.ibm.com"
  }
}
```

`defaultMode` accepts `orchestrate` or `single`. Provider keys use the provider name as the key. Unknown fields survive a write.

## Roles

Baton has four roles: orchestrator, planner, implementor, and reviewer. Defaults are Claude `claude-opus-5-5` for orchestrator and planner, Codex `gpt-6.2-sol` for implementor, and Codex `gpt-6.2-astra` for reviewer. A normal orchestration uses the planner role for its coordinator turn and the implementor role for matching worker tasks. `/roles` shows the resolved rows and Enter shows the provider, model, effort, prompt source, config source, and parsed hook events. `baton roles --json` exposes the same registry.

Override models in `~/.baton/roles.json`:

```json
{
  "schema_version": 1,
  "roles": {
    "orchestrator": { "provider": "claude", "model": "claude-opus-5-5" },
    "planner": { "provider": "claude", "model": "claude-opus-5-5" },
    "implementor": { "provider": "codex", "model": "gpt-6.2-sol" },
    "reviewer": { "provider": "codex", "model": "gpt-6.2-astra" }
  }
}
```

A role must set `provider` and `model` together. `effort` is accepted only for a provider that supports reasoning effort. Malformed JSON, a partial provider/model pair, an unknown provider, an unknown hook event, an empty hook command, or a timeout outside 1 to 300 seconds fails closed.

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

Known events are `before_plan`, `after_plan`, `before_implement`, `after_implement`, `before_review`, and `after_review`. Hook declarations are validated and shown by `/roles`; execution is not wired yet.

## MCP, skills, and memory

- `/mcp` lists, adds, removes, or syncs MCP servers. `baton mcp` exposes Baton state over stdio.
- `/skills` lists skills from the Claude and Codex skill directories. Enter inserts `$name`; a Claude thread rewrites the final payload to `/name`.
- `/memory` lists Claude user, project, local, and rule files, Codex user and project instructions, and the auto-memory index and topics. Enter opens or creates a file in `VISUAL` or `EDITOR`. Auto-memory edits the user `autoMemoryEnabled` setting. The governed-memory CLI is `baton memories`; the `$memories` prompt token still expands governed notes.

## Verify every harness

From a source checkout, the harness matrix probe records either a live turn or an explicit account block for each advertised harness:

```sh
uv run --no-project python .agents/skills/verify-baton/scripts/harness_matrix_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The default run skips a live Claude turn and checks Cursor's capability only. Add `--include-claude` only when you approve a live Claude subscription turn.

The probe requires Python 3.10 or newer. `uv run --no-project python` supplies it; a bare `python3` on macOS 3.9 exits with a version message.

## Troubleshooting

- `baton doctor` names a missing harness binary and the next step. Install the CLI or put it on `PATH`.
- `/auth` says `missing <VARIABLE>`. Set that variable in the environment, the harness config, or `config.json`, then reopen `/auth`.
- `/auth` says `cannot read config`. Fix `~/.baton/config.json`; the picker did not overwrite it.
- Cursor rejects a custom endpoint. Remove the cursor `apiBaseUrl` and use `cursor-agent login`.
- A turn fails auth after the banner looks ready. The harness can still reject an unentitled model or expired login. Run the provider's login command again.
- `/worktree` refuses a dirty checkout. Commit or stash first, or use `/fork` to stay in the current checkout.

## Next

Return to the [README](README.md) for the command surface, or read [Install Baton](install.md) for upgrades and uninstall.
