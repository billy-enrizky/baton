# Plan review

## Problem statement

A coding harness can propose a multi-step plan while it works, but that plan is otherwise buried in the provider stream. A terminal user needs one stable place to see the current steps, the active step, and what is still pending without reading raw events or asking the model to repeat itself.

## Goals and non-goals

Goals:

- Turn a Codex or ACP plan event into one Baton event shape.
- Let `/plan` open a sanitized read-only overlay for the plan reported by the current thread and harness.
- Show completed, in-progress, and pending steps plus any explanation or priority.
- Expose the same event in `exec --stream-json`.

Non-goals:

- Accepting, revising, or rejecting a proposed plan.
- Persisting the plan across a restart.
- Reconstructing a plan from prose or transcript tool rows.
- Rendering ACP file or markdown plans, adding a CLI or MCP reader for the snapshot, and forwarding orchestration worker plans into the parent TUI.

## Sub-features

- `/plan` opens the last plan reported for the current thread and current harness.
- The header reads `plan · <done>/<total> done` and names the scroll and close keys.
- The body lists the explanation when one was supplied, then one row per step.
- Each step uses a stable marker: `✓` completed, `▸` in progress, and `·` pending, cancelled, or unknown.
- Step content, the explanation, and priority are sanitized before rendering.
- Up, Down, PageUp, PageDown, Home, and End scroll; `j`, `k`, `q`, and Esc are also accepted.
- With no plan for the current thread and harness, the command keeps the composer open and says `no plan available for this thread`.
- A plan from another thread or another harness is never shown.

## Approach

The Codex and ACP adapters translate their native plan event to `TurnEvent::PlanUpdated`. Codex supplies `plan[].step`, `plan[].status`, and an optional explanation; its `inProgress` status is normalized to `in_progress`. ACP v2 supplies `plan_update` with an item list and stable `planId`, `plan_removed` carries that id, and the v1 `plan` frame remains a compatibility input. ACP entries supply `content`, `status`, and `priority`.

The TUI keeps an in-memory snapshot per thread and provider. `/plan` reads only the snapshot for the visible thread and current harness, and an ACP removal clears a snapshot only when its plan id matches. Starting any new work from the thread (a turn, `/multi`, or orchestration) or rewinding the thread history clears that thread's snapshots, and a restart clears the map.

The retention path covers events handled by the TUI process, including the in-process multi-provider children. Orchestration workers run as separate `baton exec` subprocesses, so their plan events are not forwarded to the parent TUI snapshot map yet.

The overlay is read-only. It does not change plan mode, send a provider message, or store provider state. Enter, revise, and reject are deliberately left to a later slice with an explicit state transition and provider command.

## Alternatives considered

| Alternative | Decision | Reason |
|---|---|---|
| Parse each provider payload in the TUI | Rejected | Codex and ACP use different field names. Normalizing in each adapter keeps provider spellings out of the renderer and gives `exec --stream-json` one stable shape. |
| Persist every plan in SQLite immediately | Deferred | The ACP item path has a `planId`, but Codex plan events do not, and the TUI cannot map either to a durable turn row yet. Persistence needs a synthetic identity and a restart replacement policy before the action slice defines accept, revise, and reject. |
| Reuse transcript tool rows as the plan source | Rejected | Tool rows lose ACP priority and structured status, and a provider can change the plan without emitting a matching display row. |
| Map ACP `plan_update` items and `plan_removed` in the same slice | Accepted | The v2 item list has the same step shape as the legacy frame. The snapshot keeps `planId`, so a removal clears only the matching item plan. File and markdown plans remain a named non-goal. |
| Keep one global snapshot slot | Rejected | A background worker's plan would be dropped, and parallel supervision is a core terminal workflow. A small per-thread/provider map keeps the plan that was actually reported. |

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| A provider sends control characters in step text, priority, or explanation | All three pass through `sanitize_terminal_text`; the status is normalized to a known vocabulary before rendering. |
| A stale plan is shown after new work, rewind, thread switch, or harness switch | Starting a turn, `/multi`, or orchestration removes the visible thread's snapshots, as does applying a rewind. The thread id and provider must both match before `/plan` opens or the overlay draws. |
| Switching back to an old thread loses its plan | The per-thread/provider map retains it until that thread starts another turn or Baton exits. Plans are not persisted across restart. |
| An orchestration worker's plan never reaches the parent TUI | Workers run as `baton exec --json` subprocesses. Their plan event is consumed and discarded: it is not written by `--json`, not persisted, and not inserted into the parent snapshot map. Opening that worker thread reports the empty state. Forwarding it is an explicit non-goal and tracked follow-up. |
| ACP file or markdown `plan_update` arrives and the view stays stale | Those payload types are ignored rather than approximated. A later `plan_removed` clears the item plan; a live ACP plan event has not been recorded yet. |
| A `plan_removed` for another plan clears the visible plan | The snapshot carries its ACP `planId`, and only a removal with the same id clears it. A removal for an ignored file or markdown plan is a no-op. |
| A harness reports an empty list | The same empty-state notice is used for a missing report and a deliberately empty plan. It states that no plan is available rather than pretending steps exist. |
| A plan is treated as approval | The overlay is read-only and never changes mode or sends a message. Accept and reject require their own confirmation and provider transition. |
| A long explanation or step is silently clipped | The overlay renders one terminal line per item with `truncate_chars`. Wrapping is left to the action slice, which owns the taller layout and scroll math. |
| A plan is shown without its reporting turn | The snapshot is cleared when a new turn starts, so it describes the current or most recently settled turn. The header does not name a turn number. |
| Provider snapshots accumulate in memory | The map is bounded by the threads and harnesses observed in one TUI process, and a restart clears it. A cap becomes necessary if a future background service retains them. |
| An agent cannot read the plan through CLI or MCP | This slice is TUI-only. The event is present in `exec --stream-json`, but no durable reader exists. |

## How to get to it (user POV)

Run a turn on a harness that reports plans, then type `/plan`. A Codex app server sends `turn/plan/updated`; an ACP v2 agent sends `sessionUpdate: "plan_update"` with an item plan, and a v1 peer can send `sessionUpdate: "plan"`. In a clean isolated instance, `/plan` reports the empty state. The populated Codex path is driven through a fake app server in a real PTY:

```sh
.agents/skills/verify-baton/scripts/plan_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

## Proof

- `codex_adapter_maps_plan_updates`
- `acp_adapter_maps_plan_updates`
- `acp_adapter_maps_the_legacy_plan_frame`
- `plan_event_fills_the_view_and_plan_command_opens_it`
- `plan_command_reports_an_empty_plan`
- `plan_from_another_thread_is_not_shown`
- `plan_from_another_harness_is_not_shown`
- `background_plan_is_kept_for_its_thread`
- `plan_removal_matches_the_plan_id`
- `starting_multi_clears_the_visible_plan`
- `plan_events_keep_the_entries_and_explanation`
- `plan_statuses_use_one_vocabulary`
- `plan_removal_keeps_the_plan_id`
- `.agents/skills/verify-baton/scripts/plan_probe.py` proves the populated overlay in a PTY.
- `.agents/skills/verify-baton/scripts/slash_sweep.py` runs `/plan` and requires the empty-state notice.

## Gotchas

- The legacy ACP `plan` frame is still accepted, but the adapter cannot prove which protocol version the peer negotiated. The v2 item path is covered directly.
- ACP file and markdown plans are ignored. Only a `plan_removed` with the snapshot's own `planId` clears it.
- A plan is cleared when the thread starts a new turn, `/multi`, or orchestration, and when it rewinds. A retained plan from the previous settled turn is not labeled with that turn number.
- Orchestration workers run outside the TUI process, so their plan events are discarded and opening the worker thread shows the empty state.
- The populated path is PTY-tested against a fake Codex server. No live provider plan event has been recorded yet.
