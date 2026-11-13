# Orchestration

## Sub-features
- Coordinator splits a goal into tasks
- Per-task harness selection
- One git worktree per worker
- Parallel worker execution
- Durable run/task records
- `--list` and `--run` inspection
- `--plan-file` for deterministic plans
- Orchestrate mode as the default for new interactive sessions
- Worker commits, `--integrate`, and `worktrees --clean`
- Push worker branches and open PRs with `--open-prs` or `p` in the report
- Live per-worker progress in the TUI and with `--progress`

## How to get to it (user POV)

New interactive sessions start in orchestrate mode: type the goal in the composer. If the source checkout is dirty, baton asks first: Enter orchestrates from HEAD, `s` runs a single harness, Esc cancels. The startup banner shows the version, cwd, mode, model, permissions, and billing. Exact greetings run one normal turn; a plan with no tasks falls back to one worker.
`/orchestrate off` or `/switch` moves to a single harness; `/orchestrate on` moves back. `/runs` lists recent runs and `/runs <id>` reopens a stored worker report.
`baton --mode single` starts a single-harness session; `baton --mode orchestrate`
forces orchestration. Set `"defaultMode": "single"` in `~/.baton/config.json` to opt out.

```sh
baton orchestrate --max-tasks 3 "Audit billing, add tests, update docs"
baton orchestrate --list
baton orchestrate --run <run-id> --json
baton orchestrate --integrate <run-id> --json
baton orchestrate --integrate <run-id> --task 2 --json
baton orchestrate --open-prs <run-id> --json
baton orchestrate --open-prs <run-id> --task 2 --json
baton orchestrate --progress --json "Audit billing, add tests, update docs"
```

In the TUI:
```
/orchestrate --max-tasks 3 Audit billing, add tests, update docs
```

The transcript shows a live per-worker list while the run is active: queued, running, completed with commit, failed with the error, or cancelled. Ctrl+C cancels the run and terminates the worker process groups. `--progress` prints the same lines to stderr. When it
finishes, the full-screen report lists each worker. Up/Down selects a worker,
Enter opens that worker's thread (with its worktree as cwd), PageUp/PageDown
scrolls, and Esc closes the report. `/clear` also dismisses it. Press `i` to
integrate completed worker branches: the report asks for confirmation, merges in
the background, then shows the merged, skipped, and conflicted workers. Press `p`
to push eligible worker branches and open one pull request per worker; the report
asks for confirmation, shows the PR URLs, and keeps them durable across restarts.

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
- `--open-prs` needs a git remote and `gh` on PATH. It uses `origin`, or the only remote when origin is absent, and bases each PR on the source checkout's current branch. Eligible tasks have a completed worker, a commit, a live branch, no merge marker, and no recorded PR. A retry skips recorded PRs and links an existing PR for the same head branch instead of creating a duplicate.
- The coordinator retries once on invalid JSON. A second invalid plan fails the run before workers start.
- The multi-harness path costs real provider turns; use `--plan-file` for smoke tests.
