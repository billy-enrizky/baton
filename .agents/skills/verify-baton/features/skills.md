# Skills picker

## Problem

A terminal user can see installed Claude Code and Codex skills, but Baton only lists them. The user still has to remember the provider syntax and retype it. Claude also expands a skill only when `/name` starts the message, and Baton treats every leading slash as one of its own commands. A naive `/name` insertion would run `/review` instead of the skill. The old picker also marked every row as selected and could not reach a skill past the eighth row.

## Goals

- Insert a selected skill into the composer with one key.
- Use the portable token model: insert `$name` in the composer, then rewrite the final Claude payload to `/name` only when the harness is Claude.
- Keep the selected row visible past eight skills, sanitize row labels, and reject names that cannot form a provider token.
- Keep the stored transcript in the user's `$name` form while the provider payload follows its native syntax.

## Non-goals

- Installing, updating, or removing skills. Baton does not own an install workflow; it reads existing skill directories.
- A skill marketplace, metadata editor, or per-skill enable switch.
- Skill execution inside the orchestration coordinator.
- Rewriting a Codex-only skill for Claude or a Claude-only skill for Codex.

## Proposed architecture

`src/sync.rs::scan_skill_roots` reads the global Claude and Codex skill roots, prefers Claude on duplicate names, and requires a `SKILL.md` or `skill.md` file. `skill_description` reads only the first 4 KB and the first 60 frontmatter lines, and accepts a single-line scalar. A container directory such as Codex's `.system` is scanned one level deep, so its built-in skills are listed instead of the container itself. `scan_skills_in(cwd)` adds the project roots `.claude/skills` and `.codex/skills`, and a project skill shadows a global skill with the same name.

`src/tui/mod.rs` maps those rows into `Overlay::SkillsPicker`. `insert_skill` validates the name, inserts `$name ` at a token boundary, closes the picker, and reports the source. It adds a warning when the source differs from the current harness or when the thread is in orchestrate mode.

`src/skill_dispatch.rs::prompt_for_provider_from_in` runs after the handoff or goal wrapper is built. For Claude, it finds the last known `$name` mention using the project and global scan for the turn's working directory, moves it to the start as `/name`, and keeps the rest of the payload as arguments. Other providers keep the prompt unchanged. The stored message remains the original `$name` text.

## Sub-features

- The Claude root is `CLAUDE_CONFIG_DIR/skills` when that variable is set, otherwise `~/.claude/skills`. The Codex root is `CODEX_HOME/skills` when set, otherwise `~/.codex/skills`.
- The project roots are `<cwd>/.claude/skills` and `<cwd>/.codex/skills`. A project skill shadows a global skill of the same name, and Claude still wins over Codex at the same level.
- Each root lists directories with `SKILL.md` or `skill.md`. A directory without either file is treated as a container and its immediate children are checked once. Deeper nesting is not scanned.
- Rows show `name (source)` and append ` · description` when the skill's frontmatter has a single-line `description:`. The description is capped at 160 characters at scan time and truncated to the rendered row budget. Duplicate names keep the Claude row and hide the Codex duplicate. Empty discovery leaves the overlay closed and reports `no skills found in the configured Claude or Codex skill directories`.
- `/skills` opens the picker. Enter inserts `$name ` at the caret, adding a leading space when the preceding character is not whitespace. The picker closes and reports `inserted $name (source)`.
- The validator requires at least one ASCII letter, permits ASCII letters, digits, `:`, `_`, and `-`, and rejects `memories` because `$memories` is a Baton memory token. A rejected name leaves the picker open.
- The notice adds `switch to <source> before sending` when the row source differs from the current harness, and `/orchestrate off before sending` in orchestrate mode. It does not silently claim the current harness will use a different harness's skill.
- Claude receives one native skill command per payload. The last known `$name` mention becomes `/name` at the start; all other text, including a goal prefix or handoff, remains as arguments. Earlier skill mentions stay literal in the arguments.
- Claude dispatch ignores unknown names, mid-word mentions, punctuation immediately after the name, and names without an ASCII letter. That boundary prevents `$123` from becoming a command.
- The picker scrolls an eight-row window with Up and Down, wraps at both ends, and marks only the selected row with `❯`. Names are capped at 64 characters and sources at 16, with a trailing ellipsis when truncated.
- Row labels replace newline, tab, and terminal control characters before rendering. Long labels cannot add visual rows or push the selected row outside the window.

## How to get to it (user POV)

```text
/skills
Enter
Down
Enter
```

The first Enter opens the picker. Down selects the next source row. The second Enter inserts `$name ` into the composer. If the notice says `/orchestrate off before sending`, switch the thread to one harness before submitting. For a Claude skill, Baton converts the final provider payload from `$name` to `/name`.

## Driving it with the verification harness

```sh
.agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The probe creates isolated `CLAUDE_CONFIG_DIR` and `CODEX_HOME` trees with one skill each and a real `SKILL.md`. It requires `skills (2)`, `enter insert`, both source rows, selects the Codex row with Down, presses Enter, requires the insertion notice plus the source and orchestrate warnings, and checks the composer line itself for `$codex-demo `.

### Unit proof

```sh
cargo test --locked --lib tui::tests::skills_picker -- --nocapture
cargo test --locked --lib skill_dispatch::tests -- --nocapture
cargo test --locked --lib sync::tests::skill_roots -- --nocapture
cargo test --locked --lib sync::tests::project_skills -- --nocapture
cargo test --locked --lib sync::tests::skill_description -- --nocapture
cargo test --locked --lib tui::tests::skills_command_lists_project_skills -- --nocapture
cargo test --locked --lib tui::tests::skills_picker_shows_sanitized_descriptions -- --nocapture
```

### Coverage map

`skills_picker_inserts_skill_tokens_at_token_boundaries` proves a Codex token starts a new word at a mid-word caret and a Claude token keeps a trailing source label. `skills_picker_warns_when_source_or_mode_needs_switching` proves both warnings. `skills_picker_rejects_reserved_and_unsafe_names_and_keeps_the_picker_open` proves `$memories` refusal, control and newline rejection, one rendered line, and the picker staying open. `skills_picker_window_follows_selection_past_eight` proves the window, the single selected cursor, and Down wrapping from the last row to the first. `skills_picker_visible_window_fits_a_short_terminal` proves the selected row stays visible when the overlay is shorter than the default eight-row window. `skill_tokens_submit_as_messages_instead_of_slash_commands` proves `$name` reaches the engine as a normal message. `skill_dispatch::tests` proves Claude rewriting, goal-prefix placement, last-mention precedence, ignored boundaries, digit-only rejection, handoff-history exclusion, goal-text exclusion, and unchanged Codex prompts. `runtime::tests::claude_payload_uses_native_skill_syntax_and_keeps_the_transcript_token` proves the provider payload is `/review` while the stored user message stays `$review`. `sync::tests::skill_roots_scan_flat_and_container_directories` and `claude_skills_win_duplicate_names_and_codex_adds_the_rest` prove `SKILL.md` discovery, one-level container scanning, Claude precedence, and source labels. `sync::tests::project_skills_shadow_global_skills_and_keep_their_harness`, `a_project_codex_skill_is_listed_when_no_claude_skill_exists`, `skill_dispatch::tests::project_skills_are_known_from_the_project_cwd`, `project_skills_do_not_rewrite_codex_prompts`, and `tui::tests::skills_command_lists_project_skills` prove the project roots, their precedence, the Claude rewrite, and the `/skills` listing. `sync::tests::skill_descriptions_come_from_frontmatter`, `skill_description_is_capped_and_reads_the_lowercase_file`, and `tui::tests::skills_picker_shows_sanitized_descriptions` prove description parsing, the 160-character cap, the lowercase file, row rendering, truncation, and sanitization. The PTY probe now asserts `codex-demo (codex) · Run the demo checks`. The PTY probe proves the real picker, warning, and composer insertion.

## Alternatives

- Insert `/name` directly into the composer. Rejected because Baton treats every leading `/` as one of its own commands, so `/review` would open Baton's review overlay and an unknown skill would produce `unknown command`.
- Insert `/name` only after a hidden mode or bypass submit. Rejected because it adds a second composer grammar and still breaks when a goal or handoff prefix is prepended.
- Insert `$name` for every source and rewrite only the final Claude payload. Chosen because it keeps the stored transcript portable and survives goal and handoff wrappers because the rewrite runs after they are built.
- Prefix the existing composer text for Claude. Rejected because `/skills` runs as a command and clears the composer before the picker opens, and a later goal or handoff wrapper can still move a leading slash away from the start.
- Keep the picker display-only. Rejected because it leaves the user to remember the invocation syntax and was the gap this slice closes.

## Risks and mitigations

- Claude expands one skill per payload. The last known mention wins; earlier mentions remain literal arguments. The feature does not promise multiple native Claude skills in one message.
- A selected token belongs to its source harness. The row and notice name that source, and the notice warns when a switch is required. Baton does not silently rewrite a Claude-only skill for Codex or the reverse.
- New threads start in orchestrate mode. The coordinator may rewrite or route the goal, so the notice says `/orchestrate off before sending`; the feature does not claim native skill dispatch through orchestration.
- `$memories` is reserved by Baton memory expansion. The validator rejects a skill named `memories`, and the notice names the collision instead of reporting a false success.
- Skill discovery is synchronous and now reads the first 4 KB of each skill file for the description. A pathological local tree can still delay the TUI. The accepted local limit is a few hundred entries; move the scan to a background task if a real tree exceeds that or lives on a slow network mount.
- A directory without `SKILL.md` is not a skill. One level of container scanning covers `.system`; deeper custom nesting is not claimed.
- Duplicate names show one row, with Claude first. A same-named Codex skill cannot be selected from this picker.
- Names and sources come from the filesystem. Rows use a single-line sanitizer and truncation, and insertion uses a validated ASCII token rather than rendered text.

## Gotchas

- `/skills` is a list and insert surface, not an install command. It lists project skills for the current thread's working directory ahead of global skills, and shows a frontmatter description when one exists.
- The first Enter opens the picker; the second Enter inserts the selected row.
- The composer keeps `$name`; only the Claude provider payload becomes `/name`.
- An orchestrate-mode thread needs `/orchestrate off` before a skill can reach one harness.
- A selected row past eight entries requires Up or Down to bring it into the visible window.
- The PTY proof covers composer insertion, not a live provider skill execution. The dispatch rewrite is covered by Rust unit tests.
