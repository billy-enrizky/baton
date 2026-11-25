# Billing mode picker

## Sub-features
- `/auth` opens a scrollable picker with a subscription row and an API row for every harness in `PROVIDERS`.
- An API row names the configured key variable and whether it resolves, for example `key CODEX_ICA_API_KEY` or `missing XAI_API_KEY`.
- The current mode carries a `●` marker, only the selected row carries `❯`, and the header always shows the selected position and the total, for example `1/12`.
- Enter applies a valid choice and closes the picker. A missing API key or a malformed `config.json` keeps the picker open and names the exact variable or parse error.
- `/auth <provider>` opens a two-row picker for that provider with the current mode marked. `/auth <provider> subscription|api` writes the same mode without opening the picker. Completion lists all twelve choices.
- The completion list paints one cursor. On an untouched `/auth ` list, Enter opens the bare picker; after Up or Down, Enter completes the highlighted row instead of running the bare command.
- `/auth` and `/auth <provider>` stay readable while work is running, but a write refuses until it ends. `/auth <provider> <mode> extra` is a usage error rather than a silent partial write.
- `ProviderAuthFile` and `EffectiveProviderConfig` have a manual `Debug` that prints `api_key` as `[redacted]`. Serialization still writes the real value to `config.json`, where it belongs.

## Problem statement

The picker had twelve rows but a fixed nine-line box. Grok, OpenCode, and Antigravity were hidden, and the selection could move off-screen with no position indicator. The picker showed no key readiness, the usage text named only Claude and Codex, and the write path used the lenient config loader, so a malformed `config.json` could be replaced by an empty file.

## Goals and non-goals

Goals: one view that lists every provider and mode, names the key variable, refuses unavailable API mode without losing the picker, and never overwrites a malformed config. The selected row must stay visible for all twelve choices. The route into the picker must honor both an explicit completion selection and the bare command, and it must not silently change the billing source while a turn or orchestration is live. The CLI and TUI must share the strict read-modify-write behavior and the same provider list.

Non-goals: provider login, named profiles, endpoint editing, key entry, keychain integration, cost estimation, or changing the harness-native environment variable.

## Proposed architecture

`auth_choices` yields the twelve `(provider, mode)` pairs from `PROVIDERS`. The overlay height is `min(12 + 3, terminal height - transcript minimum - composer - notice - status)`, and `draw_auth_picker` windows that many rows around the selection. A terminal with room shows every choice; a short one scrolls with the selected row visible. `apply_auth_mode` is the TUI write path. It refuses while a turn or orchestration is live, resolves the effective config, refuses API mode when no key resolves, loads `config.json` with `load_config_file_strict`, writes the mode, and invalidates the banner cache. The command and the picker Enter both call it. The `/auth` handler rejects more than two arguments, and the composer completes the arrow-selected row before considering the bare command complete. The CLI `baton auth <provider> <mode>` uses the same strict loader, and its unknown-provider error names every built-in.

`/auth <provider>` opens the scoped picker and writes nothing until a row is chosen. Completion derives its choices from the same provider list, and the usage notice names every provider.

## Alternatives considered

- A fixed twelve-line box was rejected because short terminals would clip it. The picker fills the height the terminal actually has, so a tall terminal shows all twelve choices and a short one scrolls.
- Showing only providers with keys was rejected because the user needs to see what is missing and where it comes from.
- Writing the mode before the key check was rejected because it leaves a harness in API mode with no credential.
- The lenient config loader was rejected because config may contain hand-edited keys. A parse error is reported and the file is left untouched.
- Entering a key in the picker was deferred. The picker names the variable so the user can set it in the environment, the harness config, or `config.json`.
- Applying billing changes to an already-running process was rejected because the turn already resolved its auth environment. The command asks the user to interrupt first and change it before the next turn.
- A single guard in the TUI command handler was rejected because the picker Enter bypassed it. The write helper carries the guard so command and picker share it.
- The CLI using the lenient loader was rejected because a malformed config would be replaced. Both surfaces use the strict loader.

## Verification paths

### Unit
```sh
cargo test --locked --lib auth
cargo test --locked --lib slash_suggestions_paint_one_cursor
cargo test --locked --test harness auth_cli_keeps_a_malformed_config_and_names_every_provider
```

The tests prove that a tall terminal shows every provider and a short one scrolls with the selected row visible, only one row carries the selection cursor, a keyed API choice writes `config.json` and closes the picker, a missing key keeps the picker open with the variable name, a malformed config is refused without a write, and `/auth <provider>` opens the scoped picker with the current mode marked while leaving `config.json` byte-identical. `arrow_selected_auth_completion_completes_instead_of_running_bare_auth`, `slash_suggestions_paint_one_cursor`, and `auth_refuses_a_running_turn_and_extra_arguments` cover the completion route and guards. `auth_cli_keeps_a_malformed_config_and_names_every_provider` covers the CLI write and usage list.

### PTY
```sh
uv run --no-project python .agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence/auth-probe
```

The probe filters `/auth antigravity `, requires both billing choices and one cursor, then presses Down and Enter and requires the composer box itself to contain `/auth antigravity api`, with no bare picker opened. It clears the line, opens `/auth`, requires `billing mode · 1/12`, the keyed Codex row, `antigravity api` without scrolling, and the apply hint, wraps to `12/12`, applies `codex api`, checks the persisted mode, runs `/auth codex` and requires the scoped `codex billing` picker with the api row marked and an unchanged config, selects `grok api` without a key, and requires the picker to stay open with `no API key for grok` and the missing XAI key row.

## Risks and mitigations

- A key can be set in `config.json`, the harness config, or the environment. The picker reads the resolved value and names the configured source variable, so it does not guess where a missing key should live.
- A live turn already resolved its auth environment. The write helper refuses until the turn and any orchestration end, so the banner and the child process cannot disagree. Read-only `/auth` queries still work.
- `load_config_file_strict` rejects malformed JSON. The picker stays open so the user can fix the file instead of losing its contents.
- `config.json` contains plaintext keys by design and is written with owner-only permissions (0600) after the directory is created with 0700. The picker and notices print only variable names and presence, never values. `ProviderAuthFile` and `EffectiveProviderConfig` redact keys in `Debug`; only serialization writes the real key.
