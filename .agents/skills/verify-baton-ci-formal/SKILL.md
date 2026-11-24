---
name: verify-baton-ci-formal
description: "Model-check Baton's CI admission rules. Use when changing ci/classify-paths.sh, ci/check-ci.sh, the workflow gate jobs, or the verify aggregator."
---

# Verify Baton CI admission

The CI gate is a state machine: a path classifier selects gates, each selected gate either runs or is skipped, the workflow guard checks command identity, and `verify` decides whether the run is green. A bug here can merge a broken change while every job reports success.

In the workflow, `changes` is an untrusted fast path that only classifies, and `selection` owns the trusted guard and the recomputation check. Pull requests and merge groups stage base tools; main pushes and dispatches stage head tools as a post-merge proof. `test` is a five-shard matrix and `inventory` sums their logs; inventory cannot run without test. `selection` is the gate that must succeed for the workflow guard to count.

The model is committed at `ci/formal/ci-admission/`.

## What is verified

- `ci/classify-paths.sh`: selected gates for push, merge group, dispatch, pull request, and unknown paths.
- `ci/check-ci.sh`: command identity and workflow guard.
- `.github/workflows/ci.yml`: `changes`, `selection`, `check`, `build`, `test`, `inventory`, `debt`, and `verify` gate names and relationships. `changes` has no checkout and no pass/fail guards; `selection` runs them.

The model covers selection, start, result, skip cause, command identity, a `CommandDriftAllowed` constant, and the `verify` verdict. The real configuration sets `CommandDriftAllowed = FALSE`; there is no label override in the workflow. It does not cover test inventory completeness, shard union, the shallow-fetch implementation, or the shell implementation inside a gate command beyond its identity.

## Properties

- `NoFalseGreen`: a green `verify` requires successful `changes`, `selection`, and `workflow_guard`; every selected gate started and succeeded; every skipped gate was unselected.
- `FullSelection`: push, merge group, and workflow dispatch select every gate.
- `NoRetryMasking`: a failure never becomes a green run.
- `ChangesFailOpen`: an unknown diff selects every gate.
- `InventoryNeedsTest`: inventory cannot be selected while test is unselected.
- `CommandsMatch`: with `CommandDriftAllowed = FALSE`, a green run uses the base workflow commands. The one mutation config that sets the constant to TRUE exists only to prove the property depends on it.

## Run

From the repository root:

```sh
JAVA_HOME="$HOME/.local/lib/jdk-21.0.13.1+2/Contents/Home" \
TLA2TOOLS_JAR="$HOME/Documents/baton/local_docs/verification/turn-state-machine/.tools/tla2tools.jar" \
ci/formal/ci-admission/run.sh
```

If `TLA2TOOLS_JAR` is not set, `run.sh` downloads TLA+ 1.7.4 to `${TMPDIR:-/tmp}`.

Read every failing trace with the pstack `/statechart` skill. It presents the shortest path to the violating state in the gate names used here.

## Mutations that must fail

- `BatonCiAdmissionSkipSelected.cfg`: a selected gate is skipped but counted as success. Catches `NoFalseGreen`.
- `BatonCiAdmissionMergeSelection.cfg`: merge group uses a partial selection. Catches `FullSelection`.
- `BatonCiAdmissionRetryMask.cfg`: failure becomes success. Catches `NoRetryMasking`.
- `BatonCiAdmissionUnknownSelection.cfg`: unknown path selects nothing. Catches `ChangesFailOpen`.
- `BatonCiAdmissionCommandDrift.cfg`: sets `CommandDriftAllowed = TRUE` so changed commands pass. Catches `CommandsMatch`.
- `BatonCiAdmissionInventoryWithoutTest.cfg`: selects inventory without test on a partial merge group. Catches `InventoryNeedsTest`.
- `BatonCiAdmissionReportWithoutStart.cfg`: a gate reports success without starting. Catches `NoFalseGreen`.

## When to update

Update the model and this skill together when a gate name, selection output, `verify` rule, or command-identity rule changes. A failing invariant is a finding, not a reason to weaken the model.

The model does not need a new configuration for the fast path: moving the guard from `changes` to `selection` preserves the `NoFalseGreen` requirement that both gate jobs succeed before `verify` can be green. Job-level wiring and Git typechanges are enforced by `CI013` fixtures and the self-test, not by the admission model.
