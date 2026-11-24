# Git status, commit, push, checks, and pull request

## Sub-features
- `/git` opens a picker with Status, Commit all changes, Commit staged changes, Push current branch, Checks, Open pull request, and Cancel. Esc closes it.
- `/git status` opens a scrollable repository view with the branch, upstream and ahead/behind counts, in-progress operation, unresolved conflicts, and every changed path with its status code.
- `/git commit [--staged] [--] [message]` opens a scrollable confirmation with the generated or supplied message, the branch, the mode, and every planned path. Enter commits; Esc returns to the picker. `--staged` is recognized only as the first argument, and `--` starts the message.
- `/git push` opens a remote picker, then a confirmation naming the local branch, remote, remote branch, and ahead/behind counts. Enter pushes; Esc returns. The push never forces, never pulls or merges, and does not set an upstream.
- Checks, or `/git checks`, opens the current pull request from `gh` and lists every check with its bucket, state, and description. The PR number, title, state, mergeability, head branch, and URL sit above the check rows. Up/Down, PageUp/PageDown, Home, and End scroll; Esc closes.
- `/git pr [title]` opens a confirmation naming the `origin` repository, source branch, base branch, commit count, title, base and head SHAs, and a body that lists the newest commits first with the count of earlier commits the body cap left out. Rows wrap by display width. Enter runs `gh pr create`; Esc returns to the picker.
- The commit uses the thread's repository root. It refuses an operation in progress, unresolved conflicts, an empty selection, a stale branch, a moved HEAD, or a changed worktree fingerprint. It is blocked while another thread sharing the worktree has open work.
- Pull request creation refuses a detached HEAD, an operation, conflicts, a missing `origin`, a branch that is not pushed at HEAD, no commits ahead of the base, an existing pull request, a missing authenticated `gh`, or a moved HEAD, base branch, or source branch after confirmation.

## Problem statement

Baton could commit and push worker branches during an orchestration run, but a normal thread had no terminal path to inspect the repository, commit its work, or finish the delivery loop. The user had to leave the TUI, run git and `gh` by hand, and return knowing the branch, remote, and review state from memory.

## Goals and non-goals

Goals: one `/git` submenu that shows the repository state, prepares a deterministic commit message, confirms every exact path, commits all changes or only staged changes, pushes the current branch to a chosen remote, lists pull request checks, and opens a pull request from a previewed title and body. The flow must refuse an operation, conflict, concurrent worktree work, stale plan, unpushed branch, existing pull request, or moved HEAD before it mutates the repository, remote, or forge.

Non-goals: pull, fetch, pull request review, merge, draft toggling, reviewers, labels, multi-forge routing, force-push, upstream configuration, amend, rebase, hunk staging, file-level selection, issue linking, or rewriting history. The confirmation does not run an editor for the title or body. The slice does not change the orchestration worker ship path, which keeps its existing `open_prs_with_gh` implementation.

## Proposed architecture

`src/git_workflow.rs` owns every git read and commit. `inspect` checks `git rev-parse --is-inside-work-tree` for locale-independent repository membership, resolves the root with `--show-toplevel` while stripping only the newline git appends, and reads the branch, upstream, ahead and behind counts, in-progress operation, merge/cherry-pick/revert/rebase/sequencer/bisect marker, and NUL-delimited porcelain status with `--untracked-files=all --no-renames`. The status separates staged, unstaged, and untracked entries, lists every untracked file rather than a directory, and marks any `U`, `AA`, or `DD` entry as an unresolved conflict. A configured upstream whose tracking ref cannot be counted is shown as no upstream instead of a fabricated zero.

`plan_commit` refuses an operation, unresolved conflicts, or an empty selection; chooses the staged files or every changed file; validates a supplied message; and generates `baton: update <file>` for one file or `baton: update N files` for many. The plan stores the branch, the current HEAD, the exact path list, and a git tree fingerprint. The all-changes fingerprint comes from a temporary copy of the current index, filled with `git add -A -- .`, and written with `git write-tree`. The staged fingerprint is the real index tree. Starting from the current index keeps an already-staged force-added ignored file in all-changes mode. The plan does not rename-detect, so a rename is two exact paths.

`commit` inspects again, refuses a changed operation, conflict, branch, HEAD, or path list, recomputes the pre-staging tree and refuses a mismatch before touching the index, stages all changes only in all-changes mode, compares the resulting real index tree, rechecks the branch and HEAD, then runs `git commit --quiet --message` and returns the short SHA. After the commit it compares `HEAD^{tree}` with the plan; a pre-commit hook that changed the tree produces an explicit error naming the created commit rather than a false success. A post-staging mismatch or a failed all-changes commit leaves the index staged; the error says so and the picker refreshes. Git hooks, signing, and identity otherwise remain git's responsibility.

The TUI has three overlays: the picker, a read-only status view, and the confirmation, plus a push target picker and push confirmation. The list views scroll with Up/Down, PageUp/PageDown, Home, and End, and render every path. Their visible-row budgets subtract the operation and conflict warning rows. Branches, paths, messages, and notices pass through `git_line`, which flattens control characters and then applies the terminal sanitizer. The open-work gate refuses while orchestration is starting, enumerates every thread (including the current thread) whose canonical cwd is under the same repository root, and fails closed on a store error. Every commit and push attempt re-runs that gate, so a picker opened while work was open can proceed after it finishes. The composer keeps exact `/git commit` and `/git push` ahead of longer completions. No background thread is used; every `/git` action and inspection is synchronous.

`list_push_targets` reads the configured remotes. `upstream_target` reads `branch.<branch>.remote` and `branch.<branch>.merge` rather than splitting the display string, so a remote named `team/origin` works. A target is marked upstream only when `inspect` resolved the tracking ref and the configured remote and merge branch are known; the target's remote branch comes from the configured merge branch, which may differ from the local branch. A configured upstream whose tracking ref is gone is shown as a non-upstream same-name target, not a fabricated 0/0. `plan_push` refuses a detached HEAD, operation, conflict, missing remote, missing commits, a behind upstream, or an upstream with nothing to push. `push` rechecks branch, HEAD, operation, and conflicts, then runs `git -c push.autoSetupRemote=false -c push.recurseSubmodules=check push --porcelain --no-follow-tags -- <remote> <validated-head>:<full-remote-ref>` with `GIT_TERMINAL_PROMPT=0`. The source is the validated commit, not the mutable branch; the destination is always `refs/heads/<short-branch>`, so a branch literally named `refs/heads/topic` maps to `refs/heads/refs/heads/topic`. `--no-follow-tags` prevents `push.followTags` from publishing unrelated tags, and `push.recurseSubmodules=check` prevents `only` from reporting a push that skipped the superproject. The remote is after `--`, so a remote named `--force` stays a remote. There is no `--force`, `--force-with-lease`, `pull`, `merge`, or `--set-upstream`. The TUI adds a remote picker and a confirmation; the open-work gate runs before the picker and again before the push.

`checks` resolves the repository root, runs `gh pr view --json number,title,url,state,mergeable,headRefName,isDraft`, then `gh pr checks <number> --json name,state,bucket,description,link`. A pending checks exit code 8 is a successful report, and a `no checks reported` message becomes an empty list. The path can be overridden with `BATON_GH_BIN`. The TUI shows the PR header and up to 200 checks, 12 rows at a time, with a pass/fail/pending marker and the description.

`plan_pr` inspects the repository, refuses a detached HEAD, an operation, or conflicts, requires `origin`, and reads `nameWithOwner` and the base from `gh repo view <origin-url> --json nameWithOwner,defaultBranchRef`. `remote_url_for_gh` lowercases only an `http://` or `https://` scheme, strips a userinfo component only for those schemes, and returns every other URL unchanged, so `ssh://git@host/repo` and `git@host:repo` reach `gh` intact and an HTTP token never appears in a notice or an error. It treats `no pull requests found` from `gh pr view <branch> --repo <owner/name> --json url,number,state` as no existing PR and refuses any other successful result. It requires commits ahead of `origin/<base>`, records the cached `origin/<base>` SHA as `base_head`, and requires the cached `refs/remotes/origin/<branch>` to resolve to the current HEAD, so a local plan cannot target a stale branch. The commit list is one bounded `git log -n 20 --format=%s <range>` in git's default newest-first order. `pull_request_text` sanitizes every subject, keeps the newest rows, and when the `## Summary` body would exceed 4,000 characters it drops the oldest listed subject and reports the remaining count as `- and N earlier commit(s)`, where `N` is the total ahead count minus the listed rows. The title is the supplied title, then the actual HEAD subject, then `Baton changes`; it never borrows an earlier commit's subject and is capped at 200 characters.

`create_pr` rechecks the branch, operation, conflicts, and HEAD, then runs one `git ls-remote --heads <remote> refs/heads/<base> refs/heads/<branch>` with `GIT_TERMINAL_PROMPT=0`. The read has no `--exit-code`, so a missing branch is reported by name instead of as a generic failure. It refuses creation when the live base tip differs from `base_head` or the live source tip differs from the previewed HEAD, naming the branch that moved. The remote read is killed after 30 seconds. It then runs `gh pr create --repo <owner/name> --base <base> --head <branch> --title <title> --body <body>`, so `gh repo set-default` cannot redirect the action. Every `gh` read and creation is killed after 60 seconds. On Unix each child is a process-group leader and a timeout kills the whole group, then bounds output-reader cleanup; on other platforms the direct child is killed. It reads the last `https://` line as the URL and parses the trailing number for the notice. The TUI keeps the push confirmation and pull request confirmation separate so the user sees each remote mutation before it happens.

## Alternatives considered

- Shelling out to `git` was chosen over `git2` or `gix`. Hooks, signing, configuration, and exact porcelain behavior come from the user's git; a library would add a dependency and still need to model them.
- A per-checkout lease was not added. The slice uses the durable open-work check across threads sharing the worktree plus HEAD and tree fingerprint checks at commit time. The residual gap is a writer that changes content after the final fingerprint read; a future lease belongs in the durable store, not the TUI.
- Per-file selection was deferred. All changes or the staged index covers the common finish path; a staged/unstaged panel is a larger UI.
- `git add -A` plus `git commit` was chosen over `git commit --only -- <paths>`. The latter changes which index entries are committed and interacts poorly with partially staged files; the explicit staging and fingerprint keep the plan exact.
- A deterministic message was chosen over a model or inline editor. It stays offline and testable; `/git commit <message>` supplies a custom message until an editor exists.
- `--no-renames` was chosen so every path is deterministic. A rename is shown and committed as a deletion plus an addition.
- A remote picker was chosen over implicitly pushing the configured upstream. It makes a first push and a multi-remote repository explicit.
- `git push` without `--set-upstream` was chosen so a push does not silently rewrite tracking configuration. The confirmation says when the branch is not tracked.
- No force mode was added. A non-fast-forward push fails and the user resolves it with a pull or merge outside the flow.
- `gh` was chosen for checks and pull request creation because the repository already uses it for worker pull requests. A forge abstraction waits for a multi-forge slice.
- Pull request creation requires the branch to be pushed at HEAD instead of pushing implicitly. The user sees the remote mutation in `/git push`; creation rereads the live branch immediately before `gh pr create` and refuses when it moved.
- The title defaults to the actual HEAD subject and the body is the newest-first commit list with an explicit earlier-commit count. Both are visible in the confirmation before `gh pr create`; a supplied `/git pr <title>` replaces only the title.
- The base and source branches are both reread immediately before creation instead of trusting the cached remote-tracking refs. A commit pushed to either branch between the preview and confirmation is a real race that would change what the pull request contains, so the slice refuses and asks for a new preview.
- The commit log is bounded at 20 rows at the git level instead of reading the whole history and truncating in memory. The remainder count comes from the already-computed ahead count, so a large branch costs one short log call.

## Implementation scope and sequence

This feature file covers the whole `/git` surface. The memory-to-commit slice landed in `1c5be70`; push landed in `b03db19`; checks landed in `322efd8`; pull request creation and its remote bounds landed in `e47fab8`, `4aa05a7`, and `0fdce18`; the exact preview landed in this slice.

Implementation scope (the allowlist for a `/git` change): `src/git_workflow.rs`, `src/tui/mod.rs`, `Cargo.toml`, `Cargo.lock`, this file, `.agents/skills/verify-baton/features/README.md`, `.agents/skills/verify-baton/features/slash-commands.md`, `.agents/skills/verify-baton/scripts/slash_probe.py`, `.agents/skills/verify-baton/scripts/slash_sweep.py`, `.agents/skills/verify-baton/scripts/tui_probe.py`, `README.md`, `install.md`, `docs/ci.md`, `ci/test-inventory-floor.txt`, `.agents/parity.md` (the D14 row only), and the acceptance record under the gitignored `local_docs/verification/`. It does not change `src/lib.rs`, `src/worktree.rs`, or the upstream pins.

1. Resolve the exact branch from `git symbolic-ref HEAD`, plan the push against the validated HEAD commit, and build a canonical `refs/heads/<branch>` destination after `--`.
2. Add the Push current branch row, push target picker, push confirmation, open-work gate on every attempt, exact `/git push` completion, and TUI tests.
3. Extend the PTY probe to commit the isolated repository, assert the exact generated subject, exact committed file set, and excluded untracked ignored file, then push to a local bare remote and assert the remote head.
4. Add the Checks row, resolve the PR with `gh pr view`, list checks with `gh pr checks`, render the scrollable check view, and cover a fake `gh` in the unit and PTY probes.
5. Add the Open pull request row and `/git pr [title]`, plan the base and body, require the branch at HEAD on `origin`, preview the plan, create through `gh pr create`, and recheck HEAD before submission.
6. Update the README, install guide, D14 row, feature map, slash inventory, and test inventory floor; run the affected checks and the CI self-tests.
7. Bound every remote and `gh` child with a timeout, make each Unix child a process-group leader, and kill the whole group on timeout; bound output-reader cleanup so a pipe held by a grandchild cannot hang the TUI.
8. Record the cached base SHA in the plan and reread both live remote branches immediately before creation; refuse a moved base or source branch and prove with a separate bare remote that `gh pr create` never runs.
9. Read at most 20 commit subjects in newest-first order, drop the oldest listed subject when the body exceeds 4,000 characters, and report the remainder as earlier commits; take the title from the actual HEAD subject.
10. Wrap preview rows by display width with `unicode-width` so wide glyphs do not overflow the confirmation.
11. Extend the unit tests with the remote-move and base-move refusals, the newest-first body and remainder count, the HTTP(S)-only userinfo stripping with preserved SSH URLs, and a wide-glyph wrap case; rerun the probe and the sweep against the rebuilt and reinstalled binary.

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
- A network or credential helper can still block the synchronous TUI while `git ls-remote` or `gh` runs. The remote read is bounded at 30 seconds and every `gh` call at 60 seconds. Unix process groups are killed on timeout; output readers get one additional second to close. Cancellation and background progress remain future work.
- Checks needs `gh` on PATH and an open pull request for the current branch. A missing CLI, no pull request, or a non-GitHub remote produces a notice and leaves the picker unchanged. Pending checks use exit code 8 as a valid report, and `no checks reported` is an empty list.
- The checks view is read-only and capped at 200 rows. A check description is capped at 500 characters; names, states, descriptions, and URLs pass through `git_line` before rendering.
- Pull request creation needs an authenticated `gh`, a GitHub `origin`, a pushed branch at HEAD, commits ahead of the base, and no existing PR for the branch. Failures leave the picker usable and name the next action.
- The default base comes from `gh repo view`; a fork whose upstream base differs from the origin default needs the remote and base resolved outside this slice. The confirmation always names the remote and base it will use.
- The origin remote URL is passed to `gh repo view`, and its `nameWithOwner` is passed to every pull request lookup and creation. Another configured default repository cannot redirect the flow.
- An HTTP(S) origin URL has its userinfo removed before it reaches `gh`, so an embedded token is not placed in the child argv or error notice. SSH origins keep their remote syntax.
- `git ls-remote` adds one remote read before creation and can still race the `gh pr create` call. The race is narrowed, not removed; a remote change after the final read is the remaining window.
- The generated title and body are capped and sanitized. A multi-line custom title is collapsed to one line; control characters are replaced before it reaches `gh`.
- `gh pr create` can still reject a valid plan because of organization policy, protected branches, missing permissions, or a stale remote. Baton reports the `gh` error and does not retry with weaker flags.

## How to get to it (user POV)

Type `/git`. The picker starts on Status and says it covers the whole repository. Use Up/Down or Home/End to move, Enter to run the row, and Esc to close. Enter on Status opens the scrollable status view; Enter on Commit all changes or Commit staged changes opens the confirmation; Enter on Push current branch opens the remote picker; Enter on Checks opens the current PR's check list; Enter on Open pull request opens the title and body confirmation. In the confirmation, Up/Down/PageUp/PageDown/Home/End scroll every path, Enter commits, and Esc returns to the picker. In the push picker, choose a remote or Cancel; the push confirmation names the exact ref and never forces. In the checks view, Up/Down/PageUp/PageDown/Home/End scroll and Esc closes. In the pull request confirmation, the repository, branch, title, and HEAD stay visible while long header and body rows wrap; Up/Down/PageUp/PageDown/Home/End scroll the body, Enter creates, and Esc returns. `/git status` opens the status view. `/git commit [--staged] [--] [message]` opens the confirmation directly; `--staged` is only a flag as the first argument, and quotes are literal text joined by single spaces. `/git push` opens the remote picker directly. `/git checks` opens the checks view directly. `/git pr [title]` opens the pull request confirmation directly.

## Verification paths

### Unit
```sh
cargo test --locked --lib git_workflow::tests
cargo test --locked --lib tui::tests::git_command_flow
cargo test --locked --lib tui::tests::git_checks_picker_shows_the_current_pr_and_checks
cargo test --locked --lib tui::tests::git_pr_confirm_creates_the_planned_pull_request
cargo test --locked --lib tui::tests::git_status_uses_the_repository_root_and_closes_with_escape
cargo test --locked --lib tui::tests::git_confirmation_scrolls_past_the_visible_window
cargo test --locked --lib tui::tests::git_commit_is_blocked_by_open_work_in_the_worktree
cargo test --locked --lib tui::tests::git_push_picker_and_push_to_a_local_remote
cargo test --locked --lib tui::tests::git_push_escape_and_no_remote_are_explicit
cargo test --locked --lib tui::tests::git_picker_commit_all_confirms_and_commits
cargo test --locked --lib tui::tests::git_commands_block_while_orchestration_starts
cargo test --locked --lib tui::tests::git_command_outside_a_repository_shows_a_notice
cargo test --locked --lib tui::tests::git_completion_lists_status_and_commit_actions
cargo test --locked --lib git_workflow::tests::checks_reports_the_current_pull_request_and_its_checks
cargo test --locked --lib git_workflow::tests::checks_accepts_pending_exit_8_and_an_empty_check_list
cargo test --locked --lib git_workflow::tests::checks_treats_no_checks_reported_as_an_empty_list
cargo test --locked --lib git_workflow::tests::checks_reports_a_missing_gh_or_missing_pull_request
cargo test --locked --lib git_workflow::tests::plan_pr_builds_the_base_title_and_body
cargo test --locked --lib git_workflow::tests::plan_pr_refuses_existing_pr_and_unready_remote_states
cargo test --locked --lib git_workflow::tests::create_pr_uses_the_planned_arguments_and_rejects_a_moved_head
cargo test --locked --lib git_workflow::tests::pull_request_text_lists_newest_first_and_counts_earlier_commits
cargo test --locked --lib git_workflow::tests::pull_request_text_drops_the_oldest_listed_subject_to_fit_the_body_cap
cargo test --locked --lib git_workflow::tests::pull_request_text_prefers_the_supplied_title_then_the_head_subject
cargo test --locked --lib git_workflow::tests::create_pr_refuses_remote_branches_that_moved_after_the_preview
cargo test --locked --lib git_workflow::tests::create_pr_refuses_a_changed_branch_operation_and_conflicts
cargo test --locked --lib git_workflow::tests::remote_commands_time_out_and_kill_the_child
```

The module tests cover clean and dirty inspection, staged versus unstaged codes, upstream ahead and behind, a missing upstream, merge refusal, every other operation marker including the sequencer, an unmerged index without a marker, untracked ignored exclusion, force-added ignored inclusion, staged-only commits, generated and supplied messages, message validation and the 200-character cap, a changed branch, a changed path set, a content change that does not mutate the index, a moved HEAD, a failing hook that leaves the index staged, a successful hook whose tree change is reported, detached HEAD commit support, a trailing-space repository root, push target ordering, a remote name with a slash, a differently named upstream branch, a gone tracking ref, a real push to a local bare remote under `push.recurseSubmodules=only` and `push.followTags=true`, canonical head-refspec arguments, a branch named `refs/heads/topic`, a remote named `--force`, nothing-to-push and behind-upstream refusal, an actual non-fast-forward rejection, no-remote/detached/operation refusal, unborn and missing-remote refusal, a moved-head push refusal, no upstream auto-setup, PR check parsing, pending exit 8, an empty check list, no checks reported, pull request planning, repository binding, generated body and commit list, existing-PR refusal, missing origin, no commits ahead, a branch not on origin, a stale branch, a missing or invalid `gh` response, `gh pr create` argument construction, the live remote read refusal, URL and number parsing, HTTP(S) userinfo stripping for lower and upper case schemes with `ssh://` and scp-style URLs preserved, the planned base SHA, a moved HEAD refusal, a moved remote source branch refusal and a moved remote base branch refusal that each prove `gh pr create` did not run, newest-first subject order with an earlier-commit remainder, the body cap dropping the oldest listed subject and counting the remaining earlier commits, the title coming from the supplied value then the actual HEAD subject and never from an earlier commit, changed-branch, in-progress-operation, and unresolved-conflict creation refusals, and a timed-out child kill. TUI tests cover the picker and Esc, the status overlay from a subdirectory, the repository-root scope, an operation and conflict warning with every-file scrolling, confirmation scrolling, the actual commit from the picker, staged planning, free-form `--staged` text, a worktree-scoped open-work block that clears after the work finishes, an orchestration-start block, an exact `/git commit` through the composer, a push target picker with upstream ahead/behind, a real push confirmation and notice, push cancel/no-remote, a stale push confirmation cancelled after open work starts, the checks view from both the picker and exact `/git checks` with a fake `gh`, the pull request confirmation and created notice with a fake `gh`, display-width wrapping for wide glyphs, a non-repository notice, and completion. Every git fixture pins `main` and disables hooks, signing, `push.autoSetupRemote`, and ambient untracked-file settings in normal cases; hook tests opt back into a local hook.

### PTY
```sh
.agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The probe creates an isolated repository with a configured identity, disabled hooks and signing, an empty `core.excludesFile`, an untracked ignored `.env`, a fake `gh`, and a local bare remote. It opens `/git`, selects Commit all changes, requires the confirmation to list `src/parser/mod.rs`, commits, then checks the exact subject `baton: update 4 files`, the exact committed set (`AGENTS.md`, `CLAUDE.md`, `README.md`, `src/parser/mod.rs`), and that `.env` is absent. It then opens `/git push`, selects `origin/main`, confirms, checks the pushed notice, and asserts the bare remote head equals the local head. It reopens `/git`, selects Checks, and requires the fake PR `#42`, `Add checks`, the passing `Test`, and the pending `Lint` row. It runs exact `/git checks` and `/git status` from the composer and requires both overlays. It checks out `feat/pr`, commits a feature file, pushes the branch to the bare remote, runs `/git pr`, requires the preview with the `example/repo` identity, `feat/pr -> origin/main`, `Add pull request`, the body row, and the create hint, presses Enter, and requires `opened pull request #43` with the fake URL. The fake `gh` rejects a lookup or creation that omits `--repo example/repo`. It then returns to `main`. The evidence summary records the binary path, SHA-256, version, and repository source commit.

## Gotchas
- Git must have a configured `user.name` and `user.email`; the probe and tests configure both in their isolated repositories.
- The status uses `--untracked-files=all`, so an untracked directory is listed as its files. A repository with a large untracked tree can make inspection slower.
- The generated message names a file for one change and counts files for many. A supplied message replaces it.
- The plan stores a git tree fingerprint and HEAD. A content change, a new file, or a same-branch commit after confirmation is refused.
- A root commit has no parent; use `git update-ref -d HEAD`, not `HEAD~1`.
- Repository membership uses git's exit status, not an English error string, so localized git still reports the outside-repository notice.
- The push picker marks a target upstream only when its tracking ref resolves and its configured merge branch is known. The target uses the configured remote branch, which may differ from the local branch. A gone tracking ref or a same-name target is shown as `same-name branch · not tracked`, and the push does not set an upstream.
- The tests and probe use a local bare remote, so no network or account is required. The real push disables git's terminal credential prompt.
