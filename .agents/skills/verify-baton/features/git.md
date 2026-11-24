# Git status, commit, and push

## Sub-features
- `/git` opens a picker with Status, Commit all changes, Commit staged changes, Push current branch, and Cancel. Esc closes it.
- `/git status` opens a scrollable repository view with the branch, upstream and ahead/behind counts, in-progress operation, unresolved conflicts, and every changed path with its status code.
- `/git commit [--staged] [--] [message]` opens a scrollable confirmation with the generated or supplied message, the branch, the mode, and every planned path. Enter commits; Esc returns to the picker. `--staged` is recognized only as the first argument, and `--` starts the message.
- `/git push` opens a remote picker, then a confirmation naming the local branch, remote, remote branch, and ahead/behind counts. Enter pushes; Esc returns. The push never forces, never pulls or merges, and does not set an upstream.
- The commit uses the thread's repository root. It refuses an operation in progress, unresolved conflicts, an empty selection, a stale branch, a moved HEAD, or a changed worktree fingerprint. It is blocked while another thread sharing the worktree has open work.

## Problem statement

Baton could commit worker branches during an orchestration run, but a normal thread had no terminal path to inspect the repository and commit its work. The user had to leave the TUI, run git by hand, and return knowing the branch and file state from memory.

## Goals and non-goals

Goals: one `/git` submenu that shows the repository state, prepares a deterministic commit message, confirms every exact path, commits all changes or only staged changes, pushes the current branch to a chosen remote, and reports the short SHA or pushed ref. The flow must refuse an operation, conflict, concurrent worktree work, or stale plan before it mutates the repository or remote.

Non-goals: pull, fetch, pull requests, checks, review, merge, force-push, upstream configuration, amend, rebase, hunk staging, file-level selection, issue linking, or rewriting history. The confirmation does not run an editor for the message. The slice does not change the orchestration worker ship path, which keeps its existing `open_prs_with_gh` implementation.

## Proposed architecture

`src/git_workflow.rs` owns every git read and commit. `inspect` checks `git rev-parse --is-inside-work-tree` for locale-independent repository membership, resolves the root with `--show-toplevel` while stripping only the newline git appends, and reads the branch, upstream, ahead and behind counts, in-progress operation, merge/cherry-pick/revert/rebase/sequencer/bisect marker, and NUL-delimited porcelain status with `--untracked-files=all --no-renames`. The status separates staged, unstaged, and untracked entries, lists every untracked file rather than a directory, and marks any `U`, `AA`, or `DD` entry as an unresolved conflict. A configured upstream whose tracking ref cannot be counted is shown as no upstream instead of a fabricated zero.

`plan_commit` refuses an operation, unresolved conflicts, or an empty selection; chooses the staged files or every changed file; validates a supplied message; and generates `baton: update <file>` for one file or `baton: update N files` for many. The plan stores the branch, the current HEAD, the exact path list, and a git tree fingerprint. The all-changes fingerprint comes from a temporary copy of the current index, filled with `git add -A -- .`, and written with `git write-tree`. The staged fingerprint is the real index tree. Starting from the current index keeps an already-staged force-added ignored file in all-changes mode. The plan does not rename-detect, so a rename is two exact paths.

`commit` inspects again, refuses a changed operation, conflict, branch, HEAD, or path list, recomputes the pre-staging tree and refuses a mismatch before touching the index, stages all changes only in all-changes mode, compares the resulting real index tree, rechecks the branch and HEAD, then runs `git commit --quiet --message` and returns the short SHA. After the commit it compares `HEAD^{tree}` with the plan; a pre-commit hook that changed the tree produces an explicit error naming the created commit rather than a false success. A post-staging mismatch or a failed all-changes commit leaves the index staged; the error says so and the picker refreshes. Git hooks, signing, and identity otherwise remain git's responsibility.

The TUI has three overlays: the picker, a read-only status view, and the confirmation, plus a push target picker and push confirmation. The list views scroll with Up/Down, PageUp/PageDown, Home, and End, and render every path. Their visible-row budgets subtract the operation and conflict warning rows. Branches, paths, messages, and notices pass through `git_line`, which flattens control characters and then applies the terminal sanitizer. The open-work gate refuses while orchestration is starting, enumerates every thread (including the current thread) whose canonical cwd is under the same repository root, and fails closed on a store error. Every commit and push attempt re-runs that gate, so a picker opened while work was open can proceed after it finishes. The composer keeps exact `/git commit` and `/git push` ahead of longer completions. No background thread is used; every `/git` action and inspection is synchronous.

`list_push_targets` reads the configured remotes. `upstream_target` reads `branch.<branch>.remote` and `branch.<branch>.merge` rather than splitting the display string, so a remote named `team/origin` works. A target is marked upstream only when `inspect` resolved the tracking ref and the configured remote and merge branch are known; the target's remote branch comes from the configured merge branch, which may differ from the local branch. A configured upstream whose tracking ref is gone is shown as a non-upstream same-name target, not a fabricated 0/0. `plan_push` refuses a detached HEAD, operation, conflict, missing remote, missing commits, a behind upstream, or an upstream with nothing to push. `push` rechecks branch, HEAD, operation, and conflicts, then runs `git -c push.autoSetupRemote=false -c push.recurseSubmodules=check push --porcelain --no-follow-tags -- <remote> <validated-head>:<full-remote-ref>` with `GIT_TERMINAL_PROMPT=0`. The source is the validated commit, not the mutable branch; the destination is always `refs/heads/<short-branch>`, so a branch literally named `refs/heads/topic` maps to `refs/heads/refs/heads/topic`. `--no-follow-tags` prevents `push.followTags` from publishing unrelated tags, and `push.recurseSubmodules=check` prevents `only` from reporting a push that skipped the superproject. The remote is after `--`, so a remote named `--force` stays a remote. There is no `--force`, `--force-with-lease`, `pull`, `merge`, or `--set-upstream`. The TUI adds a remote picker and a confirmation; the open-work gate runs before the picker and again before the push.

## Alternatives considered

- Shelling out to `git` was chosen over `git2` or `gix`. Hooks, signing, configuration, and exact porcelain behavior come from the user's git; a library would add a dependency and still need to model them.
- A per-checkout lease was not added. The slice uses the durable open-work check across threads sharing the worktree plus HEAD and tree fingerprint checks at commit time. The residual gap is a writer that changes content after the final fingerprint read; a future lease belongs in the durable store, not the TUI.
- Per-file selection was deferred. All changes or the staged index covers the common finish path; Orca's staged/unstaged panel is a larger UI.
- `git add -A` plus `git commit` was chosen over `git commit --only -- <paths>`. The latter changes which index entries are committed and interacts poorly with partially staged files; the explicit staging and fingerprint keep the plan exact.
- A deterministic message was chosen over a model or inline editor. It stays offline and testable; `/git commit <message>` supplies a custom message until an editor exists.
- `--no-renames` was chosen so every path is deterministic. A rename is shown and committed as a deletion plus an addition.
- A remote picker was chosen over implicitly pushing the configured upstream. It makes a first push and a multi-remote repository explicit.
- `git push` without `--set-upstream` was chosen so a push does not silently rewrite tracking configuration. The confirmation says when the branch is not tracked.
- No force mode was added. A non-fast-forward push fails and the user resolves it with a pull or merge outside the flow.

## Implementation scope and sequence

This feature file covers the whole `/git` surface. The memory-to-commit slice is already in `1c5be70`, including `src/git_workflow.rs`, `src/lib.rs`, and the `src/worktree.rs` test fixture. The push slice modifies only `src/git_workflow.rs`, `src/tui/mod.rs`, this file, `.agents/skills/verify-baton/features/README.md`, `.agents/skills/verify-baton/features/slash-commands.md`, `.agents/skills/verify-baton/scripts/slash_probe.py`, `README.md`, `.agents/parity.md` (the D14 row only), `docs/ci.md`, and `ci/test-inventory-floor.txt`. It does not change `src/lib.rs`, `src/worktree.rs`, or the upstream pins.

1. Resolve the exact branch from `git symbolic-ref HEAD`, plan the push against the validated HEAD commit, and build a canonical `refs/heads/<branch>` destination after `--`.
2. Add the Push current branch row, push target picker, push confirmation, open-work gate on every attempt, exact `/git push` completion, and TUI tests.
3. Extend the PTY probe to commit the isolated repository, assert the exact generated subject, exact committed file set, and excluded untracked ignored file, then push to a local bare remote and assert the remote head.
4. Update the README, D14 row, feature map, slash inventory, and test inventory floor; run all 15 formal gates and the CI self-tests.

## Rollback

The commit is a normal git commit. When it has a parent, `git reset --soft HEAD~1` removes it and leaves the changes staged; `git reset --mixed HEAD~1` leaves them unstaged. A root commit has no parent; `git update-ref -d HEAD` removes it and leaves the changes staged. A failed all-changes commit has already staged the worktree, so review `git status` before retrying. The slice creates no database rows or background processes, so reverting the code needs no migration.

A push updates the remote and cannot be undone locally. To retract pushed work, revert the commit and push the revert; Baton does not offer force-push or history rewriting.

## Risks and mitigations

- The commit covers the whole worktree, not only the thread cwd or a subdirectory. The picker header and confirmation say so, and `/git commit --staged` limits the commit to the staged index.
- Untracked ignored files are not discovered or added. A tracked ignored file is shown. A force-added ignored file is shown when staged and is committed in either mode because the plan starts from the current index. The confirmation's warning is mode-specific.
- A supplied message with control characters is refused and capped at 200 characters; the final generated message is also capped.
- A failing hook, signing, or identity check leaves all changes staged in all-changes mode. The notice says so and the picker refreshes.
- Detached HEAD is supported for commit and labeled. Push refuses a detached HEAD; `git switch -c <name>` attaches the work before pushing.
- A writer can change content between the final tree read and `git commit`; the window is tiny but not a lease. The open-work gate blocks the common concurrent case.
- A successful pre-commit hook can change the index. Baton compares the committed tree and reports a hard error naming the commit, but the commit exists; review the hook and reset if needed. Baton does not bypass validating hooks.
- Every `/git` inspection and commit is synchronous. A large repository, many untracked files, a network filesystem, or a slow hook blocks the TUI until git returns.
- Paths and repository roots must be valid UTF-8. `inspect` refuses a non-UTF-8 path instead of collapsing two paths into one replacement-character name.
- A push can update a shared remote. The remote picker and confirmation name the exact ref, a behind upstream or non-fast-forward update is refused by the preflight or git, and no force option exists.
- Git's terminal credential prompt is disabled with `GIT_TERMINAL_PROMPT=0`, but a credential helper, askpass helper, or SSH passphrase prompt may still open and block the TUI. Configure noninteractive credentials before pushing.

## How to get to it (user POV)

Type `/git`. The picker starts on Status and says it covers the whole repository. Use Up/Down or Home/End to move, Enter to run the row, and Esc to close. Enter on Status opens the scrollable status view; Enter on Commit all changes or Commit staged changes opens the confirmation; Enter on Push current branch opens the remote picker. In the confirmation, Up/Down/PageUp/PageDown/Home/End scroll every path, Enter commits, and Esc returns to the picker. In the push picker, choose a remote or Cancel; the push confirmation names the exact ref and never forces. `/git status` opens the status view. `/git commit [--staged] [--] [message]` opens the confirmation directly; `--staged` is only a flag as the first argument, and quotes are literal text joined by single spaces. `/git push` opens the remote picker directly.

## Verification paths

### Unit
```sh
cargo test --locked --lib git_workflow::tests
cargo test --locked --lib tui::tests::git_command_flow
cargo test --locked --lib tui::tests::git_status_uses_the_repository_root_and_closes_with_escape
cargo test --locked --lib tui::tests::git_confirmation_scrolls_past_the_visible_window
cargo test --locked --lib tui::tests::git_commit_is_blocked_by_open_work_in_the_worktree
cargo test --locked --lib tui::tests::git_push_picker_and_push_to_a_local_remote
cargo test --locked --lib tui::tests::git_push_escape_and_no_remote_are_explicit
cargo test --locked --lib tui::tests::git_picker_commit_all_confirms_and_commits
cargo test --locked --lib tui::tests::git_commands_block_while_orchestration_starts
cargo test --locked --lib tui::tests::git_command_outside_a_repository_shows_a_notice
cargo test --locked --lib tui::tests::git_completion_lists_status_and_commit_actions
```

The module tests cover clean and dirty inspection, staged versus unstaged codes, upstream ahead and behind, a missing upstream, merge refusal, every other operation marker including the sequencer, an unmerged index without a marker, untracked ignored exclusion, force-added ignored inclusion, staged-only commits, generated and supplied messages, message validation and the 200-character cap, a changed branch, a changed path set, a content change that does not mutate the index, a moved HEAD, a failing hook that leaves the index staged, a successful hook whose tree change is reported, detached HEAD commit support, a trailing-space repository root, push target ordering, a remote name with a slash, a differently named upstream branch, a gone tracking ref, a real push to a local bare remote under `push.recurseSubmodules=only` and `push.followTags=true`, canonical head-refspec arguments, a branch named `refs/heads/topic`, a remote named `--force`, nothing-to-push and behind-upstream refusal, an actual non-fast-forward rejection, no-remote/detached/operation refusal, unborn and missing-remote refusal, a moved-head push refusal, no upstream auto-setup, and non-UTF-8 path refusal on Linux. The TUI tests cover the picker and Esc, the status overlay from a subdirectory, the repository-root scope, an operation and conflict warning with every-file scrolling, confirmation scrolling, the actual commit from the picker, staged planning, free-form `--staged` text, a worktree-scoped open-work block that clears after the work finishes, an orchestration-start block, an exact `/git commit` through the composer, a push target picker with upstream ahead/behind, a real push confirmation and notice, push cancel/no-remote, a stale push confirmation cancelled after open work starts, a non-repository notice, and completion. Every git fixture pins `main` and disables hooks, signing, `push.autoSetupRemote`, and ambient untracked-file settings in normal cases; hook tests opt back into a local hook.

### PTY
```sh
.agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The probe creates an isolated repository with a configured identity, disabled hooks and signing, an empty `core.excludesFile`, an untracked ignored `.env`, and a local bare remote. It opens `/git`, selects Commit all changes, requires the confirmation to list `src/parser/mod.rs`, commits, then checks the exact subject `baton: update 4 files`, the exact committed set (`AGENTS.md`, `CLAUDE.md`, `README.md`, `src/parser/mod.rs`), and that `.env` is absent. It then opens `/git push`, selects `origin/main`, confirms, checks the pushed notice, and asserts the bare remote head equals the local head.

## Gotchas
- Git must have a configured `user.name` and `user.email`; the probe and tests configure both in their isolated repositories.
- The status uses `--untracked-files=all`, so an untracked directory is listed as its files. A repository with a large untracked tree can make inspection slower.
- The generated message names a file for one change and counts files for many. A supplied message replaces it.
- The plan stores a git tree fingerprint and HEAD. A content change, a new file, or a same-branch commit after confirmation is refused.
- A root commit has no parent; use `git update-ref -d HEAD`, not `HEAD~1`.
- Repository membership uses git's exit status, not an English error string, so localized git still reports the outside-repository notice.
- The push picker marks a target upstream only when its tracking ref resolves and its configured merge branch is known. The target uses the configured remote branch, which may differ from the local branch. A gone tracking ref or a same-name target is shown as `same-name branch · not tracked`, and the push does not set an upstream.
- The tests and probe use a local bare remote, so no network or account is required. The real push disables git's terminal credential prompt.
