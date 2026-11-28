# Diff pane

## What it proves

A checkpoint diff opens beside the conversation when the terminal has room.

- `/diff <turn>` and the checkpoint picker call `show_diff_view`.
- A wide terminal renders the full diff in the secondary pane. The primary transcript stays visible.
- PageUp and PageDown scroll the diff. The scroll position is clamped to the rendered rows.
- Alt+D or `/clear` closes the diff pane. Closing restores a secondary thread pane if one was present.
- A narrow terminal hides the pane and keeps the existing full-screen diff fallback.
- The fallback keeps the 200-line truncation marker. The pane renders the complete diff.

## Proof

```sh
cargo test --locked --lib a_wide_diff_view_uses_the_secondary_pane_and_page_keys_scroll_it
cargo test --locked --lib a_narrow_terminal_falls_back_to_the_full_screen_diff
cargo test --locked --lib toggling_the_diff_pane_clears_both_layouts
```

The wide test renders a real `TestBackend` frame, proves the diff header and first rows, pages to the clamped maximum, and proves the final row is visible. The narrow test proves the fallback header is used instead. The close test proves both layouts clear.

## Gotchas

- A visible diff pane takes precedence over a secondary thread pane. Closing the diff restores the thread pane.
- PageUp and PageDown scroll the diff while it is visible. Move the diff away with Alt+D to return those keys to the transcript.
- A resize below the split threshold switches to the full-screen fallback without losing the diff.
- `/diff` without a turn and with no checkpoint still opens the checkpoint picker first.
