# Archive

## What it proves

A thread can be put away without deleting it. Archive hides a thread from the default lists and from `/inbox`, and the reverse state restores it.

- `archive_thread` runs one immediate transaction: it refuses a thread with a non-terminal turn, clears any snooze, and sets `archived_at`.
- A thread with a running turn cannot be archived, so live work is never hidden.
- `/archive` hides the open thread and starts a new one, or takes an id prefix for another thread in the project.
- `/threads all` lists archived threads with an `archived` marker. Opening one unarchives it.
- `/unarchive [prefix]` restores an archived thread.
- `/inbox` and the default thread lists exclude archived threads.
- `baton threads --all` and `baton threads --json` expose `archived_at`, and `status` never defaults to an archived thread.

## Proof

```sh
cargo test --locked --lib archive
cargo test --locked --lib threads_all_marks_archived_threads
cargo test --locked --lib opening_an_archived_thread_unarchives_it
cargo test --locked --lib archived_threads_stay_out_of_the_inbox
cargo test --locked --test core threads_json_hides_archived_until_all
```

`archive_hides_a_thread_and_unarchive_restores_it` proves the list filters and both reverse states. `archive_refuses_a_running_turn_and_clears_a_snooze` proves the claim and that archive supersedes a snooze. `archive_survives_a_store_reopen` proves the column persists. The TUI tests drive `/archive`, `/threads all`, `/unarchive`, and opening an archived thread through `handle_command` and `draw`. The CLI test proves the default list, the `--all` list, the JSON field, and the status fallback.

## Gotchas

- Archive targets one thread. Use an id prefix for a thread you do not have open.
- A crash can leave a stale running turn, which blocks archive until the next startup reconciliation marks it unverified. That is deliberate: the database is the claim.
- Archive deletes nothing. Checkpoints, messages, and worktrees stay as they were.
