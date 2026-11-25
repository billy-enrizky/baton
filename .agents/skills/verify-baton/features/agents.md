# Subagent calls

## Sub-features
- `/agents` lists the subagent tool calls the current thread started.
- Claude `Task` and `Agent` tool calls and Codex `collabAgentToolCall`/`subAgentActivity` items become `TurnEvent` child events and are persisted per turn.
- The picker reads persisted rows first and falls back to live or persisted `Task`/`Agent` tool lines for older data.
- Each row carries a running, done, or failed status and the turn number when the row came from the transcript.
- A child from the current turn appears from the live `SubagentStarted`/`SubagentFinished` events and is marked `live`; the live list clears when the turn settles.
- Enter or `y` copies the selected label. Escape closes.
- The list is read-only and derived from the primary transcript and live tool list, so it adds no second source of truth.
- `baton agents -t <thread-id> [--json]` reads the persisted subagent rows for scripts and agents.

## How to get to it (user POV)

Run a turn that starts a subagent, then type `/agents`, or run `baton agents -t <thread-id>` from a shell. The overlay names the count and every key. A running child appears from the live tool list; a finished child appears from the persisted tool message with its turn.

## Approach

The Claude adapter emits `SubagentStarted`/`SubagentFinished` for `Task` and `Agent` tool calls, and the Codex adapter emits them for `spawnAgent` collab calls and `subAgentActivity` items. The runtime pairs the events by call id, writes one row per child after the turn, and maps a child still running at turn end to `unknown`. The `/agents` handler reads `list_subagents` first. It appends `state.live_subagents` entries that are not already present by call id, and only when both are empty does `agent_entries` walk `state.tool_lines` and `tool_messages(state, None)`, stripping the `✓`, `✗`, or `·` marker and deduplicating by label and status. `apply_turn_event` pairs live starts and finishes by id and clears the live list on `TurnCompleted`, `TurnInterrupted`, or `TurnFailed`.

## Gotchas
- A provider that starts children without one of the mapped events still appears only through the tool fallback.
- During a running turn, `/agents` shows a live row from the structured event. After the turn settles, the persisted row is the source; a child still running when the turn ends is stored as `unknown`.
- The overlay shows at most ten rows.
- The label is the provider's tool summary or child path, not a Baton child session, and two identical calls can read the same.

## Proof

- `agents_command_lists_task_and_agent_tool_calls`
- `agents_command_prefers_persisted_subagent_rows`
- `live_subagent_events_feed_agents_picker`
- `subagents_round_trip_in_order`
- `subagent_events_pair_into_records`
- `subagent_events_are_persisted_with_the_turn`
- `claude_adapter_reports_task_subagents`
- `codex_adapter_reports_collab_and_subagent_activity`
- `agents_json_reports_subagent_rows_for_a_thread`
- The 51-command sweep runs `/agents` and requires the empty-state notice.
