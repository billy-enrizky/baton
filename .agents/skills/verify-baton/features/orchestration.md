# Orchestration

## Sub-features
- Coordinator splits a goal into tasks
- Per-task harness selection
- One git worktree per worker
- Parallel worker execution
- Durable run/task records
- `--list` and `--run` inspection, plus idempotent `--archive`
- `--plan-file` for deterministic plans
- Orchestrate mode as the default for new interactive sessions
- Worker commits, `--integrate`, and `worktrees --clean`
- Push worker branches and open PRs with `--open-prs` or `p` in the report
- Live per-worker progress in the TUI and with `--progress`
- `/multi --worktrees` fan-out through a generated plan, one isolated worker per provider
- `/worktree` conversation fork bound to a one-task orchestration run

## How to get to it (user POV)

New interactive sessions start in orchestrate mode: type the goal in the composer. If the source checkout is dirty, baton asks first: Enter orchestrates from HEAD, `s` runs a single harness, Esc cancels. The startup banner shows the version, cwd, mode, model, permissions, and billing. Exact greetings run one normal turn; a plan with no tasks falls back to one worker.
`/orchestrate off` or `/switch` moves to a single harness; `/orchestrate on` moves back. `/runs` lists recent runs, `/runs --all` includes archived runs, and `/runs <id>` reopens a stored worker report. MCP exposes the same durable history through `baton_orchestration_runs` and `baton_orchestration_report`.
`baton --mode single` starts a single-harness session; `baton --mode orchestrate`
forces orchestration. Set `"defaultMode": "single"` in `~/.baton/config.json` to opt out.

```sh
baton orchestrate --max-tasks 3 "Audit billing, add tests, update docs"
baton orchestrate --list
baton orchestrate --list --all --json
baton orchestrate --run <run-id> --json
baton orchestrate --archive <run-id> --json
baton orchestrate --integrate <run-id> --json
baton orchestrate --integrate <run-id> --task 2 --json
baton orchestrate --open-prs <run-id> --json
baton orchestrate --open-prs <run-id> --task 2 --json
baton orchestrate --progress --json "Audit billing, add tests, update docs"
```

In the TUI:
```
/orchestrate --max-tasks 3 Audit billing, add tests, update docs
/multi --worktrees codex claude compare the API
```

`/multi --worktrees` does not call the coordinator. It writes a deterministic plan with one task per provider and starts the normal orchestration run, so the result is a durable report with worker worktrees, commits, `i` integration, and `p` PR shipping. Plain `/multi` keeps the live merged view and shared working tree.

`/worktree [title]` forks the current conversation into an isolated worktree and binds the new thread to a completed one-task run. The first successful turn commits through the worker follow-up path; the report then exposes integrate, ship, archive, and cleanup. Dirty source checkouts are refused, and the original thread stays in the source checkout.

The transcript shows a live per-worker list while the run is active: queued, running, completed with commit, failed with the error, or cancelled. Ctrl+C cancels the run and terminates the worker process groups. If the orchestrator process dies, the next Baton start marks unfinished tasks failed, keeps completed tasks and commits, settles the run, and shows a recovery notice in the TUI. A run owned by another live Baton process is left alone. `--progress` prints the same lines to stderr. When it
finishes, the full-screen report lists each worker. Up/Down selects a worker,
Enter opens that worker's thread (with its worktree as cwd), PageUp/PageDown
scrolls, and Esc closes the report. `/clear` also dismisses it. Press `i` to
integrate completed worker branches: the report asks for confirmation, merges in
the background, then shows the merged, skipped, and conflicted workers. Press `p`
to push eligible worker branches and open one pull request per worker; the report
asks for confirmation, shows the PR URLs, and keeps them durable across restarts. Press `a` to archive the run; archiving is non-destructive, keeps durable history, and leaves worktrees for `baton worktrees --clean`.

## Driving it with the PTY/CLI harness
Use a fresh git repo and a plan file for a cheap deterministic proof. This runs one real Codex worker.

```sh
repo=$(mktemp -d)
home=$(mktemp -d)
git -C "$repo" init -q
git -C "$repo" config user.email test@test
git -C "$repo" config user.name test
echo base > "$repo/a.txt"
git -C "$repo" add .
git -C "$repo" commit -qm init
cat > "$repo/plan.json" <<'JSON'
{"summary":"one worker","tasks":[{"title":"pwd check","provider":"codex","prompt":"Run pwd and reply with the exact output. Do not modify files."}]}
JSON
BATON_HOME="$home" target/debug/baton -C "$repo" --yolo orchestrate --plan-file "$repo/plan.json" --json "verify worker cwd" > /tmp/orchestrate.json
jq -e '.status == "completed" and .tasks[0].text and .tasks[0].worktree.path' /tmp/orchestrate.json
BATON_HOME="$home" target/debug/baton orchestrate --list --json | jq -e '.runs | length == 1'
BATON_HOME="$home" target/debug/baton orchestrate --run "$(jq -r .run_id /tmp/orchestrate.json)" --json | jq -e '.tasks | length == 1'
BATON_HOME="$home" target/debug/baton ps --json | jq -e '.processes == []'
```

The worker text should contain the worktree path from `.tasks[0].worktree.path`, not the source repo path.

For the full multi-harness coordinator path, omit `--plan-file` and ask for explicit harnesses, for example:

```sh
BATON_HOME="$home" target/debug/baton -C "$repo" --yolo orchestrate --max-tasks 2 --json \
  "Use exactly two workers. One codex worker reports the package name from Cargo.toml. One claude worker reports the number of .rs files under src. Do not modify files."
```

## Gotchas
- Worker worktrees branch from `HEAD`; uncommitted source changes are not in the worker checkout. The report sets `dirty_source`.
- `--plan` is global plan permission mode. Use `--plan-file` for the orchestration plan.
- A run with one success and one failure is `partial`; all failures is `failed` and exits non-zero.
- Worktrees are kept for review under `$BATON_HOME/worktrees/<run>/<task>`. Successful workers commit their changes as `baton worker: <title>`; the report and `--run --json` expose the SHA. `--integrate` merges completed branches into a clean source checkout, aborts on conflict, and skips already-merged workers. Use `baton worktrees` to list managed worktrees; JSON includes `shipped`, and the text state shows `clean · shipped` for a branch with a PR. `baton worktrees --clean` removes clean worktrees whose branch is merged or shipped; dirty worktrees, and unmerged branches without a recorded PR, require `--force`.
- `baton.json` `worktree.sharedDirectories` symlinks existing, gitignored directories such as `node_modules` or `target` into each worker worktree. Shared paths appear in the report; edits inside them affect the primary checkout.
- Open a worker from the report and send a follow-up to continue its worktree. A successful follow-up commits the new changes, updates the task text and commit, refreshes the run verdict, and clears `merged_at` and `pr_url` when a new commit exists. A follow-up with no file changes keeps the previous commit and markers. A failed follow-up marks the task failed when it has no commit. If it already has a successful commit, the last result and its merge/PR markers stay intact; the thread shows the failure.
- `--open-prs` needs a git remote and `gh` on PATH. It uses `origin`, or the only remote when origin is absent, and bases each PR on the source checkout's current branch. Eligible tasks have a completed worker, a commit, a live branch, no merge marker, and no recorded PR. A retry skips recorded PRs and links an existing PR for the same head branch instead of creating a duplicate.
- `--archive` is idempotent and refuses a run whose durable status is still `running`. Archived runs remain readable by id; they are hidden only from the default list, and `--list --all` includes them.
- The coordinator retries once on invalid JSON. An empty plan, a second invalid plan, or an unusable coordinator response falls back to one direct worker instead of failing the run. Cancellation is the only pre-worker terminal outcome.
- Orchestration runs record their owner PID and process start time. Startup recovery settles a run only when that owner identity is gone. Ownerless rows from older builds are left running because their liveness cannot be proven.
- The multi-harness path costs real provider turns; use `--plan-file` for smoke tests. Exact greetings use a direct plan and do not call the coordinator; `/orchestrate hi` in the TUI routes to a single normal turn.
