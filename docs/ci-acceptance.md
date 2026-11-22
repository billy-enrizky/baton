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

## Recorded test timings

Run 44988029960, `main` at `a18251c`, `ci/test-timings.sh` on the captured log:

| Test binary | Seconds | Tests |
|---|---|---|
| `unittests src/lib.rs` | 30.11 | 321 |
| `tests/cli_bench.rs` | 1.66 | 2 |
| `tests/opencode_adapter.rs` | 0.13 | 4 |
| `tests/core.rs` | 0.10 | 35 |
| `tests/codex_adapter.rs` | 0.05 | 9 |
| `tests/harness.rs` | 0.05 | 16 |
| `tests/claude_adapter.rs` | 0.02 | 3 |
| `tests/acp_adapter.rs` | 0.01 | 4 |
| `unittests src/main.rs` | 0.00 | 0 |

Total test execution is 32 seconds of a 161-second test job. The other ~130 seconds is compilation.

That changes the optimization order. Sharding tests would move 32 seconds across runners and duplicate a 130-second compile in each, which is worse. The critical path is compile:
1. Stop compiling four times in one run. Check, Build, Test, and Debt each build the same crate from the same cache seed.
2. Only then consider shards, and only if the compile work per job still exceeds the budget.

A per-test harness is also visible: `tests/cli_bench.rs` spawns the binary twice and costs more than 35 core tests together, so it is a candidate to fold into an existing binary rather than a shard.

## Rejected: smaller test debug info

PR #117 set `[profile.test] debug = "line-tables-only"`. Measured locally on a cold target it looked promising: 65.3 user seconds and a 1.0 GB target against 72.9 and 1.2 GB, and `debug = false` reached 60.9 and 821 MB.

On the runner it moved nothing: test job 160 s and 163 s across two runs, against a 155 to 171 s band on `main`. The PR was closed unmerged rather than keeping a setting whose benefit could not be shown. Local CPU time was the wrong proxy; the job is not bound by that slice.

## Per-job cache keys

All four Rust jobs shared one key, and the first finisher on main owned it. That finisher was a lint job, so the archive held no test build.

`shared-key` now includes `github.job`. The test job restores its own 221 MB archive as an exact match, up from a shared 157 MB, and the compile count dropped from 197 crates to 119. Wall time did not move: 165 s before and 165 s after.

Conclusion: the shared-key defect is fixed and worth keeping, but restoring `target` does not make Cargo reuse most of it. Whatever invalidates 119 fingerprints is a deeper problem than a cache key.

## Dispatch was broken and is fixed

A `workflow_dispatch` run failed every gate at `Resolve trusted base ref`. A dispatch event has no `github.event.before`, the step falls back to `HEAD~1`, and the gate jobs checked out one commit. Push and PR runs never exercise that path, so it was broken from the day the workflow landed.

Fixed with `fetch-depth: 2` on the gate checkouts. Dispatch run 44991010841 is green.

## Owner decision: runner size

Every lever inside the job has been measured and exhausted: debug info (no change), per-job cache keys (no wall-clock change), sharding (would duplicate the 130-second compile), fewer test binaries (links are 0.1 to 0.4 seconds warm).

The remaining lever is CPU. The test job is compile-bound at roughly 130 seconds on the standard four-vCPU public runner. Public-repo standard runners are free; larger runners are billed per minute:

| Runner | Rate |
|---|---|
| Linux 4-core larger | $0.012/min |
| Linux 8-core larger | $0.022/min |

Measured job minutes per push run today: 6.6. At the 8-core rate that is about $0.145 per run if durations hold, and about $0.09 if the compile roughly halves as expected.

Monthly scenarios, if the owner approves:

| Runs per month | 8-core, durations hold | 8-core, compile halves |
|---|---|---|
| 100 | $14.50 | $9.00 |
| 300 | $43.50 | $27.00 |
| 600 | $87.00 | $54.00 |

Recommendation: one measured 8-core run before any commitment. If it brings the test job under 100 seconds, the p95 target is reachable and the cost is bounded by the table. If it does not, the bottleneck is not CPU and the next step is profiling the compile itself.

## Cache behaviour

The test job restores a 157 MB cache in about a second, reported as `full match: false`, which means it is a restore-key prefix match rather than the exact key. Dependencies are therefore mostly present and the remaining time is the crate's own compile and the linking of nine test binaries.

That points at two unmeasured candidates, in order:
1. Fewer test binaries. Nine links of one large crate is a structural cost that sharding cannot remove.
2. A larger runner. Standard hosted is four vCPU; a bigger runner is an owner decision with a monthly cost, so it needs the first candidate measured before it is worth asking for.

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
