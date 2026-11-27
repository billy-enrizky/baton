# Plan review

## Problem statement

A coding harness can propose a multi-step plan while it works, but that plan is otherwise buried in the provider stream. A terminal user needs one stable place to see the current steps, the active step, and what is still pending without reading raw events or asking the model to repeat itself.

## Goals and non-goals

Goals:

- Turn a Codex or ACP plan event into one Baton event shape.
- Let `/plan` open a sanitized overlay for the plan reported by the current thread and harness.
- Show completed, in-progress, and pending steps plus any explanation or priority.
- Render ACP markdown plans and show the URI of an ACP file plan.
- Let Enter accept a plan and leave plan mode, Esc return the plan to the composer for revision, and `r` reject it into a different-approach draft.
- Expose the same event in `exec --stream-json`.

Non-goals:

- Rejecting or discarding a proposed plan as a separate action.
- Sending a provider message on accept; acceptance changes the permission mode only.
- Persisting plans for threads the user has never opened or for providers that are no longer known.
- Reconstructing a plan from prose or transcript tool rows.
- Opening a file plan from the overlay or adding a CLI or MCP reader for the snapshot.
- Forwarding a worker's plan while the worker is still running; the parent receives it when the worker settles.

## Sub-features

- `/plan` opens the last plan reported for the current thread and current harness.
- The header reads `plan · <done>/<total> done` and names the scroll and close keys. When the harness supplied a turn id for the report, the header also names that turn.
- The body lists the explanation when one was supplied, then one row per step. A markdown plan renders through the transcript markdown renderer. A file plan shows `file plan: <uri>`. An unknown future kind shows an explicit placeholder.
- Each step uses a stable marker: `✓` completed, `▸` in progress, and `·` pending, cancelled, or unknown.
- Long explanations and steps wrap onto continuation lines that align under the step text. Nothing is clipped with an ellipsis, and the scroll maximum counts the wrapped rows.
- Step content, the explanation, and priority are sanitized before rendering.
- Up, Down, PageUp, PageDown, Home, and End scroll; `j`, `k`, `q`, and Esc are also accepted.
- With no plan for the current thread and harness, the command keeps the composer open and says `no plan available for this thread`.
- A plan from another thread or another harness is never shown.
- The last plan for each thread and harness is stored in SQLite and rehydrated when the TUI starts or the thread is opened, so `/plan` survives a restart.
- A new submit, `/multi`, orchestration preview, orchestration start, transcript rewind, or native rewind clears the thread's stored plans as well as the in-memory ones.
- An orchestration worker's final plan reaches the parent TUI, the worker thread's snapshot, and the `plans` table after the worker settles.
- In plan mode the header offers `enter accept`, `r reject`, and `esc revise`; outside plan mode it offers `esc close`.
- Enter in plan mode switches the thread to the `default` permission mode, closes the overlay, and says `plan accepted`. Enter outside plan mode keeps the overlay open and states that the plan is read-only.
- Esc in plan mode closes the overlay and pre-fills an empty composer with `revise the plan: `; an existing draft is left untouched. `r` in plan mode closes the overlay and pre-fills `reject this plan and propose a different approach: ` without changing the permission mode. Outside plan mode `r` keeps the overlay open and reports that it is read-only. `q` always closes without touching the composer.

## Approach

The Codex and ACP adapters translate their native plan event to `TurnEvent::PlanUpdated`. Codex supplies `plan[].step`, `plan[].status`, an optional explanation, and the reporting `turnId`; its `inProgress` status is normalized to `in_progress`. ACP v2 supplies `plan_update` with an item list and stable `planId`, a markdown body, or a file URI; `plan_removed` carries that id, and the v1 `plan` frame remains a compatibility input. ACP item entries supply `content`, `status`, and `priority`; markdown and file plans become one `PlanEntry` with `kind` and, for a file, `uri`. The ACP session update carries no turn id, so its snapshot turn id stays empty. The turn id is carried on the snapshot, shown in the overlay header, and projected as `turn_id` by `exec --stream-json`.

The TUI keeps an in-memory snapshot per thread and provider. `/plan` reads only the snapshot for the visible thread and current harness, and an ACP removal clears a snapshot only when its plan id matches. New work clears the previous snapshots before the submit leaves the TUI; a later `TurnStarted` does not remove a plan already emitted by that turn. `/multi` and orchestration clear at their shared start paths, a rewind clears its completed thread, and a restart clears the map. ACP drain frames from another session are filtered before dispatch.

The retention path covers events handled by the TUI process, including the in-process multi-provider children. Each accepted plan is written to the `plans` table keyed by thread and harness; `hydrate_plans` loads that thread's rows when the TUI starts and when `load_thread_state` opens a thread, and the whole-thread clear paths delete both the memory and the row. Orchestration workers run as `baton exec --json` subprocesses. The exec stream keeps the last plan, including clearing it on a matching `plan_removed`, and adds a `plan` object to the final `--json` result; the parent parses that object (and still accepts JSONL rows from an older worker), emits `WorkerPlanReported`, stores the plan under the worker thread, and the TUI inserts it into that thread's snapshot and the `plans` table.

The overlay never edits the plan and never sends a provider message. Accept is one permission-mode transition: in plan mode Enter calls the same `apply_permission_mode` path `/mode default` uses, persists the thread, and closes the overlay, so the next turn runs with the accepted mode. Esc returns control to the composer with a `revise the plan: ` draft when the composer is empty, which lets the user describe the change and send it as the next turn. `r` prepares a different-approach draft and leaves the thread in plan mode; rejecting without feedback stays out of scope.

## Alternatives considered

| Alternative | Decision | Reason |
|---|---|---|
| Parse each provider payload in the TUI | Rejected | Codex and ACP use different field names. Normalizing in each adapter keeps provider spellings out of the renderer and gives `exec --stream-json` one stable shape. |
| Persist every plan in SQLite immediately | Accepted | One row per thread and harness is enough for a restart, and the turn id from Codex is stored when present. ACP plans without a turn id still persist by thread and harness. Clearing follows the same submit, multi, orchestration, and rewind paths as the in-memory map. |
| Reuse transcript tool rows as the plan source | Rejected | Tool rows lose ACP priority and structured status, and a provider can change the plan without emitting a matching display row. |
| Map ACP `plan_update` items, markdown, and files in the same slice | Accepted | The v2 item list has the same step shape as the legacy frame. The snapshot keeps `planId`, so a removal clears only the matching plan. Markdown and file plans use the same `PlanEntry` stream with an optional `kind` and `uri`, and item plans keep their original wire shape. |
| Keep one global snapshot slot | Rejected | A background worker's plan would be dropped, and parallel supervision is a core terminal workflow. A small per-thread/provider map keeps the plan that was actually reported. |
| Send an implementation prompt on accept | Rejected | The plan text is already in the transcript, and an automatic turn would start spend without a user message. Accept changes the permission mode and states the next step; the user sends the prompt. |
| Treat Esc as reject | Rejected | The plan stays in the transcript and `/plan`; Esc prepares a revision draft instead of pretending the plan is gone. `r` is the explicit reject action and prepares a different-approach draft without changing permission mode. |
| Allow Enter to accept outside plan mode | Rejected | Outside plan mode the plan is progress information, not an approval gate. Enter reports that the view is read-only instead of silently changing permission mode. |

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| A provider sends control characters in step text, priority, or explanation | All three pass through `sanitize_terminal_text`; the status is normalized to a known vocabulary before rendering. |
| A stale plan is shown after new work, rewind, thread switch, or harness switch | The TUI removes the visible thread's snapshots before submitting a turn, at the `/multi` and orchestration start paths, and on rewind. A later `TurnStarted` does not clear a plan already emitted for that turn. The thread id and provider must both match before `/plan` opens or the overlay draws. |
| A restart shows a stale plan | Plans are stored per thread and harness and cleared by every new-work path. A plan from the last settled turn is intentionally kept, and the header names its reporting turn when the harness supplied one. |
| An orchestration worker's plan never reaches the parent TUI | Workers run as `baton exec --json` subprocesses. The final result carries the last plan, and after a worker settles the parent emits `WorkerPlanReported` and stores it under the worker thread. A worker that reports no plan opens with the empty state, and a plan reported mid-run is not shown until the worker settles. |
| ACP file or markdown `plan_update` arrives and the view stays stale | Those payload types become a `PlanEntry` with `kind` and optional `uri`. Markdown renders through the sanitized markdown renderer, file plans show the sanitized URI, and an unknown kind becomes an explicit placeholder. A later `plan_removed` still clears the matching plan ID. A markdown or file payload without its required field is ignored, so a malformed update does not clear a valid plan. |
| A `plan_removed` for another plan clears the visible plan | The snapshot carries its ACP `planId`, and only a removal with the same id clears it. A removal for a markdown or file plan clears it by the same plan ID rule. |
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

Run a turn on a harness that reports plans, then type `/plan`. A Codex app server sends `turn/plan/updated`; an ACP v2 agent sends `sessionUpdate: "plan_update"` with an item, markdown, or file plan, and a v1 peer can send `sessionUpdate: "plan"`. In a clean isolated instance, `/plan` reports the empty state. The populated Codex item path is driven through a fake app server in a real PTY. The ACP probe starts a fake `agy` ACP server, runs one markdown plan turn and one file plan turn, opens `/plan` after each, and checks the rendered markdown and file URI:

```sh
.agents/skills/verify-baton/scripts/plan_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence

uv run --no-project python .agents/skills/verify-baton/scripts/acp_plan_probe.py \
  --binary target/debug/baton \
  --evidence-dir local_docs/verification/evidence
```

## Proof

- `codex_adapter_maps_plan_updates`
- `acp_adapter_maps_plan_updates`
- `acp_adapter_maps_the_legacy_plan_frame`
- `acp_adapter_preserves_markdown_file_and_unknown_plan_kinds`
- `plan_kinds_round_trip_and_keep_the_items_wire_shape`
- `plan_view_renders_markdown_and_file_plans`
- `plan_documents_round_trip_through_the_store`
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
- `plans_round_trip_per_provider`
- `saving_a_plan_replaces_the_same_provider`
- `plans_are_scoped_and_removable`
- `unknown_stored_providers_are_skipped`
- `plan_event_is_persisted_and_hydrated_after_restart`
- `submit_clears_the_persisted_plan`
- `plan_removal_clears_the_persisted_plan`
- `thread_switch_hydrates_its_own_plan`
- `parse_exec_output_reads_a_stream_json_result_and_plan`
- `parse_exec_output_keeps_the_single_json_result`
- `parse_exec_output_accepts_an_untyped_result_after_other_rows`
- `parse_exec_output_reports_stderr_when_nothing_parses`
- `worker_plan_reaches_the_progress_callback`
- `worker_plan_progress_fills_the_worker_snapshot`
- `removing_an_open_plan_renders_the_empty_state`
- `plan_events_keep_the_entries_and_explanation`
- `plan_statuses_use_one_vocabulary`
- `plan_removal_keeps_the_plan_id`
- `.agents/skills/verify-baton/scripts/plan_probe.py` proves the populated item overlay in a PTY, including `/mode plan`, the `enter accept`, `r reject`, and `esc revise` hints, the Esc revision draft, the reject draft and notice, and the Enter acceptance notice.
- `.agents/skills/verify-baton/scripts/acp_plan_probe.py` proves markdown and file plans through a fake ACP agent and the real TUI overlay.
- `.agents/skills/verify-baton/scripts/slash_sweep.py` runs `/plan` and requires the empty-state notice.

## Gotchas

- The legacy ACP `plan` frame is still accepted, but the adapter cannot prove which protocol version the peer negotiated. The v2 item path is covered directly.
- ACP markdown and file plans are rendered, but a file plan is shown as a URI; the overlay does not open the file. Only a `plan_removed` with the snapshot's own `planId` clears it.
- A plan is cleared when the thread submits new work, runs `/multi` or orchestration, and when it rewinds. A retained plan names its reporting turn when the harness supplied one, and it survives a restart through the `plans` table.
- Orchestration workers run outside the TUI process, so their plan is forwarded only after the worker settles; a plan reported mid-run is not live.
- The item path is PTY-tested against a fake Codex server. The markdown and file path is PTY-tested against a fake ACP server. No live provider document plan event has been recorded yet.
