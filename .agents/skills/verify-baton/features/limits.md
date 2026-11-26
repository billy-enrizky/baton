# Rate limits

## What it proves

A harness that reports rate limits shows them, including before the first turn; a harness that does not says so.

- `/limits` asks the current harness for a live snapshot, then opens a scrollable report. The current harness comes first, then every other built-in harness.
- A Codex snapshot shows the plan, when it was reported, the primary and secondary windows with used percent, window length, and reset time, and any credit balance.
- A harness with no snapshot shows `no rate-limit data from this harness`.
- When no harness has data, the report starts with `no rate-limit data yet`.
- While the read is in flight, the report ends with `refreshing rate limits from <harness>`. A failure names the harness and the reason instead of leaving the line pending.
- An older response never replaces the result of a newer `/limits` request.
- Pressing `/limits` again while the same harness read is in flight reopens the report without starting a second read.
- `/limits` takes no arguments; anything after it prints `use /limits`.

## Proof

```sh
cargo test --locked --lib rate_limit
cargo test --locked --lib limits_requests_a_live_read_and_renders_it
cargo test --locked --lib an_older_rate_limit_read_does_not_replace_a_newer_one
cargo test --locked --lib pressing_limits_again_does_not_start_a_second_read
cargo test --locked --lib a_failed_rate_limit_read_is_named_in_the_report
cargo test --locked --lib limits_reports_a_codex_snapshot
cargo test --locked --lib limits_names_every_harness_without_data
cargo test --locked --lib a_rate_limit_event_updates_the_limits_view
cargo test --locked --lib a_rate_limit_event_persists_for_the_next_session
cargo test --locked --lib rate_limits_round_trip_and_skip_invalid_rows
cargo test --locked --lib rate_limit_events_carry_the_snapshot
cargo test --locked --test codex_adapter codex_adapter_reads_rate_limits_without_a_turn
uv run --no-project python scripts/limits_probe.py --binary target/debug/baton
```

The parser tests feed the Codex `account/rateLimits/updated` shape and the `account/rateLimits/read` response, including the per-limit `codex` snapshot, the default fallback, a missing secondary reset, and an empty payload. The adapter test proves the read request runs without starting a turn. The engine test proves the request reaches the adapter and returns a `RateLimitsLoaded` event. The TUI tests prove the live request, the in-flight line, a delivered snapshot, a named failure, and stale-response rejection. The PTY probe drives two `/limits` requests against a fake app-server on an unsaved thread and requires the second snapshot to replace the first. The store test proves the row round-trips, a newer snapshot replaces it, and invalid or unknown rows are skipped. The stream test proves the JSON event shape for `--stream-json`.

## Gotchas

- The snapshot is stored in `provider_rate_limits` and hydrated at startup, so it survives a restart. `/limits` also reads the current Codex snapshot on demand, so the first view no longer waits for a turn.
- The live read is refused for an API-key account that is not signed in to a Codex account. The report names the app-server reason instead of showing a blank or estimated window.
- Reset times are Unix seconds from the provider, formatted in local time.
- Other harnesses do not report these windows to Baton yet. The report says so rather than estimating.
