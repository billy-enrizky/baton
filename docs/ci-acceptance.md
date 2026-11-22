# Baton CI acceptance record

Three recorded runs: one billing failure and two green.

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

`--log` is required for the cache key and each step's timestamps. `ci/budget.sh` prints no budget line of its own, so a command's duration is its step's `completedAt` minus `startedAt`.

For a PR run, also record the selected gate outputs from the `changes` job and the cache key from the `rust-cache` step log.

## Run log

| Run | Event | Commit | Workflow revision | Created | Queue | Feedback | Slowest job | Cache key | Selected gates | Result | Failure or budget miss |
|---|---|---|---|---|---|---|---|---|---|---|---|
| [44969473815](https://github.com/billy-enrizky/baton/actions/runs/44969473815) | pull_request | `cb1119bc` | Phase 1C `ci.yml` in PR #90 | 2026-11-22 01:01:42Z | not observed | 2s | none started | none | none | failure | Account billing, before any step ran. The repository is now public, which removed the billing dependency |
| [44982816103](https://github.com/billy-enrizky/baton/actions/runs/44982816103) | pull_request | `223e5f30` | Phase 1C `ci.yml` | 2026-11-22 03:56:06Z | 0s | 190s | Test 165s | cold | all | success | none |
| [44983114852](https://github.com/billy-enrizky/baton/actions/runs/44983114852) | push | `107ffa4` | Phase 1C `ci.yml` on main | 2026-11-22 04:00:30Z | 0s | 195s | Test 171s | cold | all | success | none |

First real measurements, cold cache, `ubuntu-24.04`:

| Job | PR run 44982816103 | Push run 44983114852 |
|---|---|---|
| Changes | 13s | 10s |
| Selection guard | 11s | 5s |
| Check | 55s | 69s |
| Build | 69s | 70s |
| Test | 165s | 171s |
| Debt ratchet | 54s | 53s |
| Verify | 5s | 5s |
| **PR feedback** | **190s** | 195s |

Both runs were cold: the first had no cache, and the push run saved the first one. The next code PR measures the warm number.

The target is 60s median and 120s p95. The gap is the test job, which compiles the suite and then runs it, and the compile is repeated in Check, Build, and Debt. Optimization order: record per-test-binary times, shard from that data, then look at sharing compilation between jobs.

That is the first recorded Actions observation. It proves the workflow file parses and schedules every job. No step ran, so it is not evidence about any gate.

Every later row must come from a run that executed at least the `changes` job. A run that stops before its first step is recorded here as a billing or quota event, not as a gate result.

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
| Included Actions minutes (private repo) | Not recorded |
| Per-minute rate above the included allowance | Not recorded |
| Monthly minute cap | Not set |
| Pause threshold | Not set |
| Paid runner decision | Not evaluated |

Billable minutes are summed per job as `ceil(job_seconds / 60)`. The estimate must include cancelled and superseded runs, workflow_dispatch, and the calibration runs. It must separate docs-only from Rust PR volume.

## Proof log

| Proof | Evidence | Result |
|---|---|---|
| First green `main` full-suite run | Run 44983114852 | Passed 2026-11-22 |
| First PR run with `verify` | Run 44982816103 | Passed 2026-11-22 |
| Budget overrun fails | Phase 1B `ci/selftest.sh`, exit 124 | Passed locally on 2026-11-21; CI pending |
| Hidden TUI change cannot skip tests | Phase 1 classifier fixture plus Phase 3 selector proof | Classifier fixture passes locally; selector proof pending |
| No-op test command fails the workflow guard | Phase 1C `ci/check-ci.sh --base` | Passed locally against the Phase 1C workflow; CI pending |
| Formal CI gate | `ci/formal/ci-admission/run.sh` | Passed locally on 2026-11-21; requires a JDK, not yet in CI |
| End-to-end CI probe | `.agents/skills/verify-baton-ci/scripts/ci_probe.sh` | Pending |
| Branch protection requires `verify` | Repository owner action | Pending |
| Merge-queue full suite | Blocked by repository ownership and plan | Pending |
