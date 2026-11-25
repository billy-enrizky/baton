# Billing mode picker

## Sub-features
- `/auth` opens a scrollable picker with a subscription row and an API row for every harness in `PROVIDERS`.
- An API row names the configured key variable and whether it resolves, for example `key CODEX_ICA_API_KEY` or `missing XAI_API_KEY`.
- The current mode carries a `●` marker, only the selected row carries `❯`, and the header always shows the selected position and the total, for example `1/12`.
- Enter applies a valid choice and closes the picker. A missing API key or a malformed `config.json` keeps the picker open and names the exact variable or parse error.
- `/auth <provider>` opens a two-row picker for that provider with the current mode marked. `/auth <provider> subscription|api` writes the same mode without opening the picker. Completion lists all twelve choices.
- `ProviderAuthFile` and `EffectiveProviderConfig` have a manual `Debug` that prints `api_key` as `[redacted]`. Serialization still writes the real value to `config.json`, where it belongs.

## Problem statement

The picker had twelve rows but a fixed nine-line box. Grok, OpenCode, and Antigravity were hidden, and the selection could move off-screen with no position indicator. The picker showed no key readiness, the usage text named only Claude and Codex, and the write path used the lenient config loader, so a malformed `config.json` could be replaced by an empty file.

## Goals and non-goals

Goals: one view that lists every provider and mode, names the key variable, refuses unavailable API mode without losing the picker, and never overwrites a malformed config. The selected row must stay visible for all twelve choices.

Non-goals: provider login, named profiles, endpoint editing, key entry, keychain integration, cost estimation, or changing the harness-native environment variable.

## Proposed architecture

`auth_choices` yields the twelve `(provider, mode)` pairs from `PROVIDERS`. The overlay height is `min(12 + 3, terminal height - transcript minimum - composer - notice - status)`, and `draw_auth_picker` windows that many rows around the selection. A terminal with room shows every choice; a short one scrolls with the selected row visible. `apply_auth_mode` is the only write path. It resolves the effective config, refuses API mode when no key resolves, loads `config.json` with `load_config_file_strict`, writes the mode, and invalidates the banner cache. The command and the Enter key both call it.

`/auth <provider>` opens the scoped picker and writes nothing until a row is chosen. Completion derives its choices from the same provider list, and the usage notice names every provider.

## Alternatives considered

- A fixed twelve-line box was rejected because short terminals would clip it. The picker fills the height the terminal actually has, so a tall terminal shows all twelve choices and a short one scrolls.
- Showing only providers with keys was rejected because the user needs to see what is missing and where it comes from.
- Writing the mode before the key check was rejected because it leaves a harness in API mode with no credential.
- The lenient config loader was rejected because config may contain hand-edited keys. A parse error is reported and the file is left untouched.
- Entering a key in the picker was deferred. The picker names the variable so the user can set it in the environment, the harness config, or `config.json`.

## Verification paths

### Unit
```sh
cargo test --locked --lib -- auth_picker auth_query switch_query
```

The tests prove that a tall terminal shows every provider and a short one scrolls with the selected row visible, only one row carries the selection cursor, a keyed API choice writes `config.json` and closes the picker, a missing key keeps the picker open with the variable name, a malformed config is refused without a write, and `/auth <provider>` opens the scoped picker with the current mode marked while leaving `config.json` byte-identical.

### PTY
```sh
uv run --no-project python .agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence/auth-probe
```

The probe filters `/auth antigravity `, opens `/auth`, requires `billing mode · 1/12`, the key and missing rows, `antigravity api` without scrolling, and the apply hint, wraps to `12/12`, applies `codex api`, checks the persisted mode, runs `/auth codex` and requires the scoped `codex billing` picker with the api row marked and an unchanged config, selects `grok api` without a key, and requires the picker to stay open with `no API key for grok`.

## Risks and mitigations

- A key can be set in `config.json`, the harness config, or the environment. The picker reads the resolved value and names the configured source variable, so it does not guess where a missing key should live.
- Changing a mode affects new turns but not an already-running turn. The notice says which provider and mode changed.
- `load_config_file_strict` rejects malformed JSON. The picker stays open so the user can fix the file instead of losing its contents.
