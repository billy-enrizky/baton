# MCP stdio server

## Sub-features
- Newline-delimited JSON-RPC 2.0 over stdio (`baton mcp`)
- `initialize` echoes the requested protocol version and names `baton`
- `tools/list` exposes 12 tools
- Read-only tools: status, threads, turns, queue, messages, search, latest diff, doctor, processes, orchestration runs, orchestration report
- `baton_fork` creates a new thread or attaches a native Codex/OpenCode session
- `baton_queue` can clear the queue explicitly; everything else is read-only
- Unknown tools return a tool result with `isError: true` and `unknown baton tool <name>`

## How to get to it
Point an MCP client at `baton mcp`. The server's working directory decides the
default thread for tools that omit `thread`.

## Driving it with the PTY/CLI harness
```sh
.agents/skills/verify-baton/scripts/mcp_probe.py \
  --binary ~/.cargo/bin/baton \
  --evidence-dir local_docs/verification/evidence
```

The probe seeds one failed OpenCode turn (the 401 admits the message and records
its native `msg_` id without model tokens), starts `baton mcp` in the same
throwaway cwd, and sends `initialize`, `tools/list`, `baton_status`,
`baton_turns`, `baton_threads`, `baton_doctor`, and one unknown tool. It passes
only when the protocol version echoes, all 12 tools are present, the status and
turn counts match the seeded thread, doctor reports ready harnesses, the unknown
tool is an error, stderr is empty, and the server exits 0.

## Gotchas
- MCP stdio is one JSON object per line, not Content-Length framing.
- `tools/call` wraps the payload as pretty-printed JSON inside `content[0].text`.
  Parse that string before asserting fields.
- Run `baton mcp` with the project cwd. `baton_threads` without `all` filters by
  the server cwd.
- The probe's OpenCode key may be valid on another machine. Then the seed turn
  succeeds and spends model tokens; the probe only requires a terminal turn, but
  keep the 401 fixture in mind.
