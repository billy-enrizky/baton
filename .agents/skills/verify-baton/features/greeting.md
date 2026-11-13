# Greeting in orchestrate mode

## Sub-features
- A conversational greeting in orchestrate mode runs a single turn, not a planner
- No orchestration run is created for `hi`, `hello`, `thanks`, or similar
- The reply streams into the transcript with the provider footer
- A non-greeting prompt in the same mode still goes to the coordinator

## How to get to it
Start `baton` in an empty orchestrate-mode thread and type `hi`.

## Driving it with the PTY/CLI harness
```sh
.agents/skills/verify-baton/scripts/greeting_probe.py \
  --binary ~/.cargo/bin/baton \
  --evidence-dir local_docs/verification/evidence
```

The probe guards the reported `goal · hi` / `run failed · planner returned no
tasks` regression. It types `hi` in the default orchestrate mode, waits for the
turn to finish, and passes only when the screen has no planner failure, no
`goal ·` run line, a provider reply, one completed turn, zero orchestration
runs, and the TUI exits 0. It spends one small Codex turn.

## Gotchas
- Do not add `/orchestrate off`. The point is the default mode.
- The greeting list is a fixed vocabulary. A greeting the planner does not know
  still goes to the coordinator by design; add a case to
  `is_conversational_greeting` and the planner test together if that changes.
- Assert on a completed turn and zero runs in SQLite, not only the screen.
