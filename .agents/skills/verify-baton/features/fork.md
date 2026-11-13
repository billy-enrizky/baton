# Fork

## Sub-features
- Branch a conversation without changing the source
- Optional cutoff at a completed turn
- JSON identity for the source and fork
- New thread opens with a transcript handoff, not a shared native session

## How to get to it (user POV)
Run `baton fork [-t source] [--turn N] [--json] [title]`, or use `/fork` in the TUI.

## Driving it with the PTY/CLI harness
```sh
tmp=$(mktemp -d)
cwd=$(mktemp -d)
BATON_HOME="$tmp" target/debug/baton -C "$cwd" new "source thread" >/dev/null
source=$(BATON_HOME="$tmp" target/debug/baton -C "$cwd" threads --json | jq -r '.[0].thread_id // .threads[0].thread_id')
BATON_HOME="$tmp" target/debug/baton -C "$cwd" fork -t "$source" --json branch | jq -e '.source_thread_id and .thread_id'
BATON_HOME="$tmp" target/debug/baton -C "$cwd" threads --json | jq -e '(length // .count) == 2'
```

## Gotchas
- A whole-thread fork is refused while a source turn is open.
- A `--turn` fork may branch through a completed cutoff while a later source turn is still running.
- Native sessions, usage, queues, checkpoints, and review notes are intentionally not copied.
