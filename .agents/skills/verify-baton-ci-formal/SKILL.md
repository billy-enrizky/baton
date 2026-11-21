---
name: verify-baton-ci-formal
description: "Model-check Baton's CI admission rules. Use when changing ci/classify-paths.sh, ci/check-ci.sh, the workflow gate jobs, or the verify aggregator."
---

# Verify Baton CI admission

The CI gate is a state machine: a path classifier selects gates, each selected gate either runs or is skipped, the workflow guard checks command identity, and `verify` decides whether the run is green. A bug here can merge a broken change while every job reports success.

The model is committed at `ci/formal/ci-admission/`.

## What is verified

- `ci/classify-paths.sh`: selected gates for push, merge group, dispatch, pull request, and unknown paths.
- `ci/check-ci.sh`: command identity and workflow guard.
- `.github/workflows/ci.yml`: `changes`, `selection`, `check`, `build`, `test`, `debt`, and `verify` gate names and relationships.

The model covers selection, start, result, skip cause, command identity, explicit `ci-change` override, and the `verify` verdict. It does not cover test inventory completeness, shard union, GitHub API behavior, or the shell implementation inside a gate command beyond its identity.

## Properties

- `NoFalseGreen`: a green `verify` requires successful `changes`, `selection`, and `workflow_guard`; every selected gate started and succeeded; every skipped gate was unselected.
- `FullSelection`: push, merge group, and workflow dispatch select every gate.
- `NoRetryMasking`: a failure never becomes a green run.
- `ChangesFailOpen`: an unknown diff selects every gate.
- `CommandsMatch`: a green run uses the base workflow commands or an explicit `ci-change` override.

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
- `BatonCiAdmissionCommandDrift.cfg`: changed commands pass without `ci-change`. Catches `CommandsMatch`.
- `BatonCiAdmissionReportWithoutStart.cfg`: a gate reports success without starting. Catches `NoFalseGreen`.

## When to update

Update the model and this skill together when a gate name, selection output, `verify` rule, or command-identity rule changes. A failing invariant is a finding, not a reason to weaken the model.

Coverage confirmed at: `013dfaf` plus the CI helper hardening in this PR.
