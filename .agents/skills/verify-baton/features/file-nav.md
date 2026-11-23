# File navigation

## Problem

A terminal user has to recall a path or shell-expand one to reference a project file, and a plain path still leaves the user to scroll to a symbol or compiler location. Users revisit the same few files, so a per-directory recent list promotes the last pick within its match tier. Baton needs one small file-navigation surface that works from the shell and from the TUI without embedding an editor.

## Goals

- Find a project file from a terminal query without pasting an absolute path.
- Carry a selected line target from `/files` or `/grep` into the composer as `path:line`, and map the same target to editor-specific argv for Ctrl+O and `baton open`.
- Launch the selected `/files` target or `/grep` match in `VISUAL` or `EDITOR` without leaving the TUI workflow.
- Keep discovery, containment, recency, and editor launch local and dependency-light.

## Non-goals

- An embedded editor or a second editor protocol.
- Changing `VISUAL` precedence or adding a configurable launch key.
- A TUI ignored-file mode in this slice. `baton files --ignored` covers the CLI path, and `/files --ignored` is the next slice.
- Replacing the TUI `/open` thread picker.

## Proposed architecture

`src/files.rs` owns discovery, ranking, line-target parsing, editor parsing, and the editor argv mapping. `src/cli.rs` exposes `Open { path, print }`; `src/main.rs` parses `path[:line]`, resolves containment, records the recent file, and builds the launch command. `src/tui/mod.rs` reuses the shared parser for `/files` and inserts `path:line`; `/grep` produces `path:line` targets from content matches. Ctrl+O in either picker calls `launch_picker_editor`, which prepares the same `EditorCommand` before closing the picker, stores it as a pending launch, and the run loop suspends the terminal, runs the editor, restores raw mode and the alternate screen, forces a full redraw, and reports the result. `src/store.rs` stores recency. Tests in `src/files.rs` and `tests/files_cli.rs` cover the mappings and the real binary path.

## Sub-features

### Discovery and ranking

- `baton files [query...]` lists files below the current `-C` directory with paths relative to it.
- Ripgrep is preferred. `baton files` falls back to `git ls-files` when `rg` is unavailable inside a repository. An empty directory returns an empty list on both backends.
- Hidden project files such as `.github`, `.vscode`, `.idea`, `.husky`, and `.env.example` remain discoverable. Tool caches and build output are pruned: `.git`, `node_modules`, `.next`, `.venv`, package-manager stores, `.local/share`, and the `target` directory directly under `-C`. From the repository root, `src/target` stays discoverable.
- Every query token must match either as a substring or as an in-order subsequence. Ranking tiers are: exact filename, filename prefix, every token as a substring in the filename, every token as a substring in the path, every token as a subsequence of the filename, then every token as a subsequence of the path. Tokens are independent and order-free, so `pmod`, `pmod rs`, and `rs pmod` all find `src/parser/mod.rs`; subsequences may cross path separators and are case-insensitive. Substring tiers always beat subsequence tiers. Matched characters are highlighted in the picker, including when the picker opens from `/files <query>`, and shorter paths win inside the two subsequence tiers when recency is equal.
- Query tokens filter during discovery, so unmatched files do not consume the match cap, but the tree walk and ripgrep output capture are unchanged. The merged match list is capped at 20,001 across both discovery passes before ranking.
- Tracked and untracked nonignored files are listed by default. `--ignored` adds gitignored files to the same ranked pool, so a short `.env` is visible at the default `--limit`; ignored rows beyond a full 20,001-match cap are dropped.
- `--json` returns `schema_version`, an absolute `cwd`, `query`, `ignored`, `limit`, `truncated`, `capped`, `count`, and `files`. `capped` means a match was dropped at the 20,001 cap; `truncated` also covers the global `--limit`. A broken pipe on JSON output exits cleanly.

### Recent paths

- Recently selected paths are stored per canonical project directory, up to 200; worktrees do not share them. The CLI uses its canonical `-C` root and the TUI uses the thread's canonical project directory.
- Recency breaks ranking ties before length and path order and never beats a better match tier. `/files` or `/grep` Enter, Ctrl+O in either picker, and `baton open` record a selection. `baton files` and the `/files` picker read the list; `/grep` records selections but ranks its results by path and line. `baton files` prunes dead project directories.

### Editors

- `baton open <path[:line]>` resolves only files inside the current `-C` directory, prefers `VISUAL` over `EDITOR`, splits quoted editor commands including empty quoted arguments, preserves Windows path separators and Unix backslashes, and waits for the editor process.
- A trailing 1-based `:<line>` is parsed with the same rules as `/files`. Code-family editors (`code`, `code-insiders`, `codium`, `cursor`, `windsurf`) receive `--goto path:line`. `vim`, `vi`, `view`, `gvim`, `mvim`, `nvim`, `emacs`, `emacsclient`, and `nano` receive `+line path`. Helix, Sublime, Zed, and unknown editors receive `path:line`.
- `--print` prints the quoted launch command without starting the editor. It still requires `VISUAL` or `EDITOR` so the argv form is known. Quoting targets the host shell: POSIX single quotes on Unix and best-effort double quotes on Windows.

### `/files` picker

- `/files [query]` opens a TUI picker over the capped, nonignored inventory for the current thread's project directory. Discovery runs on a background thread behind a loading panel, so the TUI keeps drawing and Esc always closes while loading.
- Enter inserts the selected path or `path:line`, adds a trailing space, shows `inserted path[:line]`, and closes the picker. Typing narrows the loaded rows, Backspace edits the filter, Esc clears the filter and then closes, and Up/Down/PageUp/PageDown scroll.
- A load that finishes after the picker closes or is replaced is ignored. `/files --ignored` is rejected with a pointer to `baton files --ignored`.
- Enter while the scan is loading has no row to act on and does nothing. The loading panel shows only the Esc affordance.

### Ctrl+O editor launch

- Ctrl+O in `/files` or `/grep` prepares and launches the selected target in `VISUAL` or `EDITOR`. The loaded heading names `ctrl+o open`, `enter insert`, and the Esc behavior; the field order differs between pickers. The loading panel keeps `ctrl+o open` and `enter insert` hidden but still shows Esc, and Ctrl+O pressed while loading reports a wait notice.
- `/files` passes its command or live-filter line target. `/grep` passes the selected match line. Both use the same editor-specific argv mapping as `baton open`.
- The target is resolved and the editor executable is preflighted before the picker closes. A loading scan, no matching row, no editor, an unclosed quote, an executable not on `PATH`, a missing file, or a path outside the thread project directory leaves the picker open with a sanitized notice.
- A prepared launch records the recent file, closes the picker, shows the transient `opening path[:line]` notice, suspends the TUI, runs the editor in the thread directory, restores the terminal, and reports `opened path[:line]` or the editor error.
- The TUI is blocked while the editor runs, so queued turn events and approvals render only after it exits. Enter insertion remains unchanged.

### `/grep` picker

- `baton grep <query> [--case-sensitive] [--word] [--regex] [--json]` searches file contents with ripgrep and a `git grep` fallback. Hidden project files such as `.github` are searched, VCS and cache directories are pruned, binary files are skipped, and matches are sorted by path and line before the global `--limit` caps them.
- JSON returns `schema_version`, `cwd`, `query`, the flags, `limit`, `truncated`, `count`, and `matches` with `path`, `line`, and a 300-character `text` preview. The query is trimmed, multiple words form one phrase, and no matches exits 0.
- `/grep [--case|--case-sensitive] [--word] [--regex] <query>` opens a TUI picker over the same search. Discovery runs on a background thread behind a loading panel, typing narrows the loaded matches by path, line, or text, Backspace edits the filter, Enter inserts `path:line `, shows `inserted path:line`, and closes the picker, Ctrl+O launches the selected match at its line, Esc clears the filter and then closes, and Up/Down/PageUp/PageDown scroll.
- The picker caps at 200 matches. When the cap is hit, the footer says to close the picker before running `/grep` again. An empty filtered result shows `0 loaded` in a truncated heading and `no matches in the loaded results` in the body rather than claiming the project has no match. Esc during loading closes the picker, and a search that finishes after the picker closed or was replaced is ignored. Enter while the search is loading has no row to act on and does nothing.
- `base_query`, the live filter, match rows, inserted targets, launch labels, launch errors, and picker headings are sanitized before they reach the terminal.

### Line targets

- A trailing `:<line>` after a non-empty path on the `/files` command or live filter is a line target, not part of the filter. A bare `:12` is filter text. `/files pmod rs:12` loads the multi-token matches, shows `line 12` in the heading, and inserts `src/parser/mod.rs:12 ` when Enter is pressed.
- Line numbers are 1-based positive integers that must fit `u64`. Spaces around the colon are ignored; `:0` and `:00` stay part of the query; leading zeros are normalized (`:007` becomes `:7`). A literal colon suffix is written with a backslash, so `app\:12` filters `app:12` with no target. An integer above `u64::MAX` is filter text, not a line target.
- `/grep` produces a match line for Ctrl+O, but its query is a content search and does not parse a line-target suffix.

## How to get to it (user POV)

```sh
baton -C <project> files parser
baton -C <project> files pmod rs --json
baton -C <project> files parser md --ignored --json
VISUAL='code --wait' baton -C <project> open src/main.rs
VISUAL='code --wait' baton -C <project> open src/main.rs:42 --print
VISUAL='code --wait' baton -C <project> open src/main.rs --print
```

```text
/files pmod rs:12
Ctrl+O launches the selected path at line 12.
/grep parse
Ctrl+O launches the selected match at its reported line.
```

## Driving it with the verification harness

### Shared parser and editor mapping

```sh
cargo test --locked --lib files::tests::split_line_targets_are_shared_by_the_cli_and_picker -- --exact
cargo test --locked --lib files::tests::open_commands_apply_editor_specific_line_syntax -- --exact
cargo test --locked --lib tui::tests::line_targets_split_from_the_query -- --exact
```

### Files CLI

```sh
cargo test --locked --test files_cli files_json_reports_paths_limit_query_and_ignored -- --exact
cargo test --locked --test files_cli files_scope_to_the_current_directory -- --exact
cargo test --locked --test files_cli files_fall_back_to_git_when_ripgrep_is_missing -- --exact
cargo test --locked --test files_cli files_match_subsequence_tokens -- --exact
cargo test --locked --test files_cli ignored_files_reach_the_default_limit -- --exact
cargo test --locked --test files_cli empty_pruned_and_ignored_only_projects_list_cleanly -- --exact
cargo test --locked --test files_cli file_listings_survive_a_closed_pipe -- --exact
cargo test --locked --test files_cli open_print_uses_the_editor_and_rejects_escape_paths -- --exact
cargo test --locked --test files_cli open_print_passes_line_targets_to_common_editors -- --exact
cargo test --locked --test files_cli open_runs_a_known_editor_with_the_line_argument -- --exact
cargo test --locked --test files_cli open_prefers_visual_over_editor_in_a_real_process -- --exact
cargo test --locked --test files_cli open_runs_the_editor_and_propagates_failure -- --exact
cargo test --locked --test files_cli json_cwd_renders_a_non_utf8_directory_lossily -- --exact  # Linux only
cargo test --locked --lib cli::tests::files_accepts_query_ignored_and_json -- --exact
cargo test --locked --lib cli::tests::open_accepts_path_and_print -- --exact
```

### Files picker and launch

```sh
cargo test --locked --lib tui::tests::files_picker -- --nocapture
cargo test --locked --lib tui::tests::files_picker_ctrl_o_prepares_editor_launch -- --exact
cargo test --locked --lib tui::tests::files_picker_ctrl_o_keeps_the_picker_when_setup_fails -- --exact
cargo test --locked --lib tui::tests::files_picker_ctrl_o_uses_a_typed_line_target -- --exact
cargo test --locked --lib tui::tests::files_picker_ctrl_o_waits_for_the_scan -- --exact
cargo test --locked --lib tui::tests::files_picker_inserts_a_line_target -- --exact
cargo test --locked --lib tui::tests::files_picker_sanitizes_paths_insertion_and_launch_notices -- --exact
cargo test --locked --lib tui::tests::file_row_marks_matches_and_keeps_the_gutter -- --exact
```

### Grep picker and launch

```sh
cargo test --locked --lib grep:: -- --nocapture
cargo test --locked --test grep_cli -- --nocapture
cargo test --locked --lib tui::tests::grep_picker -- --nocapture
cargo test --locked --lib tui::tests::grep_picker_ctrl_o_prepares_editor_launch -- --exact
cargo test --locked --lib tui::tests::grep_picker_ctrl_o_keeps_the_picker_when_setup_fails -- --exact
cargo test --locked --lib tui::tests::grep_picker_ctrl_o_waits_for_the_search -- --exact
```

### Recent files

```sh
cargo test --locked --lib store::tests::recent_files -- --nocapture
cargo test --locked --lib store::tests::missing_recent_projects_are_pruned -- --exact
```

### PTY probe

```sh
.agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

### Coverage map

`files::tests` covers ranking, deduplication, the shared match cap, query-first filtering, path containment, editor parsing, Unix backslashes, cache pruning, substring-over-subsequence ranking, multi-token highlighting, and the lowercase-length guard. `tests/files_cli.rs` builds real git repositories and drives the built binary. It asserts every JSON field, ignored opt-in, default-limit reachability, empty and pruned-only directories, `VISUAL` precedence with both editor variables set, `-C` scope, the `git ls-files` fallback with a `PATH` that has git but no ripgrep, the editor run path including a nonzero exit and a missing editor, a closed stdout pipe for plain and JSON output, and a lossy JSON `cwd` for a non-UTF-8 directory on Linux.

`store::tests::recent_files_upsert_and_order`, `recent_files_are_project_scoped`, `recent_files_are_capped`, `recent_files_survive_reopen`, and `missing_recent_projects_are_pruned` cover the recent-file table. `files::tests::recent_paths_break_ties_without_beating_match_quality` covers the tie-break, `files_picker_prefers_a_recent_file_and_records_the_pick` covers picker recording and ordering, `grep_picker_ctrl_o_prepares_editor_launch` proves the grep launch records the path and line, and `open_records_a_recent_file_for_files_ranking` proves the CLI path.

`tui::tests::files_picker` covers insertion, line-target insertion, filter reset, scrolling, loading, cancelled and superseded loads, cap warnings, the ignored-flag hint, empty projects, and missing directories. `files_picker_ctrl_o_prepares_editor_launch`, `files_picker_ctrl_o_keeps_the_picker_when_setup_fails`, `files_picker_ctrl_o_uses_a_typed_line_target`, and `files_picker_ctrl_o_waits_for_the_scan` cover the shared launch lifecycle. `files_picker_sanitizes_paths_insertion_and_launch_notices` proves escape and bell bytes never reach the picker, composer, success label, heading query or filter, or launch notice.

`grep::tests` covers line matches across files, case, word, and regex flags, the sorted cap, hidden-file search with cache pruning, the `git grep` multi-match parser, binary and gitignored exclusion, empty queries, and missing matches. `grep_falls_back_to_git_when_ripgrep_is_missing` proves the fallback through the built binary. `tests/grep_cli.rs` proves the JSON contract, plain output, the flags, no-match success, the empty-query error, and the closed pipe through the built binary. `tui::tests::grep_picker` covers insertion, flags, narrowing, filter reset, loading, a superseded load, usage, no matches, prepared launch, setup failure, and waiting for the search. `grep_picker_sanitizes_match_text_and_insertion` proves escape and bell bytes never reach the match rows, heading query or filter, composer, or inserted target.

The PTY `slash_probe.py` checks the `/files` and `/grep` headings and affordances, recent-file ordering, Enter insertion, and a real fake Code launch from each picker. It proves `--goto <path>:12` for `/files`, `--goto <path>:1` for `/grep`, the `opened` notices after terminal restore, and that a failing editor still returns to a usable `/status` screen.

## Alternatives

- Ripgrep with a `git ls-files` fallback was chosen over ripgrep-only. The fallback keeps file discovery usable in a normal clone without another dependency; reopen if its ignore semantics diverge from ripgrep in a way that changes user choices.
- Discovery is scoped to `-C`, not the repository root, because that matches `rg`, `git ls-files`, and the shell's current directory. Reopen if users expect `baton -C src` to search the enclosing worktree.
- Gitignored files join one ranked pool with an explicit `--ignored` flag. Keeping them out by default avoids surfacing secrets and build output, while one pool keeps ignored rows reachable at the default limit; reopen if everyday work needs ignored files without the flag.
- The picker uses a background load plus a client-side token filter instead of synchronous discovery or a tree walk per keystroke. A synchronous scan blocks key handling for the whole scan; reopen for a small project if the loading state becomes more annoying than that block.
- Enter inserts and Ctrl+O launches in `/files` or `/grep`. One hidden Enter action would be faster but unpredictable, while a separate mode adds its own escape and selection state. Reopen if users consistently want Enter to open and insertion becomes secondary.
- Query tokens match independently as substring or in-order subsequence instead of using a fuzzy-scoring library, a joint alignment, or glob syntax. This keeps ranking explainable and dependency-free; reopen if real queries miss files users expect.
- Line targets use the inline `:<line>` form with a backslash escape instead of a second prompt. One field keeps the picker interaction small; reopen if users need multiple targets or a symbol chooser.
- Ctrl+O waits for the editor in place instead of a second terminal, detached process, tmux window, or embedded viewer. This avoids a placement and lifecycle model. A hung editor must be killed from another terminal; reopen if attach exists.
- In-TUI Ctrl+O is available in both pickers instead of insert-only plus a shell `baton open`, and instead of limiting launch to `/files`. A `/grep` user often goes from a match directly to the code, and making that path explicit in both pickers avoids retyping the target outside the TUI. The accepted cost is the suspend/restore lifecycle, a blocked event loop while the editor runs, and the preflight and hung-editor handling documented above; reopen if attach or a second-pane editor makes that cost unnecessary.
- A TUI ignored-file mode is deferred. Orca includes gitignored files in a second Quick Open pass, but a terminal picker that silently includes `.env` needs an explicit warning, one shared cap, and tests. The next slice is `/files --ignored`, matching the CLI opt-in.
- The editor mapping is deliberately small and explicit: Code-family editors use `--goto`, Vim-family, Emacs-family, and nano use `+line`, and Helix, Sublime, Zed, and unknown editors receive `path:line`. A configurable placeholder was rejected for this slice because it adds configuration and hides the fallback; reopen if a common editor needs another form.

The reasons and reversal conditions for the broader file-navigation decisions live in the [parity ledger](../../../../docs/parity.md). This document keeps the slice-specific choices and proof.

## Risks and mitigations

- A line target can be stale or beyond the file length. The parser accepts only a positive integer that fits `u64`; the editor owns clamping. An overflowing suffix stays filter text. `baton open` reports the exact argv in `--print`, and no semantic-symbol claim is made.
- An unknown editor may not understand `path:line`. Known common editors use their native form, and `baton open --print` shows the exact argv. The TUI has no argv preview, so an unknown editor receives `path:line` without that check.
- A missing editor must fail clearly. `baton open` and TUI Ctrl+O refuse an unset variable, an unclosed quote, and an executable that is not on `PATH`. A configured editor that passes preflight but fails to start closes the picker and reports the post-close error after the terminal is restored; that time-of-check-to-time-of-use branch is reasoned from the run-loop error path, not separately tested.
- Windows wrappers may be named `.exe`, `.cmd`, or `.bat`. The program basename is lowercased and all three suffixes are stripped before mapping.
- A path can escape `-C`. `resolve_project_file` canonicalizes and rejects any target outside the root.
- The TUI event loop is blocked while an editor runs. The engine and turn event channels are unbounded, and Baton holds approvals in memory until answered, so queued work renders after the editor exits. A provider-side cancel or disconnect can still invalidate a pending approval, and a hung editor must be killed from another terminal. The in-place wait is accepted because the alternative placement models are larger and the user chose the editor.
- Project file names and user query/filter text can contain terminal control bytes. Picker rows, headings, inserted targets, launch labels, and launch errors are sanitized before rendering. The resolved canonical target reaches the filesystem and editor argv; the raw picker path is used for the sanitized label and recent-file record. That is the intended trust boundary.
- The inventory and result caps bound memory: 20,001 paths per discovery pool, 200 grep rows, and 200 recent paths per project. The heading or footer reports truncation. A result that arrives after the picker closes or is replaced is discarded.
- Recency is only a tie-break, so a recent weak match never outranks a better match. A store failure is ignored because recency is a ranking hint.

## Gotchas

- `baton files` uses the global `--limit` for output. A cap hit prints "results truncated at the 20,001-match cap; refine the query"; a limit hit prints "raise --limit".
- `--ignored` makes gitignored files eligible, so build outputs and secrets such as `.env` can appear in terminal output. It is opt-in for that reason.
- CLI discovery and `baton open` are scoped to the current `-C` directory, not the repository root. Running `baton -C repo/src open ../README.md` is rejected. The TUI picker uses the open thread's project directory, which can differ from the process `-C` after `/open`.
- The TUI `/open` still selects threads. `/files` owns project paths, Enter inserts a path or `path:line`, and Ctrl+O launches it with the same editor mapping as `baton open`; `/grep` uses the same launch path with its match line. Unknown editors receive `path:line`.
- A `:line` given on the `/files` command stays for that picker, even after the live filter clears; rerun `/files` without a suffix to drop it.
- The picker loads nonignored files only. `--ignored` remains a CLI option until the next slice.
- `baton open` does not embed an editor. GUI editors should use a wait flag such as `code --wait` if the terminal should block until the file closes.
- Backslash escaping applies only before a quote or whitespace, so Windows paths and UNC prefixes survive parsing. A backslash before a closing quote is still ambiguous on Windows. `--print` quoting is best effort on Windows; the run path uses argv.
- Non-UTF-8 file names are skipped, because the plain and JSON contracts are UTF-8.
- Content search respects ignore rules differently by backend: inside a git repository, both respect `.gitignore`. Outside a git repository only ripgrep can run, and it still reads `.ignore` and `.rgignore`; the `git grep` fallback requires a repository and reads neither. Both search hidden project files and prune VCS and cache paths. Matches are sorted by path and line before capping; the CLI cap is the global `--limit`, the TUI cap is 200, and the footer says when it truncated. Regex syntax is the backend's syntax. Esc does not kill an in-flight search.
- Each discovery pass walks the tree once and keeps at most 20,001 matches; `--ignored` runs the primary and ignored passes and merges them under one 20,001 cap. Substring matches are admitted before subsequence-only matches, so fuzzy decoys cannot evict an exact or name match, but a query with more than 20,001 substring matches still hits the cap.
- Query tokens are independent and order-free inside the subsequence tiers. Token order changes neither matching nor highlight positions; the union is sorted and deduplicated. A token that misses both substring and subsequence rejects the path. Typing narrows only the loaded rows, so a path dropped at the cap needs a new `/files <query>` run. The picker shows the cap warning while the inventory is truncated.
