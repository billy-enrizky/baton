# Slash command choices

## Sub-features
- Typing `/` lists every command in the completion registry, including `/help` and the `/exit` alias.
- Typing a command followed by a space lists valid argument choices, or its argument syntax when the command takes free-form text.
- Partial command and argument text filters the same list.
- Tab or Enter completes the selected choice; Enter on a complete choice runs it.
- Free-form commands still accept typed text.

## How to get to it (user POV)
Open the TUI and type `/mode `, `/model`, `/switch `, `/effort `, `/auth `, `/mcp `, `/orchestrate `, `/git `, or `/runs `.

## Driving it with the PTY harness
```sh
.agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The probe types `/find ` and requires its argument syntax, then types `/mode `, requires all four permission choices and the completion hint, presses Tab to accept `default`, presses Enter, and requires the resulting `permission mode: default` notice. It then types `/model`, requires the model picker header, filter hint, and Enter/Escape affordances, requires a model count of at least two and at least two model option rows, and closes it with Escape. It opens `/skills` against isolated skill trees with `SKILL.md` files, requires the count, `enter insert`, both source rows, selects the Codex row, requires the insertion notice plus source and orchestrate warnings, and checks the composer line for `$codex-demo `. It opens `/settings`, requires both global rows, the marked options row, the read-only harness row, `enter apply`, and `esc`, writes `single` and the bell on, checks both notices, and verifies the persisted `config.json`. It types `/files ` and requires the `files [--ignored] [query]` argument syntax, types `/files --` and requires the `/files --ignored` completion, then runs `/files --ignored env` and requires the `files --ignored matching "env"` heading, the `.env` row, and the ignored-files warning. It runs `/files pmod rs:12`, requires the `files matching "pmod rs"` heading, the `line 12` target, the `src/parser/mod.rs` row found by two independent subsequence tokens, and the `enter insert`, `ctrl+o open`, and `esc close` affordances, presses Enter, and requires the `inserted src/parser/mod.rs:12` notice. It reopens `/files ` and requires the picked file as the first recent row. It reopens `/files pmod rs:12`, presses Ctrl+O with a fake Code executable, proves `--goto <path>:12` reached the editor, and requires the `opened src/parser/mod.rs:12` notice after the TUI restored. It types `/grep ` and requires the grep usage, types `/grep --` and requires the `--case`, `--case-sensitive`, `--word`, and `--regex` completions, then runs `/grep parse`, requires the `grep "parse"` heading, the `src/parser/mod.rs:1` row, and the `ctrl+o open`, `enter insert`, and `esc close` affordances, presses Enter, and requires the `inserted src/parser/mod.rs:1` notice. It reopens `/grep parse`, presses Ctrl+O with the fake Code executable, proves `--goto <path>:1` reached the editor, and requires the `opened src/parser/mod.rs:1` notice after the TUI restored. It opens `/pin`, toggles a pinned row, closes that picker, creates and pins a second thread, then runs a real `/pin move <second-id> down`. It requires the moved notice, refreshed picker, and swapped row order, closes the picker, runs the same move again, requires the last-position boundary notice, reopens the picker, sends Ctrl+Down, requires the moved notice and refreshed row order while the picker stays open, and requires a clean `/quit`.

The retry probe owns `/retry ` completion because it needs failed turns. It
requires `/retry 1` and `/retry 10` with their failed-turn descriptions, then
types the exact `/retry 1` form to prove it wins over the longer completion
prefix, opens the picker, and completes the retry through the fake harness.

Installed-binary proof: the same slash probe passed against the v0.4.0 default curl install and the v0.4.0 Homebrew binary. The stable summaries live in the gitignored `v0.4.0-release/curl/summary.json` and `v0.4.0-release/brew/summary.json` evidence directories.

## Gotchas
- The displayed permission banner uses the provider default when the explicit value is `default`. The command notice reports the selected value; the banner reports the effective provider mode.
- `/open` choices come from the current store. A temporary home has no threads, so it may show no choices until a thread is created.
- Turn-number choices for `/review`, `/commands`, and `/fork --turn` come from the current thread history.
- `/mcp remove`, `/runs`, and `/copy` choices come from the MCP registry, current store, and conversation.
- `/pin move ` offers both directions only for pinned threads in the current project. The probe enters the full ID from the picker row because the move notice, refreshed order, and boundary result are part of the proof.
- `/git` completes status, commit, and commit --staged. The probe commits the isolated repository and checks the real git subject and exact file list.
