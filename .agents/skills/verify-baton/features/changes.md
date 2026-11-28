# Changes view

## What it proves

Every diff in a thread is readable in one place without expanding the transcript.

- `/changes` opens a full-screen, scrollable view of every `diff` message in the thread, grouped by turn with a `turn N` heading when more than one turn has a diff.
- `/changes <turn>` shows one turn. A turn with no diff says `no diff in turn N`.
- A thread with no diffs says `no diff recorded yet` instead of opening an empty view.
- Up and Down, PageUp and PageDown, Home and End scroll. `[` and `]` jump to the previous or next diff block. Esc or `q` closes.
- The collapsed transcript preview and Ctrl+T expansion are unchanged; this view is the full, unwindowed copy.

## Proof

```sh
cargo test --locked --lib changes_view_lists_every_diff_and_filters_by_turn
cargo test --locked --lib changes_view_scrolls_to_the_end_and_jumps_between_diffs
cargo test --locked --lib changes_reports_an_empty_thread
```

The tests seed two diffs, render `/changes`, assert both blocks, filter with `/changes 2`, reject a missing turn and a non-numeric argument, scroll to the end of a thirty-line diff, jump to the second block with `]`, and close with Esc. The view reuses `DiffCache`, so the lines are the same ones the transcript renders.

## Gotchas

- The view is read-only. Use `/review` for line notes and `/diff` for a checkpoint against the working tree. `/diff` has its own secondary-pane path on a wide terminal; this view covers the every-diff report.
- A very long thread renders every diff in one pass, bounded by the diff cache and the terminal width.
