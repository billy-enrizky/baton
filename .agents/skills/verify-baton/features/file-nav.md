# File navigation

## Problem and non-goals
A terminal user has to recall a path or shell-expand one to reference a project file. `baton files` ranks the tree below the current `-C` directory and `baton open` launches the user's editor, so neither task needs a second app. Non-goals in this slice: an embedded editor, content search, line-number targets, fuzzy matching, recent-file ranking, and the TUI picker.

## Sub-features
- `baton files [query...]` lists files below the current `-C` directory with paths relative to it.
- Ripgrep is preferred. `baton files` falls back to `git ls-files` when `rg` is unavailable inside a repository. An empty directory returns an empty list on both backends.
- Hidden project files such as `.github`, `.vscode`, `.idea`, `.husky`, and `.env.example` remain discoverable. Tool caches and build output are pruned: `.git`, `node_modules`, `.next`, `.venv`, package-manager stores, `.local/share`, and the `target` directory directly under `-C`. From the repository root, `src/target` stays discoverable.
- Every query token must appear in the path. A filename that contains every token ranks above a path-only match, a filename that starts with the full query ranks higher, and an exact filename match ranks first.
- Query tokens filter during discovery, so unmatched files do not consume the match cap, but the tree walk and ripgrep output capture are unchanged. The merged match list is capped at 20,001 across both discovery passes before ranking.
- Tracked and untracked nonignored files are listed by default. `--ignored` adds gitignored files to the same ranked pool, so a short `.env` is visible at the default `--limit`; ignored rows beyond a full 20,001-match cap are dropped.
- `--json` returns `schema_version`, an absolute `cwd`, `query`, `ignored`, `limit`, `truncated`, `capped`, `count`, and `files`. `capped` means a match was dropped at the 20,001 cap; `truncated` also covers the global `--limit`. A broken pipe on JSON output exits cleanly.
- `baton open <path>` resolves only files inside the current `-C` directory, prefers `VISUAL` over `EDITOR`, splits quoted editor commands including empty quoted arguments, preserves Windows path separators and Unix backslashes, and waits for the editor process.
- `--print` prints the quoted launch command without starting the editor. Quoting targets the host shell: POSIX single quotes on Unix and best-effort double quotes on Windows.

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
cargo test --locked --test files_cli file_listings_survive_a_closed_pipe -- --exact
cargo test --locked --test files_cli open_runs_the_editor_and_propagates_failure -- --exact
cargo test --locked --test files_cli json_cwd_renders_a_non_utf8_directory_lossily -- --exact  # Linux only
cargo test --locked --lib cli::tests::files_accepts_query_ignored_and_json -- --exact
cargo test --locked --lib cli::tests::open_accepts_path_and_print -- --exact
```

`tests/files_cli.rs` builds real git repositories and drives the built binary. It asserts every JSON field, ignored opt-in, default-limit reachability, empty and pruned-only directories, `VISUAL` precedence with both editor variables set, `-C` scope, the `git ls-files` fallback with a `PATH` that has git but no ripgrep, the editor run path including a nonzero exit and a missing editor, a closed stdout pipe for plain and JSON output, and a lossy JSON `cwd` for a non-UTF-8 directory on Linux. `files::tests` covers ranking, deduplication, the shared match cap, query-first filtering, path containment, editor parsing, Unix backslashes, and cache pruning.

## Alternatives
Ripgrep-only, repository-root scope, ignored-last ordering, and fuzzy matching now were rejected. The reasons and reversal conditions live in the `docs/parity.md` alternatives table under File navigation, File discovery inputs, and File query matching.

## Gotchas
- `baton files` uses the global `--limit` for output. A cap hit prints "results truncated at the 20,001-match cap; refine the query"; a limit hit prints "raise --limit".
- `--ignored` makes gitignored files eligible, so build outputs and secrets such as `.env` can appear in terminal output. It is opt-in for that reason.
- Discovery and `baton open` are scoped to the current `-C` directory, not the repository root. Running `baton -C repo/src open ../README.md` is rejected.
- The TUI `/open` still selects threads. File opening is `baton open` until the `/files` picker slice.
- `baton open` does not embed an editor. GUI editors should use a wait flag such as `code --wait` if the terminal should block until the file closes.
- Backslash escaping applies only before a quote or whitespace, so Windows paths and UNC prefixes survive parsing. A backslash before a closing quote is still ambiguous on Windows. `--print` quoting is best effort on Windows; the run path uses argv.
- Non-UTF-8 file names are skipped, because the plain and JSON contracts are UTF-8.
- A full scan of a very large tree is bounded by the query-first filter and the match cap, but it still walks the tree once per discovery pass.
