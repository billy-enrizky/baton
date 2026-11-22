# Thread pins

## Sub-features
- `/pin` opens a scrolling picker of every thread in the current project.
- Up/Down selects a thread; Enter toggles its durable pin without opening it; Esc closes.
- Rows show the full thread ID, a single `❯` cursor on the selected row, and a `pinned` marker.
- Pinned threads sort before unpinned threads in display order for `/threads`, `/open`, `/pin`, and `baton threads`; implicit CLI/MCP resolution stays newest-first.
- `/pin move <id> up` and `/pin move <id> down` swap a pinned thread with the adjacent pinned thread in the current project, never across the unpinned group.
- Pinned threads use canonical UTC microsecond order keys. Existing pins are backfilled from `updated_at` on open; a backfilled key at or below the highest existing key advances to one microsecond above it.
- Moving at a boundary reports `already first among pinned threads` or `already last among pinned threads`; unpinned threads must be pinned before moving.
- `/pin <id-prefix>` toggles directly; slash completion offers the valid thread prefixes.
- Slash completion for `/pin move ` offers `up` and `down` for every pinned thread.
- `baton threads --json` includes `pinned` and `pin_order_key`.

## How to get to it (user POV)
Create two threads in one directory, open the TUI there, type `/pin`, select a thread, and press Enter.

From a shell:
```sh
baton -C <project> new "first"
baton -C <project> new "second"
baton -C <project> threads --json
```

## Driving it with the verification harness
```sh
.agents/skills/verify-baton/scripts/slash_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

The probe opens `/pin` for the TUI's current durable thread, requires the picker heading and row, presses Enter, and independently requires the pinned row marker and the pinned notice. It creates a second thread, pins it, runs a real `/pin move <second-id> down`, requires the moved notice, the refreshed picker, and the swapped row order, then runs the same move again and requires the last-position boundary notice. It exits cleanly with `/quit`.

For the focused store and TUI checks:
```sh
cargo test --locked --test core thread_pins_persist_and_sort_before_recent_threads -- --exact
cargo test --locked --test core threads_json_reports_pinned -- --exact
cargo test --locked --test core thread_pin_survives_store_reopen -- --exact
cargo test --locked --test core store_thread_lookup_accepts_symlinked_project_paths -- --exact
cargo test --locked --test core pin_order_keys_sort_and_can_be_swapped -- --exact
cargo test --locked --test core pin_order_key_backfills_on_open -- --exact
cargo test --locked --test core concurrent_pin_moves_serialize_without_duplicate_keys -- --exact
cargo test --locked --lib store::tests::pin_order_keys_advance_within_one_microsecond -- --exact
cargo test --locked --lib tui::tests::pin_picker_toggles_without_opening_the_thread -- --exact
cargo test --locked --lib tui::tests::thread_picker_window_follows_selection_past_eight -- --exact
cargo test --locked --lib tui::tests::pin_prefix_rejects_foreign_and_ambiguous_matches -- --exact
cargo test --locked --lib tui::tests::pin_move_reorders_and_refreshes_the_picker -- --exact
cargo test --locked --lib tui::tests::pin_move_choices_are_exposed -- --exact
cargo test --locked --lib mcp::tests::default_status_keeps_newest_thread_when_older_is_pinned -- --exact
```

## Gotchas
- `/pin` only lists durable threads. The TUI persists its current thread when an action such as `/model` calls `ensure_persisted`; the shared slash probe does that before `/pin`.
- The picker shows all threads in the current project, not every project. Direct prefixes are scoped to that project.
- Threads stored under a symlinked project path and its canonical path are merged into the same picker list.
- Pinning does not change the conversation's `updated_at`. Unpinned threads stay newest-first; pinned threads use `pin_order_key`, which starts newest-pin-first and then follows manual moves.
- Manual order is per visible project list. A move swaps the key with the adjacent pinned thread in that project, so an unrelated project's pin cannot change the boundary result.
- Keys are canonical RFC3339 UTC timestamps with exactly six fractional digits. Do not write `updated_at`, which has milliseconds, directly into `pin_order_key`.
- Backfill truncates both `updated_at` and the existing maximum to microseconds before comparison, and malformed timestamps inherit the maximum instead of reading the wall clock.
- Direct `/pin <prefix>` resolves only within the current project and rejects an ambiguous prefix. Completion inserts an eight-character prefix; the picker shows the full ID to disambiguate.
- The probe closes the post-toggle picker before running the move. A successful move opens a refreshed picker; a boundary move only changes the notice.

## Design record

The problem is that recency alone hides long-running work as newer threads arrive. Pins keep selected threads above the unpinned list, and manual order lets the terminal user decide which pinned threads deserve the top rows.

Goals: durable pin state, pinned-first display without changing semantic newest-first resolution, adjacent manual moves, restart-safe order, and a zero-cost PTY proof. Non-goals: drag and drop, cross-project ordering, snooze, archive, or provider-specific pin behavior.

The order key is a nullable canonical UTC microsecond string. Display order is `pinned DESC, pin_order_key DESC, updated_at DESC, rowid DESC`; semantic queries remain `updated_at DESC, rowid DESC`. Pinning assigns a strictly increasing key and unpinning clears it. A move starts an immediate write transaction, reads the project's current pinned rows in that transaction, swaps the adjacent keys, checks both affected rows, and commits or rolls back as one unit.

Alternatives considered:
- Integer positions require renumbering every later row on a move and complicate concurrent inserts. Rejected because a move should touch two rows.
- Dense gap keys reduce writes but need a rebalance path and a policy for exhausted gaps. Rejected because the extra state is not justified at thread-list sizes.
- Changing `updated_at` to represent manual order would corrupt recency and semantic resolution. Rejected because display and semantic order must stay separate.

Risks and mitigations:
- Duplicate timestamps or same-microsecond pins can tie. Keys are truncated to microseconds before comparison, and any key at or below the maximum advances by one microsecond.
- Concurrent writers can lose a move. Pin and move operations open `BEGIN IMMEDIATE` before reading, use one connection transaction, and fail if an update affects anything other than one row.
- A malformed legacy `updated_at` cannot be parsed for backfill. The backfill uses the highest existing key, then `created_at`, then the Unix epoch as its base, so repair never reads the wall clock.
- Backfill runs on store open. A read-only probe skips the write transaction unless a pinned row has a missing key; repair then runs in one immediate transaction and writes one update per missing row. The max-key and backfill predicate have partial indexes, so normal open does one lightweight no-repair probe.
- Pin order is local UI state, not a security boundary. The store is under `BATON_HOME`; no pin operation crosses the network.
