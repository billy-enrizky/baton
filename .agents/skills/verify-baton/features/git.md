# Git status and commit

## Sub-features
- `/git` opens a picker with Status, Commit all changes, Commit staged changes, and Cancel. Esc closes it.
- `/git status` opens a scrollable repository view with the branch, upstream and ahead/behind counts, in-progress operation, unresolved conflicts, and every changed path with its status code.
- `/git commit [--staged] [--] [message]` opens a scrollable confirmation with the generated or supplied message, the branch, the mode, and every planned path. Enter commits; Esc returns to the picker. `--staged` is recognized only as the first argument, and `--` starts the message.
- The commit uses the thread's repository root. It refuses an operation in progress, unresolved conflicts, an empty selection, a stale branch, a moved HEAD, or a changed worktree fingerprint. It is blocked while another thread sharing the worktree has open work.

## Problem statement

Baton could commit worker branches during an orchestration run, but a normal thread had no terminal path to inspect the repository and commit its work. The user had to leave the TUI, run git by hand, and return knowing the branch and file state from memory.

## Goals and non-goals

Goals: one `/git` submenu that shows the repository state, prepares a deterministic commit message, confirms every exact path, commits all changes or only staged changes, and reports the short SHA. The flow must refuse an operation, conflict, concurrent worktree work, or stale plan before it mutates the repository.

Non-goals: push, pull, fetch, pull requests, checks, review, merge, amend, rebase, hunk staging, file-level selection, issue linking, or rewriting history. The confirmation does not run an editor for the message. The slice does not change the orchestration worker ship path, which keeps its existing `open_prs_with_gh` implementation.

## Proposed architecture

`src/git_workflow.rs` owns every git read and commit. `inspect` checks `git rev-parse --is-inside-work-tree` for locale-independent repository membership, resolves the root with `--show-toplevel`, and reads the branch, upstream, ahead and behind counts, in-progress operation, merge/cherry-pick/revert/rebase/bisect marker, and NUL-delimited porcelain status with `--untracked-files=all --no-renames`. The status separates staged, unstaged, and untracked entries, lists every untracked file rather than a directory, and marks any `U`, `AA`, or `DD` entry as an unresolved conflict. A configured upstream whose tracking ref cannot be counted is shown as no upstream instead of a fabricated zero.

`plan_commit` refuses an operation, unresolved conflicts, or an empty selection; chooses the staged files or every changed file; validates a supplied message; and generates `baton: update <file>` for one file or `baton: update N files` for many. The plan stores the branch, the current HEAD, the exact path list, and a git tree fingerprint. The all-changes fingerprint comes from a temporary copy of the current index, filled with `git add -A -- .`, and written with `git write-tree`. The staged fingerprint is the real index tree. Starting from the current index keeps an already-staged force-added ignored file in all-changes mode. The plan does not rename-detect, so a rename is two exact paths.

`commit` inspects again, refuses a changed operation, conflict, branch, HEAD, or path list, recomputes the pre-staging tree and refuses a mismatch before touching the index, stages all changes only in all-changes mode, compares the resulting real index tree, then runs `git commit --quiet --message` and returns the short SHA. A post-staging mismatch or a failed all-changes commit leaves the index staged; the error says so and the picker refreshes. Git hooks, signing, and identity remain git's responsibility.

The TUI has three overlays: the picker, a read-only status view, and the confirmation. Both list views scroll with Up/Down, PageUp/PageDown, Home, and End, and render every path. Their visible-row budgets subtract the operation and conflict warning rows. Branches, paths, messages, and notices pass through `git_line`, which flattens control characters and then applies the terminal sanitizer. The open-work gate enumerates every thread whose canonical cwd is under the same repository root, not only the current cwd, and fails closed on a store error. No background thread is used; every `/git` action and inspection is synchronous.

## Alternatives considered

- Shelling out to `git` was chosen over `git2` or `gix`. Hooks, signing, configuration, and exact porcelain behavior come from the user's git; a library would add a dependency and still need to model them.
- A per-checkout lease was not added. The slice uses the durable open-work check across threads sharing the worktree plus HEAD and tree fingerprint checks at commit time. The residual gap is a writer that changes content after the final fingerprint read; a future lease belongs in the durable store, not the TUI.
- Per-file selection was deferred. All changes or the staged index covers the common finish path; Orca's staged/unstaged panel is a larger UI.
- `git add -A` plus `git commit` was chosen over `git commit --only -- <paths>`. The latter changes which index entries are committed and interacts poorly with partially staged files; the explicit staging and fingerprint keep the plan exact.
- A deterministic message was chosen over a model or inline editor. It stays offline and testable; `/git commit <message>` supplies a custom message until an editor exists.
- `--no-renames` was chosen so every path is deterministic. A rename is shown and committed as a deletion plus an addition.

## Implementation scope and sequence

Create `src/git_workflow.rs` and `.agents/skills/verify-baton/features/git.md`. Modify `src/lib.rs`, `src/tui/mod.rs`, `src/worktree.rs` (add `core.hooksPath` and `commit.gpgsign=false` to the test fixture so a developer's global hooks or signing do not leak into tests), `.agents/skills/verify-baton/features/README.md`, `.agents/skills/verify-baton/features/slash-commands.md`, `.agents/skills/verify-baton/scripts/slash_probe.py`, `README.md`, `docs/parity.md` (the D14 row plus the refreshed T3, Orca, Codex, and Baton pins), `docs/ci.md`, and `ci/test-inventory-floor.txt`.

1. Add `GitStatus`, `GitChange`, `CommitMode`, `CommitPlan`, `CommitOutcome`, repository and status helpers.
2. Add operation and conflict refusal, HEAD and tree fingerprinting, message generation, commit revalidation, and module tests.
3. Add the `/git` command, completions, picker, scrollable status and confirmation, open-work gate, rendering, and TUI tests.
4. Extend the PTY probe to commit the isolated repository and assert the exact generated subject, exact committed file set, and excluded untracked ignored file.
5. Update the README, parity row and pins, feature map, slash inventory, and test inventory floor; run all 15 formal gates and the CI self-tests.

## Rollback

The commit is a normal git commit. When it has a parent, `git reset --soft HEAD~1` removes it and leaves the changes staged; `git reset --mixed HEAD~1` leaves them unstaged. A root commit has no parent; `git update-ref -d HEAD` removes it and leaves the changes staged. A failed all-changes commit has already staged the worktree, so review `git status` before retrying. The slice creates no database rows or background processes, so reverting the code needs no migration.

## Risks and mitigations

- The commit covers the whole worktree, not only the thread cwd or a subdirectory. The picker header and confirmation say so, and `/git commit --staged` limits the commit to the staged index.
- Untracked ignored files are not discovered or added. A tracked ignored file is shown. A force-added ignored file is shown when staged and is committed in either mode because the plan starts from the current index. The confirmation's warning is mode-specific.
- A supplied message with control characters is refused and capped at 200 characters; the final generated message is also capped.
- A failing hook, signing, or identity check leaves all changes staged in all-changes mode. The notice says so and the picker refreshes.
- Detached HEAD is supported and labeled, but the commit is not attached to a branch. `git switch -c <name>` keeps it; push remains out of scope.
- A writer can change content between the final tree read and `git commit`; the window is tiny but not a lease. The open-work gate blocks the common concurrent case.
- Every `/git` inspection and commit is synchronous. A large repository, many untracked files, a network filesystem, or a slow hook blocks the TUI until git returns.
- Paths and repository roots must be valid UTF-8. `inspect` refuses a non-UTF-8 path instead of collapsing two paths into one replacement-character name.

## How to get to it (user POV)

Type `/git`. The picker starts on Status and says it covers the whole repository. Use Up/Down or Home/End to move, Enter to run the row, and Esc to close. Enter on Status opens the scrollable status view; Enter on Commit all changes or Commit staged changes opens the confirmation. In the confirmation, Up/Down/PageUp/PageDown/Home/End scroll every path, Enter commits, and Esc returns to the picker. `/git status` opens the same status view. `/git commit [--staged] [--] [message]` opens the confirmation directly; `--staged` is only a flag as the first argument, and quotes are literal text joined by single spaces.

## Verification paths

### Unit
```sh
cargo test --locked --lib git_workflow::tests
cargo test --locked --lib tui::tests::git_command_flow
cargo test --locked --lib tui::tests::git_status_uses_the_repository_root_and_closes_with_escape
cargo test --locked --lib tui::tests::git_confirmation_scrolls_past_the_visible_window
cargo test --locked --lib tui::tests::git_commit_is_blocked_by_open_work_in_the_worktree
cargo test --locked --lib tui::tests::git_command_outside_a_repository_shows_a_notice
cargo test --locked --lib tui::tests::git_completion_lists_status_and_commit_actions
```

The module tests cover clean and dirty inspection, staged versus unstaged codes, upstream ahead and behind, a missing upstream, merge refusal, every other operation marker, an unmerged index without a marker, untracked ignored exclusion, force-added ignored inclusion, staged-only commits, generated and supplied messages, message validation and the 200-character cap, a changed branch, a changed path set, a content change that does not mutate the index, a moved HEAD, a failing hook that leaves the index staged, detached HEAD commit support, and non-UTF-8 path refusal on Linux. The TUI tests cover the picker and Esc, the status overlay from a subdirectory, the repository-root scope, an operation and conflict warning with every-file scrolling, confirmation scrolling, the actual commit, staged planning, free-form `--staged` text, a worktree-scoped open-work block, a non-repository notice, and completion. Every git fixture pins `main` and disables hooks, signing, and ambient untracked-file settings.

### PTY
```sh
.agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The probe creates an isolated repository with a configured identity, disabled hooks and signing, and an untracked ignored `.env`. It opens `/git`, selects Commit all changes, requires the confirmation to list `src/parser/mod.rs`, commits, then checks the exact subject `baton: update 4 files`, the exact committed set, and that `.env` is absent. It also runs `ci/test-inventory.sh` elsewhere in the slice verification.

## Gotchas
- Git must have a configured `user.name` and `user.email`; the probe and tests configure both in their isolated repositories.
- The status uses `--untracked-files=all`, so an untracked directory is listed as its files. A repository with a large untracked tree can make inspection slower.
- The generated message names a file for one change and counts files for many. A supplied message replaces it.
- The plan stores a git tree fingerprint and HEAD. A content change, a new file, or a same-branch commit after confirmation is refused.
- A root commit has no parent; use `git update-ref -d HEAD`, not `HEAD~1`.
- Repository membership uses git's exit status, not an English error string, so localized git still reports the outside-repository notice.
