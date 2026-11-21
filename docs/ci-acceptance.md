# Baton CI acceptance record

Status: no recorded runs yet.

This file records actual GitHub Actions evidence for the CI workflow. Do not add estimated runs, synthetic runs, or runs from an older workflow unless the run URL, commit, and workflow revision are recorded.

## Measurement rules

- PR feedback starts at the workflow run `createdAt` and ends when the required `verify` job completes.
- Queue time is `startedAt - createdAt`.
- Setup time is the first job start to the first check command start.
- Record a cancelled or failed run. Do not remove it because it is slow or red.
- Record the exact commit, workflow revision, event, selected gates, slowest job, cache result, and failure step.

## Run log

| Run | Event | Commit | Created | Queue | Feedback | Slowest job | Cache | Selected gates | Result | Failure or budget miss |
|---|---|---|---|---|---|---|---|---|---|---|
| None yet | | | | | | | | | | |

## Last-20 summary

| Field | Value |
|---|---|
| PR runs sampled | 0 |
| PR p50 | Not measured |
| PR p95 | Not measured |
| Queue p50 | Not measured |
| Queue p95 | Not measured |
| Main runs sampled | 0 |
| Cache hit rate | Not measured |
| Monthly minute estimate | Not measured |
| Paid runner decision | Not evaluated |

## Proof log

| Proof | Evidence | Result |
|---|---|---|
| First green `main` full-suite run | Not run | Pending |
| First PR run with `verify` | Not run | Pending |
| Budget overrun fails | Not run | Pending |
| Hidden TUI change cannot skip tests | Not run | Pending |
| Merge-queue full suite | Blocked by repository ownership and plan | Pending |
