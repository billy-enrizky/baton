# Native rewind

`/rewind --native` completion is offered only on Codex and OpenCode. Other harnesses see `/rewind [turn] · native rewind is Codex/OpenCode only` and plain `/rewind` still works.

## Sub-features
- CLI rewind to before a turn
- Codex native `thread/resume` + `thread/revert`
- OpenCode native `revert/stage` + `revert/commit` with `files: false`
- Early provider-session persistence so a failed first turn can still be resumed, forked, or rewound
- Provider session preserved after a native rewind
- Baton turn, message, usage, and queue rows truncated
- Files untouched by native rewind
- Durable recovery marker after a provider revert, with store-only retry
- TUI `/rewind --native` with the same recovery path

## How to get to it (user POV)
Run:

```sh
baton rewind -t <thread> --turn 2 --json
baton rewind -t <thread> --turn 2 --native --json
```

In the TUI, `/rewind --native` opens the same turn picker and runs the Codex or OpenCode rollback in the background. The code-restore confirmation still appears when a checkpoint exists.

## Driving it with the PTY/CLI harness
A native rewind needs a Codex or OpenCode thread with a terminal turn and a recorded provider id.

OpenCode is the cheap probe because a turn with an invalid provider key still admits the user message and records its `msg_` id, then fails with HTTP 401. No model tokens are spent:

```sh
tmp=$(mktemp -d)
cwd=$(mktemp -d)
BATON_HOME="$tmp" target/debug/baton -C "$cwd" exec -p opencode --json "first" > /tmp/oc-first.json || true
thread=$(jq -r '.thread_id' /tmp/oc-first.json)
BATON_HOME="$tmp" target/debug/baton -C "$cwd" exec -t "$thread" -p opencode --json "second" > /tmp/oc-second.json || true
BATON_HOME="$tmp" target/debug/baton -C "$cwd" turns -t "$thread" --json | jq -e 'map(.provider_turn_id != null) | all'
BATON_HOME="$tmp" target/debug/baton -C "$cwd" rewind -t "$thread" --turn 2 --native --json
BATON_HOME="$tmp" target/debug/baton -C "$cwd" turns -t "$thread" --json | jq -e 'length == 1'
BATON_HOME="$tmp" target/debug/baton -C "$cwd" ps --json | jq -e '.processes == []'
```

The Codex probe from the original flow still applies when a Codex session is available.

```sh
tmp=$(mktemp -d)
cwd=$(mktemp -d)
BATON_HOME="$tmp" target/debug/baton -C "$cwd" exec -p codex "Reply with exactly REWIND_OK"
thread=$(BATON_HOME="$tmp" target/debug/baton -C "$cwd" threads --json | jq -r '.[0].thread_id // .threads[0].thread_id')
BATON_HOME="$tmp" target/debug/baton -C "$cwd" rewind -t "$thread" --turn 1 --native --json > /tmp/native-rewind.json
jq -e '.native == true and .provider_session_preserved == true and .provider_retry_skipped == false' /tmp/native-rewind.json
BATON_HOME="$tmp" target/debug/baton -C "$cwd" status "$thread" --json | jq -e '.turn_count == 0'
BATON_HOME="$tmp" target/debug/baton -C "$cwd" -t "$thread" resume --print
BATON_HOME="$tmp" target/debug/baton -C "$cwd" ps --json | jq -e '.processes == []'
```

The resume line must still print a Codex session id after the native rewind.

## Gotchas
- `--native` supports Codex and OpenCode. Claude, Cursor, Grok, and Antigravity fail loudly rather than silently falling back.
- The turn must be terminal and must have a recorded provider turn id. For OpenCode that id is the admitted user message id; turns recorded before Baton captured it cannot be cut natively.
- Native revert changes provider conversation history, not files. Use the checkpoint restore path for files.
- If the Baton store rewind fails after the provider revert, a `native_rewinds` marker remains. Retry the same command or TUI action: it skips the provider and finishes only the store rewind, reporting `provider_retry_skipped: true` in JSON. A rewind to a different turn is refused until the pending marker is cleared.
- The TUI refuses a native rewind while a turn is running or when the thread's current provider is not Codex or OpenCode.
- `beforeTurnId` is the provider id of the first removed turn, not the previous turn. For OpenCode it is the `messageID` passed to `revert/stage`.
- OpenCode `revert/commit` leaves the raw message list intact but removes the staged revert state; Baton owns the visible transcript, so the turns table is the source of truth after the rewind.
- If OpenCode stage or commit fails, Baton sends `revert/clear` so a later prompt cannot commit a half-staged revert.
