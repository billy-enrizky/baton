# TUI native rewind

## Sub-features
- `/rewind` opens the transcript picker; `/rewind --native` uses the provider rollback
- The picker renders user messages newest first, and Enter acts on the row that is actually selected.
- The picker shows eight rows and scrolls to keep the selected row visible; only the selected row carries the cursor.
- Up and Down select a user message; Enter rewinds to before that turn
- The provider session is preserved; files are untouched
- The composer is prefilled with the rewound message
- A checkpoint opens the code-restore confirmation; failed turns have none
- The durable recovery marker still applies when the store step fails after the provider revert

## How to get to it
Run two turns, then `/rewind --native`, select the second user message, and press Enter.

## Driving it with the PTY/CLI harness
```sh
.agents/skills/verify-baton/scripts/rewind_probe.py \
  --binary ~/.cargo/bin/baton \
  --evidence-dir local_docs/verification/evidence
```

The probe uses the configured OpenCode key as-is. A 401 still admits each user
message and records its native `msg_` id, so two failed turns are enough to
exercise the real picker and the background `revert/stage` + `revert/commit`
path with no model tokens. It passes only when the picker shows the newest
message first and the older one second, the notice says `rewound to before turn 2 natively`, the composer is prefilled
with the second message, the database has one failed turn and one user message,
and the TUI exits 0.

## Gotchas
- New threads default to orchestrate mode. Send `/orchestrate off` first.
- `/rewind --native` supports Codex and OpenCode only and refuses while a turn is running.
- The composer is prefilled after a rewind. Clear it with Escape before typing a slash command, or the command joins the prompt.
- The rendered order and the action order share `rewind_choices`; do not reverse one without the other.
- Failed turns have no checkpoint, so no code-restore confirmation appears. That is the expected path here.
- A turn that leaves the worktree clean and HEAD where it started also has no checkpoint: there is nothing to restore or show. A turn that edits files, or commits, still records one.
- `baton diff` and `/diff` compare the checkpoint with the worktree through a temporary index, so an untracked file that still exists is not shown as deleted and a new untracked file appears as an addition.
- The provider revert is history-only; files stay as they are.

The unit test `rewind_picker_uses_the_visible_newest_first_order_and_enter_matches_it` covers the rendered order, the Enter target, the eight-row window, and the single cursor.
