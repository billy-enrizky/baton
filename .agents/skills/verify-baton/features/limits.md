# Rate limits

## What it proves

A harness that reports rate limits shows them; a harness that does not says so.

- `/limits` opens a scrollable report. The current harness comes first, then every other built-in harness.
- A Codex snapshot shows the plan, when it was reported, the primary and secondary windows with used percent, window length, and reset time, and any credit balance.
- A harness with no snapshot shows `no rate-limit data from this harness`.
- When no harness has data, the report starts with `no rate-limit data yet`.
- `/limits` takes no arguments; anything after it prints `use /limits`.

## Proof

```sh
cargo test --locked --lib rate_limit
cargo test --locked --lib limits_reports_a_codex_snapshot
cargo test --locked --lib limits_names_every_harness_without_data
cargo test --locked --lib a_rate_limit_event_updates_the_limits_view
cargo test --locked --lib a_rate_limit_event_persists_for_the_next_session
cargo test --locked --lib rate_limits_round_trip_and_skip_invalid_rows
cargo test --locked --lib rate_limit_events_carry_the_snapshot
```

The parser tests feed the Codex `account/rateLimits/updated` shape, including a missing secondary reset and an empty payload. The TUI tests render a snapshot and a data-free report through `draw`, apply the `TurnEvent::RateLimits` event, and prove a new session hydrates the stored snapshot. The store test proves the row round-trips, a newer snapshot replaces it, and invalid or unknown rows are skipped. The stream test proves the JSON event shape for `--stream-json`.

## Gotchas

- The snapshot is stored in `provider_rate_limits` and hydrated at startup, so it survives a restart. It still only arrives during a turn; a first-ever run shows the unknown state until Codex reports one.
- Reset times are Unix seconds from the provider, formatted in local time.
- Other harnesses do not report these windows to Baton yet. The report says so rather than estimating.
