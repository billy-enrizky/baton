---
name: verify-baton-ci
description: "Drive Baton's local CI helpers and prove the classifier, workflow guard, budget wrapper, and debt ratchets. Use before changing ci/, .github/workflows/ci.yml, or the CI acceptance record."
---

# Verify Baton CI

This skill proves the zero-cost CI control plane without running a model turn or a GitHub runner. The real workflow run is recorded separately in `docs/ci-acceptance.md`.

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
- The valid workflow fixture with `--bootstrap`.
- A changed test command with and without `--allow-change`.
- `ci/budget.sh 1 sleep 2`, which must exit 124.
- `ci/budget.sh 5 true`, which must exit 0.

## Evidence

The probe writes a timestamped directory under the evidence path with:

- `selftest.log`
- `guard-valid.log`
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
