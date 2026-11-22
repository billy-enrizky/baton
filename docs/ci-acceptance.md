# Baton CI acceptance record

Status on 2026-11-22: the current warm Rust PR sample is 58 seconds. The target is met on that sample, not yet established as a p50.

This file records the runs that changed a decision. It is not a complete archive of every GitHub Actions run. The Actions history remains the raw record, and the collection commands below reproduce these fields.

## Targets and current status

| Target | Current evidence | Status |
|---|---|---|
| Rust PR feedback p50 under 60 seconds | PR 45004705055: 58 seconds, one warm sample | Met on the sample; p50 needs more runs |
| Rust PR feedback p95 under 120 seconds | PR 45004705055: 58 seconds, one warm sample | Provisional |
| Docs-only PR feedback p50 under 45 seconds | Historical PR 45000304120: 45 seconds on the pre-optimization workflow | Not established under the current workflow |
| Queue p50 under 10 seconds | Not enough samples for a run-level queue statistic | Not established |
| Queue p95 under 20 seconds | Requires 100 runs before p95 is acceptance-bearing | Not established |
| Setup p95 under 15 seconds | Warm Test setup was 16 seconds in PR 45004705055 | Not established |
| Hidden retries | `ci/check-ci.sh` rejects retry actions and `continue-on-error` | Enforced |
| Skipped required checks | `verify` requires every selected job | Enforced |
| Failed required check reported as success | `verify` and the formal admission model | Enforced |

A p95 from a small sample is a maximum in disguise. Until 100 runs exist, this record reports the sample count, median, and maximum, and labels any p95 provisional.

## Measurement definitions

- PR feedback starts at the workflow run `createdAt` and ends when `verify` completes on a `pull_request` run.
- For `push`, `merge_queue`, and `workflow_dispatch`, the same interval is called run create to verify, not PR feedback.
- Queue time is the run-level `startedAt - createdAt`, not a job's dependency wait.
- Per-job setup is the job `startedAt` to its first non-setup command step.
- A job duration is its `startedAt` to `completedAt`.
- Record failed and cancelled runs. Do not remove a slow or red run because it is inconvenient.
- Do not infer a cache hit, commit, or step time from prose. Use the run log or Actions API.

## Current warm path

PR 45004705055, merge candidate `fec3c862`, `ubuntu-24.04`:

| Checkpoint | Offset from run create |
|---|---|
| Run create | 0 seconds |
| Changes complete | 17 seconds |
| Gate jobs start | 18 seconds |
| Test complete | 52 seconds |
| Verify complete | 58 seconds |

The Changes job took 14 seconds, the Test job took 33 seconds, and the Verify job took 3 seconds. The remaining 8 seconds were queue, scheduling, and finalization.

Test job setup was 16 seconds, including 4 seconds for the rustup cache, 3 seconds for the Cargo cache, and 9 seconds for checkouts and job setup. The test step contained about 13 seconds of compile and link plus 1.73 seconds of lib tests. Test inventory recorded 401 tests.

The slowest test binary was no longer a 30-second outlier. The lib binary ran 326 tests in 1.73 seconds. The seven integration binaries together ran 75 tests in 0.4 seconds.

## How the critical path changed

| Run | Event | Result | What changed |
|---|---|---|---|
| [44982816103](https://github.com/billy-enrizky/baton/actions/runs/44982816103) | pull_request | 190 seconds, cold | First green full-suite PR |
| [44995838075](https://github.com/billy-enrizky/baton/actions/runs/44995838075) | push | 124 seconds | Cargo log no longer waits on a pipe held by a descendant |
| [44998824842](https://github.com/billy-enrizky/baton/actions/runs/44998824842) | workflow_dispatch | 95 seconds | Rust cache v2 and a warm test target |
| [45003589223](https://github.com/billy-enrizky/baton/actions/runs/45003589223) | pull_request | 69 seconds | Cancellation group kill removes a 30-second test |
| [45004403409](https://github.com/billy-enrizky/baton/actions/runs/45004403409) | workflow_dispatch | 60 seconds | Gates start before the full tool self-test finishes |
| [45004517552](https://github.com/billy-enrizky/baton/actions/runs/45004517552) | workflow_dispatch | 51 seconds | Same workflow, warm run |
| [45004705055](https://github.com/billy-enrizky/baton/actions/runs/45004705055) | pull_request | 58 seconds | First warm Rust PR under the current workflow |

The 30-second long pole was `orchestrate::tests::cancellation_stops_the_worker_group_and_marks_the_run_cancelled`. On Linux, `kill -TERM -PGID` was accepted but did not signal the process group. The shell leader died, its `sleep` child kept the output pipe open, and the reader threads waited the full 30 seconds. Adding `--` before the negative group operand fixed the signal. `group_alive` then used `ps` to ignore the unreaped leader zombie instead of spending another 2.5 seconds on `kill -0`.

Before PR 45003589223, lib tests took 30.06 seconds and the Test job took 62 seconds. After, lib tests took 1.69 seconds and the Test job took 44 seconds.

## Cache state

Warm Test cache in PR 45004705055:

| Cache | Key | Size | Match | Restore time |
|---|---|---|---|---|
| Rust toolchain | `rustup-1.100.0-Linux-X64-test` | 409,824,536 bytes | exact key hit | 4 seconds |
| Cargo target and registry | `v0-rust-baton-v2-Linux-X64-test-Linux-x64-2fd77104-bc9f7da2` | 253,171,713 bytes | `full match: true` | 3 seconds |

Saves are restricted to `main`. A PR cannot write the cache that another PR restores.

## Rejected experiments

| Experiment | Run | Result | Decision |
|---|---|---|---|
| Eight test threads before the cancellation fix | [44999117575](https://github.com/billy-enrizky/baton/actions/runs/44999117575) | Test 63 seconds against a 62-second warm baseline | Rejected; the 30-second test dominated |
| Eight test threads after the scoped lock | [45001829417](https://github.com/billy-enrizky/baton/actions/runs/45001829417) | Test 77 seconds | Rejected; the cancellation bug still dominated |
| Put test repositories in `/dev/shm` | [45002182299](https://github.com/billy-enrizky/baton/actions/runs/45002182299) | Test 64 seconds; `/dev/shm` had 7.9 GB free | Rejected; the suite is not disk-bound |
| Count-based nextest shards | [45002437721](https://github.com/billy-enrizky/baton/actions/runs/45002437721) | Shard 1 33 seconds, shard 2 61 seconds | Rejected; count partitions were unbalanced, and the run was red because the experiment changed the job list |
| Install the pinned toolchain instead of restoring 391 MB | [45004219154](https://github.com/billy-enrizky/baton/actions/runs/45004219154), [45004357746](https://github.com/billy-enrizky/baton/actions/runs/45004357746) | Test 40 and 43 seconds against a 44 to 45 second baseline | Rejected; no consistent gain |
| Smaller test debug info | PR #117 | 160 and 163 seconds against a 155 to 171 second band | Rejected; no runner benefit |

The nextest experiment is not evidence for sharding. It showed that a count partition can put one 30-second test into a 61-second shard. After the cancellation fix the whole lib binary takes 1.73 seconds, so sharding is no longer on the critical path.

The per-repository worktree lock in PR #132 did not change CI wall time. It is kept because one process-wide lock made unrelated repositories wait on each other; it is a scope fix, not a speed claim.

## Cited run log

| Run | Event | Commit | Result | Create to Verify | Test | Decision evidence |
|---|---|---|---|---|---|---|
| [44969473815](https://github.com/billy-enrizky/baton/actions/runs/44969473815) | pull_request | `cb1119bc` | failure | not measured | not started | Billing stopped every job before its first step |
| [44982816103](https://github.com/billy-enrizky/baton/actions/runs/44982816103) | pull_request | `223e5f30` | success | 190 seconds | 165 seconds | First green full-suite PR, cold cache |
| [44983114852](https://github.com/billy-enrizky/baton/actions/runs/44983114852) | push | `107ffa4c` | success | 195 seconds | 171 seconds | First green full-suite main push, cold cache |
| [44988029960](https://github.com/billy-enrizky/baton/actions/runs/44988029960) | push | `a18251c0` | success | 195 seconds | 165 seconds | Per-binary timing sample before the pipe fix |
| [44995838075](https://github.com/billy-enrizky/baton/actions/runs/44995838075) | push | `5f44e30f` | success | 124 seconds | 101 seconds | Removed the 60-second pipe wait |
| [44997380622](https://github.com/billy-enrizky/baton/actions/runs/44997380622) | push | `bb0b195e` | failure | 165 seconds | 144 seconds | Worktree race reproduced under concurrent workers |
| [44998824842](https://github.com/billy-enrizky/baton/actions/runs/44998824842) | workflow_dispatch | `978df9e7` | success | 95 seconds | 62 seconds | Warm cache v2 baseline |
| [44999117575](https://github.com/billy-enrizky/baton/actions/runs/44999117575) | workflow_dispatch | `e5697af8` | success | 85 seconds | 63 seconds | Eight test threads rejected |
| [44999152137](https://github.com/billy-enrizky/baton/actions/runs/44999152137) | push | `f93b549a` | success | 88 seconds | 60 seconds | Best pre-cancellation warm push |
| [45000304120](https://github.com/billy-enrizky/baton/actions/runs/45000304120) | pull_request | `485d53d2` | success | 45 seconds | skipped | Historical docs-only path before the quick preflight |
| [45000399622](https://github.com/billy-enrizky/baton/actions/runs/45000399622) | push | `3d6467bd` | success | 97 seconds | 70 seconds | 30.06-second lib tests |
| [45001645126](https://github.com/billy-enrizky/baton/actions/runs/45001645126) | workflow_dispatch | `d4188016` | success | 87 seconds | 62 seconds | Per-repository lock did not move wall time |
| [45001829417](https://github.com/billy-enrizky/baton/actions/runs/45001829417) | workflow_dispatch | `7b63aa9` | success | 113 seconds | 77 seconds | Eight threads after scoped locking rejected |
| [45002182299](https://github.com/billy-enrizky/baton/actions/runs/45002182299) | workflow_dispatch | `0c383f16` | success | 92 seconds | 64 seconds | `/dev/shm` test directories rejected |
| [45002437721](https://github.com/billy-enrizky/baton/actions/runs/45002437721) | workflow_dispatch | `8952eb5` | failure | not measured | skipped | Nextest count shards unbalanced; job list change failed CI013 |
| [45002826292](https://github.com/billy-enrizky/baton/actions/runs/45002826292) | workflow_dispatch | `3ecb4bb` | failure | not measured | skipped | Cancellation diagnostic proved the 30-second pipe wait |
| [45003043436](https://github.com/billy-enrizky/baton/actions/runs/45003043436) | workflow_dispatch | `0b3a181` | failure | not measured | skipped | Process-group probe identified the missing `--` |
| [45003589223](https://github.com/billy-enrizky/baton/actions/runs/45003589223) | pull_request | `bc76fcc9` | success | 69 seconds | 44 seconds | Cancellation fix, lib tests 1.69 seconds |
| [45003991763](https://github.com/billy-enrizky/baton/actions/runs/45003991763) | workflow_dispatch | `6af6b02` | success | 71 seconds | 45 seconds | First quick-preflight dispatch, old base tools ran the full self-test |
| [45004219154](https://github.com/billy-enrizky/baton/actions/runs/45004219154) | workflow_dispatch | `fd06715` | success | 73 seconds | 40 seconds | Toolchain download experiment, first sample |
| [45004357746](https://github.com/billy-enrizky/baton/actions/runs/45004357746) | workflow_dispatch | `fd06715` | success | 73 seconds | 43 seconds | Toolchain download experiment, second sample |
| [45004403409](https://github.com/billy-enrizky/baton/actions/runs/45004403409) | workflow_dispatch | `036f29d` | success | 60 seconds | 43 seconds | Quick preflight with a base that supports it |
| [45004517552](https://github.com/billy-enrizky/baton/actions/runs/45004517552) | workflow_dispatch | `036f29d` | success | 51 seconds | 32 seconds | Same workflow, warm repeat |
| [45004670088](https://github.com/billy-enrizky/baton/actions/runs/45004670088) | push | `1539bae` | success | 69 seconds | 47 seconds | Main push, full save path, not a PR sample |
| [45004705055](https://github.com/billy-enrizky/baton/actions/runs/45004705055) | pull_request | `fec3c862` | success | 58 seconds | 33 seconds | Current warm Rust PR sample |

## Proof log

| Proof | Evidence | Result |
|---|---|---|
| First green full-suite PR | Run 44982816103 | Passed 2026-11-22 |
| First green full-suite main push | Run 44983114852 | Passed 2026-11-22 |
| Pipe wait removed | Run 44995838075, Test 101 seconds | Passed 2026-11-22 |
| Warm cache v2 | Run 44998824842, Test 62 seconds | Passed 2026-11-22 |
| Worktree creation race fixed | PR #129, Test run 44997380622 failure reproduced first | Passed 2026-11-22 |
| Worker group cancellation fixed | PR #133, lib tests 30.06 to 1.69 seconds | Passed 2026-11-22 |
| Gates start early | PR #135, dispatches 45004403409 and 45004517552 | Passed 2026-11-22 |
| Current warm Rust PR under 60 seconds | PR 45004705055, 58 seconds | Passed on one sample |
| Installer checksum rejection | `ci/installer-selftest.sh` | Passed locally and in CI |
| Release assets and checksums | Release run 44999297170, `v0.3.0` | Passed 2026-11-22 |
| Homebrew formula | Tap `billy-enrizky/homebrew-baton` | `brew audit`, `brew test`, and install passed locally |
| Formal gates | All 15 suites under `local_docs/verification/*/run.sh` | Passed 2026-11-22 |
| CI admission model | `ci/formal/ci-admission/run.sh` | Passed locally |

## Open gaps

- The current workflow has one warm Rust PR sample. The p50 claim needs at least three samples under the same workflow, and the p95 needs 100 runs before it is accepted.
- The historical docs-only sample was 45 seconds. The current workflow has no docs-only sample yet.
- Queue p50, queue p95, setup p95, and cache hit rate are not established.
- The repository is public. Standard GitHub-hosted runners are free. No paid-runner cost is approved or required by the current measurements.
- A paid 8-core runner remains an unmeasured option. It is not recommended until warm PR p50 or p95 misses both targets.
- Official merge queue requires organization ownership or GitHub Enterprise Cloud. Until then, the PR `verify` run plus the following `main` push are the merge proof.
- Branch protection must require `verify` and block direct pushes. This remains an owner action.

## Collection commands

```sh
gh run list --workflow ci.yml --limit 100 \
  --json databaseId,event,headSha,createdAt,updatedAt,conclusion,url
gh run view <run-id> --json jobs
gh run view <run-id> --log --job=<job-id>
gh api repos/billy-enrizky/baton/actions/jobs/<job-id> \
  --jq '.steps[] | [.number,.name,.started_at,.completed_at] | @tsv'
```

Read the cache key and size from the step named `Restore Cargo cache`. The action is `Swatinem/rust-cache`. Do not give the workflow a new step name unless this command changes too.
