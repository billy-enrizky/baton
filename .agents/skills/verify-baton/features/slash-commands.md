# Slash command choices

## Sub-features
- Typing `/` lists every command in the completion registry, including `/help` and the `/exit` alias.
- Typing a command followed by a space lists valid argument choices, or its argument syntax when the command takes free-form text.
- Partial command and argument text filters the same list.
- Tab completes the selected choice; Enter runs the line when it already names something the handler acts on and completes it otherwise.
- `/auth <provider>` and `/switch <provider>` are complete commands. Enter runs them, so a billing or harness query is never rewritten into `/auth <provider> subscription` or `/switch <provider> <model>`.
- Every command answers after a space with a completion list, an argument syntax hint, or an explicit no-arguments hint. A command with no choices says `enter runs it` instead of claiming a choice will complete.
- The `/auth` picker sizes to the terminal: it shows all twelve rows when there is room and keeps the selected row visible when there is not.
- `/effort <value>` accepts only the six offered levels. Any other token opens the effort picker and sets a notice naming the valid levels, so a typo cannot be stored as a thread effort. `normalize_reasoning_effort` stays lenient for role and harness configuration, which may carry provider-specific values.
- Free-form commands still accept typed text.

## How to get to it (user POV)
Open the TUI and type `/mode `, `/model`, `/switch `, `/effort `, `/auth `, `/mcp `, `/orchestrate `, `/git `, or `/runs `. The `/git` list includes Status, Commit all changes, Commit staged changes, Push current branch, Checks, Open pull request, and Cancel.

## Driving it with the PTY harness
```sh
.agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The probe types `/find ` and requires its argument syntax, then types `/mode `, requires all four permission choices and the completion hint, presses Tab to accept `default`, presses Enter, and requires the resulting `permission mode: default` notice. It then types `/model`, requires the model picker header, filter hint, and Enter/Escape affordances, requires a model count of at least two and at least two model option rows, and closes it with Escape. It opens `/skills` against isolated skill trees with `SKILL.md` files, requires the count, `enter insert`, both source rows, selects the Codex row, requires the insertion notice plus source and orchestrate warnings, and checks the composer line for `$codex-demo `. It opens `/settings`, requires both global rows, the marked options row, the read-only harness row, `enter apply`, and `esc`, writes `single` and the bell on, checks both notices, and verifies the persisted `config.json`. It types `/files ` and requires the `files [--ignored] [query]` argument syntax, types `/files --` and requires the `/files --ignored` completion, then runs `/files --ignored env` and requires the `files --ignored matching "env"` heading, the `.env` row, and the ignored-files warning. It runs `/files pmod rs:12`, requires the `files matching "pmod rs"` heading, the `line 12` target, the `src/parser/mod.rs` row found by two independent subsequence tokens, and the `enter insert`, `ctrl+o open`, and `esc close` affordances, presses Enter, and requires the `inserted src/parser/mod.rs:12` notice. It reopens `/files ` and requires the picked file as the first recent row. It reopens `/files pmod rs:12`, presses Ctrl+O with a fake Code executable, proves `--goto <path>:12` reached the editor, and requires the `opened src/parser/mod.rs:12` notice after the TUI restored. It types `/grep ` and requires the grep usage, types `/grep --` and requires the `--case`, `--case-sensitive`, `--word`, and `--regex` completions, then runs `/grep parse`, requires the `grep "parse"` heading, the `src/parser/mod.rs:1` row, and the `ctrl+o open`, `enter insert`, and `esc close` affordances, presses Enter, and requires the `inserted src/parser/mod.rs:1` notice. It reopens `/grep parse`, presses Ctrl+O with the fake Code executable, proves `--goto <path>:1` reached the editor, and requires the `opened src/parser/mod.rs:1` notice after the TUI restored. It opens `/pin`, toggles a pinned row, closes that picker, creates and pins a second thread, then runs a real `/pin move <second-id> down`. It requires the moved notice, refreshed picker, and swapped row order, closes the picker, runs the same move again, requires the last-position boundary notice, reopens the picker, sends Ctrl+Down, requires the moved notice and refreshed row order while the picker stays open, and requires a clean `/quit`.

The probe filters `/auth antigravity ` and requires both billing choices. It opens `/auth`, requires the `1/12` position, the key row, the missing-key row, the `antigravity api` row that a tall terminal now shows without scrolling, and the apply hint, wraps to `12/12`, applies `codex api`, checks the persisted mode, runs `/auth codex`, requires the `codex billing:` notice that names the provider and proves the query did not rewrite `config.json`, then selects `grok api` without a key, and requires the picker to stay open with `no API key for grok` before Escape.

The probe opens `/git`, selects Checks against a fake `gh`, requires the PR header and pass/pending rows, then creates and pushes `feat/pr`, runs `/git pr`, requires the confirmation with `example/repo`, `feat/pr -> origin/main`, the generated title and body row, presses Enter, and requires the `opened pull request #43` notice with the fake URL. The fake `gh` rejects a pull request lookup or creation that omits the repository binding.

The probe executes `/git checks` and `/git status` directly from the composer, requiring each overlay and its close hint. The single-cursor `/auth` invariant is covered by the TUI unit test; the PTY proof covers the 12-row window, key readiness, missing-key refusal, and persistence.

The full sweep runs every command in one isolated session and requires each
command's stable surface, no panic or unknown-command text, and a clean `/exit`.
Before running each command it types the command and a space and requires a
completion list, an argument hint, or the no-arguments hint, so a command that
offers nothing after a space fails the sweep:

```sh
uv run --no-project python .agents/skills/verify-baton/scripts/slash_sweep.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

`/quit` is covered by the slash probe.

Both the probe and the sweep record the absolute binary path, its SHA-256, `baton --version`, and the repository source commit in `summary.json`, so an installed-binary claim identifies the exact artifact that ran.

The retry probe owns `/retry ` completion because it needs failed turns. It
requires `/retry 1` and `/retry 10` with their failed-turn descriptions, then
types the exact `/retry 1` form to prove it wins over the longer completion
prefix, opens the picker, and completes the retry through the fake harness.

Installed-binary proof: the same slash probe passed against the v0.4.0 default curl install and the v0.4.0 Homebrew binary. The stable summaries live in the gitignored `v0.4.0-release/curl/summary.json` and `v0.4.0-release/brew/summary.json` evidence directories.

## Gotchas
- `/model` merges the live list with the static known catalog, so a short or degraded live probe still shows every known model.
- The displayed permission banner uses the provider default when the explicit value is `default`. The command notice reports the selected value; the banner reports the effective provider mode.
- `/open` choices come from the current store. A temporary home has no threads, so it may show no choices until a thread is created.
- Turn-number choices for `/review`, `/commands`, and `/fork --turn` come from the current thread history.
- `/mcp remove`, `/runs`, and `/copy` choices come from the MCP registry, current store, and conversation.
- `/pin move ` offers both directions only for pinned threads in the current project. The probe enters the full ID from the picker row because the move notice, refreshed order, and boundary result are part of the proof.
- `/git` completes status, commit, commit --staged, push, checks, and pr. The unit tests execute both the picker Checks row and exact `/git checks`. The probe commits the isolated repository, checks the real git subject and exact file list, pushes to a local bare remote, checks its head, opens Checks against a fake `gh`, and requires the PR header and the pass/pending rows. It then pushes `feat/pr`; `/git pr` requires the repository, base, title, body, and HEAD preview, and Enter requires the created PR notice and URL.
- The sweep forbids `unknown /git action`, so the nested-command regression that produced it cannot pass unnoticed.
- Enter used to replace any line that was a strict prefix of a completion, which turned the `/auth codex` query into `/auth codex subscription` and changed billing mode on the next Enter. `is_complete_command` now marks `/auth <provider>` and `/switch <provider>` as complete, and the unit tests assert that Enter clears the input and that `config.json` is unchanged.
- A command whose usage string was missing showed nothing after a space. `command_usage` now covers every entry in `SLASH_COMMANDS`, and `every_slash_command_offers_a_hint_after_a_space` enforces that invariant.
