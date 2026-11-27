# Thread tabs

## Problem statement

A terminal user who moves between several threads has only two tools: retype a thread id with `/open` or reopen the picker with `/threads`. Both replace the whole surface and lose the sense of what else is open. A user watching an implementation thread, a review thread, and a side chat needs one persistent strip that names what is open and one key that returns to each.

## Goals and non-goals

Goals:

- Keep every opened thread in a tab strip above the transcript.
- Show each tab's position and a short title, with the active tab highlighted.
- Switch with Alt plus a number from 1 through 9.
- Keep one tab per thread, refresh its title, and close the tab when its thread is archived.
- Keep an unsaved draft in its tab so switching away and back does not lose it.
- Keep composer text with the thread it was typed in.
- Keep a live turn running when its tab is not active, with its stream and tool rows preserved.

Non-goals:

- Advancing a background thread's queued follow-ups on its own. A finished background turn leaves its queue for the next visit.
- Running a tab switch under an orchestration, `/multi`, review, activity, report, picker, or pending request surface.
- A secondary pane, split view, or draggable layout.
- Reordering, renaming, or closing tabs by mouse.
- A `/tabs` picker. Navigation is keyboard-first in this slice.

## Sub-features

- A new tab is added when a thread becomes active through `/open`, `/threads`, `/inbox`, `/fork`, `/btw`, `/worktree`, `/task`, `/new`, a native rewind, or a command palette thread choice.
- One tab per thread id. Opening a thread that already has a tab refreshes that tab instead of adding another.
- The strip renders only when more than one tab is open, so a single-thread session keeps its full transcript height.
- Each label reads `<position>:<title>`, sanitized and truncated to 18 characters. Up to nine tabs render; a ` ...` marker appears when more remain.
- The active tab is accent and bold in color mode and reverse video in monochrome mode.
- Alt+1 through Alt+9 switch to the tab in that position. Alt+0 is ignored so a terminal shortcut is never swallowed. A missing position reports `no tab N`.
- A switch is allowed while a turn runs. The active thread's running flag, queued provider, streaming text, tool rows, and live subagent rows move into a per-thread `ThreadRunState`, and the target tab's state is restored.
- A tab whose turn is still running shows a `*` after its title. The marker clears when that turn settles, whether it was active or in the background.
- Turn events for a non-active open tab update its stored stream and rows. A background finish clears its live buffers; the next visit reloads the finished transcript from the store.
- A failed background turn sets a notice that names the tab title and the error, so a failure is not silent while another tab is active.
- A switch is refused with a notice while an orchestration or `/multi` run is active, a review, activity, or run report is open, a picker is open, or an approval or question waits for an answer.
- Archiving a thread removes its tab. Archiving the active thread still starts `/new`, and the new draft gets its own tab.
- A draft that has never been sent has no store row. Its tab stores the whole thread row, so switching back restores its title, provider, model, cwd, permission mode, and goal before the first message persists it.
- Composer text is saved under the current thread id on every thread open and restored for the thread being opened, so a tab switch never sends one thread's draft to another. Archiving a thread drops its saved composer text.
- Permission mode, session mode, goal, and an orchestration-derived title refresh the active tab's stored row.
- The strip is only drawn on the normal transcript surface. Full-screen report, multi, review, and activity surfaces keep their existing layouts.

## Approach

`TuiState` holds `open_tabs: Vec<OpenThreadTab>`, and each tab holds a `ThreadRow`. Threads that exist in the store are authoritative; a tab's stored row is the fallback for an unsaved draft. `remember_open_thread` updates an existing tab by id or appends a new one, and `forget_open_tab` removes one on archive.

Every path that replaces `state.thread` calls `remember_open_thread` with the same row. The turn-finished refresh, an orchestration title derivation, and the draft config setters call `sync_active_tab` or `remember_open_thread` so the active tab never reports a stale title or mode. `switch_open_tab` reads the tab, prefers the stored thread when it exists, and otherwise calls `load_thread_state` with the tab row.

`TuiState` also holds `background_runs: HashMap<String, ThreadRunState>`. `save_run_state` moves the active running flag, queued provider, streaming text, tool rows, and live subagent rows into that map; `restore_run_state` moves the target thread's entry back, or clears the active buffers when there is none. `load_thread_state` and `open_thread_by_id` save before the swap and restore after it, next to the composer save and restore.

`apply_background_turn_event` handles deltas, tool rows, and subagent rows for an open tab that is not active. `EngineEvent::TurnQueued`, `TurnStarted`, and `TurnFinished` update the same entry for a non-active thread. A thread with no tab, such as an orchestration worker or a `/multi` child, does not grow the map.

The key handler reads Alt plus a digit before overlay handling. It checks the pending request and surface guards first, then switches; the running flag is no longer a guard. `draw` adds a one-row strip above the transcript only when at least two tabs are open, and offsets every later chunk by that row. The strip reads the active `running` flag for the active tab and the stored entry for every other tab.

## Alternatives considered

| Alternative | Decision | Reason |
|---|---|---|
| Store only a thread id and title in the tab | Rejected | `/new` and `/task` produce drafts with no store row. Switching back from another tab would report `no thread matches` and lose the draft. |
| Persist every draft when `/new` runs | Rejected | It would add empty rows to `/threads` and change the documented "unsaved until you send a message" behavior. |
| Allow switching during a running turn | Accepted with per-thread run state | The active buffers move aside and the target's buffers restore. Background deltas and tool rows update the stored entry, and a background finish clears it. Queue auto-advance for a background thread stays out of scope because it needs a thread-specific submit path. |
| Show tabs on full-screen surfaces | Rejected | Run reports, multi runs, reviews, and activity own the whole area. Drawing a strip would shrink their designed layout. |
| Trim the tab strip to the active thread and the tab before it | Rejected | Extra state with no proven need. Nine labels cover the useful case and a marker names the rest. |

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Two tabs for one thread | `remember_open_thread` searches by id before appending; a test opens the same thread twice and asserts one tab. |
| Active thread missing from the strip | Every assignment path calls `remember_open_thread`, and the initial state seeds the first tab. |
| Unsaved draft lost on a switch | The tab stores the full row and `switch_open_tab` falls back to it when the store has no row. |
| Stale draft mode or title | `apply_permission_mode`, `set_session_mode`, `set_goal_state`, the orchestration title derivation, and the turn-finished refresh update the tab row. |
| Composer text follows a tab switch | `stash_composer` saves the active thread's text before the swap, and `restore_composer` loads the target thread's text after it. |
| Switching tears a live turn | `save_run_state` and `restore_run_state` move the running flag, queued provider, stream, tool rows, and live subagents with the thread. The `thread-run-state-machine` gate checks that a switch preserves every thread's state. |
| A background finish leaves a tab stuck running | The non-active `TurnFinished` branch clears the stored running flag and live buffers even when another tab is active. |
| A worker thread grows the background map | Only a thread with an open tab is tracked; `a_worker_turn_does_not_create_a_background_tab` proves it. |
| A background turn's queued message is forgotten | The queue stays in the store and reloads on the next visit. It is not auto-sent; the feature doc and the acceptance record name that limit. |
| Switching under a picker or report leaves stale content | The key handler refuses while any picker or full-screen surface is open. |
| A long title breaks the layout | Titles pass through `sanitize_terminal_text` and `truncate_chars(18)`; the loop stops when the next label would overflow the width. |
| Monochrome loses the active marker | The active style uses reverse video when palette colors are disabled. |
| Archiving leaves a dead tab | `apply_archive_command` removes the tab before it starts `/new` for an active archive. |
| Ten or more tabs crowd the row | The strip renders nine labels and a ` ...` marker when space allows. |

## How to get to it (user POV)

Start Baton in a clean home. The composer opens with one thread and no strip. Run `/new second`. The strip shows `1:New thread` and `2:second`. Press Alt+1 to return to the first thread, then Alt+2 to return to the unsaved draft. Alt+9 reports `no tab 9`. The tabs probe writes the screen after each step.

The background turn probe starts a fake Codex turn that streams `alpha `, waits, then streams `omega`. It switches to a new tab while the first delta is live, types a draft there, returns to see `alpha`, switches away again, waits for the `*` marker to clear after completion, returns to see `alpha omega`, and confirms the other tab's draft survived:

```sh
.agents/skills/verify-baton/scripts/tabs_probe.py \
  --binary "$PWD/target/debug/baton" \
  --evidence-dir local_docs/verification/evidence

.agents/skills/verify-baton/scripts/background_turn_probe.py \
  --binary "$PWD/target/debug/baton" \
  --evidence-dir local_docs/verification/evidence
```

## Proof

- `opening_threads_adds_tabs_and_alt_number_switches`
- `opening_the_same_thread_keeps_one_tab`
- `archiving_a_thread_removes_its_tab`
- `alt_number_without_a_tab_reports_it`
- `alt_number_restores_an_unsaved_draft_tab`
- `tab_switches_keep_a_draft_per_thread`
- `a_background_turn_keeps_streaming_in_its_tab`
- `a_background_turn_finish_clears_its_run_state`
- `a_background_failure_reports_in_the_notice`
- `a_worker_turn_does_not_create_a_background_tab`
- `tabs_probe.py`
- `background_turn_probe.py`

## Limits

- A background thread's queued follow-ups do not auto-send when its turn finishes. The queue stays stored and reloads on the next visit.
- A background finish does not ring the terminal bell. The `*` marker is the signal.
