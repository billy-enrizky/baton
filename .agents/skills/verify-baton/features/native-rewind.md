# Native rewind

## Sub-features
- CLI rewind to before a turn
- Codex native `thread/resume` + `thread/revert`
- Provider session preserved after a native rewind
- Baton turn, message, usage, and queue rows truncated
- Files untouched by native rewind

## How to get to it (user POV)
Run:

```sh
baton rewind -t <thread> --turn 2 --json
baton rewind -t <thread> --turn 2 --native --json
```

## Driving it with the PTY/CLI harness
A native rewind needs a Codex thread with a completed turn and a recorded provider turn id.

```sh
tmp=$(mktemp -d)
cwd=$(mktemp -d)
BATON_HOME="$tmp" target/debug/baton -C "$cwd" exec -p codex "Reply with exactly REWIND_OK"
thread=$(BATON_HOME="$tmp" target/debug/baton -C "$cwd" threads --json | jq -r '.[0].thread_id // .threads[0].thread_id')
BATON_HOME="$tmp" target/debug/baton -C "$cwd" rewind -t "$thread" --turn 1 --native --json > /tmp/native-rewind.json
jq -e '.native == true and .provider_session_preserved == true' /tmp/native-rewind.json
BATON_HOME="$tmp" target/debug/baton -C "$cwd" status "$thread" --json | jq -e '.turn_count == 0'
BATON_HOME="$tmp" target/debug/baton -C "$cwd" -t "$thread" resume --print
BATON_HOME="$tmp" target/debug/baton -C "$cwd" ps --json | jq -e '.processes == []'
```

The resume line must still print a Codex session id after the native rewind.

## Gotchas
- `--native` is Codex-only today.
- The turn must be terminal and must have a recorded provider turn id.
- Native revert changes provider conversation history, not files. Use the checkpoint restore path for files.
- If the Baton store rewind fails after the provider revert, the provider is already truncated. Retry the Baton rewind instead of recreating history.
- `beforeTurnId` is the provider id of the first removed turn, not the previous turn.
