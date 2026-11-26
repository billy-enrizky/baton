# Diff expansion

## What it proves

A long diff in the transcript does not push the conversation off screen without a way back. It collapses to a preview with an explicit affordance, and Ctrl+T expands every long diff inline.

- A diff cell renders at most twelve rows in the transcript, then a muted footer: `… N more lines · ctrl+t expand`.
- Ctrl+T toggles expansion for the transcript. Expanded long diffs render every row and end with `ctrl+t collapse`.
- A diff that fits in twelve rows renders in full and has no hint.
- Long diff rows already wrap at the terminal width; expansion changes the number of rows, not the wrapping.
- Ctrl+T is ignored while a picker, review, activity, or orchestration report owns the screen, so it cannot steal a key from those views.

## Proof

```sh
cargo test --locked --lib ctrl_t_expands_a_long_diff
cargo test --locked --lib a_short_diff_has_no_expand_hint
cargo test --locked --lib hunk_separator_and_wrapping
uv run --no-project python scripts/diff_expand_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

`ctrl_t_expands_a_long_diff` seeds a diff message with sixteen added lines, asserts the collapsed hint and that the last line is absent, presses Ctrl+T, asserts the last line and the collapse hint, then presses it again and asserts the collapsed state. `a_short_diff_has_no_expand_hint` proves a small diff is never collapsed. `hunk_separator_and_wrapping` proves the renderer wraps long rows and keeps the hunk separator.

`scripts/diff_expand_probe.py` proves the same path in the real TUI. It creates a throwaway git repository with a sixty-line tracked file, starts Baton with a fake Codex app-server whose turn rewrites that file, and waits for the turn to settle so the runtime stores a real diff message. It then asserts the collapsed footer `… N more lines · ctrl+t expand`, that the last diff row is absent, that Ctrl+T swaps the footer to `ctrl+t collapse` and reveals the last row, and that a second Ctrl+T hides it again. The evidence keeps the collapsed, expanded, and recollapsed screens plus the raw transcript.

## Gotchas

- Expansion is a single transcript-wide toggle, not a per-diff switch. Baton keeps the conversation in place, with the viewport pinned to the bottom, instead of opening a separate transcript view.
- The preview keeps the first twelve rendered rows, which are the summary header and the earliest hunks. It is not a per-file preview.
- Diff text passes through the sanitizer before rendering; the footer is static text.
- The hint is rendered as a line of text, not a clickable widget. The terminal surface has no buttons.
