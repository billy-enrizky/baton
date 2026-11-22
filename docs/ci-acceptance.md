# Baton CI acceptance record

No recorded runs yet.

This file records actual GitHub Actions evidence for [ci.md](ci.md). Do not add estimated runs, synthetic runs, or runs from an older workflow unless the run URL, commit, and workflow revision are recorded.

## Measurement rules

- PR feedback starts at the workflow run `createdAt` and ends when the required `verify` job completes.
- Queue time is `startedAt - createdAt`.
- Per-job setup is the job `startedAt` to the first non-setup command step `startedAt`.
- Per-job run time is the first non-setup command step to job `completedAt`.
- Record a cancelled or failed run. Do not remove it because it is slow or red.
- Record the exact commit, workflow revision, event, selected gates, cache key, slowest job, failure step, and budget miss.

## Collection commands

```sh
gh run list --workflow ci.yml --limit 20 \
  --json databaseId,event,headSha,createdAt,startedAt,updatedAt,conclusion,url
gh run view <run-id> --json jobs
gh run view <run-id> --log
```

`--log` is required for the cache key and the per-step budget line, which `--json jobs` does not return.

For a PR run, also record the selected gate outputs from the `changes` job and the cache key from the `rust-cache` step log.

## Run log

| Run | Event | Commit | Workflow revision | Created | Queue | Feedback | Slowest job | Cache key | Selected gates | Result | Failure or budget miss |
|---|---|---|---|---|---|---|---|---|---|---|---|
| None yet | | | | | | | | | | | |

## Per-job log

| Run | Job | Setup | Run | Budget | Cache | Result | Failure |
|---|---|---|---|---|---|---|---|
| None yet | | | | | | | |

## Last-20 summary

| Field | Value |
|---|---|
| PR runs sampled | 0 |
| Rust PRs sampled | 0 |
| Docs-only PRs sampled | 0 |
| PR p50 | Not measured |
| PR p95 | Not measured |
| Queue p50 | Not measured |
| Queue p95 | Not measured |
| Main runs sampled | 0 |
| Cache hit rate | Not measured |
| Billable minute estimate | Not measured |
| Monthly cap decision | Not approved |
| Paid runner decision | Not evaluated |

Billable minutes are summed per job as `ceil(job_seconds / 60)`. The estimate must include cancelled and superseded runs, workflow_dispatch, and the calibration runs. It must separate docs-only from Rust PR volume.

## Proof log

| Proof | Evidence | Result |
|---|---|---|
| First green `main` full-suite run | Phase 1C workflow | Pending |
| First PR run with `verify` | Phase 1C workflow | Pending |
| Budget overrun fails | Phase 1B `ci/selftest.sh`, exit 124 | Passed locally on 2026-11-21; CI pending |
| Hidden TUI change cannot skip tests | Phase 1 classifier fixture plus Phase 3 selector proof | Classifier fixture passes locally; selector proof pending |
| No-op test command fails the workflow guard | Phase 1C `ci/check-ci.sh --base` | Passed locally against the Phase 1C workflow; CI pending |
| Formal CI gate | `ci/formal/ci-admission/run.sh` | Passed locally on 2026-11-21; requires a JDK, not yet in CI |
| End-to-end CI probe | `.agents/skills/verify-baton-ci/scripts/ci_probe.sh` | Pending |
| Branch protection requires `verify` | Repository owner action | Pending |
| Merge-queue full suite | Blocked by repository ownership and plan | Pending |
