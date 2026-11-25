# Usage report

## Sub-features
- `/usage` opens a picker with eight choices: this thread or all threads, then totals or a day, model, or provider grouping.
- The totals report shows the turn count, input and output tokens, cache rate, and cost. The grouped report adds a total line.
- The report scrolls with Up, Down, PageUp, PageDown, Home, and End. Esc or q closes it.
- Days print newest first; model and provider keys print alphabetically.
- An empty history says `no usage yet` instead of opening an empty table.

## How to get to it
Type `/usage`, choose a scope and grouping, then Enter.

## Proof
- `usage_picker_offers_every_scope_and_grouping`
- `usage_report_groups_this_thread_by_model`
- `usage_report_handles_empty_history`
- `every_remaining_picker_paints_one_cursor`
- The 54-case slash sweep opens `/usage` and requires the picker.

## Gotchas
- The report is a snapshot taken when Enter is pressed. Reopen `/usage` after a turn to refresh.
- Group keys come from the recorded provider and model, not the thread's current selection. A model that was switched away from still appears in its historical rows.
- The CLI `baton usage --by ... --json` uses the same aggregation and adds schema version 1.
