# Plan review

## Problem statement

A coding harness can propose a multi-step plan while it works, but that plan is otherwise buried in the provider stream. A terminal user needs one stable place to see the current steps, the active step, and what is still pending without reading raw events or asking the model to repeat itself.

## Goals and non-goals

Goals:

- Turn a Codex or ACP plan event into one Baton event shape.
- Let `/plan` open a sanitized overlay for the plan reported by the current thread and harness.
- Show completed, in-progress, and pending steps plus any explanation or priority.
- Let Enter accept a plan and leave plan mode, and Esc return the plan to the composer for revision.
- Expose the same event in `exec --stream-json`.

Non-goals:

- Rejecting or discarding a proposed plan as a separate action.
- Sending a provider message on accept; acceptance changes the permission mode only.
- Persisting the plan across a restart.
- Reconstructing a plan from prose or transcript tool rows.
- Rendering ACP file or markdown plans, adding a CLI or MCP reader for the snapshot, and forwarding orchestration worker plans into the parent TUI.

## Sub-features

- `/plan` opens the last plan reported for the current thread and current harness.
- The header reads `plan · <done>/<total> done` and names the scroll and close keys. When the harness supplied a turn id for the report, the header also names that turn.
- The body lists the explanation when one was supplied, then one row per step.
- Each step uses a stable marker: `✓` completed, `▸` in progress, and `·` pending, cancelled, or unknown.
- Long explanations and steps wrap onto continuation lines that align under the step text. Nothing is clipped with an ellipsis, and the scroll maximum counts the wrapped rows.
- Step content, the explanation, and priority are sanitized before rendering.
- Up, Down, PageUp, PageDown, Home, and End scroll; `j`, `k`, `q`, and Esc are also accepted.
- With no plan for the current thread and harness, the command keeps the composer open and says `no plan available for this thread`.
- A plan from another thread or another harness is never shown.
- In plan mode the header offers `enter accept` and `esc revise`; outside plan mode it offers `esc close`.
- Enter in plan mode switches the thread to the `default` permission mode, closes the overlay, and says `plan accepted`. Enter outside plan mode keeps the overlay open and states that the plan is read-only.
- Esc in plan mode closes the overlay and pre-fills an empty composer with `revise the plan: `; an existing draft is left untouched. `q` always closes without touching the composer.

## Approach

The Codex and ACP adapters translate their native plan event to `TurnEvent::PlanUpdated`. Codex supplies `plan[].step`, `plan[].status`, an optional explanation, and the reporting `turnId`; its `inProgress` status is normalized to `in_progress`. ACP v2 supplies `plan_update` with an item list and stable `planId`, `plan_removed` carries that id, and the v1 `plan` frame remains a compatibility input. ACP entries supply `content`, `status`, and `priority`; the ACP session update carries no turn id, so its snapshot turn id stays empty. The turn id is carried on the snapshot, shown in the overlay header, and projected as `turn_id` by `exec --stream-json`.

The TUI keeps an in-memory snapshot per thread and provider. `/plan` reads only the snapshot for the visible thread and current harness, and an ACP removal clears a snapshot only when its plan id matches. New work clears the previous snapshots before the submit leaves the TUI; a later `TurnStarted` does not remove a plan already emitted by that turn. `/multi` and orchestration clear at their shared start paths, a rewind clears its completed thread, and a restart clears the map. ACP drain frames from another session are filtered before dispatch.

The retention path covers events handled by the TUI process, including the in-process multi-provider children. Orchestration workers run as separate `baton exec` subprocesses, so their plan events are not forwarded to the parent TUI snapshot map yet.

The overlay never edits the plan and never sends a provider message. Accept is one permission-mode transition: in plan mode Enter calls the same `apply_permission_mode` path `/mode default` uses, persists the thread, and closes the overlay, so the next turn runs with the accepted mode. Esc returns control to the composer with a `revise the plan: ` draft when the composer is empty, which lets the user describe the change and send it as the next turn. Rejecting without feedback stays out of scope.

## Alternatives considered

| Alternative | Decision | Reason |
|---|---|---|
| Parse each provider payload in the TUI | Rejected | Codex and ACP use different field names. Normalizing in each adapter keeps provider spellings out of the renderer and gives `exec --stream-json` one stable shape. |
| Persist every plan in SQLite immediately | Deferred | The ACP item path has a `planId`, but Codex plan events do not, and the TUI cannot map either to a durable turn row yet. Persistence needs a synthetic identity and a restart replacement policy before the action slice defines accept, revise, and reject. |
| Reuse transcript tool rows as the plan source | Rejected | Tool rows lose ACP priority and structured status, and a provider can change the plan without emitting a matching display row. |
| Map ACP `plan_update` items and `plan_removed` in the same slice | Accepted | The v2 item list has the same step shape as the legacy frame. The snapshot keeps `planId`, so a removal clears only the matching item plan. File and markdown plans remain a named non-goal. |
| Keep one global snapshot slot | Rejected | A background worker's plan would be dropped, and parallel supervision is a core terminal workflow. A small per-thread/provider map keeps the plan that was actually reported. |
| Send an implementation prompt on accept | Rejected | The plan text is already in the transcript, and an automatic turn would start spend without a user message. Accept changes the permission mode and states the next step; the user sends the prompt. |
| Treat Esc as reject | Rejected | The plan stays in the transcript and `/plan`; Esc prepares a revision draft instead of pretending the plan is gone. A separate reject action has no provider command yet. |
| Allow Enter to accept outside plan mode | Rejected | Outside plan mode the plan is progress information, not an approval gate. Enter reports that the view is read-only instead of silently changing permission mode. |

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| A provider sends control characters in step text, priority, or explanation | All three pass through `sanitize_terminal_text`; the status is normalized to a known vocabulary before rendering. |
| A stale plan is shown after new work, rewind, thread switch, or harness switch | The TUI removes the visible thread's snapshots before submitting a turn, at the `/multi` and orchestration start paths, and on rewind. A later `TurnStarted` does not clear a plan already emitted for that turn. The thread id and provider must both match before `/plan` opens or the overlay draws. |
| Switching back to an old thread loses its plan | The per-thread/provider map retains it until that thread starts another turn or Baton exits. Plans are not persisted across restart. |
| An orchestration worker's plan never reaches the parent TUI | Workers run as `baton exec --json` subprocesses. Their plan event is consumed and discarded: it is not written by `--json`, not persisted, and not inserted into the parent snapshot map. Opening that worker thread reports the empty state. Forwarding it is an explicit non-goal and tracked follow-up. |
| ACP file or markdown `plan_update` arrives and the view stays stale | Those payload types are ignored rather than approximated. A later `plan_removed` clears the item plan; a live ACP plan event has not been recorded yet. |
| A `plan_removed` for another plan clears the visible plan | The snapshot carries its ACP `planId`, and only a removal with the same id clears it. A removal for an ignored file or markdown plan is a no-op. |
| A foreign ACP session update reaches the active turn during reply drain | Drained `session/update` frames are filtered by `sessionId` before plan, usage, or text handling. |
| A harness reports an empty list | The same empty-state notice is used for a missing report and a deliberately empty plan. It states that no plan is available rather than pretending steps exist. |
| A plan is treated as approval | Accept is offered only in plan mode, runs the same permission-mode write `/mode default` uses, and sends no provider message. Outside plan mode Enter keeps the overlay open with a read-only notice. |
| A long explanation or step is silently clipped | The overlay wraps each explanation and step at the rendered width, indents continuation rows under the step text, and keeps the scroll maximum in wrapped rows. |
| End scrolls past the rendered viewport | Rendering records the viewport-aware maximum, and keyboard handling clamps the stored offset to it before every key. |
| A matching plan removal leaves a blank open overlay | The view renders the same `no plan available for this thread` empty state when the current plan disappears. |
| A plan is shown without its reporting turn | The snapshot is cleared when a new turn starts, and the header names the reporting turn whenever the harness supplied one. ACP reports no turn id, so those plans show the header without a turn. |
| Provider snapshots accumulate in memory | The map is bounded by the threads and harnesses observed in one TUI process, and a restart clears it. A cap becomes necessary if a future background service retains them. |
| An agent cannot read the plan through CLI or MCP | This slice is TUI-only. The event is present in `exec --stream-json`, but no durable reader exists. |
| A revision draft overwrites typed text | Esc pre-fills only when the composer is empty; an existing draft is kept byte for byte and the notice still explains the return. |
| Accepting during a running turn changes the live child | The permission mode is applied when a turn starts. Accepting mid-turn persists the mode for the next turn; the running child keeps the mode it was started with, the same rule `/mode` follows. |

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
- `dispatch_submit_clears_the_previous_plan`
- `late_turn_started_does_not_erase_the_new_plan`
- `plan_scroll_end_uses_the_rendered_viewport_limit`
- `plan_step_wraps_instead_of_clipping`
- `plan_header_names_the_reporting_turn`
- `plan_enter_accepts_in_plan_mode`
- `plan_enter_explains_read_only_outside_plan_mode`
- `plan_escape_prefills_revision_in_plan_mode`
- `plan_escape_keeps_an_existing_draft`
- `removing_an_open_plan_renders_the_empty_state`
- `plan_events_keep_the_entries_and_explanation`
- `plan_statuses_use_one_vocabulary`
- `plan_removal_keeps_the_plan_id`
- `.agents/skills/verify-baton/scripts/plan_probe.py` proves the populated overlay in a PTY, including `/mode plan`, the `enter accept` and `esc revise` hints, the Esc revision draft, and the Enter acceptance notice.
- `.agents/skills/verify-baton/scripts/slash_sweep.py` runs `/plan` and requires the empty-state notice.

## Gotchas

- The legacy ACP `plan` frame is still accepted, but the adapter cannot prove which protocol version the peer negotiated. The v2 item path is covered directly.
- ACP file and markdown plans are ignored. Only a `plan_removed` with the snapshot's own `planId` clears it.
- A plan is cleared when the thread starts a new turn, `/multi`, or orchestration, and when it rewinds. A retained plan names its reporting turn when the harness supplied one.
- Orchestration workers run outside the TUI process, so their plan events are discarded and opening the worker thread shows the empty state.
- The populated path is PTY-tested against a fake Codex server. No live provider plan event has been recorded yet.
