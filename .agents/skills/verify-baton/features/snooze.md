# Snooze

## What it proves

A thread can be put down without losing it. Snooze hides a thread from `/inbox` until its wake time, and any attention event after the snooze wakes it early.

- `/snooze` opens a picker with `off`, `1h`, `4h`, `tomorrow`, `week`, and `forever`, each row showing the wake time or `until woken`.
- `/snooze <preset>` applies one, `/snooze off` clears it, and an unknown value prints the accepted set.
- `forever` stores `9999-12-31T23:59:59.000Z`, so time never expires it. `/snooze off` wakes it; a later failure or question still wakes it early.
- `/snooze` refuses a draft thread that has not been persisted yet, because there is no row to write.
- `/threads` marks a timed row with `snoozed until <time>` and an indefinite row with `snoozed until woken`.
- `/inbox` hides a snoozed thread unless a failure or worker state happened after the snooze was set. A failure that predates the snooze is what the person already put down.
- No timer runs: every surface compares the wake time with the clock, so a restart or a sleeping laptop cannot leave a thread hidden past its timer.

## Proof

```sh
cargo test --locked --lib snooze
cargo test --locked --lib a_snoozed_thread_leaves_the_inbox_until_its_wake_time
cargo test --locked --lib an_indefinite_snooze_stays_hidden_until_cleared
cargo test --locked --lib attention_newer_than_the_snooze_wakes_a_thread_early
cargo test --locked --lib a_failure_older_than_the_snooze_stays_hidden
cargo test --locked --lib snooze_picker_lists_every_preset_and_off
cargo test --locked --lib snooze_command_persists_and_clears
cargo test --locked --lib a_draft_thread_cannot_be_snoozed
cargo test --locked --lib thread_picker_marks_a_snoozed_thread
cargo test --locked --lib thread_picker_marks_an_indefinite_snooze
cargo test --locked --lib every_fixed_choice_completion_offers_the_whole_accepted_set
```

`presets_move_the_wake_forward_by_their_span` fixes a clock and checks every preset, `the_indefinite_sentinel_is_recognized_and_stays_active` covers the sentinel, `an_active_snooze_ends_at_its_timestamp` covers the wake check, and `the_wake_rule_handles_each_timestamp_combination` covers the attention rule. The `snooze-state-machine` gate covers timed expiry, indefinite persistence, clear, archive, and early attention wakes with four mutants. The inbox tests run a real failed turn through the store. The TUI tests drive `/snooze` through `handle_command`, read the stored row back, and render the picker and `/threads` through `draw`. `every_fixed_choice_completion_offers_the_whole_accepted_set` keeps the completion and the handler's accepted set in step.

## Gotchas

- Snooze targets the current thread. Open another thread from `/threads` first.
- The wake label uses local time and switches from `14:32` to `Nov 27 09:00` when the wake is not today.
- `baton threads --json` reports `snoozed_until`; an indefinite snooze is the raw `9999-12-31T23:59:59.000Z` sentinel.
- Archive is separate. It clears a snooze and is covered by `features/archive.md`.
