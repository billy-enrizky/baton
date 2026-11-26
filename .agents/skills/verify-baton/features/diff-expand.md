# Diff expansion

## What it proves

A long diff in the transcript does not push the conversation off screen without a way back. It collapses to a preview with an explicit affordance, and Ctrl+T expands every long diff inline.

- A diff cell renders at most twelve rows in the transcript, then a muted footer: `… N more lines · ctrl+t expand`.
- A collapsed multi-file diff names every changed file with its own counts. Path rows are spent before preview rows, each file keeps a bounded slice of its rows, and when the files alone do not fit the footer line says `… N more files`. A single-file diff keeps the head of the file.
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

`scripts/diff_expand_probe.py` proves the same path in the real TUI. It creates a throwaway git repository with two tracked files of sixty and forty lines, starts Baton with a fake Codex app-server whose turn rewrites both, and waits for the turn to settle so the runtime stores a real multi-file diff. It then asserts the `Edited 2 files` header, that both paths appear in the collapsed preview, the collapsed footer `… N more lines · ctrl+t expand`, that the last diff row is absent, that Ctrl+T swaps the footer to `ctrl+t collapse` and reveals the last row, and that a second Ctrl+T hides it again. The evidence keeps the collapsed, expanded, and recollapsed screens plus the raw transcript. `preview_keeps_every_file_path_and_bounds_the_rows` and `preview_caps_the_file_list_when_paths_alone_fill_the_rows` prove the budget rules, `preview_of_a_single_file_is_a_head_prefix` keeps the single-file behavior, and `cache_preview_matches_the_preview_renderer` proves one cache entry serves both forms.

## Gotchas

- Expansion is a single transcript-wide toggle, not a per-diff switch. Baton keeps the conversation in place, with the viewport pinned to the bottom, instead of opening a separate transcript view.
- The preview never exceeds twelve rows. A single-file diff shows the header and the earliest rows; a multi-file diff trades body rows for path rows so every file is named.
- Diff text passes through the sanitizer before rendering; the footer is static text.
- The hint is rendered as a line of text, not a clickable widget. The terminal surface has no buttons.
