---
name: verify-baton-ci
description: "Drive Baton's local CI helpers and prove the classifier, workflow guard, budget wrapper, and debt ratchets. Use before changing ci/, .github/workflows/ci.yml, or the CI acceptance record."
---

# Verify Baton CI

This skill proves the CI control plane without running a model turn or a GitHub runner. It compiles only tiny fixture crates for the debt-ratchet checks. The real workflow run is recorded separately in `docs/ci-acceptance.md`.

The workflow has two pre-gate jobs. `changes` is the fast path: it fetches only the base and PR head SHAs at depth one, exports the base-owned classifier and budget script, and emits the seven gate-selection outputs. It intentionally has no checkout and no pass/fail guards. `selection` is the trust gate: it stages the base tools read-only on pull requests and merge groups, stages the head tools on main pushes and dispatches, validates gate integrity, runs the full self-test, enforces the baseline ratchets, recomputes classification, compares every output with `changes`, and runs the workflow guard. `verify` requires both jobs, so moving the checks into `selection` cannot produce a green run without them.

## Launch

Run from the repository root:

```sh
bash ci/selftest.sh
```

No build and no provider binary are needed.

## Doctor

Run this first when a fixture looks wrong:

```sh
bash ci/check-ci.sh --bootstrap ci/fixtures/check-ci/valid.yml
```

The command must print `CI013 skipped: bootstrap workflow` and `workflow checks passed`.

## Drive

```sh
.agents/skills/verify-baton-ci/scripts/ci_probe.sh \
  --evidence-dir local_docs/verification/evidence
```

The probe runs:

- `ci/selftest.sh`.
- `ci/debt-selftest.sh`.
- The valid workflow fixture with `--bootstrap`.
- The real workflow with `--bootstrap`.
- A changed test command with and without `--allow-change`.
- `ci/budget.sh 1 sleep 2`, which must exit 124.
- `ci/budget.sh 5 true`, which must exit 0.

## Evidence

The probe writes a timestamped directory under the evidence path with:

- `selftest.log`
- `guard-valid.log`
- `guard-workflow.log`
- `guard-change-rejected.log`
- `guard-change-allowed.log`
- `budget-overrun.log`
- `budget-success.log`
- `summary.json`

A pass requires exit 0 for selftest and allowed commands, exit 124 for budget overrun, and nonzero for the rejected command guard.

## Cleanup

There is nothing to clean up beyond the temporary directory created by `ci/selftest.sh`. Never delete the evidence directory.

## Features

Start with `features/README.md`.

## Gotchas

- The probe never starts a model turn and never needs provider credentials.
- The budget wrapper uses GNU `timeout` when present and a Perl alarm fallback on macOS.
- `ci/check-ci.sh` requires either `--base`, `--bootstrap`, or both. A missing trust mode is a failure.
- The workflow command guard rejects required command changes unless `--allow-change` is explicit.
- `--allow-change` never skips `CI014` or `CI017`, and it never permits job-level wiring changes. It only permits the reviewed base-to-head step comparison.
- Do not add a checkout, self-test, or workflow guard back to `changes`; that puts 5 to 10 seconds on the critical path. The fast path must keep only the shallow fetch and classifier.
- `selection` must keep `gate-integrity.sh`, `selftest.sh`, and `classify-paths.sh` under `ci/budget.sh`. `ci/check-ci.sh` requires all three.
- The pull-request fast path must diff the base tip against the immutable PR head SHA. Do not use `refs/pull/<number>/merge`; it is refreshed asynchronously and can still name the previous head after a force-push.
- Git typechanges (`T`) are included in both the workflow diff and `classify-paths.sh`; a `.rs` file converted to a symlink must not fall into the docs-only path.
