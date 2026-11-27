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

Non-goals:

- Running a tab switch in the middle of a turn. The current turn owns the streaming buffer, the running flag, and the queue, so a switch is refused until the turn settles.
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
- A switch is refused with a notice while a turn runs, an orchestration or `/multi` run is active, a review, activity, or run report is open, a picker is open, or an approval or question waits for an answer.
- Archiving a thread removes its tab. Archiving the active thread still starts `/new`, and the new draft gets its own tab.
- A draft that has never been sent has no store row. Its tab stores the whole thread row, so switching back restores its title, provider, model, cwd, permission mode, and goal before the first message persists it.
- Composer text is saved under the current thread id on every thread open and restored for the thread being opened, so a tab switch never sends one thread's draft to another. Archiving a thread drops its saved composer text.
- Permission mode, session mode, goal, and an orchestration-derived title refresh the active tab's stored row.
- The strip is only drawn on the normal transcript surface. Full-screen report, multi, review, and activity surfaces keep their existing layouts.

## Approach

`TuiState` holds `open_tabs: Vec<OpenThreadTab>`, and each tab holds a `ThreadRow`. Threads that exist in the store are authoritative; a tab's stored row is the fallback for an unsaved draft. `remember_open_thread` updates an existing tab by id or appends a new one, and `forget_open_tab` removes one on archive.

Every path that replaces `state.thread` calls `remember_open_thread` with the same row. The turn-finished refresh, an orchestration title derivation, and the draft config setters call `sync_active_tab` or `remember_open_thread` so the active tab never reports a stale title or mode. `switch_open_tab` reads the tab, prefers the stored thread when it exists, and otherwise calls `load_thread_state` with the tab row.

The key handler reads Alt plus a digit before overlay handling. It checks the pending request, running, and surface guards first, then switches. `draw` adds a one-row strip above the transcript only when at least two tabs are open, and offsets every later chunk by that row.

## Alternatives considered

| Alternative | Decision | Reason |
|---|---|---|
| Store only a thread id and title in the tab | Rejected | `/new` and `/task` produce drafts with no store row. Switching back from another tab would report `no thread matches` and lose the draft. |
| Persist every draft when `/new` runs | Rejected | It would add empty rows to `/threads` and change the documented "unsaved until you send a message" behavior. |
| Allow switching during a running turn | Rejected for this slice | Turn events, streaming text, tool rows, the running flag, and queue advancement are keyed to the active thread. A switch would drop the finish event and leave the new thread stuck running. |
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
| Switching tears a live turn | The key handler refuses the switch while a turn, orchestration, or `/multi` run is active and says why. |
| Switching under a picker or report leaves stale content | The key handler refuses while any picker or full-screen surface is open. |
| A long title breaks the layout | Titles pass through `sanitize_terminal_text` and `truncate_chars(18)`; the loop stops when the next label would overflow the width. |
| Monochrome loses the active marker | The active style uses reverse video when palette colors are disabled. |
| Archiving leaves a dead tab | `apply_archive_command` removes the tab before it starts `/new` for an active archive. |
| Ten or more tabs crowd the row | The strip renders nine labels and a ` ...` marker when space allows. |

## How to get to it (user POV)

Start Baton in a clean home. The composer opens with one thread and no strip. Run `/new second`. The strip shows `1:New thread` and `2:second`. Press Alt+1 to return to the first thread, then Alt+2 to return to the unsaved draft. Alt+9 reports `no tab 9`. The probe writes the screen after each step:

```sh
.agents/skills/verify-baton/scripts/tabs_probe.py \
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
- `alt_number_is_blocked_while_a_turn_runs`
- `tabs_probe.py`
