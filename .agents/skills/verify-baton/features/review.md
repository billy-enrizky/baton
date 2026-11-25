# Review

## Sub-features
- `/review` opens the latest turn diff, or `/review <turn>` a specific turn
- Added and removed lines are selectable; context rows are not note targets
- `n` opens a note for the selected changed line, Enter saves it
- Notes are stored unsent with the turn, path, line, side, and diff fingerprint
- `s` sends notes as a steer or the next prompt; Esc closes without sending
- `[` and `]` move between files, `d` deletes the note on the selected line
- `/` searches the row text and path; Enter jumps to the next match and wraps, a miss clears the query, and Esc cancels the search input

## How to get to it
Run one turn that changes a file, wait for the turn to finish, then type `/review`.

## Driving it with the PTY/CLI harness
```sh
.agents/skills/verify-baton/scripts/review_probe.py \
  --binary ~/.cargo/bin/baton \
  --evidence-dir local_docs/verification/evidence
```

The probe creates a throwaway git repo and `BATON_HOME`, switches the new thread to
single mode with `/orchestrate off`, runs one Codex turn that edits `app.txt`,
waits for the file change and for the composer to return to idle, opens `/review`,
saves `keep this wording` on the changed line, and closes without sending. It
passes only when the overlay opened, the diff shows `beta changed`, the footer
shows `notes: 1`, the row is stored unsent, and the TUI exits 0.

## Gotchas
- New threads default to orchestrate mode. Without `/orchestrate off`, the prompt
  starts a coordinator run and `/review` reports `no turn diff yet`.
- The diff is stored at turn finish, not when the file changes. Wait for the
  composer hint `enter steers the live turn` to disappear before `/review`.
- Do not wait for a marker string that appears in your own prompt. The probe
  watches the file side effect instead.
- The note stays unsent until `s`, so the probe costs one model turn only.
- Diff paths and line text are sanitized before rendering. A file can carry terminal escapes or bidi controls; the review overlay and the transcript diff replace them with U+FFFD.
