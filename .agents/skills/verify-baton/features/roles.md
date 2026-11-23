# Roles

## Sub-features
- Shows the resolved orchestrator, planner, implementor, and reviewer roles.
- Defaults to Claude `claude-opus-5-5` for orchestrator and planner, Codex `gpt-6.2-sol` for implementor, and Codex `gpt-6.2-astra` for reviewer.
- Reads known-role prompt overrides from `~/.baton/roles/<role>.md` and model settings from `~/.baton/roles.json`.
- Requires provider and model together in an override; effort is rejected for providers that do not support it.
- Parses and validates JSON hook declarations. Hook execution is the next slice.
- `/roles` shows the four rows and their detail; `baton roles --json` returns model, effort, prompt source/path, and parsed hook declarations.

## Problem statement

Role behavior was implicit in one coordinator prompt and one worker preamble. Users could not inspect model ownership, provide role-specific markdown, or see hook declarations before a run.

## Goals and non-goals

Goals: one validated role registry, explicit defaults, user prompt and model overrides, a TUI submenu, a machine-readable CLI, and fail-closed malformed configuration.

Non-goals in this slice: executing hooks, reviewer gating, mutating role files from the TUI, project-scoped roles, and changing existing orchestration prompts until the next slice wires the registry into planning and workers.

## Approach

`src/roles.rs` loads optional `roles.json`, validates every role and hook declaration, resolves provider/model/effort, and overlays optional markdown prompts. A missing file uses embedded defaults. A malformed file errors instead of silently falling back.

## Alternatives considered

- One larger coordinator prompt was rejected because per-role model and prompt ownership cannot be audited.
- A plugin host first was rejected because it adds a trust and lifecycle surface before basic roles work.
- Silent fallback on malformed JSON was rejected because it would hide a user's intended model or prompt override.

## Risks and mitigations

- Hooks will eventually execute trusted user code. This slice parses only command argv, timeout, and known event names; it does not run them. A later execution slice must define consent, environment clearing, process limits, and failure policy.
- Model names can drift. Defaults are explicit and the Codex catalog includes both role models.
- User prompts may be empty. The next wiring slice must reject an empty resolved prompt before a provider turn.
- Project role overrides need a trust decision and are deliberately deferred.

## How to get to it (user POV)
Type `/roles` in the TUI or run `baton roles`. Enter on a row shows the resolved model, config source, prompt source, and prompt path. `baton roles --json` returns the same registry for agents.

## Driving it with the PTY harness
```sh
.agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The slash probe seeds an all-Codex role override, opens `/roles`, requires all four rows and the override model, and checks detail-only `config:` and `prompt:` fields after Enter.

## Gotchas
- This slice does not run hooks or inject role prompts into orchestration yet.
- `~/.baton/roles.json` is read as strict JSON. A typo fails the command instead of using defaults.
- A role markdown file overrides only that role's prompt; provider and model stay in JSON.
- Routed model IDs are accepted as safe provider tokens even when they are not in the static catalog. The provider preflight still decides whether the harness can start.
