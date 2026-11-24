# Memory files and auto-memory

## Sub-features
- `/memory` opens a picker for Claude Code user, project (`.claude/CLAUDE.md` when present), local, and rule files; Codex user instructions and an existing user `AGENTS.override.md`; project `AGENTS.md` and an existing root `AGENTS.override.md`; `.claude/AGENTS.md` when present; and the project's auto-memory index, topics, and folder. Codex lists an override before its fallback `AGENTS.md`, and the project and `.claude/` `AGENTS.md` rows say Claude reads that file only when no `CLAUDE.md` or `CLAUDE.local.md` is present. `.codex/AGENTS.md` is not a Codex path and is not listed.
- Selecting a missing file preflights the editor, creates the file, then opens it in `VISUAL` or `EDITOR`. Selecting `Open auto-memory folder` preflights the editor, creates the directory, then opens it in the same editor.
- The Auto-memory row edits the user `autoMemoryEnabled` key, shows the user value and effective value separately, and preserves every unrelated JSON key. A project or local setting can keep the effective value unchanged after a toggle.
- Local, then project, then user settings decide the effective enabled state. The user settings file relocates the whole directory, and a valid Cowork override wins over it; `CLAUDE_CODE_REMOTE_MEMORY_DIR` replaces the memory base and `CLAUDE_CODE_PROJECT_DIR_NAME` replaces the project segment.
- `/memories` is not a slash command. Baton's older governed-memory CLI remains available for compatibility but is not part of this TUI surface.

## Problem statement

Claude Code and Codex already own persistent instruction files and Claude Code owns project auto-memory. Baton could not show which files exist, create a missing file, open a rule, or toggle auto-memory without leaving the terminal and knowing the paths by memory.

## Goals and non-goals

Goals: one `/memory` submenu that lists the Claude Code and Codex memory files a local session can edit; create and open missing files in the configured editor; show the project's auto-memory index, topic files, and folder; edit the user auto-memory setting and show its effective state without losing unrelated keys; and make worktree, override, and malformed-settings behavior explicit.

Non-goals: editing Claude's import approval state, managed-policy memory, subagent memory, topic frontmatter, Codex `memories.*` settings, or any key other than `autoMemoryEnabled`. The picker lists the working directory only: it does not walk ancestor `CLAUDE.md` files, list nested on-demand `CLAUDE.md` or `AGENTS.md` files, or model Codex's root-to-working-directory chain, fallback filenames, or byte budget. It models the process environment plus the user, project, and local settings files; managed settings, `--settings` files, and settings-file `env` blocks are not consulted. It does not model workspace trust, so project and local `autoMemoryDirectory` values are always ignored. It does not model Claude's Windows or file-ownership exceptions for local settings, and it does not lock settings against a concurrent writer. It lists existing rule and topic files and can create the fixed top-level rows, but it does not create arbitrary nested rule or topic files. The picker is a file and setting browser, not a second memory engine. It does not copy memory text into Baton's database.

## Proposed architecture

`src/memory_files.rs` discovers the files without copying them into Baton state. Project instructions, project rules, and project settings resolve from the active working directory, so `/memory` and `/init` point at the same `CLAUDE.md` in a worktree or subdirectory. Auto-memory resolves from the git repository identity, so normal worktrees and subdirectories share one memory directory; a submodule resolves to the submodule checkout, and a worktree of that submodule shares it through `core.worktree`. The helper is separate from the governed-memory identity so existing governed-memory keys do not move. User and project rule directories are scanned recursively for Markdown files, capped at 200 with a visible warning. Symlinked directories are not followed. A symlinked rule file is listed only when its resolved target stays inside the rule root; a target outside the root is skipped with a warning.

Claude user settings come from `CLAUDE_CONFIG_DIR/settings.json` or `~/.claude/settings.json`. Project settings come from the active working directory's `.claude/settings.json`. Local settings come from the repository identity root's `.claude/settings.local.json`; outside git, or when the repository root is the home directory, they come from the working directory. Baton does not model Claude's Windows or file-ownership fallbacks. The Auto-memory toggle writes only the user `autoMemoryEnabled` key with a same-directory temp file created with `create_new`, rewritten by an atomic rename. On Unix the temp file is `0600`; Windows inherits the default ACL and Baton makes no stricter Windows permission claim. A symlink or other non-regular settings file is refused. Any malformed, non-object, or non-regular user, project, or local settings file is ignored for state and makes the toggle read-only. `CLAUDE_CODE_DISABLE_AUTO_MEMORY` accepts `1`, `true`, `yes`, or `on` to disable and `0`, `false`, `no`, or `off` to force on; other values are ignored. Either form and a remote session without `CLAUDE_CODE_REMOTE_MEMORY_DIR` make the toggle read-only and explain why.

The default auto-memory directory is `<memory base>/projects/<project>/memory`. `<project>` is the canonical identity root with every non-ASCII-alphanumeric character replaced by `-`; a name longer than 200 characters is truncated to 200 and suffixed with `-` and a base36 32-bit hash of the full path. The implementation uses the unsigned hash bit pattern; only a positive-hash long path is pinned by a live check. Resolution order is: default directory, then a valid `autoMemoryDirectory` from user settings, then a valid `CLAUDE_COWORK_MEMORY_PATH_OVERRIDE`. `autoMemoryDirectory` in project or local settings is ignored with a warning; Baton does not model workspace trust, so trusting the workspace does not make Baton apply it. `autoMemoryDirectory` and the Cowork override accept an absolute path or `~/...` but not a bare `~`; `CLAUDE_CODE_REMOTE_MEMORY_DIR` replaces the base and must be absolute; `CLAUDE_CODE_PROJECT_DIR_NAME` must be set with an explicit `CLAUDE_CONFIG_DIR` and be 1-64 ASCII letters, digits, hyphens, or underscores and not a Windows device name. An invalid value produces a warning and leaves the previous valid directory in place, or the default when none was valid.

The TUI overlay lists the files that do not exist yet as selectable rows. It sanitizes every label and detail before rendering. Enter on a file preflights the editor without project containment, then creates parent directories and a `# <file name>` header with one blank line when missing, records the pending editor launch, and restores the TUI when the editor exits. A failed preflight creates nothing. The picker stays open on a missing editor, malformed editor command, or failed file creation; the Auto-memory row stays open and refuses the toggle when settings are malformed.

## Alternatives considered

- Copying memory files into Baton's database was rejected. Claude and Codex already own these files; a second copy would drift.
- Embedding an editor was rejected. Baton already has one editor launch path and one terminal suspend/resume path.
- Writing `autoMemoryEnabled` through a bespoke Claude config writer was rejected. A JSON object edit preserves unknown fields and matches the user settings contract.
- Silently ignoring a malformed settings file was rejected because the toggle would make a change the user did not ask for.

## Implementation scope and sequence

Create `src/memory_files.rs` and `.agents/skills/verify-baton/features/memory.md`. Modify `src/lib.rs` (module registration), `src/worktree.rs` (a separate auto-memory identity helper), `src/tui/mod.rs` (command, overlay, keys, rendering, tests), `.agents/skills/verify-baton/features/README.md`, `.agents/skills/verify-baton/features/slash-commands.md`, `.agents/skills/verify-baton/scripts/slash_probe.py`, `README.md`, `docs/parity.md`, `docs/ci.md`, and `ci/test-inventory-floor.txt`. Delete `.agents/skills/verify-baton/features/memories.md`. The CI files change only to record the higher test count and floor.

1. Add the environment snapshot and file discovery, including Claude user, project, local, and `.claude/rules` rows, Codex user and root project rows plus existing overrides, and auto-memory rows. Keep the listing a value type with no filesystem handles.
2. Add the three settings scopes, enabled-state precedence, directory resolution, malformed-file read-only state, and the user-only settings writer. Keep the governed-memory identity function unchanged; add a separate auto-memory identity helper.
3. Add file and folder creation helpers plus the editor preflight. Tests cover permissions, unknown-key preservation, symlink containment, submodule and worktree roots, and a missing editor.
4. Replace `/memories` with `/memory` in the command table, add the picker overlay and key handling, and render sections, read-only state, and capped warnings.
5. Update the README, parity ledger, slash-command inventory, this feature file, and the PTY probe. The probe selects the Auto-memory and Project instructions rows by their visible labels so row-order changes cannot silently retarget the assertions.

## Rollback

The picker creates files and directories but keeps no backups. Delete a created file or folder to undo that row. Each toggle writes the opposite user value; if the key was absent before, it remains present. Remove the `autoMemoryEnabled` key from the user settings file to restore absence. Unrelated keys are never touched. Revert the commit to remove the slash command and module together.

## Risks and mitigations

- A rule symlink can point at a file outside the rule root. Baton skips it and shows a warning instead of offering the external file as a memory row, even when the target is inside the repository. Symlinked rule directories are not followed at all.
- A settings file that is a symlink or another non-regular file is ignored for state and makes the toggle read-only, because the atomic replace would otherwise replace the link. Use a regular settings file to toggle from Baton.
- Rule discovery stops at depth 8 and caps each rule root at 200 Markdown files. Auto-memory topics are capped at 200. The picker shows up to three warnings and reports how many more exist.
- `AGENTS.md` is fallback context for Claude: Claude reads it only when no `CLAUDE.md` or `CLAUDE.local.md` is present, unless Project instructions is configured otherwise. The project and `.claude/` rows say this. The Codex user row is Codex-only, and the picker lists `.claude/AGENTS.md` when present.
- Ancestor and nested instruction files are not listed. A user who edits `../CLAUDE.md` or a subdirectory's on-demand file does so outside `/memory`.
- Settings writes are read-modify-write with no lock. A concurrent edit to the same settings file can be overwritten.
- Local settings placement differs from Claude on Windows and when the repository root, `.git`, or `.claude` is not owned by the user. Baton uses the repository identity root in those cases.
- The base36 long-path suffix uses the unsigned 32-bit hash. A live check pins a positive hash; a negative-hash path is not verified against Claude. Windows canonicalization can produce a verbatim path prefix that changes the key.

## How to get to it (user POV)

Type `/memory`. The picker starts on User instructions. Use Up/Down or Home/End to move, Left/Right or Enter on Auto-memory to edit the user setting, and Enter on any file or folder row to preflight and open it in `VISUAL` or `EDITOR`. The Auto-memory detail shows the user value, the effective value, and the source, so a project or local override can keep the effective value unchanged. Esc closes the picker. A missing file is created after the editor passes preflight.

## Verification paths

### Unit
```sh
cargo test --locked --lib memory_files::tests
cargo test --locked --lib tui::tests::memory_command_opens_harness_memory_files_and_auto_memory
cargo test --locked --lib tui::tests::memory_picker_toggles_auto_memory_and_persists_the_user_setting
cargo test --locked --lib tui::tests::memory_picker_toggle_shows_when_a_project_setting_masks_the_user_value
cargo test --locked --lib tui::tests::memory_picker_creates_a_missing_file_and_prepares_the_editor
cargo test --locked --lib tui::tests::memory_picker_missing_editor_does_not_create_the_file
cargo test --locked --lib tui::tests::memory_picker_opens_the_auto_memory_folder
cargo test --locked --lib tui::tests::memory_picker_scrolls_to_the_selected_row_past_ten_entries
cargo test --locked --lib tui::tests::auto_memory_toggle_is_read_only_when_the_environment_disables_it
```

The module tests cover discovery including both Codex overrides and `.claude/AGENTS.md`, project and local precedence, the home-relative directory override, invalid types and an invalid remote base, Cowork precedence over user settings, both effective values of the disable environment variable, unknown-key preservation, Unix owner-only settings writes, malformed JSON, non-object, and non-regular settings in every scope, symlinked settings refusal, missing-file creation, symlink containment, home-root local settings, and active-worktree, submodule, submodule-worktree, and linked-superproject-worktree identity. The TUI tests cover the visible rows, a short terminal that keeps the selected row rendered, scrolling, the user-versus-effective toggle detail, the toggle side effect, the read-only state, the missing-file editor path, a missing editor that creates nothing, and the auto-memory folder launch. The cap, depth, and more-than-three-warning branches are not unit-tested.

### PTY
```sh
.agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The probe creates isolated Claude and Codex config trees plus a user rule, a project `CLAUDE.md`, `AGENTS.md`, auto-memory `MEMORY.md`, and a topic file. It clears ambient auto-memory environment variables, opens `/memory`, requires each section plus the user rule, local-instruction row, topic, and auto-memory folder rows, selects Auto-memory by its visible label, toggles it off, checks the persisted Claude setting, then selects Project instructions by label, opens it through the fake editor, and checks the exact captured target.

## Gotchas
- `/memory` reads `CLAUDE_CONFIG_DIR` and `CODEX_HOME`; a temporary home in tests does not redirect them unless those variables are set too.
- The auto-memory folder is shared by worktrees of the same repository by default, not by unrelated repositories in the same parent folder. `autoMemoryDirectory`, `CLAUDE_CODE_PROJECT_DIR_NAME`, and `CLAUDE_CODE_REMOTE_MEMORY_DIR` can break that isolation.
- The picker lists user and project entries even when the files do not exist. Enter creates them, so a stray Enter on a missing row is a file creation.
- Only the fixed top-level rows can be created from the picker. A missing nested rule or topic must be created outside Baton first.
- `/init` still creates the standard project `CLAUDE.md` template. `/memory` opens the same working-directory file once it exists. Local settings and auto-memory resolve from the repository identity root; all other project rows use the working directory.
