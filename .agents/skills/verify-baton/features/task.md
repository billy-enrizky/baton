# GitHub issue tasks

## Sub-features
- `/task` opens a picker of open issues from the `origin` repository and starts the selected issue as a task.
- `/task <number>` reads that issue directly and starts it.
- `baton task <number> [--json]` creates the same worktree from the CLI.
- Starting a task creates a worktree under `~/.baton/worktrees/<id>/issue-<number>` on a `baton/<id>/issue-<number>` branch, switches to a new single-harness thread there, and prefills the composer with the issue title, URL, and a bounded body.
- A closed issue, a missing `origin`, an unauthenticated `gh`, or a non-number argument is refused with a notice and no worktree.
- A failed issue list or an empty list closes the dead-end picker and returns the error or `/task <issue-number>` path in the composer notice.

## How to get to it (user POV)
From a checkout with an `origin` on GitHub:

```
/task
/task 12
```

Or from a shell:

```sh
baton task 12 --json
```

The worktree is created at HEAD; the thread stops before sending so the prompt can be edited.

## Driving it with the unit and CLI harness
```sh
cargo test --locked --lib -- tasks:: task_ issue_picker
```

The tests prove that an open issue is read and bound to `origin`, a closed issue and a missing origin are refused, the prompt bounds a long body and marks the cut, the issue list parses labels, the worktree lands under `baton home` on an `issue-<n>` branch, `/task <n>` switches to a single-harness thread with a prefilled composer, the picker's Enter starts the selected issue, an error or empty list returns to the composer with the issue-number path, and a running turn or a non-number argument is refused.

The CLI was exercised with a fake `gh` and a real git fixture: `baton task 12 --json` created `/tmp/.../worktrees/<id>/issue-12` on `baton/<id>/issue-12`, and `baton task 12` printed the same path and the issue URL.

## Gotchas
- The forge is `gh`, bound to `origin` with `--repo <nameWithOwner>` just like the pull request flow, so a user-level default repository cannot redirect the issue read.
- Only open issues become tasks. A closed issue is refused instead of silently reopened.
- The issue body is capped at 1,200 characters in the composer and the cut is marked with the issue URL.
- The thread is not persisted until the first message is sent. A task the user abandons leaves its worktree behind; `baton worktrees` lists it and `baton worktrees --clean` removes clean, merged-or-shipped ones.
- The picker loads at most 30 open issues. A repository with more needs `/task <number>`.
- A load error or zero open issues closes the picker. Use `/task <number>` after fixing `gh` or the origin remote.
