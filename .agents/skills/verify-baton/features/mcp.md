# MCP stdio server

## Sub-features
- Newline-delimited JSON-RPC 2.0 over stdio (`baton mcp`)
- `initialize` echoes the requested protocol version and names `baton`
- `tools/list` exposes 15 tools
- Read-only tools: status, threads, turns, agents, queue, messages, search, latest diff, doctor, processes, orchestration runs, orchestration report
- `baton_fork` creates a new thread or attaches a native Codex/OpenCode session
- `baton_queue` can clear the queue explicitly; everything else is read-only
- `--read-only` hides `baton_fork` and refuses queue clearing; queue listing and every other read tool stay available
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

The probe creates one empty thread with `baton new`, starts `baton mcp` in the
same throwaway cwd, and sends `initialize`, `tools/list`, `baton_status`,
`baton_turns`, `baton_threads`, `baton_agents`, `baton_doctor`, and one unknown
tool. It needs no provider turn or external account. It passes only when the
protocol version echoes, all 15 tools are present, the status, turn, and agent
counts match the seeded thread, doctor has a next action, the unknown tool is an
error, stderr is empty, and the server exits 0.

## Gotchas
- MCP stdio is one JSON object per line, not Content-Length framing.
- `tools/call` wraps the payload as pretty-printed JSON inside `content[0].text`.
  Parse that string before asserting fields.
- Run `baton mcp` with the project cwd. `baton_threads` without `all` filters by
  the server cwd.
- The probe seeds an empty thread, so it does not exercise provider turns; those
  are covered by the adapter tests and the slash probe.
- A read-only server returns JSON-RPC error `-32003` for a mutating tool. It does
  not write an audit row or change the queue.
