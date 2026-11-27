# Schedules

## Problem statement

A terminal user who wants a recurring prompt, such as a weekday test check, has to remember to start it. The host owns the store and the engine in the current architecture, so a schedule must be stored durably, fired by one owner, and recorded in the audit log.

## Goals and non-goals

Goals:

- Store a cron expression, a prompt, and a thread.
- Fire due schedules only from `baton serve`.
- Claim each due tick once, even if two schedulers see it.
- Write a `schedule.trigger` audit row before the turn starts.
- List and remove schedules from the CLI.

Non-goals:

- Adding or removing a schedule while a host or TUI owns the store. The next slice adds a host command or socket path for live management.
- A TUI schedule view, pause/resume, or per-schedule enable flags.
- Local timezones, DST policy, or missed-run catch-up.
- Notifications, retries, or a per-schedule concurrency limit.

## Sub-features

- `baton schedule add --thread <id> --cron "<minute hour day month weekday>" --prompt "<text>"` validates the expression, computes the next UTC occurrence, stores the row, and writes a `schedule.add` audit row.
- `baton schedule list [--json]` shows the id, next run, thread, expression, prompt preview, and enabled state.
- `baton schedule remove <id> [--json]` accepts a unique id prefix, deletes the row, and writes a `schedule.remove` audit row.
- The expression supports `*`, a number, a list, a range, and a step in each of the five fields. Weekdays are 0 for Sunday through 6 for Saturday. Day-of-month and day-of-week use the traditional OR rule when both are restricted.
- `baton serve` checks due schedules every second. A due row is claimed with a compare-and-set on its `next_run_at`; only the winner fires.
- A claimed run writes `schedule.trigger` with actor `scheduler`, confirmation `host_schedule`, the schedule id as the target, and the thread id. A failed audit write skips the run instead of starting an unaudited turn.
- The host posts the prompt into the thread through its project engine pool, using the thread's permission mode and prepending its active goal.
- An archived thread or a stored expression that no longer parses is skipped and logged. The claim has already advanced, so neither path hot-loops.

## Approach

`src/schedule.rs` parses and evaluates the five fields. `next_after` starts at the next minute and jumps by month, day, hour, or minute when a field does not match, with a five-year bound for impossible dates such as February 31.

`src/store.rs` adds the `schedules` table and the optimistic `claim_schedule` update. The claim sets `last_run_at` to the expected due time and `next_run_at` to the new run only when the stored due time still matches.

`src/serve.rs` runs `schedule_loop` on the host runtime. `fire_due_schedules` is a separate function so a test can call one tick against a real store and a fake engine link. The loop is aborted before the host engine stops.

## Alternatives considered

| Alternative | Decision | Reason |
|---|---|---|
| Add a cron crate | Rejected for the first slice | A five-field parser with `*`, lists, ranges, and steps is small and testable, and it keeps the dependency graph unchanged. |
| Fire schedules from the TUI | Rejected | The host is the single owner of turns and the engine pool. A TUI-fired schedule would stop when the TUI closes and would give a second owner. |
| A separate `run-due` command | Rejected | It would create a second scheduler owner. The host claim and the in-process tick keep one owner. |
| Claim by selecting and updating in two statements | Rejected | A compare-and-set on the due time is one statement and rejects a stale claim. |
| Skip the audit when it fails | Rejected | An unaudited turn defeats the "safe automation" goal. The run is skipped and logged. |
| Add live management over the socket in this slice | Deferred | It needs a new command scope and protocol path. The CLI is usable while no host or TUI owns the store. |

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Two schedulers fire the same due row | `claim_schedule` updates only when `next_run_at` still matches; the `schedule-claim-state-machine` gate rejects non-atomic and stale claims. |
| A claimed run never advances | The claim sets the next run in the same statement; the gate rejects a no-advance claim. |
| A bad stored expression hot-loops | The host advances the row by an hour and logs the parse error. |
| A missing or archived thread receives a turn | The host checks the thread before the audit and submit; an archived thread is skipped. |
| An unaudited turn starts | The trigger audit is written before the engine submit; a failed audit skips the run. |
| The schedule prompt loses the thread goal | The host prepends the active unpaused goal, matching the TUI submit path. |
| A schedule survives its thread | The row is keyed by thread id; a missing thread is logged and skipped. The next slice can add a foreign-key or cleanup path. |

## How to get to it (user POV)

Stop any TUI or host that owns the store. Add a schedule, list it, start `baton serve`, and watch the audit log:

```sh
baton schedule add --thread <id> --cron "0 9 * * 1-5" --prompt "Check the failing tests"
baton schedule list --json
baton serve
baton audit list --json
baton schedule remove <id>
```

The live integration test starts a host against a real SQLite store and a fake harness, with a due schedule, and waits for the `schedule.trigger` row and the advanced next run.

## Proof

- `schedule::tests::steps_run_within_the_hour`
- `schedule::tests::weekdays_skip_the_weekend`
- `schedule::tests::monthly_runs_roll_into_the_next_month`
- `schedule::tests::leap_day_is_found`
- `schedule::tests::an_impossible_date_reports_itself`
- `schedule::tests::lists_and_ranges_select_values`
- `schedule::tests::day_of_month_or_weekday_matches_the_traditional_rule`
- `schedule::tests::invalid_expressions_are_refused`
- `store::tests::schedules_round_trip_and_claim_once`
- `store::tests::a_schedule_claim_requires_the_expected_due_time`
- `serve::tests::fire_due_schedules_submits_and_advances`
- `serve::tests::fire_due_schedules_skips_an_archived_thread`
- `tests/schedule_cli.rs::schedule_add_list_remove_and_audit`
- `tests/schedule_cli.rs::schedule_add_refuses_an_invalid_expression`
- `tests/serve.rs::serve_fires_a_due_schedule_and_audits_it`
- `schedule-claim-state-machine` gate

## Limits

- Add and remove schedules while no host or TUI owns the store.
- The TUI does not fire or display schedules yet.
- Cron is UTC only; there is no pause/resume, local timezone, missed-run catch-up, notification, or retry.
