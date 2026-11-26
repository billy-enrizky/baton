# Composer file mentions

## Sub-features
- Typing `@` and at least one character in the composer offers up to eight ranked project paths: up to four matching directories first, then files. Directories come from the parents of the indexed files and carry a trailing slash.
- Tab or Enter replaces only the `@token` with `@<path> ` and keeps the rest of the prompt and the caret position.
- The project index loads once per working directory in a background thread. Ordinary text never starts a load, and the typing path never runs `rg` per keystroke.
- `a@b` is an address, not a mention: a mention must start the token.
- Ignored files are excluded, and the ranking is the same subsequence matcher `/files` uses.

## How to get to it (user POV)
Type a prompt and mention a file:

```
explain @src/ma
```

Typing `@src` first offers `src/` with the `project directory` label; accepting it leaves `@src/` with the caret inside the mention, so the next characters narrow the files inside.

The composer shows `src/main.rs` and the other matches. Press Tab or Enter to accept, then finish the sentence. The sent prompt contains `@src/main.rs`.

## Driving it with the PTY harness
```sh
uv run --no-project python .agents/skills/verify-baton/scripts/file_ref_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence/file-mentions
```

The probe creates a git fixture with `src/main.rs`, `src/parser/mod.rs`, and `README.md`, types `explain @src/ma`, requires the ranked `src/main.rs` row, the `project file` description, and the completion footer, presses Tab, and requires `explain @src/main.rs` with no partial token left behind. It then types `@src`, requires the `src/` and `project directory` rows, presses Tab, and requires `@src/` without a trailing space, types `ma`, requires `src/main.rs`, and completes to `@src/main.rs `. Finally it types `mail a@b` and requires that no mention row and no index-loading hint appear.

## Unit proof
```sh
cargo test --locked --lib -- file_reference ordinary_text_does_not
```

`file_reference_token_detects_a_mention_and_ignores_an_address` covers the token boundary, the bare `@`, an address, and a multi-byte path. `file_reference_suggestions_replace_only_the_mention` seeds the index and proves Tab edits only the token. `file_reference_enter_completes_instead_of_sending` proves Enter on a mention completes it instead of dispatching the prompt. `file_reference_offers_directories_before_files` proves directory rows come first and Tab inserts the trailing slash. `files::tests::directory_candidates_list_unique_parents_with_trailing_slashes` proves unique parent derivation, ordering, trailing slashes, and the empty-query case. `ordinary_text_does_not_start_a_file_index_load` proves plain text leaves the index untouched and the composer quiet.

## Gotchas
- The inserted path is project-relative on purpose. The provider runs in the thread's working directory, and an absolute path would put the home directory into the transcript, every later handoff, and any egress path.
- The index is capped at `crate::files::MAX_FILES`. A larger repository still ranks the capped set, the same limit `/files` uses.
- The suggestion cache key includes the index length and the loading flag, and a completed load clears the cache, so a mention typed before the index arrives refreshes.
- Switching to a thread with a different working directory starts a new load. Events from the previous request are discarded by request id.
- An `@` with no following character does not open the list; a path fragment is required.
- Directories are derived from the indexed files, so an empty directory with no tracked files is not offered. Ignored directories are absent for the same reason as ignored files.
