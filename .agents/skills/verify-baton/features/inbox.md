# Cross-thread attention inbox

## Sub-features
- `/inbox` lists threads with failed, unverified, interrupted, needs-input, or queued work.
- Row shows the thread id prefix, title, latest status, and every attention reason.
- Up/Down selects a row; Enter opens that thread; Esc closes without changing the current thread.
- An empty inbox says so instead of opening an empty picker.

## How to get to it (user POV)
Open the TUI and type `/inbox`.

## Driving it with the unit harness
```sh
cargo test --locked --lib inbox
cargo test --locked --lib inbox_lists_attention_threads_and_opens_the_selected_one
```

The store test builds a failed turn plus a queued message, a clean completed
turn, and a worker task waiting for input. It requires only the attention
threads to be returned. The TUI test requires the picker rows and Enter opening
the target thread.

## Gotchas
- The inbox is read-only. It does not acknowledge or clear a reason.
- `needs_input` is an orchestration task status. The task row for a worker thread is the source.
- The current thread may be absent when it has no attention reason. That is correct.
- A completed worker is not yet an attention row; that is the next slice.
