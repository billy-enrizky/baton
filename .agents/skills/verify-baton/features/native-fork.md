# Native fork

## Sub-features
- Codex whole-thread native fork
- Codex fork through a completed provider turn
- OpenCode whole-session native fork
- New native session attached to the baton fork
- Source thread remains unchanged
- Unsupported provider, unsupported turn boundary, or missing session fails with guidance

## How to get to it (user POV)
Create or select a Codex thread with a native session, then run:

```sh
baton fork -t <source> --native --json "branch"
baton fork -t <source> --native --turn 1 --json "branch at turn 1"
```

For OpenCode, use a whole-session fork:

```sh
baton fork -t <source> --native --json "branch"
```

OpenCode native fork does not support `--turn` yet.

## Driving it with the PTY/CLI harness
A temp home has no native session. For a real proof, use a copied state directory or create one short Codex turn first:

```sh
tmp=$(mktemp -d)
cwd=$(mktemp -d)
BATON_HOME="$tmp" target/debug/baton -C "$cwd" exec -p codex "Reply with exactly NATIVE_FORK_OK"
source=$(BATON_HOME="$tmp" target/debug/baton -C "$cwd" threads --json | jq -r '.[0].thread_id // .threads[0].thread_id')
BATON_HOME="$tmp" target/debug/baton -C "$cwd" fork -t "$source" --turn 1 --native --json branch > /tmp/native-fork.json
jq -e '.native == true and .native_session_id and .through_turn == 1' /tmp/native-fork.json
fork=$(jq -r '.thread_id' /tmp/native-fork.json)
BATON_HOME="$tmp" target/debug/baton -C "$cwd" -t "$fork" resume --print
BATON_HOME="$tmp" target/debug/baton -C "$cwd" ps --json | jq -e '.processes == []'
```

The resume line must contain the same native session id returned in the fork JSON.

MCP uses the same helper:

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"baton_fork","arguments":{"thread":"<source>","turn":1,"native":true,"title":"mcp native"}}}' \
  | BATON_HOME="$tmp" target/debug/baton -C "$cwd" mcp | tail -1 | jq -r '.result.content[0].text' | jq -e '.native == true and .native_session_id'
```

## Gotchas
- `--native` supports Codex and OpenCode. Claude still fails loudly rather than silently falling back.
- A source thread needs a native provider session. Codex `--turn N` also needs a recorded provider turn id; OpenCode currently refuses `--turn`.
- The provider fork runs before the baton thread is created. If baton creation fails, use the reported native id to avoid losing the provider work.
- The source thread and its provider session are never changed.
- The native fork helper disposes its Codex app-server or OpenCode server; `baton ps --json` must be empty afterwards.
