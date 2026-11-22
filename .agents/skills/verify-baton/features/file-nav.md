# File navigation

## Problem and non-goals
A terminal user has to recall a path or shell-expand one to reference a project file, and a plain path still leaves the user to scroll to a symbol or compiler location. `baton files` ranks the tree below the current `-C` directory, `/files` inserts a picked path or `path:line` into the TUI composer, and `baton open` launches the user's editor, so none of the tasks needs a second app. Non-goals in this slice: an embedded editor, content search, multi-token subsequence matching, recent-file ranking, launching the editor from the picker, and editor-specific line launching from `baton open`.

## Sub-features
- `baton files [query...]` lists files below the current `-C` directory with paths relative to it.
- Ripgrep is preferred. `baton files` falls back to `git ls-files` when `rg` is unavailable inside a repository. An empty directory returns an empty list on both backends.
- Hidden project files such as `.github`, `.vscode`, `.idea`, `.husky`, and `.env.example` remain discoverable. Tool caches and build output are pruned: `.git`, `node_modules`, `.next`, `.venv`, package-manager stores, `.local/share`, and the `target` directory directly under `-C`. From the repository root, `src/target` stays discoverable.
- Every query token must appear in the path. Ranking tiers are: exact filename, filename prefix, every token in the filename, every token in the path, filename subsequence, then path subsequence. A single-token query that matches no substring falls back to an in-order subsequence match, so `pmod` finds `src/parser/mod.rs`; the subsequence may cross path separators and is case-insensitive. Multi-token queries still require every token as a substring. Matched characters are highlighted in the picker, including when the picker opens from `/files <query>`, and shorter paths win inside the two subsequence tiers.
- Query tokens filter during discovery, so unmatched files do not consume the match cap, but the tree walk and ripgrep output capture are unchanged. The merged match list is capped at 20,001 across both discovery passes before ranking.
- Tracked and untracked nonignored files are listed by default. `--ignored` adds gitignored files to the same ranked pool, so a short `.env` is visible at the default `--limit`; ignored rows beyond a full 20,001-match cap are dropped.
- `--json` returns `schema_version`, an absolute `cwd`, `query`, `ignored`, `limit`, `truncated`, `capped`, `count`, and `files`. `capped` means a match was dropped at the 20,001 cap; `truncated` also covers the global `--limit`. A broken pipe on JSON output exits cleanly.
- `baton open <path>` resolves only files inside the current `-C` directory, prefers `VISUAL` over `EDITOR`, splits quoted editor commands including empty quoted arguments, preserves Windows path separators and Unix backslashes, and waits for the editor process.
- `--print` prints the quoted launch command without starting the editor. Quoting targets the host shell: POSIX single quotes on Unix and best-effort double quotes on Windows.
- `/files [query]` opens a TUI picker over the capped, nonignored inventory for the current thread's project directory. Discovery runs on a background thread behind a loading panel, so the TUI keeps drawing and Esc always closes while loading. Enter inserts the selected path and a trailing space into the composer, typing narrows the loaded rows, Backspace edits the filter, Esc clears the filter and then closes, and Up/Down/PageUp/PageDown scroll. A load that finishes after the picker closes or is replaced is ignored. `/files --ignored` is rejected with a pointer to `baton files --ignored`.
- A trailing `:<line>` on the query is a line target, not part of the filter. `/files pmod:12` loads the `pmod` matches, shows `line 12` in the heading, and inserts `src/parser/mod.rs:12 ` when Enter is pressed. The same suffix works when typed into the live filter. Line numbers are 1-based; spaces around the colon are ignored; `:0` and `:00` stay part of the query; leading zeros are normalized (`:007` becomes `:7`). A literal colon suffix is written with a backslash, so `app\:12` filters `app:12` with no target.

## How to get to it (user POV)
```sh
baton -C <project> files parser
baton -C <project> files parser md --ignored --json
VISUAL='code --wait' baton -C <project> open src/main.rs
baton -C <project> open src/main.rs --print
```

## Driving it with the verification harness
```sh
cargo test --locked --lib files:: -- --nocapture
cargo test --locked --test files_cli files_json_reports_paths_limit_query_and_ignored -- --exact
cargo test --locked --test files_cli open_print_uses_the_editor_and_rejects_escape_paths -- --exact
cargo test --locked --test files_cli open_prefers_visual_over_editor_in_a_real_process -- --exact
cargo test --locked --test files_cli files_scope_to_the_current_directory -- --exact
cargo test --locked --test files_cli files_fall_back_to_git_when_ripgrep_is_missing -- --exact
cargo test --locked --test files_cli empty_pruned_and_ignored_only_projects_list_cleanly -- --exact
cargo test --locked --test files_cli ignored_files_reach_the_default_limit -- --exact
cargo test --locked --test files_cli files_match_a_single_token_subsequence -- --exact
cargo test --locked --test files_cli file_listings_survive_a_closed_pipe -- --exact
cargo test --locked --test files_cli open_runs_the_editor_and_propagates_failure -- --exact
cargo test --locked --test files_cli json_cwd_renders_a_non_utf8_directory_lossily -- --exact  # Linux only
cargo test --locked --lib tui::tests::files_picker -- --nocapture
cargo test --locked --lib tui::tests::file_row_marks_matches_and_keeps_the_gutter -- --exact
cargo test --locked --lib tui::tests::line_targets_split_from_the_query -- --exact
cargo test --locked --lib tui::tests::files_picker_inserts_a_line_target -- --exact
cargo test --locked --lib cli::tests::files_accepts_query_ignored_and_json -- --exact
cargo test --locked --lib cli::tests::open_accepts_path_and_print -- --exact
```

`tui::tests::files_picker` covers insertion, line-target insertion, a line target typed in the live filter and cleared by deleting its suffix, typed narrowing, subsequence narrowing, filter reset, scrolling past eight rows, the loading panel, a cancelled load, a superseded load, the cap warning under a filter, the ignored-flag hint, Enter without rows, empty projects, and a missing directory. `line_targets_split_from_the_query` covers 1-based parsing, whitespace, `:0`, and non-numeric suffixes. `files::tests::subsequence_queries_match_in_order` covers in-order matches, out-of-order rejection, multi-token exclusion, case-insensitive matching, substring-over-subsequence ranking, last-occurrence highlighting, and a lowercase-length guard. `files::tests::substring_matches_survive_a_fuzzy_cap` proves a substring match survives a cap-clearing set of fuzzy decoys, and `files_match_a_single_token_subsequence` proves it through the built binary. `tui::tests::file_row_marks_matches_and_keeps_the_gutter` proves the highlight style differs from both base row styles and the gutter is preserved. The PTY `slash_probe.py` opens `/files pmod:12`, checks the `line 12` heading and `src/parser/mod.rs`, confirms an unmatched `README.md` row is absent, presses Enter, and checks the `inserted src/parser/mod.rs:12` notice. `tests/files_cli.rs` builds real git repositories and drives the built binary. It asserts every JSON field, ignored opt-in, default-limit reachability, empty and pruned-only directories, `VISUAL` precedence with both editor variables set, `-C` scope, the `git ls-files` fallback with a `PATH` that has git but no ripgrep, the editor run path including a nonzero exit and a missing editor, a closed stdout pipe for plain and JSON output, and a lossy JSON `cwd` for a non-UTF-8 directory on Linux. `files::tests` covers ranking, deduplication, the shared match cap, query-first filtering, path containment, editor parsing, Unix backslashes, and cache pruning.

## Alternatives
Ripgrep-only, repository-root scope, ignored-last ordering, and multi-token subsequence matching were rejected; a single-token subsequence fallback is implemented. For the picker, a background load plus a client-side token filter was chosen over synchronous discovery on the TUI loop and over re-running discovery per keystroke; insert into the composer was chosen over launching the editor from the picker. A hand-rolled greedy matcher was chosen over a fuzzy-scoring library or glob matching: a library adds a dependency and needs its own ranking evaluation, and globs change the query language. Line targets use the inline `:<line>` form with a backslash escape instead of a second prompt. The reasons and reversal conditions live in the `docs/parity.md` alternatives table under File navigation, File discovery inputs, File query matching, File picker interaction, and File line targets.

## Gotchas
- `baton files` uses the global `--limit` for output. A cap hit prints "results truncated at the 20,001-match cap; refine the query"; a limit hit prints "raise --limit".
- `--ignored` makes gitignored files eligible, so build outputs and secrets such as `.env` can appear in terminal output. It is opt-in for that reason.
- Discovery and `baton open` are scoped to the current `-C` directory, not the repository root. Running `baton -C repo/src open ../README.md` is rejected.
- The TUI `/open` still selects threads. `/files` owns project paths, and `baton open` launches the editor. The picker inserts a path, appending `:<line>` when the query ends in one, and it does not launch the editor. `baton open` still takes a bare path and does not accept a line suffix.
- A `:line` given on the `/files` command stays for that picker, even after the live filter clears; rerun `/files` without a suffix to drop it.
- The picker loads nonignored files only. `--ignored` remains a CLI option.
- `baton open` does not embed an editor. GUI editors should use a wait flag such as `code --wait` if the terminal should block until the file closes.
- Backslash escaping applies only before a quote or whitespace, so Windows paths and UNC prefixes survive parsing. A backslash before a closing quote is still ambiguous on Windows. `--print` quoting is best effort on Windows; the run path uses argv.
- Non-UTF-8 file names are skipped, because the plain and JSON contracts are UTF-8.
- Discovery walks the tree once and keeps at most 20,001 matches. Substring matches are admitted before subsequence-only matches, so fuzzy decoys cannot evict an exact or name match, but a query with more than 20,001 substring matches still hits the cap. Typing narrows only the loaded rows, so a path dropped at the cap needs a new `/files <query>` run. The picker shows the cap warning while the inventory is truncated.
