# Baton CI acceptance record

Status on 2026-11-22: the current warm Rust PR samples are 58, 60, and 68 seconds. A separate CI-docs PR finished at 59 seconds. The Rust median is 60 seconds, at the target boundary rather than robustly under it. The Rust-specific p50 is not accepted until 100 runs or four weeks exist.

This file records the runs that changed a decision. It is not a complete archive of every GitHub Actions run. The Actions history remains the raw record, and the collection commands below reproduce these fields.

## Targets and current status

| Target | Current evidence | Status |
|---|---|---|
| Rust PR feedback p50 under 60 seconds | PR 45004705055: 58 seconds; PR 45009466461: 60 seconds; PR 45010710576: 68 seconds; n=3, median 60 seconds, max 68 seconds | Provisional; at the boundary, not yet met robustly. Accepted only at 100 runs or four weeks |
| Rust PR feedback p95 under 120 seconds | Three Rust samples, n=3, max 68 seconds | Provisional |
| Docs-only PR feedback p50 under 45 seconds | Current n=0; historical n=1, median 44 seconds, max 44 seconds on the pre-optimization workflow | Not established under the current workflow |
| Queue p50 under 10 seconds | n=26 non-retry attempts, median 0 seconds, max 0 seconds; the one manual retry is excluded | Provisional met |
| Queue p95 under 20 seconds | n=26 non-retry attempts, max 0 seconds; requires 100 runs before p95 is acceptance-bearing | Not established |
| Setup p95 under 15 seconds | Warm Test setup was 16 seconds in PR 45004705055 | Provisional miss, n=1, max 16 seconds |
| Hidden retries | `ci/check-ci.sh` rejects retry actions and `continue-on-error` | Enforced |
| Skipped required checks | `verify` requires every selected job | Enforced |
| Failed required check reported as success | `verify` and the formal admission model | Enforced |

A p50 or p95 from a small sample is provisional. Until 100 runs or four weeks exist, p50 rows report n, median, and maximum; p95 rows report n and maximum. This record does not claim an accepted percentile.

## Measurement definitions

- PR feedback starts at the workflow run `createdAt` and ends when `verify` completes on a `pull_request` run.
- For `push`, `merge_queue`, and `workflow_dispatch`, the same interval is called run create to verify, not PR feedback.
- Queue time is the run-level `startedAt - createdAt`, not a job's dependency wait.
- A retried workflow records each attempt separately. Retry queue time is not mixed with the first attempt's queue.
- Per-job setup is the job `startedAt` to its first non-setup command step.
- A job duration is its `startedAt` to `completedAt`.
- Record failed and cancelled runs when they change a decision. Do not remove a slow or red run because it is inconvenient.
- Do not infer a cache hit, commit, or step time from prose. Use the run log or Actions API.
- The cited log is selective. It records run, attempt, event, 12-character commit prefix, result, create-to-Verify, Test duration, and the decision. The linked Actions run and API supply the workflow revision, selected gates, cache key, slowest job, failure step, and budget miss. The collection commands reproduce those fields.
- A 12-character commit prefix is exact when the linked run resolves it; the run page carries the full SHA.

## Current warm path

PR 45004705055, merge candidate `fec3c862`, `ubuntu-24.04`:

| Checkpoint | Offset from run create |
|---|---|
| Run create | 0 seconds |
| Changes complete | 17 seconds |
| Gate jobs start | 18 seconds |
| Test complete | 52 seconds |
| Verify complete | 58 seconds |

The Changes job took 14 seconds, the Test job took 33 seconds, and the Verify job took 3 seconds. Run queue was 0 seconds because `startedAt` equaled `createdAt`; the remaining 8 seconds were job scheduling and finalization.

The adjacent CI-docs PR 45005544007 took 59 seconds with a 36-second Test job. The inbox PR 45009466461 took 60 seconds with a 37-second Test job. The inbox CLI PR 45010710576 took 68 seconds with a 49-second Test job. The Rust samples are 58, 60, and 68 seconds; their median is 60.

PR 45005544007 is a `meta` selection, not a Rust sample. It is recorded for the workflow but is not counted in the Rust target.

For PR 45004705055, Test job setup was 16 seconds, including 4 seconds for the rustup cache, 3 seconds for the Cargo cache, and 9 seconds for checkouts and job setup. The test step contained about 13 seconds of compile and link plus 1.73 seconds of lib tests. Test inventory recorded 401 tests. The inbox PR 45009466461 recorded 404 tests. The inbox CLI PR 45010710576 recorded 405 tests; its lib binary ran 330 tests in 1.72 seconds and its integration binaries ran 75 tests in 0.38 seconds.

The slowest test binary was no longer a 30-second outlier. The lib binary ran 326 tests in 1.73 seconds. The seven integration binaries together ran 75 tests in 0.4 seconds.

## How the critical path changed

| Run | Event | Result | What changed |
|---|---|---|---|
| [44982816103](https://github.com/billy-enrizky/baton/actions/runs/44982816103) | pull_request | 189 seconds, cold | First green full-suite PR |
| [44995838075](https://github.com/billy-enrizky/baton/actions/runs/44995838075) | push | 123 seconds | Cargo log no longer waits on a pipe held by a descendant |
| [44998824842](https://github.com/billy-enrizky/baton/actions/runs/44998824842) | workflow_dispatch | 95 seconds | Rust cache v2 and a warm test target |
| [45003589223](https://github.com/billy-enrizky/baton/actions/runs/45003589223) | pull_request | 68 seconds | Cancellation group kill removes a 30-second test |
| [45004403409](https://github.com/billy-enrizky/baton/actions/runs/45004403409) | workflow_dispatch | 60 seconds | Gates start before the full tool self-test finishes |
| [45004517552](https://github.com/billy-enrizky/baton/actions/runs/45004517552) | workflow_dispatch | 50 seconds | Same workflow, warm run |
| [45004705055](https://github.com/billy-enrizky/baton/actions/runs/45004705055) | pull_request | 58 seconds | First warm Rust PR under the current workflow |
| [45009466461](https://github.com/billy-enrizky/baton/actions/runs/45009466461) | pull_request | 60 seconds | Inbox feature; 404 tests and a raised inventory floor |
| [45010710576](https://github.com/billy-enrizky/baton/actions/runs/45010710576) | pull_request | 68 seconds | Inbox CLI and completed-worker rows; 405 tests |

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

The nextest experiment is not evidence for sharding. It showed that a count partition can put one 30-second test into a 61-second shard. After the cancellation fix the whole lib binary takes 1.73 seconds, so sharding is no longer on the critical path.

The per-repository worktree lock in PR #132 did not change CI wall time. It is kept because one process-wide lock made unrelated repositories wait on each other; it is a scope fix, not a speed claim.

## Cited run log

The Test column is the Test job start to complete. Skipped means the gate did not run. Retry attempts are separate rows. The 8,138-second gap from attempt 1 `createdAt` (01:01:41Z) to attempt 2 `createdAt` (03:17:19Z) is not counted as queue for the normal runs.

Superseded runs 44981081994 and 44981120796 were cancelled without changing a decision. They remain in Actions history and are not copied here.

| Run | Attempt | Event | Commit | Result | Create to Verify | Test | Decision evidence |
|---|---|---|---|---|---|---|---|
| [44969473815 attempt 1](https://github.com/billy-enrizky/baton/actions/runs/44969473815/attempts/1) | 1 | pull_request | `cb1119bcf0fd` | failure | 5 seconds | skipped | Billing stopped the run at Changes |
| [44969473815 attempt 2](https://github.com/billy-enrizky/baton/actions/runs/44969473815/attempts/2) | 2 | pull_request | `cb1119bcf0fd` | failure | 142 seconds from attempt 2 `createdAt` 03:17:19Z to Verify completion 03:19:41Z | 109 seconds, failed | Test failed four TUI tests; retry wait was 8,138 seconds and is excluded |
| [44982816103](https://github.com/billy-enrizky/baton/actions/runs/44982816103) | 1 | pull_request | `223e5f301e38` | success | 189 seconds | 165 seconds | First green full-suite PR, cold cache |
| [44983114852](https://github.com/billy-enrizky/baton/actions/runs/44983114852) | 1 | push | `107ffa4c1d10` | success | 194 seconds | 171 seconds | First green full-suite main push, cold cache |
| [44988029960](https://github.com/billy-enrizky/baton/actions/runs/44988029960) | 1 | push | `a18251c02fb4` | success | 194 seconds | 165 seconds | Per-binary timing sample before the pipe fix |
| [44995838075](https://github.com/billy-enrizky/baton/actions/runs/44995838075) | 1 | push | `5f44e30f82c0` | success | 123 seconds | 101 seconds | Removed the 60-second pipe wait |
| [44997380622](https://github.com/billy-enrizky/baton/actions/runs/44997380622) | 1 | push | `bb0b195ed674` | failure | 164 seconds | 144 seconds | Worktree race reproduced under concurrent workers |
| [44998824842](https://github.com/billy-enrizky/baton/actions/runs/44998824842) | 1 | workflow_dispatch | `978df9e7f4ce` | success | 95 seconds | 62 seconds | Warm cache v2 baseline |
| [44999117575](https://github.com/billy-enrizky/baton/actions/runs/44999117575) | 1 | workflow_dispatch | `e5697af820c3` | success | 84 seconds | 63 seconds | Eight test threads rejected |
| [44999152137](https://github.com/billy-enrizky/baton/actions/runs/44999152137) | 1 | push | `f93b549aa45d` | success | 88 seconds | 60 seconds | Best pre-cancellation warm push |
| [45000304120](https://github.com/billy-enrizky/baton/actions/runs/45000304120) | 1 | pull_request | `485d53d2ecff` | success | 44 seconds | skipped | Historical docs-only path before the quick preflight |
| [45000399622](https://github.com/billy-enrizky/baton/actions/runs/45000399622) | 1 | push | `3d6467bdc373` | success | 97 seconds | 70 seconds | 30.06-second lib tests |
| [45001645126](https://github.com/billy-enrizky/baton/actions/runs/45001645126) | 1 | workflow_dispatch | `d4188016b053` | success | 87 seconds | 62 seconds | Per-repository lock did not move wall time |
| [45001829417](https://github.com/billy-enrizky/baton/actions/runs/45001829417) | 1 | workflow_dispatch | `7b63aa9b6cfe` | success | 112 seconds | 77 seconds | Eight threads after scoped locking rejected |
| [45002182299](https://github.com/billy-enrizky/baton/actions/runs/45002182299) | 1 | workflow_dispatch | `0c383f16b06d` | success | 98 seconds | 64 seconds | `/dev/shm` test directories rejected |
| [45002437721](https://github.com/billy-enrizky/baton/actions/runs/45002437721) | 1 | workflow_dispatch | `8952eb5fa585` | failure | 70 seconds | skipped | Nextest count shards unbalanced; job list change failed CI013 |
| [45002826292](https://github.com/billy-enrizky/baton/actions/runs/45002826292) | 1 | workflow_dispatch | `3ecb4bba45eb` | failure | 29 seconds | skipped | Cancellation diagnostic proved the 30-second pipe wait |
| [45003043436](https://github.com/billy-enrizky/baton/actions/runs/45003043436) | 1 | workflow_dispatch | `0b3a181e7b95` | failure | 26 seconds | skipped | Process-group probe identified the missing `--` |
| [45003589223](https://github.com/billy-enrizky/baton/actions/runs/45003589223) | 1 | pull_request | `bc76fcc916a5` | success | 68 seconds | 44 seconds | Cancellation fix, lib tests 1.69 seconds |
| [45003991763](https://github.com/billy-enrizky/baton/actions/runs/45003991763) | 1 | workflow_dispatch | `6af6b028fbc9` | success | 71 seconds | 45 seconds | First quick-preflight dispatch, old base tools ran the full self-test |
| [45004219154](https://github.com/billy-enrizky/baton/actions/runs/45004219154) | 1 | workflow_dispatch | `fd06715a1067` | success | 73 seconds | 40 seconds | Toolchain download experiment, first sample |
| [45004357746](https://github.com/billy-enrizky/baton/actions/runs/45004357746) | 1 | workflow_dispatch | `fd06715a1067` | success | 73 seconds | 43 seconds | Toolchain download experiment, second sample |
| [45004403409](https://github.com/billy-enrizky/baton/actions/runs/45004403409) | 1 | workflow_dispatch | `036f29d4e6f2` | success | 60 seconds | 43 seconds | Quick preflight with a base that supports it |
| [45004517552](https://github.com/billy-enrizky/baton/actions/runs/45004517552) | 1 | workflow_dispatch | `036f29d4e6f2` | success | 50 seconds | 32 seconds | Same workflow, warm repeat |
| [45004670088](https://github.com/billy-enrizky/baton/actions/runs/45004670088) | 1 | push | `1539baebb683` | success | 68 seconds | 47 seconds | Main push, full save path, not a PR sample |
| [45004705055](https://github.com/billy-enrizky/baton/actions/runs/45004705055) | 1 | pull_request | `fec3c8623571` | success | 58 seconds | 33 seconds | Current warm Rust PR sample |
| [45005544007](https://github.com/billy-enrizky/baton/actions/runs/45005544007) | 1 | pull_request | `4f1f5ad7822f` | success | 59 seconds | 36 seconds | Current warm CI-docs PR sample |
| [45009466461](https://github.com/billy-enrizky/baton/actions/runs/45009466461) | 1 | pull_request | `caa938fc89e5` | success | 60 seconds | 37 seconds | Inbox feature; 404 tests, inventory floor 404 |
| [45010710576](https://github.com/billy-enrizky/baton/actions/runs/45010710576) | 1 | pull_request | `c2b7aedba674` | success | 68 seconds | 49 seconds | Inbox CLI; 405 tests, inventory floor 405 |

## Proof log

| Proof | Evidence | Result |
|---|---|---|
| First green full-suite PR | Run 44982816103 | Passed 2026-11-22 |
| First green full-suite main push | Run 44983114852 | Passed 2026-11-22 |
| Pipe wait removed | Run 44995838075, Test 101 seconds | Passed 2026-11-22 |
| Warm cache v2 | Run 44998824842, Test 62 seconds | Passed 2026-11-22 |
| Worktree creation race fixed | Failure in 44997380622, green `ships_completed_worker_branches_idempotently` in 45000399622 | Passed 2026-11-22 |
| Worker group cancellation fixed | PR #133, lib tests 30.06 to 1.69 seconds | Passed 2026-11-22 |
| Gates start early | PR #135, dispatches 45004403409 and 45004517552 | Passed 2026-11-22 |
| Warm PR samples | Rust PR 45004705055: 58 seconds; Rust PR 45009466461: 60 seconds; Rust PR 45010710576: 68 seconds; CI-docs PR 45005544007: 59 seconds | Rust median 60; target is at the boundary |
| Installer checksum rejection | `ci/installer-selftest.sh`; full selftest in 45000399622 | Passed locally and in CI |
| Release assets and checksums | Release run 44999297170, `v0.3.0` | Passed 2026-11-22 |
| Homebrew formula | Tap `billy-enrizky/homebrew-baton` | `brew audit`, `brew test`, and install passed locally |
| Formal gates | All 15 suites under `local_docs/verification/*/run.sh` | Passed 2026-11-22 |
| CI admission model | `ci/formal/ci-admission/run.sh` | Passed locally |

## Review record

The EGM Goldfish loop used two reviewers: Hypatia as design critic and Raman as structural reviewer. Round 1 found the unsupported complete-archive claim and mixed measurement definitions. Rounds 2 through 4 found and fixed wrong Verify endpoints, retry handling, Rust and meta sample mixing, and percentile reporting inconsistencies. Round 7 caught an out-of-date-base run that the classifier labeled `unknown`; it was removed rather than counted as meta. Rounds 8 and 9 reported no Blocker or Major findings.

## Open gaps

- The current workflow has three warm Rust samples. The Rust median is 60 seconds and the maximum is 68 seconds. All p50 and p95 rows remain provisional until 100 runs or four weeks exist; the under-60 target is not yet met robustly.
- The historical docs-only sample was n=1, median 44 seconds, max 44 seconds. The current workflow has no docs-only sample yet.
- Queue p50 is 0 seconds on cited non-retry runs. Queue p95, setup p95, and cache hit rate are not established; setup has one 16-second sample against a 15-second target.
- The repository is public. Standard GitHub-hosted runners are free. No paid-runner cost is approved or required by the current measurements.
- A paid 8-core runner remains an unmeasured option. It is not recommended until both Rust-target metrics miss across at least 100 runs or four weeks.
- Official merge queue requires organization ownership or GitHub Enterprise Cloud. Until then, the PR `verify` run plus the following `main` push are the merge proof.
- Branch protection must require `verify` and block direct pushes. This remains an owner action.

## Collection commands

```sh
gh run list --workflow ci.yml --limit 100 \
  --json databaseId,attempt,event,headSha,createdAt,startedAt,updatedAt,conclusion,url
gh run view <run-id> --attempt <n> --json jobs
gh run view <run-id> --attempt <n> --log --job=<job-id>
gh api repos/billy-enrizky/baton/actions/jobs/<job-id> \
  --jq '.steps[] | [.number,.name,.started_at,.completed_at] | @tsv'
```

Read the cache key and size from the step named `Restore Cargo cache`. The action is `Swatinem/rust-cache`. Do not give the workflow a new step name unless this command changes too.
