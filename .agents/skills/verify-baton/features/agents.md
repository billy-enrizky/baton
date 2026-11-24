# Subagent calls

## Sub-features
- `/agents` lists the subagent tool calls the current thread started.
- Claude `Task` and `Agent` tool calls are recognized from live tool lines and persisted tool messages.
- Each row carries a running, done, or failed status and the turn number when the row came from the transcript.
- Enter or `y` copies the selected label. Escape closes.
- The list is read-only and derived from the primary transcript and live tool list, so it adds no second source of truth.

## How to get to it (user POV)

Run a turn that starts a subagent, then type `/agents`. The overlay names the count and every key. A running child appears from the live tool list; a finished child appears from the persisted tool message with its turn.

## Approach

`agent_entries` walks `state.tool_lines` first for labels that start with `Task ` or `Agent `, then walks `tool_messages(state, None)` and strips the `✓`, `✗`, or `·` marker. It deduplicates by label and status. The live entry uses turn 0 because the tool line does not carry a turn; persisted rows carry their stored turn.

## Gotchas
- A provider that starts children without a `Task` or `Agent` tool call, such as a native Codex child, does not appear yet.
- A live child that finishes before the turn is persisted can briefly disappear between the live list clearing and the tool message being stored.
- The overlay shows at most ten rows.
- The label is the provider's tool summary, not a separate child session id, so two identical calls can read the same.

## Proof

- `agents_command_lists_task_and_agent_tool_calls`
- The 50-command sweep runs `/agents` and requires the empty-state notice.
