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

- Adding or removing a schedule from any client while a host owns the store. The host terminal commands cover the attached host; a socket or TUI path is the next slice.
- A TUI schedule view, pause/resume, or per-schedule enable flags.
- Local timezones, DST policy, or missed-run catch-up.
- Notifications, retries, or a per-schedule concurrency limit.

## Sub-features

- `baton schedule add --thread <id> --cron "<minute hour day month weekday>" --prompt "<text>"` validates the expression, computes the next UTC occurrence, stores the row, and writes a `schedule.add` audit row.
- `baton schedule list [--json]` shows the id, next run, thread, expression, prompt preview, and enabled state.
- `baton schedule remove <id> [--json]` accepts a unique id prefix, deletes the row, and writes a `schedule.remove` audit row.
- `baton schedule pause <id> [--json]` accepts a unique id prefix, clears the enabled flag, and writes a `schedule.pause` audit row. A paused row never appears in the due query and cannot win a claim.
- `baton schedule resume <id> [--json]` parses the stored expression, recomputes the next UTC occurrence from the current time, sets the enabled flag, and writes a `schedule.resume` audit row. A long pause therefore does not replay a missed tick.
- At the `baton serve` host terminal, `schedule add --thread <id> --cron "<expr>" --prompt "<text>"`, `schedule list`, `schedule pause <id>`, `schedule resume <id>`, and `schedule remove <id>` manage the live host's rows. A quoted prompt or cron may contain spaces. The host writes `schedule.add`, `schedule.pause`, `schedule.resume`, or `schedule.remove` with actor `host_terminal`.
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

Add a schedule before starting a host, or manage a running host from its terminal:

```sh
baton schedule add --thread <id> --cron "0 9 * * 1-5" --prompt "Check the failing tests"
baton schedule list --json
baton schedule pause <id>
baton schedule resume <id>
baton serve
# At the host terminal:
schedule add --thread <id> --cron "0 9 * * 1-5" --prompt "Check the failing tests"
schedule list
schedule pause <id>
schedule resume <id>
schedule remove <id>
baton audit list --json
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
- `tests/schedule_cli.rs::schedule_pause_and_resume_skip_due_runs`
- `store::tests::a_paused_schedule_leaves_the_due_query_until_it_resumes`
- `serve::tests::host_schedule_pause_and_resume`
- `tests/serve.rs::serve_stdin_schedule_commands_manage_the_live_host`
- `tests/serve.rs::serve_fires_a_due_schedule_and_audits_it`
- `serve::tests::host_tokens_keep_quoted_spaces`
- `serve::tests::host_schedule_add_list_remove`
- `schedule-claim-state-machine` gate

## Limits

- A host with stdin closed cannot use the host terminal commands. The socket and TUI management paths remain.
- The TUI does not display schedules yet.
- Cron is UTC only; there is no local timezone, missed-run catch-up, notification, or retry.
