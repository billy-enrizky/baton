# Skills picker

## Problem

A terminal user can see installed Claude Code and Codex skills, but Baton only lists them. The user still has to remember the provider syntax and retype it, there is no way to take a global skill into the project where the work belongs, and there is no way to remove a project copy without leaving the TUI. Claude also expands a skill only when `/name` starts the message, and Baton treats every leading slash as one of its own commands. A naive `/name` insertion would run `/review` instead of the skill. The old picker also marked every row as selected and could not reach a skill past the eighth row.

## Goals

- Insert a selected skill into the composer with one key.
- Install a global skill into the project's matching harness root, update it from the global source, and remove the project copy after confirmation.
- Use the portable token model: insert `$name` in the composer, then rewrite the final Claude payload to `/name` only when the harness is Claude.
- Keep the selected row visible past eight skills, sanitize row labels, and reject names that cannot form a provider token.
- Keep the stored transcript in the user's `$name` form while the provider payload follows its native syntax.
- Refuse symlinks, non-regular files, oversized trees, unsafe destination roots, and accidental overwrites.

## Non-goals

- A remote skill marketplace, version pinning, or project-to-global publishing.
- Removing a global skill or publishing a project copy back to the global root.
- A metadata editor or per-skill enable switch.
- Skill execution inside the orchestration coordinator.
- Rewriting a Codex-only skill for Claude or a Claude-only skill for Codex.

## Proposed architecture

`src/sync.rs::scan_skill_roots` reads the global Claude and Codex skill roots, prefers Claude on duplicate names, and requires a `SKILL.md` or `skill.md` file. `skill_description` reads only the first 4 KB and the first 60 frontmatter lines, and accepts a single-line scalar. A container directory such as Codex's `.system` is scanned one level deep, so its built-in skills are listed instead of the container itself. `scan_skills_in(cwd)` adds the project roots `.claude/skills` and `.codex/skills`, and a project skill shadows a global skill with the same name.

`src/tui/mod.rs` maps those rows into `Overlay::SkillsPicker`, including the source path and whether the copy is project or global. `insert_skill` validates the name, inserts `$name ` at a token boundary, closes the picker, and reports the source. It adds a warning when the source differs from the current harness or when the thread is in orchestrate mode. `i` and `u` call `src/skills.rs::install_skill`, and `d` opens a confirmation before `src/skills.rs::remove_skill`.

`src/skills.rs` owns the install, update, and removal boundary. It validates the skill directory and name, refuses a project source and symlinked destination roots, copies the tree into a hidden staging directory with a file and byte cap, then renames it into place. Update first renames the existing directory to a hidden backup, swaps in the staging directory, and restores the backup if the swap fails. Removal renames the project copy to a hidden trash name, deletes it, and restores it if deletion fails. Internal `.baton-staging-*`, `.baton-backup-*`, and `.baton-trash-*` directories are hidden from discovery. Each transient name also carries the skill name after a 32-character id, so `reconcile_project_skills` can tell which project skill an interrupted run belonged to and repair it.

`src/skill_dispatch.rs::prompt_for_provider_from_in` runs after the handoff or goal wrapper is built. For Claude, it finds the last known `$name` mention using the project and global scan for the turn's working directory, moves it to the start as `/name`, and keeps the rest of the payload as arguments. Other providers keep the prompt unchanged. The stored message remains the original `$name` text.

## Sub-features

- The Claude root is `CLAUDE_CONFIG_DIR/skills` when that variable is set, otherwise `~/.claude/skills`. The Codex root is `CODEX_HOME/skills` when set, otherwise `~/.codex/skills`.
- The project roots are `<cwd>/.claude/skills` and `<cwd>/.codex/skills`. A project skill shadows a global skill of the same name, and Claude still wins over Codex at the same level.
- Each root lists directories with `SKILL.md` or `skill.md`. A directory without either file is treated as a container and its immediate children are checked once. Deeper nesting is not scanned.
- Rows show `name (source · project|global)` and append ` · description` when the skill's frontmatter has a single-line `description:`. The description is capped at 160 characters at scan time and truncated to the rendered row budget. Duplicate names keep the Claude row and hide the Codex duplicate. Empty discovery leaves the overlay closed and reports `no skills found in the configured Claude or Codex skill directories`.
- `/skills` opens the picker. Enter inserts `$name ` at the caret, adding a leading space when the preceding character is not whitespace. The picker closes and reports `inserted $name (source)`.
- `i` copies the selected global skill into `<cwd>/.claude/skills/<name>` or `<cwd>/.codex/skills/<name>` and reports `installed $name into <path>`. It refuses when the project already has that skill and asks for `u`.
- `u` replaces the project copy from the same harness's global source and reports `updated $name in <path>`. It refuses when the project copy is absent and asks for `i`. Selecting a project row uses the exact harness global source, not a cross-harness duplicate.
- `d` opens a confirmation for a project row. `y` removes the project copy, `n` returns to the picker, and Esc returns to the picker. A global row is refused with a message to edit its source directory. The confirmation names the relative destination, and success reports `removed $name from <path>`.
- The copier refuses a missing project directory, a source symlink, a symlink or non-directory in `.claude`, `.codex`, or `skills`, a source without `SKILL.md`, a project source, more than 2,048 files, more than 32 MiB, a tree deeper than 32 directories, and any non-regular file in the tree.
- Install, update, removal, and opening `/skills` reconcile first. A staged copy is deleted because only a rename can publish it, a trash copy finishes the removal the user asked for, a backup beside a live destination is dropped because the swap finished, and a backup with no destination restores the previous copy. A transient name without the id-and-name shape is left untouched, and a symlinked skill root is skipped rather than followed.
- Opening `/skills` after a repair reports `repaired N interrupted skill operation(s)`. The repair runs before the listing, so a skill that a crash left missing reappears in the same picker.
- Install never overwrites an existing project skill. Update replaces the project directory, so files removed from the global source are removed from the project copy. A failed swap restores the previous project copy. Removal stages the project directory before deletion, restores it on failure, and never touches the global source.
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

To install a global skill into the project, open `/skills`, select the global row, and press `i`. To update that project copy later, change the global skill, reopen `/skills`, select the project row, and press `u`. To remove the project copy, open `/skills`, select the project row, press `d`, and confirm with `y`. The global copy stays in place. If Baton was interrupted part way through any of those steps, the next install, update, removal, or `/skills` open repairs the leftover directory before doing anything else.

## Driving it with the verification harness

```sh
.agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence

uv run --no-project python scripts/skills_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The slash probe creates isolated `CLAUDE_CONFIG_DIR` and `CODEX_HOME` trees with one skill each and a real `SKILL.md`. It requires `skills (2)`, `enter insert`, the install, update, and remove hints, both source rows with project/global labels, selects the Codex row with Down, presses Enter, requires the insertion notice plus the source and orchestrate warnings, and checks the composer line itself for `$codex-demo `. The skills probe starts from a global-only skill, installs it with `i`, changes the global source, reopens the picker on the project row, updates with `u`, verifies the project `SKILL.md` changed from version one to version two, then reopens the picker, removes the project copy with `d` and `y`, and verifies the global copy remains.

### Unit proof

```sh
cargo test --locked --lib tui::tests::skills_picker -- --nocapture
cargo test --locked --lib skill_dispatch::tests -- --nocapture
cargo test --locked --lib sync::tests::skill_roots -- --nocapture
cargo test --locked --lib sync::tests::project_skills -- --nocapture
cargo test --locked --lib sync::tests::skill_description -- --nocapture
cargo test --locked --lib tui::tests::skills_command_lists_project_skills -- --nocapture
cargo test --locked --lib tui::tests::skills_picker_shows_sanitized_descriptions -- --nocapture
cargo test --locked --lib skills::tests -- --nocapture
cargo test --locked --lib tui::tests::skills_command_repairs_a_crashed_swap_before_listing -- --nocapture
cargo test --locked --lib skills_picker_installs_and_updates_a_project_copy -- --nocapture
cargo test --locked --lib skills_picker_explains_a_refused_install_or_update -- --nocapture
cargo test --locked --lib skills_picker_removes_a_project_copy_after_confirmation -- --nocapture
cargo test --locked --lib skills_picker_refuses_to_remove_a_global_row -- --nocapture
cargo test --locked --lib sync::tests::internal_install_directories_are_hidden -- --nocapture
JAVA_HOME=/opt/homebrew/opt/openjdk@21 bash local_docs/verification/skill-install-state-machine/run.sh
JAVA_HOME=/opt/homebrew/opt/openjdk@21 bash local_docs/verification/skill-remove-state-machine/run.sh
```

### Coverage map

`skills_picker_inserts_skill_tokens_at_token_boundaries` proves a Codex token starts a new word at a mid-word caret and a Claude token keeps a trailing source label. `skills_picker_warns_when_source_or_mode_needs_switching` proves both warnings. `skills_picker_rejects_reserved_and_unsafe_names_and_keeps_the_picker_open` proves `$memories` refusal, control and newline rejection, one rendered line, and the picker staying open. `skills_picker_window_follows_selection_past_eight` proves the window, the single selected cursor, and Down wrapping from the last row to the first. `skills_picker_visible_window_fits_a_short_terminal` proves the selected row stays visible when the overlay is shorter than the default eight-row window. `skill_tokens_submit_as_messages_instead_of_slash_commands` proves `$name` reaches the engine as a normal message. `skills::tests` proves copy, install refusal, update replacement, missing install, project-source refusal, missing project directory, missing `SKILL.md`, file-limit cleanup, source symlink refusal, and symlinked destination refusal. `skills_picker_installs_and_updates_a_project_copy` proves `i` and `u` from the real picker and the resulting notices and files. `skills_picker_explains_a_refused_install_or_update` proves the project-skill and missing-project-copy guidance. `skills_picker_removes_a_project_copy_after_confirmation` proves the confirmation, cancel path, real deletion, and success notice. `skills_picker_refuses_to_remove_a_global_row` proves the global refusal. `sync::tests::internal_install_directories_are_hidden` proves staging, backup, and trash directories never become picker rows. `skill-install-state-machine` proves install starts only from absent, update never loses a destination on a failed swap, update uses a backup, and terminal states clear staging and backup; two mutants are caught. `skill-remove-state-machine` proves removal starts only from a present project copy, leaves the global source unchanged, restores the project copy on failed deletion, and clears staging in terminal states; two mutants are caught. `skill_dispatch::tests` proves Claude rewriting, goal-prefix placement, last-mention precedence, ignored boundaries, digit-only rejection, handoff-history exclusion, goal-text exclusion, and unchanged Codex prompts. `runtime::tests::claude_payload_uses_native_skill_syntax_and_keeps_the_transcript_token` proves the provider payload is `/review` while the stored user message stays `$review`. `sync::tests::skill_roots_scan_flat_and_container_directories` and `claude_skills_win_duplicate_names_and_codex_adds_the_rest` prove `SKILL.md` discovery, one-level container scanning, Claude precedence, and source labels. `sync::tests::project_skills_shadow_global_skills_and_keep_their_harness`, `a_project_codex_skill_is_listed_when_no_claude_skill_exists`, `skill_dispatch::tests::project_skills_are_known_from_the_project_cwd`, `project_skills_do_not_rewrite_codex_prompts`, and `tui::tests::skills_command_lists_project_skills` prove the project roots, their precedence, the Claude rewrite, and the `/skills` listing. `sync::tests::skill_descriptions_come_from_frontmatter`, `skill_description_is_capped_and_reads_the_lowercase_file`, and `tui::tests::skills_picker_shows_sanitized_descriptions` prove description parsing, the 160-character cap, the lowercase file, row rendering, truncation, and sanitization. The PTY probes prove the real picker, project/global labels, warnings, composer insertion, install, update, and removal.

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
- An update uses two renames, old to backup and staging to destination. A rename failure restores the backup, but a process crash between those renames can leave a hidden backup and no project skill. Removal uses a rename to a hidden trash path before deletion; a crash between those operations can leave a hidden trash copy. Discovery hides both names so neither masquerades as a skill. Automatic crash recovery remains open.
- Update replaces the whole project skill directory. It does not merge project edits into the global source.

## Gotchas

- `/skills` lists project skills for the current thread's working directory ahead of global skills, and shows a frontmatter description when one exists. Enter inserts, `i` installs, `u` updates, and `d` removes a project copy after confirmation.
- The picker marks each row as `project` or `global`. Install is global to project only; there is no project-to-global publish. Removal is project to absent only.
- The first Enter opens the picker; the second Enter inserts the selected row. `i` installs a global copy, `u` updates an existing project copy from its exact harness global source, and `d` removes a project copy only after the confirmation.
- The composer keeps `$name`; only the Claude provider payload becomes `/name`.
- An orchestrate-mode thread needs `/orchestrate off` before a skill can reach one harness.
- A selected row past eight entries requires Up or Down to bring it into the visible window.
- The PTY proofs cover composer insertion and local install, update, and removal, not a live provider skill execution. The dispatch rewrite is covered by Rust unit tests.
