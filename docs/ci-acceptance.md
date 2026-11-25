# Baton CI acceptance record

Status on 2026-11-25: the first 100 successful PR runs record p50 62.0 seconds, p95 97.0 seconds, and max 150 seconds. The last 50 record p50 59.0 seconds, p95 69.0 seconds, and max 83 seconds. The 100-run window spans earlier CI designs, so the next 100 runs after `ba82720` are the current-design acceptance window. The latest 2026-11-25 eight-run sample is 56, 58, 59, 63, 64, 64, 75, and 76 seconds with a 64-second median, so the current design is close to the one-minute target but has not met it at p50. A free ARM runner was rejected as a replacement because it would remove x64 runtime tests from the pre-merge gate; the x64 Test job stays in place.

## 2026-11-25 latest runs

| Run | Event | Whole run | Test job | Note |
|---|---|---|---|---|
| [45695681488](https://github.com/billy-enrizky/baton/actions/runs/45695681488) | push `main` | 63 seconds | 46 seconds | Plan wrapping and turn id; all selected gates green |
| [45695474874](https://github.com/billy-enrizky/baton/actions/runs/45695474874) | pull request #299 | 59 seconds | 43 seconds | Plan wrapping and turn id; fastest run of the sample |
| [45691509144](https://github.com/billy-enrizky/baton/actions/runs/45691509144) | pull request #298 | 75 seconds | 55 seconds | Task picker notice; Test remains the critical path |
| [45687797545](https://github.com/billy-enrizky/baton/actions/runs/45687797545) | push `main` | 64 seconds | 45 seconds | Auth completion slice; all selected gates green |
| [45685900366](https://github.com/billy-enrizky/baton/actions/runs/45685900366) | push `main` | 56 seconds | 43 seconds | Plan review fixes; all selected gates green |
| [45687300533](https://github.com/billy-enrizky/baton/actions/runs/45687300533) | pull request #296 | 76 seconds | 57 seconds | Auth completion slice; Test remains the critical path |
| [45684994155](https://github.com/billy-enrizky/baton/actions/runs/45684994155) | pull request #295 | 64 seconds | 50 seconds | Plan review fixes; Verify adds the tail |
| [45681317020](https://github.com/billy-enrizky/baton/actions/runs/45681317020) | pull request #295 | 58 seconds | 44 seconds | First plan-view run |

The eight-run median is 64 seconds. The remaining gap is runner allocation, roughly 29 seconds of setup and cache restore in Test, 23 to 29 seconds of test execution, and the Verify tail. Sharding is the next candidate only after a partition can preserve a complete-set proof.

This file records the runs that changed a decision. It is not a complete archive of every GitHub Actions run. The Actions history remains the raw record, and the collection commands below reproduce these fields.

## Targets and current status

| Target | Current evidence | Status |
|---|---|---|
| Rust PR feedback p50 under 60 seconds | 100 successful PR runs: p50 62.0 seconds. Last 50: 59.0 seconds. Last 20: 59.5 seconds. Last 10: 60.0 seconds | Miss on the 100-run window by 2.0 seconds; met on the last 50 and last 20 windows. The 100-run window starts before the fast classifier and measurement changes, so the next 100 runs after `ba82720` are the acceptance window |
| Rust PR feedback p95 under 120 seconds | 100 successful PR runs: p95 97.0 seconds, max 150 seconds. Last 50: p95 69.0 seconds, max 83 seconds | Met on the 100-run window |
| Docs-only PR feedback p50 under 45 seconds | n=2: 44 seconds historical, 38 seconds in run 45077008165; median 41 seconds, max 44 seconds | Provisional met |
| Queue p50 under 10 seconds | 100 successful PR runs: p50 0 seconds, p95 0 seconds at the recorded resolution | Met |
| Queue p95 under 20 seconds | 100 successful PR runs: p95 0 seconds at the recorded resolution | Met |
| Setup p95 under 15 seconds | Warm Test setup was 16 seconds in PR 45004705055 | Provisional miss, n=1, max 16 seconds |
| Hidden retries | `ci/check-ci.sh` rejects retry actions and `continue-on-error` | Enforced |
| Skipped required checks | `verify` requires every selected job | Enforced |
| Workflow-level false green | `verify` and the formal admission model | Enforced for the workflow and aggregator; adversarial test-process tampering is a recorded residual risk |

A p50 or p95 from a small sample is provisional. Until 100 runs or four weeks exist, p50 rows report n, median, and maximum; p95 rows report n and maximum. This record does not claim an accepted percentile.

The 2026-11-24 sample is the first 100-run window, collected from the GitHub Actions API (`created_at` to `updated_at`, successful `pull_request` runs only). It spans 2026-11-22 to 2026-11-24 and therefore includes runs from before the fast classifier, the base and head SHA fetch, and the measurement repairs landed. Those runs are real and stay in the sample; the p50 miss is recorded rather than explained away. The next acceptance window is the 100 successful PR runs after `ba82720`, when every run uses the current design.

## 2026-11-24 100-run sample and critical path

Sample: 100 successful `pull_request` runs from the Actions API, `created_at` to `updated_at`, 2026-11-22 to 2026-11-24. Cancelled and failed runs are excluded because they do not produce feedback.

| Window | p50 | p95 | Max |
|---|---|---|---|
| Last 10 | 60.0 s | 66.0 s | 68 s |
| Last 20 | 59.5 s | 68.0 s | 70 s |
| Last 50 | 59.0 s | 69.0 s | 83 s |
| Last 100 | 62.0 s | 97.0 s | 150 s |

Verdict: p95 meets the 120-second target on the full window. p50 misses the 60-second target by 2.0 seconds on the full window and meets it on the last 50 and last 20. The full window includes runs from before the fast classifier, the base and head SHA fetch, and the measurement repairs, so it measures those changes as well as steady state.

Critical path, run 45428696350 (64 seconds total):

| Segment | Seconds |
|---|---|
| Queue and `Changes` | 5 |
| `Test` job | 51 |
| `Verify` start gap and run | 6 |
| Run scheduling | 2 |

Inside the `Test` job: 3 s before checkout, 4 s checkout, 2 s trusted base tools, 6 s Rust toolchain cache restore, 1 s toolchain activation, 11 s Cargo cache restore, 17 s `cargo test`, about 7 s teardown and post steps.

The recorded per-binary timings artifact for `ba82720` shows the test execution itself is 5.7 seconds: the library binary runs 564 tests in 4.62 seconds and every integration binary together takes 1.05 seconds. The other 11 seconds of the test step is incremental compilation of the crate and the test binaries, not test execution. This is why sharding and selection cannot reach the target on their own: they divide at most 5.7 seconds.

The toolchain-cache conditional was rechecked against the current `ubuntu-24.04` image. The image ships rustc 1.100.0 and rustfmt, but not the components `rust-toolchain.toml` requests, so setup without the cache downloads them in about 9 seconds against 6 seconds of restore plus 1 second of activation (run [45108914603](https://github.com/billy-enrizky/baton/actions/runs/45108914603)). Skipping the restore whenever the image has the toolchain would take the slower path in the common case, so it is rejected.

Remaining levers, in measured order: a remote object cache such as sccache for the 11-second compile, a larger runner (spend), or splitting the crate so incremental compilation touches less. Diff-aware selection addresses at most the 5.7 seconds of execution.

## 2026-11-24 post-`ba82720` window

Sample: 21 successful `pull_request` runs after `ba82720` (2026-11-24T15:51:12Z), collected from the Actions API by `created_at` to `updated_at`. This is the start of the acceptance window that uses the fast classifier, the immutable base/head SHAs, and the current test profile. It is not yet 100 runs.

| Window | n | p50 | p95 | Max |
|---|---|---|---|---|
| All successful PR runs after `ba82720` | 21 | 63.0 s | 79.0 s | 83 s |

Verdict: p95 is under the 120-second target. p50 misses the 60-second target by 3.0 seconds. The sample includes a 31-second docs run and a 34-second workflow-only run; the code runs cluster at 53 to 83 seconds. The run's fixed cost is about 11 seconds from creation to the first job start, and the `Test` job still spends about 26 seconds in setup, toolchain restore, and Cargo cache restore before its 11-second compile and 6 seconds of test execution. Cutting the miss needs a shorter setup/restore path, not more test sharding or selection.

Owner decisions with cost, not silently enabled:
- A larger runner for the `Test` job can shorten the setup/restore path, but it requires spend and is not approved.
- A remote object cache needs an additional service or cache tier; the measured compile is only about 11 seconds, so the expected gain does not justify the added dependency yet.
- Splitting the crate so the test link touches less is a larger refactor and has no measurement yet.

The next acceptance window is the first 100 successful PR runs after the current design; this 21-run sample stays in the record as an interim miss.

## 2026-11-24 stale merge-ref classification failure

Run 45403231197 failed `Selection guard` with `selection changed between jobs` on a docs-only pull request. The `Changes` job reported `full_suite_reason=code` and selected every gate; the guard recomputed `full_suite_reason=docs`.

Cause: `Changes` fetched `refs/pull/<number>/merge`. GitHub refreshes that ref asynchronously, so after a force-push it still described the previous head, which had been rebased away. The job diffed the new base SHA against the stale merge tree, so the merged code commit from the new base appeared as a deletion and the file list contained `src/feature.rs` as well as the docs file.

Reproduction (`/tmp` fixture, base-owned `classify-paths.sh`):

| Diff pair | Files | Selection |
|---|---|---|
| new base to stale merge ref (old command) | `notes.md`, `src_feature.rs` | `full_suite_reason=code`, every gate |
| new base to event head SHA (new command) | `notes.md` | `docs_only=true`, `full_suite_reason=docs` |

Fix: `Changes` now fetches `github.event.pull_request.base.sha` and `github.event.pull_request.head.sha` and diffs that pair, the same base-to-HEAD diff the guard recomputes. Both values are fixed in the event payload for the run. `ci/check-ci.sh` rule `CI018` rejects a `--files` classification that does not read the head SHA or that reads `refs/pull/`, with the fixture `ci/fixtures/check-ci/invalid-stale-merge-ref.yml`.

Cost: one extra shallow commit object per run, measured in the next pull request sample. No gate changes: every code and meta pull request still runs the full suite.

## 2026-11-24 rejected recorded-time shard experiment

A five-leg matrix (`tui`, `orchestrate`, `git`, `rest`, `integration`) was built from local module wall times. The local union executed all 640 tests and `ci/test-inventory.sh` returned `test_count=640`. The TLA+ admission model was extended with an `inventory` gate and `InventoryNeedsTest`; the real config checked 2,490,965 states and 360,485 distinct states, and the new mutation failed as required.

The end-to-end result regressed. PR run 45359573400 completed in 87 seconds and dispatch run 45359597573 in 70 seconds, against 64 seconds for the previous single Test job. The slowest leg was `integration` at 46 seconds, `inventory` added 18 seconds, and every leg repeated about 28 seconds of setup and cache restore. The experiment branch was closed without merging. The single Test job stays until a shared-artifact or diff-aware selector has measured evidence.

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

The adjacent CI-docs PR 45005544007 took 59 seconds with a 36-second Test job. The inbox PR 45009466461 took 60 seconds with a 37-second Test job. The inbox CLI PR 45010710576 took 68 seconds with a 49-second Test job. The completions PR 45011836996 took 63 seconds with a 47-second Test job. The Rust samples are 52, 55, 56, 58, 60, 61, 62, 63, 64, 65, 65, 68, 68, 69, 70, and 74 seconds; their median is 63.5.

PR 45005544007 is a `meta` selection, not a Rust sample. It is recorded for the workflow but is not counted in the Rust target.

For PR 45004705055, Test job setup was 16 seconds, including 4 seconds for the rustup cache, 3 seconds for the Cargo cache, and 9 seconds for checkouts and job setup. The test step contained about 13 seconds of compile and link plus 1.73 seconds of lib tests. Test inventory recorded 401 tests. The inbox PR 45009466461 recorded 404 tests. The inbox CLI PR 45010710576 recorded 405 tests. The completions PR 45011836996 recorded 406 tests; its lib binary ran 331 tests in 1.67 seconds and its integration binaries ran 75 tests in 0.40 seconds.

The slowest test binary was no longer a 30-second outlier. The lib binary ran 326 tests in 1.73 seconds. The seven integration binaries together ran 75 tests in 0.4 seconds.

## 2026-11-23 fast classifier and test profile

The `changes` job no longer checks out the repository. For a pull request it fetches only the base commit and `refs/pull/<number>/merge` at depth one, exports the base-owned classifier and budget script from the base commit, and classifies the base-to-merge diff. The trusted guards moved to `selection`, which still gates `verify`.

The test profile now keeps line tables and drops full debuginfo. The Cargo cache key includes `Cargo.toml`, so the profile change cannot restore incompatible artifacts.

| Run | Event | State | Create to Verify | Changes complete | Test job | Notes |
|---|---|---|---|---|---|---|
| [45114396442](https://github.com/billy-enrizky/baton/actions/runs/45114396442) | workflow_dispatch | warm cache, forced Rust rebuild | 51 seconds | 6 seconds | 35 seconds | Final classifier branch; realistic Rust source rebuild, 509 tests |
| [45111528561](https://github.com/billy-enrizky/baton/actions/runs/45111528561) | workflow_dispatch | warm cache, forced Rust rebuild | 52 seconds | 6 seconds | 38 seconds | Earlier combined branch; kept as a corroborating sample |
| [45111377033](https://github.com/billy-enrizky/baton/actions/runs/45111377033) | workflow_dispatch | warm cache, unchanged Rust source | 51 seconds | 5 seconds | 32 seconds | Test step reused the Baton artifacts; descriptive only |
| [45115234795](https://github.com/billy-enrizky/baton/actions/runs/45115234795) | workflow_dispatch | warm cache, head tools | 53 seconds | 6 seconds | 33 seconds | Post-merge proof path; merged self-test, workflow guard, required commands, and budgets all passed |
| [45117405274](https://github.com/billy-enrizky/baton/actions/runs/45117405274) | workflow_dispatch | warm cache, head tools | 62 seconds | 7 seconds | 48 seconds | Slower runner repeat; 5-second setup, 5-second checkout, 8-second toolchain restore, 7-second Cargo restore; all checks passed |
| [45119247835](https://github.com/billy-enrizky/baton/actions/runs/45119247835) | workflow_dispatch | warm cache, head tools | 48 seconds | 5 seconds | 32 seconds | Final head-tools run; debt self-test passed, Clippy debt 61 <= 61, all gates green |
| [45119477704](https://github.com/billy-enrizky/baton/actions/runs/45119477704) | push | main, warm cache | 59 seconds | 6 seconds | 43 seconds | First post-merge main push; merged tools, debt self-test, Clippy 61 <= 61, cache seed |
| [45119777637](https://github.com/billy-enrizky/baton/actions/runs/45119777637) | pull_request | meta CI docs, warm cache | 52 seconds | 6 seconds | 35 seconds | First post-merge PR sample; full suite selected, all gates green |
| [45111201206](https://github.com/billy-enrizky/baton/actions/runs/45111201206) | pull_request | cold after profile change | 85 seconds | 5 seconds | 71 seconds | Pull-request cache did not inherit the branch cache; Selection failed at the expected CI013 workflow comparison |

The 51-second run is the critical-path proof for this slice. It is not counted as a PR p50 sample because it is a dispatch. The 48-second head-tools dispatch and the 59-second main push prove the merged scripts, required-command enforcement, and the cache seed. The 52-second meta PR is the first post-merge PR sample; a Rust PR sample is still required for the Rust p50.

The toolchain-cache removal experiment was rejected. Removing the 391 MB `~/.rustup` restore made Setup Rust take about 9 seconds instead of about 6 seconds of restore plus 1 second of activation. Matched samples were 57 and 69 seconds with the cache removed, against 69 and 71 seconds with the cache retained; the difference was noise, not a gain. Runs 45108914603, 45109176611, 45109045484, and 45109314728 are recorded for that decision.

## Rejected ARM runner experiment

The only Test job stays on `ubuntu-24.04` for every event. A temporary job on `ubuntu-24.04-arm` ran all 406 tests in both a cold and a warm run. The normal Test job was skipped in both experiment runs after Changes failed.

| Run | State | ARM experiment job | Test step | `cargo test` real | Tests | Result |
|---|---|---|---|---|---|---|
| [45012384561](https://github.com/billy-enrizky/baton/actions/runs/45012384561) | cold | 85 seconds | 56 seconds | 55.43 seconds | 406 | ARM test passed; run failed Changes with CI013 |
| [45012538857](https://github.com/billy-enrizky/baton/actions/runs/45012538857) | warm | 33 seconds | 22 seconds | 21.57 seconds | 406 | ARM test passed; run failed Changes with CI013 |

The experiment omitted trusted-tool staging, the inventory step, the timing artifact, and the final cache keys. The dispatch failed at Changes, so no later gate, including the selection guard, exercised those paths. The 406-test x64 sample is [run 45011836996](https://github.com/billy-enrizky/baton/actions/runs/45011836996): a 47-second job with a 16-second test step and a partial Cargo cache because it added `clap_complete`. The warm ARM experiment was a 33-second job with a 22-second test step and a full Cargo cache. The jobs differ in both cache state and executed steps, so these timings are descriptive and not matched. No ARM performance advantage is claimed.

The runner was rejected because replacing the x64 Test job would move Linux x64 runtime tests after merge, and official merge queue is unavailable for this repository. A runtime-only x64 regression could then land without a pre-merge test. The option is closed unless a future proposal is additive, not a replacement, and includes an independent required x64 pre-merge Test job. That proposal would need a matched rerun with identical steps, event shape, cache policy, and test command, and it would follow the same 100-run or four-week measurement policy. A merge queue alone is not enough if it runs the same ARM job.

Both runs were `workflow_dispatch`, and `ci/check-ci.sh` rejects the temporary job-list change with CI013. `--allow-change` does not relax job-list equality.

The warm ARM experiment restored 340,962,469 bytes for rustup in about 3 seconds and 248,109,421 bytes for Cargo in about 4 seconds. Those caches used a temporary key and were not adopted.

## Trusted-input proof and current samples

The trusted-input change was proven twice. Branch dispatch [45016871008](https://github.com/billy-enrizky/baton/actions/runs/45016871008) was green at 66 seconds with a 46-second Test job. The following `main` push [45017072401](https://github.com/billy-enrizky/baton/actions/runs/45017072401) was green at 54 seconds with a 35-second Test job. Both jobs reported `test_count=406`, used `$RUNNER_TEMP/trusted-ci/test-inventory-floor.txt`, and ran `$RUNNER_TEMP/trusted-ci/test-timings.sh`.

The two PR samples around that work were 51 seconds for the ARM rejection docs (run 45016341050, Test 33 seconds) and 64 seconds for the parser (run 45016610933, Test 44 seconds). They are meta samples, not Rust samples. The spread shows that the Rust p50 target cannot be claimed from these runs.

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
| [45011836996](https://github.com/billy-enrizky/baton/actions/runs/45011836996) | pull_request | 63 seconds | Shell completions; 406 tests and a new dependency |
| [45021604178](https://github.com/billy-enrizky/baton/actions/runs/45021604178) | pull_request | 61 seconds | Thread pins; 413 tests, Test 37 seconds |
| [45021753186](https://github.com/billy-enrizky/baton/actions/runs/45021753186) | push | 64 seconds | Thread-pin main push; 413 tests, Test 49 seconds |
| [45035044248](https://github.com/billy-enrizky/baton/actions/runs/45035044248) | pull_request | 52 seconds | Manual pin order; 419 tests, Test 33 seconds |
| [45037001045](https://github.com/billy-enrizky/baton/actions/runs/45037001045) | pull_request | 65 seconds | Keyboard pin reorder; 421 tests, Test 44 seconds |
| [45046974039](https://github.com/billy-enrizky/baton/actions/runs/45046974039) | pull_request | 70 seconds | Project file navigation; 447 tests, Test 35 seconds, Build 40 seconds |
| [45047226926](https://github.com/billy-enrizky/baton/actions/runs/45047226926) | push | 60 seconds | File-navigation main push; 447 tests, Test 38 seconds |
| [45051808580](https://github.com/billy-enrizky/baton/actions/runs/45051808580) | pull_request | 64 seconds | TUI files picker; 456 tests, Test 36 seconds |
| [45051948812](https://github.com/billy-enrizky/baton/actions/runs/45051948812) | push | 67 seconds | Files-picker main push; 456 tests, Test 46 seconds |
| [45055995298](https://github.com/billy-enrizky/baton/actions/runs/45055995298) | pull_request | 56 seconds | Fuzzy file matching; 460 tests, Test 35 seconds |
| [45056141672](https://github.com/billy-enrizky/baton/actions/runs/45056141672) | push | 67 seconds | Fuzzy-matching main push; 460 tests, Test 47 seconds |
| [45060299027](https://github.com/billy-enrizky/baton/actions/runs/45060299027) | pull_request | 65 seconds | Line targets; 464 tests, Test 46 seconds |
| [45060456619](https://github.com/billy-enrizky/baton/actions/runs/45060456619) | push | 69 seconds | Line-target main push; 464 tests, Test 50 seconds |
| [45064707055](https://github.com/billy-enrizky/baton/actions/runs/45064707055) | pull_request | 74 seconds | Content search; 486 tests, Test 48 seconds |
| [45064863755](https://github.com/billy-enrizky/baton/actions/runs/45064863755) | push | 69 seconds | Content-search main push; 486 tests, Test 52 seconds |
| [45111201206](https://github.com/billy-enrizky/baton/actions/runs/45111201206) | pull_request | 85 seconds, cold | Fast classifier; PR cache did not inherit the seeded branch cache |
| [45114396442](https://github.com/billy-enrizky/baton/actions/runs/45114396442) | workflow_dispatch | 51 seconds, warm | Final classifier plus line-table test profile; forced Rust rebuild |
| [45113871281](https://github.com/billy-enrizky/baton/actions/runs/45113871281) | pull_request | 97 seconds, cold | Profile prerequisite PR; old cache key, 75-second Test, then merged as `b854a27` |
| [45114063161](https://github.com/billy-enrizky/baton/actions/runs/45114063161) | push | 102 seconds, cold | Profile main push; same old cache key, 82-second Test; new-key seed follows the classifier PR |

The 30-second long pole was `orchestrate::tests::cancellation_stops_the_worker_group_and_marks_the_run_cancelled`. On Linux, `kill -TERM -PGID` was accepted but did not signal the process group. The shell leader died, its `sleep` child kept the output pipe open, and the reader threads waited the full 30 seconds. Adding `--` before the negative group operand fixed the signal. `group_alive` then used `ps` to ignore the unreaped leader zombie instead of spending another 2.5 seconds on `kill -0`.

Before PR 45003589223, lib tests took 30.06 seconds and the Test job took 62 seconds. After, lib tests took 1.69 seconds and the Test job took 44 seconds.

## Cache state

Warm Test cache in PR 45004705055:

| Cache | Key | Size | Match | Restore time |
|---|---|---|---|---|
| Rust toolchain, slim minimum | `rustup-min-1.100.0-Linux-X64` | 190 MB | exact key hit | 2 to 5 seconds on fast runners; 8 seconds on the slowest sampled runner |
| Rust toolchain, slim lint | `rustup-lint-1.100.0-Linux-X64` | 199 MB | exact key hit | 2 to 5 seconds on fast runners |
| Cargo target and registry | `v0-rust-baton-v2-Linux-X64-test-Linux-x64-2fd77104-fda39e8f` | 330 MB | `full match: true` | 5 to 10 seconds |

The old full rustup cache was about 390 MB per job. The slim minimum and lint caches are shared across Check, Build, Test, and Debt instead of keyed per job. Test saves the minimum cache and Debt saves the lint cache on `main` or an owner-dispatched run. Check and Build only restore. A workflow dispatch requires write access, so a normal PR cannot write the cache.

## Rejected experiments

| Experiment | Run | Result | Decision |
|---|---|---|---|
| Eight test threads before the cancellation fix | [44999117575](https://github.com/billy-enrizky/baton/actions/runs/44999117575) | Test 63 seconds against a 62-second warm baseline | Rejected; the 30-second test dominated |
| Eight test threads after the scoped lock | [45001829417](https://github.com/billy-enrizky/baton/actions/runs/45001829417) | Test 77 seconds | Rejected; the cancellation bug still dominated |
| Put test repositories in `/dev/shm` | [45002182299](https://github.com/billy-enrizky/baton/actions/runs/45002182299) | Test 64 seconds; `/dev/shm` had 7.9 GB free | Rejected; the suite is not disk-bound |
| Count-based nextest shards | [45002437721](https://github.com/billy-enrizky/baton/actions/runs/45002437721) | Shard 1 33 seconds, shard 2 61 seconds | Rejected; count partitions were unbalanced, and the run was red because the experiment changed the job list |
| Install the pinned toolchain instead of restoring 391 MB | [45004219154](https://github.com/billy-enrizky/baton/actions/runs/45004219154), [45004357746](https://github.com/billy-enrizky/baton/actions/runs/45004357746) | Test 40 and 43 seconds against a 44 to 45 second baseline | Rejected; no consistent gain |
| Remove the rustup cache again on the current workflow | [45108914603](https://github.com/billy-enrizky/baton/actions/runs/45108914603), [45109176611](https://github.com/billy-enrizky/baton/actions/runs/45109176611), [45109045484](https://github.com/billy-enrizky/baton/actions/runs/45109045484), [45109314728](https://github.com/billy-enrizky/baton/actions/runs/45109314728) | Removed-cache runs 57 and 69 seconds; retained-cache runs 69 and 71 seconds; Setup Rust about 9 seconds without cache versus 6 restore plus 1 activation | Rejected; no proven gain and a slower Test setup path |
| Temporary ARM Test job evaluated as a replacement | [45012384561](https://github.com/billy-enrizky/baton/actions/runs/45012384561), [45012538857](https://github.com/billy-enrizky/baton/actions/runs/45012538857) | Cold 85-second and warm 33-second ARM jobs; 406 tests each | Rejected; would remove x64 runtime from the pre-merge gate while merge queue is unavailable |
| Skip `dtolnay/rust-toolchain` when the slim toolchain cache hits | [45142952375](https://github.com/billy-enrizky/baton/actions/runs/45142952375) | All Rust jobs succeeded but the run took 85 seconds; Test and Build were 68 seconds | Rejected; skipping the setup action left Cargo on a slower path even though the toolchain directory existed |
| Share a slim 190 MB toolchain cache instead of four full 390 MB job caches | [45142327957](https://github.com/billy-enrizky/baton/actions/runs/45142327957) cold seed; [45142584909](https://github.com/billy-enrizky/baton/actions/runs/45142584909), [45143262549](https://github.com/billy-enrizky/baton/actions/runs/45143262549), [45143495137](https://github.com/billy-enrizky/baton/actions/runs/45143495137), [45143668852](https://github.com/billy-enrizky/baton/actions/runs/45143668852) warm | Toolchain restore fell from 9 seconds to 2 to 5 seconds on fast runs and the archive halved; total warm samples were 62, 64, 47, and 62 seconds; Test jobs were 46, 48, 33, and 43 seconds | Kept; the step improved and archive size fell, but total p50 remains dominated by Cargo restore and hosted-runner variance |

The nextest experiment is not evidence for sharding. It showed that a count partition can put one 30-second test into a 61-second shard. After the cancellation fix the whole lib binary takes 1.73 seconds, so sharding is no longer on the critical path.

The per-repository worktree lock in PR #132 did not change CI wall time. It is kept because one process-wide lock made unrelated repositories wait on each other; it is a scope fix, not a speed claim.

## Latest feature samples

| Run | Event | Create to Verify | Test | Notes |
|---|---|---|---|---|
| [45147067545](https://github.com/billy-enrizky/baton/actions/runs/45147067545) | pull_request | 62 seconds | 44 seconds | Claude watchdog fix; 518 tests |
| [45147281203](https://github.com/billy-enrizky/baton/actions/runs/45147281203) | push | 64 seconds | not recorded | Claude watchdog main push |
| [45158239304](https://github.com/billy-enrizky/baton/actions/runs/45158239304) | pull_request | 53 seconds | 32 seconds | `/grep` editor launch; 522 tests |
| [45158698921](https://github.com/billy-enrizky/baton/actions/runs/45158698921) | push | 59 seconds | not recorded | `/grep` editor launch main push |
| [45172642965](https://github.com/billy-enrizky/baton/actions/runs/45172642965) | pull_request | 55 seconds | 41 seconds | `/files --ignored`; 523 tests |
| [45173850895](https://github.com/billy-enrizky/baton/actions/runs/45173850895) | push | 51 seconds | 35 seconds | `/files --ignored` main push |

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
| [45011836996](https://github.com/billy-enrizky/baton/actions/runs/45011836996) | 1 | pull_request | `0add32c101cf` | success | 63 seconds | 47 seconds | Shell completions; 406 tests |
| [45012384561](https://github.com/billy-enrizky/baton/actions/runs/45012384561) | 1 | workflow_dispatch | `91b0d4fc3221` | failure | 96 seconds | 85 seconds | Cold ARM experiment; 406 tests passed, CI013 rejected the temporary job list |
| [45012538857](https://github.com/billy-enrizky/baton/actions/runs/45012538857) | 1 | workflow_dispatch | `91b0d4fc3221` | failure | 44 seconds | 33 seconds | Warm ARM experiment; 406 tests passed, 21.57-second `cargo test` in a 22-second step, CI013 rejected the temporary job list |
| [45016341050](https://github.com/billy-enrizky/baton/actions/runs/45016341050) | 1 | pull_request | `d92a0b46f08a` | success | 51 seconds | 33 seconds | ARM rejection docs; meta sample |
| [45016485687](https://github.com/billy-enrizky/baton/actions/runs/45016485687) | 1 | push | `709027e2ceb5` | success | 53 seconds | 38 seconds | Main push after ARM rejection docs; meta sample |
| [45016610933](https://github.com/billy-enrizky/baton/actions/runs/45016610933) | 1 | pull_request | `0637fac16a3c` | success | 64 seconds | 44 seconds | Inventory parser; meta sample |
| [45016871008](https://github.com/billy-enrizky/baton/actions/runs/45016871008) | 1 | workflow_dispatch | `12e85cdfafe9` | success | 66 seconds | 46 seconds | Trusted floor and timing script; 406 tests; `ci-repair` dispatch proof |
| [45017072401](https://github.com/billy-enrizky/baton/actions/runs/45017072401) | 1 | push | `5ea2de872f53` | success | 54 seconds | 35 seconds | Trusted floor and timing script; 406 tests; final post-merge proof |
| [45021604178](https://github.com/billy-enrizky/baton/actions/runs/45021604178) | 1 | pull_request | `18f9585a4d20` | success | 61 seconds | 37 seconds | Thread pins; 413 tests |
| [45021753186](https://github.com/billy-enrizky/baton/actions/runs/45021753186) | 1 | push | `d78caa933bbb` | success | 64 seconds | 49 seconds | Thread-pin main push; 413 tests |

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
| Warm PR samples | Rust PR 45004705055: 58 seconds; Rust PR 45009466461: 60 seconds; Rust PR 45011836996: 63 seconds; Rust PR 45010710576: 68 seconds; Rust PR 45021604178: 61 seconds; Rust PR 45035044248: 52 seconds; Rust PR 45037001045: 65 seconds; Rust PR 45046974039: 70 seconds; Rust PR 45051808580: 64 seconds; Rust PR 45055995298: 56 seconds; Rust PR 45060299027: 65 seconds; Rust PR 45064707055: 74 seconds; CI-docs PR 45005544007: 59 seconds; Rust PR 45069517940: 55 seconds; Rust PR 45075059836: 62 seconds; Rust PR 45075683884: 69 seconds; Rust PR 45080151770: 68 seconds; main push 45080301181: 65 seconds | 16-sample Rust median 63.5; above target |
| ARM runner rejected | Runs 45012384561 and 45012538857: 406 tests, 85 and 33 second jobs | Rejected 2026-11-22; x64 Test retained |
| Test inventory integrity | `ci/selftest.sh`: matching announced/executed count, short execution, missing totals | Passed locally; CI proof pending the next PR |
| Trusted test inputs | Branch dispatch [45016871008](https://github.com/billy-enrizky/baton/actions/runs/45016871008) and main push [45017072401](https://github.com/billy-enrizky/baton/actions/runs/45017072401): 406 tests, trusted floor and timing script, manifest clean | Passed |
| Installer checksum rejection | `ci/installer-selftest.sh`; full selftest in 45000399622 | Passed locally and in CI |
| Release hardening | PR #160, PR run 45075059836 (62 seconds), main push 45075200247 (65 seconds) | Passed 2026-11-22 |
| v0.4.0 version bump | PR #161, PR run 45075683884 (69 seconds), main push 45075877048 (67 seconds) | Passed 2026-11-22 |
| Release assets and checksums | Release run [45076016585](https://github.com/billy-enrizky/baton/actions/runs/45076016585), tag `v0.4.0`, exactly five archives plus `SHA256SUMS`; all five sums verified | Passed 2026-11-22 |
| Immutable latest release | `isDraft=false`, `isPrerelease=false`, `isImmutable=true`, `releases/latest=v0.4.0` | Passed 2026-11-22 |
| Homebrew formula | Tap commit `03741e5`, `brew audit --strict`, reinstall, `brew test`, version 0.4.0, and stable JSON before and after push | Passed 2026-11-22 |
| Clean curl install | Default `releases/latest` path into a clean bin, `baton 0.4.0`, no source fallback | Passed 2026-11-22 |
| Installed feature smoke | PTY slash probe passed against the curl and Homebrew binaries; summaries in gitignored `v0.4.0-release/curl/summary.json` and `v0.4.0-release/brew/summary.json` | Passed 2026-11-22 |
| Formal gates | All 15 suites under `local_docs/verification/*/run.sh` | Passed 2026-11-22 |
| CI admission model | `ci/formal/ci-admission/run.sh` | Passed locally |
| Branch protection | `main` requires `Verify`, requires an up-to-date branch, blocks force pushes and deletions, and has admin enforcement off. User push restrictions are not available on a user-owned repository | Configured 2026-11-23; direct owner push remains a documented residual risk |
| Fast classifier warm path | Branch dispatch 45114396442 with a forced Rust rebuild: 51 seconds, Changes complete at 6 seconds, Test 35 seconds | Passed 2026-11-23; post-merge PR sample pending |
| Head-tools branch proof | Branch dispatch 45115234795: 53 seconds, Selection 12 seconds, merged self-test and workflow guard passed | Passed 2026-11-23; followed by main push 45119477704 |
| Head-tools repeat | Branch dispatch 45117405274: 62 seconds, no skipped checks, slower setup and cache steps | Passed 2026-11-23; recorded without exclusion |
| Head-tools final dispatch | Branch dispatch 45119247835: 48 seconds, Debt ratchet 32 seconds, debt self-test passed, Clippy debt 61 <= 61 | Passed 2026-11-23 |
| Clippy ratchet color and count | `CARGO_TERM_COLOR=never` in the ratchet; compiler summary lines excluded; one-lint fixture passes a one-entry baseline | Passed locally and in 45119247835 |
| First post-merge main push | Push 45119477704: 59 seconds, Test 43, head tools, debt self-test, Clippy 61 <= 61 | Passed 2026-11-23 |
| First post-merge meta PR | Pull request 45119777637: 52 seconds, Test 35, full suite selected | Passed 2026-11-23 |
| v0.4.1 version bump | PR #238, PR run 45522919344: full suite, Test 43 seconds | Passed 2026-11-24 |
| v0.4.1 release assets and checksums | Release run [45523844591](https://github.com/billy-enrizky/baton/actions/runs/45523844591), tag `v0.4.1`, five archives plus `SHA256SUMS`; all five sums verified | Passed 2026-11-24 |
| v0.4.1 immutable latest release | `isDraft=false`, `isPrerelease=false`, `isImmutable=true`, `releases/latest=v0.4.1` | Passed 2026-11-24 |
| v0.4.1 Homebrew formula | Tap commit `8ccbead`, `brew audit --strict --online`, upgrade, `brew test`, version 0.4.1 | Passed 2026-11-24 |
| v0.4.1 curl install | Local `install.sh` with `BATON_VERSION=v0.4.1` into a clean `bin`, `baton 0.4.1`, no source fallback | Passed 2026-11-24 |
| v0.4.1 installed feature smoke | PTY slash probe passed against the Homebrew and curl binaries; the 50-command sweep passed against the Homebrew binary; `/auth`, `/model`, `/agents`, `/git`, `/memory`, and `/roles` present | Passed 2026-11-24 |
| v0.4.1 formal gates | All 15 suites at `afab876`; log `local_docs/verification/evidence/formal-afab876-20261124-200523.log` | Passed 2026-11-24 |
| v0.4.2 version bump | PR #245, PR run 45551608346: full suite, Test 51 seconds | Passed 2026-11-25 |
| v0.4.2 release assets and checksums | Release run [45551789917](https://github.com/billy-enrizky/baton/actions/runs/45551789917), tag `v0.4.2`, five archives plus `SHA256SUMS`; all five sums verified | Passed 2026-11-25 |
| v0.4.2 immutable latest release | `isDraft=false`, `isPrerelease=false`, `isImmutable=true`, `releases/latest=v0.4.2` | Passed 2026-11-25 |
| v0.4.2 Homebrew formula | Tap commit `1fa3223`, `brew audit --strict --online`, upgrade, `brew test`, version 0.4.2 | Passed 2026-11-25 |
| v0.4.2 curl install | Local `install.sh` with `BATON_VERSION=v0.4.2` into a clean `bin`, `baton 0.4.2`, no source fallback | Passed 2026-11-25 |
| v0.4.2 installed feature smoke | PTY question probe passed against the Homebrew, Cargo, and curl binaries; slash probe and 50-command sweep passed against the Homebrew binary; focused `/auth` and `/model` probes passed | Passed 2026-11-25 |
| v0.4.3 version bump | PR #252, PR run 45563560971 and main push 45563774098: full suite, Test passed | Passed 2026-11-25 |
| v0.4.3 release assets and checksums | Release run [45563843655](https://github.com/billy-enrizky/baton/actions/runs/45563843655), tag `v0.4.3`, five archives plus `SHA256SUMS`; all five sums verified | Passed 2026-11-25 |
| v0.4.3 immutable latest release | `isDraft=false`, `isPrerelease=false`, `isImmutable=true`, `releases/latest=v0.4.3` | Passed 2026-11-25 |
| v0.4.3 Homebrew formula | Tap commit `ab46981`, `brew audit --strict --online`, upgrade, `brew test`, version 0.4.3 | Passed 2026-11-25 |
| v0.4.3 curl install | Local `install.sh` with `BATON_VERSION=v0.4.3` into a clean `bin`, `baton 0.4.3`, no source fallback | Passed 2026-11-25 |
| v0.4.3 installed feature smoke | Homebrew 0.4.3 passed the slash probe, the 50-command sweep, the Codex question probe, and the Antigravity approval plus question probe; the Cargo 0.4.3 binary passed the slash probe | Passed 2026-11-25 |
| v0.4.3 formal gates | All 15 suites at `da9f3ef`; log `local_docs/verification/evidence/formal-da9f3ef-20261125-014211.log` | Passed 2026-11-25 |

## v0.4.0 release assets

Published 2026-11-22 from tag `v0.4.0` at `b26b8824388159cf26a2539175e6d1a867d7c738`.

| Asset | SHA-256 |
|---|---|
| `baton-aarch64-apple-darwin.tar.gz` | `86b1c79e435c127df4bc651319e4b8cc0f73e10f5f1da01eb3e8b677d689a0e5` |
| `baton-aarch64-unknown-linux-gnu.tar.gz` | `921aac5157f1c828e0cbf9953f462f226f71d6098d7482c3a33836bf047eae37` |
| `baton-x86_64-apple-darwin.tar.gz` | `9ab0fb6c12302fe032db742994224b0858cb6d4624435508c7689e6b93901243` |
| `baton-x86_64-pc-windows-msvc.zip` | `5f37e1338c0c1cbcccdc73972e9f416190ac2c66cdd0585d0224c031ea3f49c3` |
| `baton-x86_64-unknown-linux-gnu.tar.gz` | `b9b769772a67986961a68d3eda69b6cd4e788c0747b29d1f8e5f6a604df7275d` |

The tap formula pins the four Unix archives to these values, and `shasum -a 256 -c SHA256SUMS` reported OK for all five downloads.

## v0.4.1 release assets

Published 2026-11-24 from tag `v0.4.1` at `afab876ca4390c5e29c6f5ad4562e6ab1ce194e7`.

| Asset | SHA-256 |
|---|---|
| `baton-aarch64-apple-darwin.tar.gz` | `82ad334c902ed132557f75f6f820a3651385efef6d6ff8e92e0d0299a0e0ce3d` |
| `baton-aarch64-unknown-linux-gnu.tar.gz` | `6c7c044120c0a0f7dd2abe496a58cb96cc8ee2e68601c513538134fd764f73af` |
| `baton-x86_64-apple-darwin.tar.gz` | `6c9f6269f440e6833f12acc9de20358329c2a8ee08d1ccb4ad71df82d7015a99` |
| `baton-x86_64-pc-windows-msvc.zip` | `69a4c1ed350331ba8aee27f0f089806d37b1f76723f0148b2874caa93a4d3d9c` |
| `baton-x86_64-unknown-linux-gnu.tar.gz` | `34b20966a76521f5f22e4b22fadb0d412661ef525cbaf7694308f222e64f392a` |

The tap formula pins the four Unix archives to these values. The v0.4.1 Homebrew binary and the v0.4.1 curl install both passed the PTY slash probe.

## v0.4.2 release assets

Published 2026-11-25 from tag `v0.4.2` at `21c1b739788d5c8989928c2a526f6981fc585f13`.

| Asset | SHA-256 |
|---|---|
| `baton-aarch64-apple-darwin.tar.gz` | `a0b3e8968df7b1b66376b18357ca797afa66e804fcb3d6eace31bfee0c53f143` |
| `baton-aarch64-unknown-linux-gnu.tar.gz` | `80228e04072c81bc40f10c98de0464afafaa939100d4a15d602deb0c7311fd94` |
| `baton-x86_64-apple-darwin.tar.gz` | `ec50fcd0a1e31d88c96e0fa5145482ed9cbdc6bccb941ac7eb6f656ace6350af` |
| `baton-x86_64-pc-windows-msvc.zip` | `b03c68c7aa7dd81a41caa1940de6fec703ede719590bba8131ff974528a258fd` |
| `baton-x86_64-unknown-linux-gnu.tar.gz` | `989743557881ef706f2fdb1e87532be04707a54d5a72b4b1980e3b42fc9be2ae` |

The tap formula pins the four Unix archives to these values. The v0.4.2 Homebrew, Cargo, and curl binaries all passed the PTY question probe; the Homebrew binary passed the slash probe and the 50-command sweep.

## v0.4.3 release assets

Published 2026-11-25 from tag `v0.4.3` at `662bc0421139acca1110e1ddd51df61f6500281d`.

| Asset | SHA-256 |
|---|---|
| `baton-aarch64-apple-darwin.tar.gz` | `27d5ace85d7f889004b20741baf98c0ea8aa56e0b739deee61075eeed5fa95c4` |
| `baton-aarch64-unknown-linux-gnu.tar.gz` | `356e52ebb933e8ed721e16d36552549dbd2126ba45637b5be91b06ec043c8dd2` |
| `baton-x86_64-apple-darwin.tar.gz` | `e2857e3f49344e3d3be69bd96f4f1cdce5a53f73572daea9f4ee6599aea05cd8` |
| `baton-x86_64-pc-windows-msvc.zip` | `baccad9530ebc05c81a0b8fbface9ad0e1987a8468fe9476b5037951333cf966` |
| `baton-x86_64-unknown-linux-gnu.tar.gz` | `00622e3396bd8ad36114ef23a567021c99806f6cd5337ed48f4b09cda0b9ffa4` |

The tap formula pins the four Unix archives to these values. The Homebrew 0.4.3 binary passed the slash probe, the 50-command sweep, the Codex question probe, and the Antigravity approval plus question probe. The Cargo 0.4.3 binary passed the slash probe. The curl 0.4.3 install reported `baton 0.4.3`.

Installed performance on the 0.4.1 Homebrew binary: typing p50 7.9 ms and p95 8.0 ms, `/` suggestion p50 0.7 ms and p95 0.9 ms, and process start p50 20.5 ms on a 13.9 ms control spawn (ratio 1.34 against the 1.36 baseline). `startup.store` at 0.52 ms and slash p50 at 0.7 ms exceed the recorded 0.32 ms and 0.35 ms baselines. The 0.4.0 and 0.4.1 binaries measure the same values and the process ratio is flat, so the sub-millisecond baselines need a fresh host record or a wider absolute slack. That decision is open.

## Review record

The EGM Goldfish loop used two reviewers: Hypatia as design critic and Raman as structural reviewer. Round 1 found the unsupported complete-archive claim and mixed measurement definitions. Rounds 2 through 4 found and fixed wrong Verify endpoints, retry handling, Rust and meta sample mixing, and percentile reporting inconsistencies. Round 7 caught an out-of-date-base run that the classifier labeled `unknown`; it was removed rather than counted as meta. Rounds 8 and 9 reported no Blocker or Major findings. Rounds 10 and 11 reviewed the ARM runner proposal with four fresh reviewers. They found test-step and job-duration conflation, mixed PR-feedback and job-duration metrics, and a blocker: an ARM-only PR Test job would remove Linux x64 runtime coverage from the pre-merge gate while merge queue is unavailable. The proposal was rejected and the x64 Test job retained.
 Round 12 found that run-mode TI004 could never execute and that the Test job read the floor and timing script from writable or head-controlled paths. The parser now compares announced and executed totals, and this change feeds it the read-only trusted floor and trusted timing script.

## Open gaps

- The historical sixteen-sample Rust median is 63.5 seconds. The new warm samples include 48, 51, 52, 53, 55, 59, and 62 seconds, with feature main pushes at 51 and 59 seconds. The post-merge feature sample now exists, but all p50 and p95 rows remain provisional until 100 runs or four weeks exist.
- The free ARM runner was measured and rejected because it would remove x64 runtime tests from the pre-merge gate. A future proposal must be additive, not a replacement, keep an independent required x64 pre-merge Test job, and provide a matched rerun under the 100-run or four-week measurement policy. A merge queue alone is insufficient if it runs the same ARM job.
- The docs-only samples are 44 seconds (historical) and 38 seconds (run 45077008165); n=2, median 41 seconds, max 44 seconds.
- Queue p50 is 0 seconds on cited non-retry runs. Queue p95, setup p95, and cache hit rate are not established; setup has one 16-second sample against a 15-second target.
- The repository is public. Standard GitHub-hosted runners are free. No paid-runner cost is approved or required by the current measurements.
- A paid 8-core runner remains an unmeasured option. It is not recommended until both Rust-target metrics miss across at least 100 runs or four weeks.
- Official merge queue requires organization ownership; private organization repositories additionally require GitHub Enterprise Cloud. Until then, the PR `verify` run plus the following `main` push are the merge proof.
- Branch protection on `main` was configured on 2026-11-23: required `Verify`, strict up-to-date, no force pushes, no deletions, and admin enforcement off for the documented `ci-repair` override. GitHub rejected user push restrictions on this user-owned repository, so direct owner pushes remain a visible residual risk. The branch-protection API response is the proof.

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
