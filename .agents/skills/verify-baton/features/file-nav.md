# File navigation

## Problem and non-goals
A terminal user has to recall a path or shell-expand one to reference a project file, and a plain path still leaves the user to scroll to a symbol or compiler location. Users revisit the same few files, so a per-directory recent list promotes the last pick within its match tier. `baton files` ranks the tree below the current `-C` directory, `/files` inserts a picked path or `path:line` into the TUI composer, Ctrl+O launches that target in the editor, and `baton open` provides the same launch from the shell. Non-goals in this slice: an embedded editor, launching from `/grep`, changing `VISUAL` precedence, a configurable launch key, and replacing the launch command with a second editor protocol.

## Goals

- Find a project file from a terminal query without pasting an absolute path.
- Carry a selected line target from `/files` or `/grep` into the composer and then into `baton open` with editor-specific syntax.
- Launch the selected `/files` target in `VISUAL` or `EDITOR` without leaving the TUI workflow.
- Keep discovery, containment, recency, and editor launch local and dependency-light.
- Test the line-target parser and editor argv forms through unit and built-binary paths.

## Proposed architecture

`src/files.rs` owns discovery, ranking, line-target parsing, editor parsing, and the editor argv mapping. `src/cli.rs` exposes `Open { path, print }`; `src/main.rs` parses `path[:line]`, resolves containment, records the recent file, and builds the launch command. `src/tui/mod.rs` reuses the shared parser for `/files` and inserts `path:line`; `/grep` produces `path:line` targets from content matches. `/files` Ctrl+O prepares the same `EditorCommand` before closing the picker, stores it as a pending launch, and the run loop suspends the terminal, runs the editor, restores raw mode and the alternate screen, forces a full redraw, and reports the result. `src/store.rs` stores recency. Tests in `src/files.rs` and `tests/files_cli.rs` cover the mappings and the real binary path.

## Sub-features
- `baton files [query...]` lists files below the current `-C` directory with paths relative to it.
- Ripgrep is preferred. `baton files` falls back to `git ls-files` when `rg` is unavailable inside a repository. An empty directory returns an empty list on both backends.
- Hidden project files such as `.github`, `.vscode`, `.idea`, `.husky`, and `.env.example` remain discoverable. Tool caches and build output are pruned: `.git`, `node_modules`, `.next`, `.venv`, package-manager stores, `.local/share`, and the `target` directory directly under `-C`. From the repository root, `src/target` stays discoverable.
- Every query token must match either as a substring or as an in-order subsequence. Ranking tiers are: exact filename, filename prefix, every token as a substring in the filename, every token as a substring in the path, every token as a subsequence of the filename, then every token as a subsequence of the path. Tokens are independent and order-free, so `pmod`, `pmod rs`, and `rs pmod` all find `src/parser/mod.rs`; subsequences may cross path separators and are case-insensitive. Substring tiers always beat subsequence tiers. Matched characters are highlighted in the picker, including when the picker opens from `/files <query>`, and shorter paths win inside the two subsequence tiers when recency is equal.
- Query tokens filter during discovery, so unmatched files do not consume the match cap, but the tree walk and ripgrep output capture are unchanged. The merged match list is capped at 20,001 across both discovery passes before ranking.
- Recently selected paths are stored per canonical project directory, up to 200; worktrees do not share them. The CLI uses its canonical `-C` root and the TUI uses the thread's canonical project directory. They break ranking ties before length and path order and never beat a better match tier. `/files` or `/grep` Enter, `/files` Ctrl+O, and `baton open` record a selection; `baton files` and the picker read the same list.
- Tracked and untracked nonignored files are listed by default. `--ignored` adds gitignored files to the same ranked pool, so a short `.env` is visible at the default `--limit`; ignored rows beyond a full 20,001-match cap are dropped.
- `--json` returns `schema_version`, an absolute `cwd`, `query`, `ignored`, `limit`, `truncated`, `capped`, `count`, and `files`. `capped` means a match was dropped at the 20,001 cap; `truncated` also covers the global `--limit`. A broken pipe on JSON output exits cleanly.
- `baton open <path[:line]>` resolves only files inside the current `-C` directory, prefers `VISUAL` over `EDITOR`, splits quoted editor commands including empty quoted arguments, preserves Windows path separators and Unix backslashes, and waits for the editor process. A trailing 1-based `:<line>` is parsed with the same rules as `/files`. VS Code-family editors receive `--goto path:line`, Vim/Emacs/nano receive `+line path`, and Helix/Sublime/Zed plus unknown editors receive `path:line`.
- `--print` prints the quoted launch command without starting the editor. It still requires `VISUAL` or `EDITOR` so the argv form is known. Quoting targets the host shell: POSIX single quotes on Unix and best-effort double quotes on Windows.
- `/files [query]` opens a TUI picker over the capped, nonignored inventory for the current thread's project directory. Discovery runs on a background thread behind a loading panel, so the TUI keeps drawing and Esc always closes while loading. Enter inserts the selected path and a trailing space into the composer, typing narrows the loaded rows, Backspace edits the filter, Esc clears the filter and then closes, and Up/Down/PageUp/PageDown scroll. A load that finishes after the picker closes or is replaced is ignored. `/files --ignored` is rejected with a pointer to `baton files --ignored`.
- Ctrl+O in `/files` prepares and launches the selected path in `VISUAL` or `EDITOR`, and the footer advertises `enter insert`, `ctrl+o open`, and the Esc behavior. The line target from the command or live filter is passed through the same editor-specific argv mapping as `baton open`. The picker stays open with a notice when discovery is loading, no row matches, no editor is configured, the configured executable is not on `PATH`, the editor command is malformed, or the selected path is missing or outside the thread project directory. A prepared launch records the recent file, closes the picker, suspends the TUI, runs the editor in the thread directory, restores the terminal, and reports either `opened path` or the editor error. The TUI is blocked while the editor runs, so queued turn events and approvals render only after it exits. Enter insertion remains unchanged.
- `baton grep <query> [--case-sensitive] [--word] [--regex] [--json]` searches file contents with ripgrep and a `git grep` fallback. Hidden project files such as `.github` are searched, VCS and cache directories are pruned, binary files are skipped, and matches are sorted by path and line before the global `--limit` caps them. JSON returns `schema_version`, `cwd`, `query`, the flags, `limit`, `truncated`, `count`, and `matches` with `path`, `line`, and a 300-character `text` preview. The query is trimmed, multiple words form one phrase, and no matches exits 0.
- `/grep [--case|--case-sensitive] [--word] [--regex] <query>` opens a TUI picker over the same search. Discovery runs on a background thread behind a loading panel, typing narrows the loaded matches by path, line, or text, Enter inserts `path:line ` into the composer, Esc clears the filter and then closes, and Up/Down/PageUp/PageDown scroll. The picker caps at 200 matches; when the cap is hit, the footer says to rerun `/grep` for a fresh search. Esc during loading hides the panel, and the search already running finishes in the background. Match text and inserted targets are sanitized before they reach the terminal.
- A trailing `:<line>` on the query is a line target, not part of the filter. `/files pmod rs:12` loads the multi-token matches, shows `line 12` in the heading, and inserts `src/parser/mod.rs:12 ` when Enter is pressed. The same suffix works when typed into the live filter. Line numbers are 1-based; spaces around the colon are ignored; `:0` and `:00` stay part of the query; leading zeros are normalized (`:007` becomes `:7`). A literal colon suffix is written with a backslash, so `app\:12` filters `app:12` with no target.

## How to get to it (user POV)
```sh
baton -C <project> files parser
baton -C <project> files pmod rs --json
baton -C <project> files parser md --ignored --json
VISUAL='code --wait' baton -C <project> open src/main.rs
VISUAL='code --wait' baton -C <project> open src/main.rs:42 --print
VISUAL='code --wait' baton -C <project> open src/main.rs --print

# In the TUI
/files pmod rs:12
# Ctrl+O launches the selected path at line 12
```

## Driving it with the verification harness
```sh
cargo test --locked --lib files:: -- --nocapture
cargo test --locked --test files_cli files_json_reports_paths_limit_query_and_ignored -- --exact
cargo test --locked --test files_cli open_print_uses_the_editor_and_rejects_escape_paths -- --exact
cargo test --locked --test files_cli open_print_passes_line_targets_to_common_editors -- --exact
cargo test --locked --test files_cli open_runs_a_known_editor_with_the_line_argument -- --exact
cargo test --locked --test files_cli open_prefers_visual_over_editor_in_a_real_process -- --exact
cargo test --locked --test files_cli files_scope_to_the_current_directory -- --exact
cargo test --locked --test files_cli files_fall_back_to_git_when_ripgrep_is_missing -- --exact
cargo test --locked --test files_cli empty_pruned_and_ignored_only_projects_list_cleanly -- --exact
cargo test --locked --test files_cli ignored_files_reach_the_default_limit -- --exact
cargo test --locked --test files_cli files_match_subsequence_tokens -- --exact
cargo test --locked --test files_cli file_listings_survive_a_closed_pipe -- --exact
cargo test --locked --test files_cli open_runs_the_editor_and_propagates_failure -- --exact
cargo test --locked --test files_cli json_cwd_renders_a_non_utf8_directory_lossily -- --exact  # Linux only
cargo test --locked --lib store::tests::recent_files -- --nocapture
cargo test --locked --lib store::tests::missing_recent_projects_are_pruned -- --exact
cargo test --locked --lib tui::tests::files_picker -- --nocapture
cargo test --locked --lib tui::tests::files_picker_ctrl_o_prepares_editor_launch -- --exact
cargo test --locked --lib tui::tests::files_picker_ctrl_o_keeps_the_picker_when_setup_fails -- --exact
cargo test --locked --lib tui::tests::files_picker_ctrl_o_uses_a_typed_line_target -- --exact
cargo test --locked --lib tui::tests::files_picker_ctrl_o_waits_for_the_scan -- --exact
cargo test --locked --lib tui::tests::files_picker_sanitizes_paths_insertion_and_launch_notices -- --exact
cargo test --locked --lib grep:: -- --nocapture
cargo test --locked --test grep_cli -- --nocapture
cargo test --locked --lib tui::tests::grep_picker -- --nocapture
cargo test --locked --lib tui::tests::file_row_marks_matches_and_keeps_the_gutter -- --exact
cargo test --locked --lib tui::tests::line_targets_split_from_the_query -- --exact
cargo test --locked --lib tui::tests::files_picker_inserts_a_line_target -- --exact
cargo test --locked --lib files::tests::open_commands_apply_editor_specific_line_syntax -- --exact
cargo test --locked --lib files::tests::split_line_targets_are_shared_by_the_cli_and_picker -- --exact
cargo test --locked --lib cli::tests::files_accepts_query_ignored_and_json -- --exact
cargo test --locked --lib cli::tests::open_accepts_path_and_print -- --exact
.agents/skills/verify-baton/scripts/slash_probe.py --binary target/debug/baton --evidence-dir local_docs/verification/evidence
```

`store::tests::recent_files_upsert_and_order`, `recent_files_are_project_scoped`, `recent_files_are_capped`, `recent_files_survive_reopen`, and `missing_recent_projects_are_pruned` cover the recent-file table. `files::tests::recent_paths_break_ties_without_beating_match_quality` covers the tie-break, `tui::tests::files_picker_prefers_a_recent_file_and_records_the_pick` covers recording and ordering in the picker, and `open_records_a_recent_file_for_files_ranking` proves the CLI path. `grep::tests` covers line matches across files, case, word, and regex flags, the sorted cap, hidden-file search with cache pruning, the `git grep` multi-match parser, binary and gitignored exclusion, empty queries, and missing matches. `grep_falls_back_to_git_when_ripgrep_is_missing` proves the fallback through the built binary, and `grep_picker_sanitizes_match_text_and_insertion` proves escape and bell characters never reach the buffer or composer. `tests/grep_cli.rs` proves the JSON contract, plain output, the flags, no-match success, the empty-query error, and the closed pipe through the built binary. `tui::tests::grep_picker` covers insertion, search flags, typed narrowing, filter reset, the loading panel, a superseded load, usage, and no matches. The PTY `slash_probe.py` also runs `/grep parse` and checks the heading, `src/parser/mod.rs:1`, and the insert notice. `tui::tests::files_picker` covers insertion, line-target insertion, a line target typed in the live filter and cleared by deleting its suffix, typed narrowing, subsequence narrowing, filter reset, scrolling past eight rows, the loading panel, a cancelled load, a superseded load, the cap warning under a filter, the ignored-flag hint, Enter without rows, empty projects, and a missing directory. `files_picker_ctrl_o_prepares_editor_launch` proves Ctrl+O records the picked path, prepares the exact absolute target and line, and closes the picker, while `files_picker_ctrl_o_waits_for_the_scan` keeps the loading picker open. `line_targets_split_from_the_query` covers 1-based parsing, whitespace, `:0`, and non-numeric suffixes. `files::tests::subsequence_queries_match_in_order` covers per-token in-order matches, order-free multi-token matching, rejection when a token misses, case-insensitive matching, substring-over-subsequence ranking, multi-token highlighting, last-occurrence highlighting, and a lowercase-length guard. `files::tests::substring_matches_survive_a_fuzzy_cap` proves both single-token and multi-token substring matches survive cap-clearing fuzzy decoys, and `files_match_subsequence_tokens` proves single-token, multi-token, reversed-token, and miss cases through the built binary. `tui::tests::files_picker_matches_multiple_fuzzy_tokens` proves the picker inserts a multi-token match. `tui::tests::file_row_marks_matches_and_keeps_the_gutter` proves the highlight style differs from both base row styles and the gutter is preserved. The PTY `slash_probe.py` picks `/files pmod rs:12`, reopens `/files ` and checks the picked file as the first recent row, checks the `line 12` heading and `src/parser/mod.rs`, confirms an unmatched `README.md` row is absent, presses Enter, and checks the `inserted src/parser/mod.rs:12` notice. It also reopens the same target, presses Ctrl+O with a fake Code executable, proves `--goto <path>:12` reached the editor, and checks the `opened src/parser/mod.rs:12` notice after the TUI restored. `tests/files_cli.rs` builds real git repositories and drives the built binary. It asserts every JSON field, ignored opt-in, default-limit reachability, empty and pruned-only directories, `VISUAL` precedence with both editor variables set, `-C` scope, the `git ls-files` fallback with a `PATH` that has git but no ripgrep, the editor run path including a nonzero exit and a missing editor, a closed stdout pipe for plain and JSON output, and a lossy JSON `cwd` for a non-UTF-8 directory on Linux. `files::tests` covers ranking, deduplication, the shared match cap, query-first filtering, path containment, editor parsing, Unix backslashes, and cache pruning.

`files_picker_ctrl_o_uses_a_typed_line_target` proves a line typed in the live filter reaches the editor argv. `files_picker_ctrl_o_keeps_the_picker_when_setup_fails` covers no editor, an unclosed quote, a missing executable, a missing file, and a path outside the thread project directory, while `files_picker_ctrl_o_waits_for_the_scan` keeps the loading picker open. `files_picker_sanitizes_paths_insertion_and_launch_notices` proves escape and bell bytes never reach the picker, composer, success label, or launch notice. The PTY probe also runs a failing editor and then `/status`, proving the TUI restores and accepts input after the editor exits nonzero.

`files::tests::open_commands_apply_editor_specific_line_syntax` covers code, code-insiders, codium, cursor, windsurf, Vim, Emacs, nano, Helix, unknown-editor, no-line, `code.exe`, `code.cmd`, and `code.BAT` forms. `files::tests::split_line_targets_are_shared_by_the_cli_and_picker` covers the shared parser. `tests/files_cli.rs` `open_print_passes_line_targets_to_common_editors` proves Code, Vim, and Helix through the built binary, and `open_runs_a_known_editor_with_the_line_argument` proves the real run path passes `--goto` and the line to a Code-family executable.

## Risks and mitigations

- A line target can be stale or beyond the file length. The parser validates only that it is a positive integer; the editor owns clamping. `baton open` reports the exact argv in `--print`, and no semantic-symbol claim is made.
- An unknown editor may not understand `path:line`. Known common editors use their native form, and `baton open --print` shows the exact argv. The TUI has no argv preview, so an unknown editor receives `path:line` without that check.
- A missing editor must fail clearly. `baton open` and TUI Ctrl+O refuse an unset variable, an unclosed quote, and an executable that is not on `PATH`. A configured editor that passes preflight but fails to start closes the picker and reports the post-close error after the terminal is restored.
- Windows wrappers may be named `.exe`, `.cmd`, or `.bat`. The program basename is lowercased and all three suffixes are stripped before mapping.
- A path can escape `-C`. `resolve_project_file` canonicalizes and rejects any target outside the root.
- Ctrl+O launches, but `/grep` still only inserts. The action is explicit rather than hidden in Enter so a user can choose between composing around the path and opening it.
- The editor owns the terminal while it runs. Baton leaves the alternate screen, disables raw mode and capture, restores the exact modes afterward, and forces a full redraw. An editor error is reported after the terminal comes back.
- The TUI event loop is blocked while the editor runs. A `--wait` editor can delay rendering an approval or turn event until it exits; Baton does not drop or cancel those queued events.
- A file name can contain terminal control bytes. `/files` rows, inserted targets, launch labels, and launch errors are sanitized before rendering. The raw path still reaches the local filesystem and editor argv, which is the intended trust boundary.

## Alternatives
Ripgrep-only, repository-root scope, and ignored-last ordering were rejected. Multi-token subsequence matching is implemented with independent, order-free tokens; a joint alignment across tokens and a fuzzy-scoring library were rejected because they add ranking complexity without a demonstrated miss. For the picker, a background load plus a client-side token filter was chosen over synchronous discovery on the TUI loop and over re-running discovery per keystroke. Enter keeps insertion, while Ctrl+O performs the explicit editor launch, so the user can choose either action without a second mode. A hand-rolled per-token greedy matcher was chosen over a fuzzy-scoring library or glob matching: a library adds a dependency and needs its own ranking evaluation, and globs change the query language. Line targets use the inline `:<line>` form with a backslash escape instead of a second prompt. The reasons and reversal conditions live in the `docs/parity.md` alternatives table under File navigation, File discovery inputs, File query matching, File picker interaction, File line targets, File content search, and Recent file ranking.

For Ctrl+O, Baton suspends the current TUI in place and waits for the editor. A second terminal, detached process, tmux window, or embedded viewer was rejected: each adds a placement or lifecycle model, while a wait editor such as Code or Vim already owns the user's attention. A detached GUI editor that returns immediately remains possible, but the user chose that editor configuration and Baton cannot know when its window closes.

Line targets are shared by `/files` and `baton open`; `/grep` produces `path:line` targets from content matches, but its query is a content search and does not parse a line target. The editor mapping is deliberately small and explicit: Code-family editors use `--goto`, Vim/Emacs/nano use `+line`, and Helix/Sublime/Zed plus unknown editors receive the common `path:line` form. A configurable placeholder was rejected for this slice because the known mapping covers the common editors and the fallback remains visible in `--print`.

## Gotchas
- `baton files` uses the global `--limit` for output. A cap hit prints "results truncated at the 20,001-match cap; refine the query"; a limit hit prints "raise --limit".
- `--ignored` makes gitignored files eligible, so build outputs and secrets such as `.env` can appear in terminal output. It is opt-in for that reason.
- CLI discovery and `baton open` are scoped to the current `-C` directory, not the repository root. Running `baton -C repo/src open ../README.md` is rejected. The TUI picker uses the open thread's project directory, which can differ from the process `-C` after `/open`.
- The TUI `/open` still selects threads. `/files` owns project paths, Enter inserts a path or `path:line`, and Ctrl+O launches it with the same editor mapping as `baton open`; unknown editors receive `path:line`.
- A `:line` given on the `/files` command stays for that picker, even after the live filter clears; rerun `/files` without a suffix to drop it.
- The picker loads nonignored files only. `--ignored` remains a CLI option.
- `baton open` does not embed an editor. GUI editors should use a wait flag such as `code --wait` if the terminal should block until the file closes.
- `/files` Ctrl+O resolves the target and preflights the editor executable before closing the picker. An editor that exists at preflight but exits nonzero is reported after the TUI restores, and the composer remains usable.
- Backslash escaping applies only before a quote or whitespace, so Windows paths and UNC prefixes survive parsing. A backslash before a closing quote is still ambiguous on Windows. `--print` quoting is best effort on Windows; the run path uses argv.
- Non-UTF-8 file names are skipped, because the plain and JSON contracts are UTF-8.
- Recency only breaks ties inside a match tier, so a recent weakly matching file never outranks a better match. The list is capped at 200 per canonical `-C` directory, dead project directories are pruned by `baton files`, and a store failure is ignored because recency is a ranking hint.
- Content search respects ignore rules differently by backend: inside a git repository, both respect `.gitignore`. Outside a git repository only ripgrep can run, and it still reads `.ignore` and `.rgignore`; the `git grep` fallback requires a repository and reads neither. Both search hidden project files and prune VCS and cache paths. Matches are sorted by path and line before capping; the CLI cap is the global `--limit`, the TUI cap is 200, and the footer says when it truncated. Regex syntax is the backend's syntax. Esc does not kill an in-flight search.
- Each discovery pass walks the tree once and keeps at most 20,001 matches; `--ignored` runs the primary and ignored passes and merges them under one 20,001 cap. Substring matches are admitted before subsequence-only matches, so fuzzy decoys cannot evict an exact or name match, but a query with more than 20,001 substring matches still hits the cap.
- Query tokens are independent and order-free inside the subsequence tiers. Token order changes neither matching nor highlight positions; the union is sorted and deduplicated. A token that misses both substring and subsequence rejects the path. Typing narrows only the loaded rows, so a path dropped at the cap needs a new `/files <query>` run. The picker shows the cap warning while the inventory is truncated.
