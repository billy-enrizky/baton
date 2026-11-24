# Subagent calls

## Sub-features
- `/agents` lists the subagent tool calls the current thread started.
- Claude `Task` and `Agent` tool calls and Codex `collabAgentToolCall`/`subAgentActivity` items become `TurnEvent` child events and are persisted per turn.
- The picker reads persisted rows first and falls back to live or persisted `Task`/`Agent` tool lines for older data.
- Each row carries a running, done, or failed status and the turn number when the row came from the transcript.
- Enter or `y` copies the selected label. Escape closes.
- The list is read-only and derived from the primary transcript and live tool list, so it adds no second source of truth.

## How to get to it (user POV)

Run a turn that starts a subagent, then type `/agents`. The overlay names the count and every key. A running child appears from the live tool list; a finished child appears from the persisted tool message with its turn.

## Approach

The Claude adapter emits `SubagentStarted`/`SubagentFinished` for `Task` and `Agent` tool calls, and the Codex adapter emits them for `spawnAgent` collab calls and `subAgentActivity` items. The runtime pairs the events by call id, writes one row per child after the turn, and maps a child still running at turn end to `unknown`. The `/agents` handler reads `list_subagents` first; only when that table is empty does `agent_entries` walk `state.tool_lines` and `tool_messages(state, None)`, stripping the `✓`, `✗`, or `·` marker and deduplicating by label and status.

## Gotchas
- A provider that starts children without one of the mapped events still appears only through the tool fallback.
- The structured rows appear after the turn settles. During a running turn, the live tool line is the only `/agents` fallback, and a child still running when the turn ends is stored as `unknown`.
- The overlay shows at most ten rows.
- The label is the provider's tool summary or child path, not a Baton child session, and two identical calls can read the same.

## Proof

- `agents_command_lists_task_and_agent_tool_calls`
- `agents_command_prefers_persisted_subagent_rows`
- `subagents_round_trip_in_order`
- `subagent_events_pair_into_records`
- `subagent_events_are_persisted_with_the_turn`
- `claude_adapter_reports_task_subagents`
- `codex_adapter_reports_collab_and_subagent_activity`
- The 50-command sweep runs `/agents` and requires the empty-state notice.
