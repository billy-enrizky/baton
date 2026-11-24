# Billing mode picker

## Sub-features
- `/auth` opens a scrollable picker with a subscription row and an API row for every harness in `PROVIDERS`.
- An API row names the configured key variable and whether it resolves, for example `key CODEX_ICA_API_KEY` or `missing XAI_API_KEY`.
- The current mode carries a `●` marker, only the selected row carries `❯`, and the header shows the position when the list is longer than the window.
- Enter applies a valid choice and closes the picker. A missing API key or a malformed `config.json` keeps the picker open and names the exact variable or parse error.
- `/auth <provider>` prints the resolved auth for that provider. `/auth <provider> subscription|api` writes the same mode without opening the picker. Completion lists all twelve choices.

## Problem statement

The picker had twelve rows but a fixed nine-line box. Grok, OpenCode, and Antigravity were hidden, and the selection could move off-screen with no position indicator. The picker showed no key readiness, the usage text named only Claude and Codex, and the write path used the lenient config loader, so a malformed `config.json` could be replaced by an empty file.

## Goals and non-goals

Goals: one view that lists every provider and mode, names the key variable, refuses unavailable API mode without losing the picker, and never overwrites a malformed config. The selected row must stay visible for all twelve choices.

Non-goals: provider login, named profiles, endpoint editing, key entry, keychain integration, cost estimation, or changing the harness-native environment variable.

## Proposed architecture

`auth_choices` yields the twelve `(provider, mode)` pairs from `PROVIDERS`. `draw_auth_picker` windows `AUTH_PICKER_ROWS` rows around the selection and appends the position only when the list is longer than the window. `apply_auth_mode` is the only write path. It resolves the effective config, refuses API mode when no key resolves, loads `config.json` with `load_config_file_strict`, writes the mode, and invalidates the banner cache. The command and the Enter key both call it.

`/auth <provider>` remains read-only and uses `describe_auth`. Completion derives its choices from the same provider list, and the usage notice names every provider.

## Alternatives considered

- A full twelve-line box was rejected because short terminals would clip it. A window matches the model, memory, and skill pickers.
- Showing only providers with keys was rejected because the user needs to see what is missing and where it comes from.
- Writing the mode before the key check was rejected because it leaves a harness in API mode with no credential.
- The lenient config loader was rejected because config may contain hand-edited keys. A parse error is reported and the file is left untouched.
- Entering a key in the picker was deferred. The picker names the variable so the user can set it in the environment, the harness config, or `config.json`.

## Verification paths

### Unit
```sh
cargo test --locked --lib auth_picker
```

The tests prove that the first and last windows show every provider, only one row carries the selection cursor, a keyed API choice writes `config.json` and closes the picker, a missing key keeps the picker open with the variable name, and a malformed config is refused without a write.

### PTY
```sh
uv run --no-project python .agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence/auth-probe
```

The probe filters `/auth antigravity `, opens `/auth`, requires `billing mode · 1/12`, the key and missing rows, and the apply hint, wraps to `12/12`, applies `codex api`, checks the persisted mode, selects `grok api` without a key, and requires the picker to stay open with `no API key for grok`.

## Risks and mitigations

- A key can be set in `config.json`, the harness config, or the environment. The picker reads the resolved value and names the configured source variable, so it does not guess where a missing key should live.
- Changing a mode affects new turns but not an already-running turn. The notice says which provider and mode changed.
- `load_config_file_strict` rejects malformed JSON. The picker stays open so the user can fix the file instead of losing its contents.
