# Thread pins

## Sub-features
- `/pin` opens a scrolling picker of every thread in the current project.
- Up/Down selects a thread; Enter toggles its durable pin without opening it; Esc closes.
- Rows show the full thread ID and a `pinned` marker.
- Pinned threads sort before unpinned threads in display order for `/threads`, `/open`, `/pin`, and `baton threads`; implicit CLI/MCP resolution stays newest-first.
- The picker marks pinned rows with `pinned`.
- `/pin <id-prefix>` toggles directly; slash completion offers the valid thread prefixes.
- `baton threads --json` includes `pinned`.

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

The probe opens `/pin` for the TUI's current durable thread, requires the picker heading and row, presses Enter, and independently requires the pinned row marker and the pinned notice. It exits cleanly with `/quit`.

For the focused store and TUI checks:
```sh
cargo test --locked --test core thread_pins_persist_and_sort_before_recent_threads -- --exact
cargo test --locked --test core threads_json_reports_pinned -- --exact
cargo test --locked --test core thread_pin_survives_store_reopen -- --exact
cargo test --locked --lib tui::tests::pin_picker_toggles_without_opening_the_thread -- --exact
cargo test --locked --lib tui::tests::thread_picker_window_follows_selection_past_eight -- --exact
cargo test --locked --lib tui::tests::pin_prefix_rejects_foreign_and_ambiguous_matches -- --exact
cargo test --locked --lib mcp::tests::default_status_keeps_newest_thread_when_older_is_pinned -- --exact
```

## Gotchas
- `/pin` only lists durable threads. The TUI persists its current thread when an action such as `/model` calls `ensure_persisted`; the shared slash probe does that before `/pin`.
- The picker shows all threads in the current project, not every project. Direct prefixes are scoped to that project.
- Pinning does not change the conversation's `updated_at`; within each group the order remains recent first.
- Direct `/pin <prefix>` resolves only within the current project and rejects an ambiguous prefix. Completion inserts an eight-character prefix; the picker shows the full ID to disambiguate.
