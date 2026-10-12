# baton

One terminal CLI for every coding agent harness. Same thread, any model, native prompt caching.

Start a thread with codex and gpt-6.1-sol. Hit a wall. Run `/switch claude claude-opus-5-5` and keep going in the same thread with Claude Code holding the context. Switch back later and the Codex session, and its provider-side prompt cache, resumes where you left off.

baton is a terminal-first reimagining of [t3code](https://github.com/pingdotgg/t3code). Adapter protocol knowledge and the native session scanner are ported from t3code (MIT).

## What it does

- **One CLI, many harnesses.** Claude Code and Codex today, driven through their supported programmatic surfaces (`@anthropic-ai/claude-agent-sdk`, `codex app-server`).
- **The thread is the product.** Switching harness or model mid-thread is a first-class operation with a token-budgeted context handoff.
- **High prompt cache hit rates.** baton never replays history into a harness. Each harness keeps its own native session, so its provider KV cache stays warm. The cache hit rate is always visible in the status bar.
- **Reads the same context jsonl.** `baton import` discovers existing native sessions (`~/.claude/projects/**/*.jsonl`, `~/.codex/sessions/**/rollout-*.jsonl`) and imports them as resumable baton threads.
- **Checkpoints.** Every turn ends with a hidden git ref capturing the working tree. Diff and restore without touching HEAD or your index.
- **Subscription or API billing.** Each provider runs on your harness login (subscription rate) or an API key, switchable per provider.

## Install

Requires Node >= 22.5 (uses the built-in `node:sqlite`), plus the harness CLIs you want to drive (`claude`, `codex`).

```
npm install -g .   # from this repo
baton
```

Development: `npm run dev` (tsx), `npm run build`, `npm test`, `npm run typecheck`.

## Usage

```
baton                    # open the TUI on the latest thread in this directory
baton new [title]        # create a thread
baton threads            # list threads here
baton import [--dry-run] # import recent claude/codex sessions as threads
baton usage              # token usage and cache hit rates
baton auth claude api    # switch a provider to API-key billing
baton diff <id> [turn]   # checkpoint diff against the working tree
baton restore <id> <turn>
```

In the TUI: `/help /switch /model /mode /auth /new /threads /open /usage /checkpoints /diff /quit`.

Approvals arrive inline (`y` allow, `a` allow for session, `n` deny). `ctrl+c` interrupts a running turn, press it twice to exit.

## How context and caching work

Each provider keeps its own native session per baton thread. On every turn only the new user message is sent; the harness resumes its own session, which is exactly what keeps provider prompt caches warm. When a provider serves a thread for the first time, baton prefixes a deterministic, budget-capped handoff (recent messages plus git status) to the first user message.

OpenAI and Anthropic caches are separate systems: switching harnesses means a cold cache for the new one, and switching back re-warms within that provider's cache TTL. The cache badge in the status bar (green/yellow/red) makes this cost visible.

## Billing modes

`baton auth <provider> <subscription|api>` (or `/auth` in the TUI) selects per provider:

- `subscription`: the harness uses its existing CLI login (Claude Pro/Max, ChatGPT plan).
- `api`: baton injects `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` into the harness subprocess. Keys come from the environment or `~/.baton/config.json`.

Switching billing mode usually means a different provider account, so caches do not carry across modes.

## Storage

`~/.baton/baton.sqlite` (threads, provider sessions, message mirror, checkpoints, usage) and `~/.baton/config.json`. `BATON_HOME` relocates everything.

## License

MIT.
