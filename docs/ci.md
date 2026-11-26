# Baton CI/CD design

This document defines the GitHub Actions system for Baton, the measurements that justify it, and the acceptance record for rollout. The parity scope is in [parity.md](../.agents/parity.md). The recorded measurement rules are in [ci-acceptance.md](ci-acceptance.md).

The design is staged. Phase 1 creates one honest full-suite gate with hard budgets and real CI data. Sharding and diff-aware selection arrive only after that data exists, because a shard plan built from macOS timings would be a guess. Formatting and Clippy ratchets prevent new debt while the full lint baseline is cleaned.

## Problem and scope

Baton's pull-request feedback was above the one-minute target even when the test suite itself took only a few seconds. The critical path was dominated by a full `Changes` checkout, two trusted-tool checkouts, serialized guards, a Rust toolchain cache restore, a Cargo cache restore, and a test build with full debuginfo. The CI must stay honest while it gets faster: no skipped gate, no retry that hides a failure, no weaker command, and no invented measurement.

The scope is the GitHub Actions workflow for the public Baton repository, the trusted `ci/` control plane, path classification, cache and budget policy, the recorded acceptance evidence, and the project-local verification skills. This slice covers Linux x64 pull requests and main pushes.

The workflow requires no repository secrets. It uses the built-in `GITHUB_TOKEN`, and the job-level `secrets` map is rejected by `ci/check-ci.sh`. The only spend option considered is a larger runner, which a user-owned repository cannot use and which is recorded here rather than silently enabled.

## Non-goals

- Replacing `cargo test` with a new test framework or changing test semantics.
- Moving the required x64 runtime gate to ARM or a paid runner without matched evidence.
- Enabling the official merge queue before the repository has an eligible organization owner.
- Running the TLA+ formal gate inside GitHub Actions before the JDK and jar are pinned artifacts.
- Per-test sharding before recorded per-test durations show that test execution, not setup or compile, is the bottleneck.
- A full adversarial sandbox for test code in this slice. The manifest and log checks remain detection controls, not a security boundary.

## Goals

| Metric | Target | Gated from |
|---|---|---|
| PR feedback p50, Rust PR | Under 60 seconds | Phase 3 |
| PR feedback p95, Rust PR | Under 120 seconds | Phase 3 |
| PR feedback p50, docs-only PR | Under 45 seconds | Phase 3 |
| Any single build or test command's warm budget | Under 90 seconds; Clippy and format debt are separate ratchets with their own ceilings | Phase 3 |
| Queue p50 | Under 10 seconds | Phase 2 |
| Queue p95 | Under 20 seconds | Phase 5, after 100 runs exist |
| Setup p95 | Under 15 seconds | Phase 2 |
| Hidden retries | Zero | Now |
| Skipped required checks | Zero | Now |
| Workflow-level false green | Zero in the workflow and aggregator; adversarial test-process tampering remains a residual risk | Now for the workflow; Phase 3 for a sandbox |

Resident memory is measured by `scripts/rss_probe.py`: idle at the composer, peak while exercising the slash list and typing, and peak during a provider turn. The provider turn runs a fake Codex app server that streams thirty deltas over about 1.5 seconds, and the probe asserts the stream reached the TUI before it reports the peak. The recorded baseline on this host is 20.2 MB idle, 20.4 MB render peak, and 22.1 MB provider peak; five consecutive checks pass and a baseline scaled to 70 percent of idle and 50 percent of the peaks fails all three metrics.

Startup is measured by `scripts/bench.py` against `scripts/bench-baseline.json` for the debug binary and `scripts/bench-release-baseline.json` for release. The gate compares the minimum of N process starts divided by the minimum of N control spawns, because a busy host slows every process by 20 percent or more and an absolute millisecond threshold then reports the host as a regression. The whole sample repeats three times by default and the minimum ratio and phase values across repeats are compared, because one batch can catch a transient slow or fast control spawn; the recorded baseline carries the repeat count. The debug baseline was re-recorded on this host on 2026-11-25 with three repeats after a control-spawn fluctuation failed a single-batch run while the absolute process time improved. A baseline scaled to 70 percent still fails both the ratio and the store phase. Phase checks use the minimum sample and add `--slack-ms` (0.1 ms by default), because a 0.35 ms phase cannot resolve a 10 percent band on a shared machine; phases recorded below `--resolution-ms` are reported and not enforced. `startup.process` is reported and not enforced because it includes runtime init outside Baton's work. A scaled-down baseline is the negative control: it fails the ratio and the phase it understates. `scripts/typing_probe.py` measures typing latency as keystroke-to-quiet. Each probe collects 80 samples, cycling the sample text when it is shorter than the sample count, and `--repeat 3` whole-probe runs are compared by their minimum p50 and p95 with a 0.1 ms slack. The slash mode types a plain character before each slash and times the first output after each key. The gate compares the slash-to-control ratio, so host load that slows both key paths cancels; the absolute slash time to the suggestion list is reported beside it. The debug typing and slash baselines were re-recorded on 2026-11-25 after an interleaved A/B with the 0.4.0 binary at `0b899f2` showed the older binary measuring the same values on this host, so the earlier gap described a quieter host rather than a product regression.

A baseline without the repeated-sample fields is refused with a re-record instruction. Negative controls prove the gate still fires: the debug and release typing baselines scaled to 70 percent of p50 and 50 percent of p95 fail both checks, and the debug slash ratio baseline scaled to 70 percent of p50 and 50 percent of p95 fails both ratio checks. Metrics recorded below `--resolution-ms` (2 ms) are reported and not enforced. The release slash baseline sits below that floor, so its absolute values and ratio are report-only; debug slash (p50 3.7 ms, p95 3.9 ms on a 3.1 ms control), debug typing, release typing, and release startup stay gated.

The release startup, typing, and slash baselines were re-recorded on 2026-11-25 from a `cargo build --release` binary: startup ratio 1.42 with `startup.store` 0.41 ms, typing p50 8.0 ms and p95 8.3 ms, and slash p50 0.6 ms, p95 0.7 ms, ratio p50 1.12, ratio p95 1.42. The release bench baseline now records three whole-sample repeats. Three consecutive checks of the startup and typing gates pass; the release slash absolutes and ratio are reported below the resolution floor.

The slash ratio control was added after host load moved debug slash p50 from 2.5 to 3.4 ms with no code change. With the ratio gate, the debug slash check passed three consecutive times while the host load average was 3.8 with a security agent at 64 percent CPU, and a negative control that scaled the ratio baseline still failed both ratio checks. Typing uses absolute keystroke-to-quiet values with a 10 percent band and no spawn control. A spawn control was prototyped and rejected: across eight identical probes the typing-to-spawn ratio ranged 0.67 to 0.81 while the absolute p50 ranged 11.9 to 12.8 ms, so the control added variance instead of cancelling it. Eight probes showed the absolute p50 spread is 7.6 percent, inside the band. The baselines were re-recorded on 2026-11-25 at debug p50 12.06 ms and p95 12.91 ms, and release p50 9.03 ms and p95 10.23 ms, after an A/B with the 0.4.0 binary at `0b899f2` measured the same values on this host. A typing failure across sessions is a host-state shift until an A/B with an older binary says otherwise.

The false-green goal is scoped to the workflow and aggregator. A test process with write access can alter its own log or manifest; that residual risk is listed in Risks and mitigations and is why the manifest check is called detection, not a sandbox.

Phase 1 shipped on 2026-11-22. Every job has `timeout-minutes`, every required command in `check`, `build`, and `test` runs under `ci/budget.sh`, and a budget miss exits 124 and fails the job. `verify` is a pure aggregator with no external command. The post-build `--version` smoke check runs outside the budget because the build it checks is already budgeted.

Phase 2 is in progress from recorded runs. The historical warm Rust PR samples are 52, 55, 56, 58, 60, 61, 62, 63, 64, 65, 65, 68, 68, 69, 70, and 74 seconds. The historical median is 63.5 seconds. The critical-path change in this slice moves path classification to a shallow base and head fetch and moves every trusted guard into `Selection`; the final branch dispatch with a forced Rust rebuild completed in 51 seconds, with `Changes` at 6 seconds and `Test` at 35 seconds. Head-tools branch dispatches completed in 53, 62, and 48 seconds; the 62-second repeat had slower setup and cache restores and skipped no check. The four warm branch runs have a 52-second median. The final head-tools run proved the trusted debt baselines, the debt self-test, and the Clippy count of 61 against the 61 baseline. The first post-merge main push then completed in 59 seconds with Test at 43 seconds, proving the merged tools and seeding the new cache. The first post-merge PR, a meta CI-docs PR with the full suite selected, completed in 52 seconds. Real Rust PR 45123430101 completed in 69 seconds and main push 45124406491 in 68 seconds; later PR 45132308493 completed in 51 seconds and main push 45132776683 in 61 seconds. The harness-matrix PR 45140130989 completed in 62 seconds. The slim toolchain-cache dispatches then recorded 62, 64, 47, and 62 seconds; the 47-second run proves the path can finish under one minute, while the four-sample median of 62 seconds shows that hosted-runner allocation and Cargo restore variance still prevent a robust p50 claim. The next two Rust PRs were 62 seconds for #174 and 53 seconds for #175; their main pushes were 64 and 59 seconds. The latest PR and main push both finished under one minute. The first 100 successful PR runs now record p50 62.0 seconds, p95 97.0 seconds, and max 150 seconds; the last 50 record p50 59.0 seconds, p95 69.0 seconds, and max 83 seconds. The 100-run window spans earlier CI designs, so the next 100 runs after `ba82720` are the current-design acceptance window. A free ARM runner was measured and rejected because it would remove x64 runtime tests from the pre-merge gate. The x64 Test job stays in place. The acceptance record names the runs, the job breakdown, and every rejected experiment.

Budget rule, applied at every phase: Phase 1 uses three times a conservative cold estimate. Phase 2 sets each budget to the measured Linux p95 plus 20 percent. That reset is the only allowed increase. After it, a budget may only shrink. Widening a budget needs a recorded regression explanation in `ci-acceptance.md`, not a code change.

## Design alternatives

| Alternative | Why it was not adopted |
|---|---|
| Keep the full head and base checkout in `Changes` | The fast path replaced a 16-second checkout and guard sequence with a shallow two-commit fetch and a base-owned classifier. The guards still run in `Selection`, so no check is removed. |
| List changed files through the GitHub pull request API | The API returns merge-base changes rather than the base-tip diff that `Selection` classifies. A shallow fetch of the event's base and head SHAs classifies the exact pair `Selection` recomputes, takes about one to two seconds, and cannot read a different tree. `refs/pull/<number>/merge` is not used because GitHub refreshes it asynchronously; after a force-push it can still name the previous head. |
| Move classification into every gate job | Each job would repeat GitHub API or git work and would still need a trusted cross-check. One fast `Changes` job plus a `Selection` recomputation gives one shared result and one place to fail closed. |
| Remove the Rust toolchain cache | Measured against a retained cache, Setup Rust took about 9 seconds instead of about 6 seconds of restore plus 1 second of activation. The removed-cache runs were 57 and 69 seconds against 69 and 71 seconds retained. No gain was proven. |
| Keep full debuginfo in the test profile | Line tables keep readable backtraces and cut the warm compile from 18.94 to 14.13 seconds on the measured run. The cache key includes `Cargo.toml`, so the profile change cannot silently reuse incompatible artifacts. |
| Shard the lib binary into five matrix legs | Tried on 2026-11-24 from recorded module times. The PR run took 87 seconds and the branch dispatch 70 seconds, against 64 seconds for the single Test job. Each leg repeated about 28 seconds of setup and cache restore, the slowest leg took 46 seconds, and the inventory job added 18 seconds. Rejected until a shared-artifact or diff-aware design has measured evidence. |
| Replace the x64 Test job with the free ARM runner | The ARM run was faster, but it would remove pre-merge Linux x64 runtime coverage while the official merge queue is unavailable. The x64 job stays required. |
| Move to a self-hosted or paid larger runner | The 21-run post-`ba82720` window is p50 63.0 s and p95 79.0 s. The fixed ~11 s run start plus ~26 s setup and cache restore in `Test` is the gap to 60 s; a larger runner is the remaining measured lever, but no spend is approved. |
| Configure the official merge queue now | The repository is public but user-owned. Merge queue requires an organization owner; that is an ownership action, not a workflow change. |

## Current baseline

Measured on 2026-11-22 from the cited runs. The Rust samples were based on `be31fc5` and the inbox slices; later slices added tests, so the executed count row is refreshed with each slice while the timing samples stay historical.

| Measurement | Recorded value | Evidence |
|---|---|---|
| Warm Rust PR feedback | 52, 55, 56, 58, 60, 61, 62, 63, 64, 65, 65, 68, 68, 69, 70, and 74 seconds; n=16, median 63.5 seconds, max 74 seconds | Runs 45035044248, 45055995298, 45004705055, 45009466461, 45021604178, 45060299027, 45011836996, 45010710576, 45037001045, 45046974039, 45051808580, 45064707055, 45069517940, 45075059836, 45075683884, and 45080151770 |
| Warm meta PR feedback, descriptive | 59 seconds, n=1 | Run 45005544007 |
| Warm Test job | 33 to 52 seconds; n=16 | Runs 45035044248, 45055995298, 45004705055, 45009466461, 45021604178, 45060299027, 45011836996, 45010710576, 45037001045, 45046974039, 45051808580, 45064707055, 45069517940, 45075059836, 45075683884, and 45080151770 |
| Main push at `7f22ab5` | Whole run 66 seconds; Test job 51 seconds. Job steps: 17 seconds of checkout and cache restore, 30 seconds of Test, 4 seconds of tail. The run starts the Test job 5 seconds in because it waits only on `changes`, so the remaining lead-in is runner startup | Run 45580109649 |
| `cargo test --lib` | 1.67 to 3.70 seconds at the cited runs; 842 library tests in the current suite | Timing runs 45009466461 and 45011836996; current count from the 2026-11-26 skill removal run |
| Integration tests | 0.40 to 0.75 seconds at the cited runs; 128 tests across nine binaries in the current suite | Timing runs 45009466461 and 45011836996; current count from the 2026-11-26 skill install run |
| Executed test count (`ci/test-inventory.sh`) | 970 macOS after the skill removal slice | Current source inventory; Linux runs the same suite plus the two platform-specific tests named below |
| `ci/test-inventory-floor.txt` | 776 | Lower-bound floor; it does not shrink when the current count grows |
| `ci/fmt-baseline.txt` | 550 | Current debt ceiling |
| `ci/clippy-baseline.txt` | 61 | Current debt ceiling |
| Cold Rust PR | 189 seconds | Run 44982816103 |
| Debt ratchet before the merge | Ratchet job 48 seconds on the critical path: 13 seconds restoring the lint toolchain, 9 restoring cargo, 3 format, 7 clippy, and the rest setup; Check ran alongside at 20 seconds | Run 45599904468 |
| Debt ratchet after the merge | The job is gone. Check absorbed it at 38 seconds, the run fell from 64 to 60 seconds wall, and the critical path moved to Test at 44 seconds | Run 45606086754 |
| Post-merge job count | Six jobs instead of seven, and three Rust jobs restoring caches instead of four | Runs 45599904468 and 45606086754 |
| Warm Rust rebuild after this slice | 51 seconds; `Changes` 6, `Test` 35, `Verify` 5 | Branch dispatch 45114396442 |
| Final head-tools dispatch | 48 seconds; `Changes` 6, `Test` 32, `Debt ratchet` 32 | Branch dispatch 45119247835 |
| First post-merge main push | 59 seconds; `Changes` 6, `Test` 43, `Verify` 5 | Push 45119477704 |
| First post-merge meta PR | 52 seconds; `Changes` 6, `Test` 35 | Pull request 45119777637 |
| Toolchain-cache removal | Not adopted; setup without the cache was slower than restore plus setup | Runs 45108914603, 45109176611, 45109045484, 45109314728 |

The workflow fails if a PR raises a baseline or lowers the test inventory floor. The 776 floor is a lower bound, not a per-test lock; Linux executes one additional library test and one additional non-UTF-8 integration test. The run-mode check now sums every `running N tests` line from the captured log and requires `passed + ignored` to equal that total. It also requires the executed count to meet the floor and rejects ignored tests. Deleting tests above the floor still needs review in the diff. The Cargo cache key includes `Cargo.toml` as well as `Cargo.lock` and `rust-toolchain.toml`, so a test-profile change cannot restore incompatible compiler artifacts.

Older local macOS measurements are historical and are not acceptance evidence. The acceptance record contains the Linux run-level data and the rejected experiments.

## Runner experiment result

The free `ubuntu-24.04-arm` runner is not adopted for Test. A temporary ARM job ran all 406 tests in 85 seconds cold and 33 seconds warm, with Test steps of 56 and 22 seconds. The 406-test x64 sample is [run 45011836996](https://github.com/billy-enrizky/baton/actions/runs/45011836996): a 47-second job with a 16-second step and a partial Cargo cache because it added `clap_complete`. The ARM run restored a full cache. The jobs differ in cache state and executed steps, so the timings are descriptive and not matched. No ARM performance advantage is claimed.

Replacing the x64 Test job would move Linux x64 runtime tests after merge, and official merge queue is unavailable for this repository. A runtime-only x64 regression could then land without a pre-merge test. That violates the rule that no phase removes a passing check. Test stays on `ubuntu-24.04`. The option is closed unless a future proposal is additive, not a replacement, keeps an independent required x64 pre-merge Test job, and provides a matched rerun with identical steps, event shape, cache policy, and command under the 100-run or four-week policy. A merge queue alone is insufficient if it runs the same ARM job.

The experiment is recorded in [ci-acceptance.md](ci-acceptance.md#rejected-arm-runner-experiment) with its runs, timings, cache sizes, and CI013 failure. The historical Rust median remains 63.5 seconds; this slice's provisional warm result is 51 seconds.

## What the gate enforces

Every check below is a real command in `ci/`. `ci/selftest.sh` proves the workflow rules, and `ci/debt-selftest.sh` runs the negative format and Clippy ratchet fixtures in the check job.

| Area | File | Enforcement |
|---|---|---|
| Budgets | `ci/budget.sh` | `timeout` with a Perl fallback; a miss exits 124 and fails the job |
| Path selection | `ci/classify-paths.sh` | `--event`, `--files`, first-match precedence `unknown` > `code` > `meta` > docs |
| Workflow invariants | `ci/check-ci.sh` | Rules CI001 to CI015, CI017, and CI018 (CI016 is unused) |
| Gate integrity | `ci/gate-integrity.sh` | Rules GI001 to GI007 |
| Test inventory | `ci/test-inventory.sh` | Rules TI001 to TI005. Run mode compares announced and executed totals; list mode counts discovered tests |
| Format debt | `ci/fmt-debt.sh` | Count of `Diff in ` lines must not exceed the baseline |
| Clippy debt | `ci/clippy-debt.sh` | Count of lint diagnostics, excluding compiler summary lines, must not exceed the baseline |
| Self-test | `ci/selftest.sh` | Runs every workflow fixture, stale-base and Git typechange fixtures, the budget negative test, and the inventory floor, short-execution, and missing-total cases |
| Debt self-test | `ci/debt-selftest.sh` | Runs in the check job and proves a format diff and a Clippy lint over a zero baseline both fail |

Workflow invariants:

| Rule | Rejects |
|---|---|
| CI001 | Missing trigger, a `push` trigger that does not target `main`, or `paths`, `paths-ignore`, or `branches-ignore` on `pull_request`, `push`, or `merge_group` |
| CI002 | Missing or non-numeric `timeout-minutes`, or a timeout of 360 minutes or more |
| CI003 | A retry action, or `continue-on-error` at step or job level |
| CI004 | `\|\| true`, `set +e`, or `set +o errexit` in a step |
| CI005 | A missing `verify` job, a missing `if: always()` on `verify`, or a missing `needs` edge |
| CI006 | Top-level or job-level `permissions` other than read |
| CI007 | A concurrency group that is not keyed by PR number and SHA, or a PR run that does not cancel |
| CI008 | A `pull_request_target` trigger |
| CI009 | A missing `changes` output |
| CI010 | A gate job whose `if` does not read its own `changes` output |
| CI011 | An action reference that is not pinned to a 40-character SHA |
| CI012 | A step-level `if` on a required command step |
| CI013 | A job list, command, `needs`, `if`, timeout, runner, output, or strategy change against the base workflow |
| CI014 | A missing required command in a gate job |
| CI015 | Top-level or job-level `env` or `defaults`, a job-level `container`, `services`, or reusable workflow |
| CI017 | A required command that does not run under `ci/budget.sh` |
| CI018 | The `changes` job reads a file list without the immutable event head SHA, or reads `refs/pull/`, which can name the previous head |

Gate-integrity rules:

| Rule | Rejects |
|---|---|
| GI001 | `rustfmt.toml` or `clippy.toml` in any spelling |
| GI002 | Any `.cargo/config.toml`, `.cargo/config`, or `.cargo/config.toml.example` |
| GI003 | `[lints]` in `Cargo.toml` |
| GI004 | A blanket all, pedantic, nursery, or correctness Clippy allow in `src/` or `tests/` |
| GI005 | A warning suppression or conditional Clippy suppression in tracked Rust sources |
| GI006 | A `#[rustfmt::skip]` attribute in tracked Rust sources |
| GI007 | A fixture path `ci/selftest.sh` references that is not in the tree |

## Risks and mitigations

| Risk | Mitigation | Residual risk |
|---|---|---|
| A PR changes the workflow to skip a selected gate | `check-ci.sh` compares the base and head workflow, `Changes` and `Selection` must both succeed, and `verify` checks every selected result | A repository owner can still merge with an explicit override; the override is visible in the PR and merge record |
| A test process tampers with its log or the trusted-tool manifest | The base supplies the harness, the log is captured as untrusted input, and the manifest is rechecked after the test gate | Test code runs with write access to the runner filesystem. This is detection, not a sandbox, until the Phase 3 boundary exists |
| The shallow fetch or base classifier is unavailable | `changes` selects every gate on failure; `selection` recomputes and fails closed on a mismatch | A transient GitHub failure can block a PR instead of silently shrinking the gate |
| A profile or dependency change reuses incompatible Cargo artifacts | The cache key includes `Cargo.lock`, `Cargo.toml`, and `rust-toolchain.toml`; the first main run after a profile change is a cold seed | One cache-miss run is slower and must be recorded separately from warm samples |
| Diff-aware test selection is mistaken for full-suite coverage | This slice only classifies which gates run. Every code or meta PR still runs the full Rust suite; docs-only is the only fast path | The Phase 3 selector is not shipped and its union proof is still required |
| A direct push or owner bypass skips `Verify` | `main` requires `Verify` and an up-to-date branch for PR merges, blocks force pushes and deletions, and keeps admin enforcement off. GitHub rejects user push restrictions on a user-owned repository, so the owner can still push directly | Owner-only direct push remains a visible residual risk; move to an organization if it must be blocked. Every push still runs `Verify` after the fact |
| The 51-second warm result is mistaken for a PR p50 | Acceptance keeps the historical 16-sample distribution and labels the new result provisional until a post-merge PR sample exists | A future regression still needs 100 runs or four weeks before the percentile is acceptance-bearing |
| Paid runner or merge-queue capacity is assumed | Costs table and owner actions state that no paid capacity is approved and merge queue is blocked on ownership | The target may require an owner decision if the free runner cannot meet p95 |

## Trust model

Pull requests and merge groups use base-owned pass/fail scripts. Main pushes and owner-controlled dispatches use the checked-out head scripts, so the first push after a CI change proves the merged tools rather than the previous tools.

- `pull_request` uses `github.event.pull_request.base.sha`.
- `merge_group` uses `github.event.merge_group.base_sha`.
- `push` uses `github.event.before`, falling back to `HEAD~1` for a first push or a zero SHA.
- `workflow_dispatch` uses `github.event.before`, falling back to `HEAD~1`.

`changes` is the fast path. It does not check out the worktree. For a pull request it initializes a temporary repository, fetches only `github.event.pull_request.base.sha` and `github.event.pull_request.head.sha` at depth one, exports the base-owned `budget.sh` and `classify-paths.sh` from the base commit, and classifies the base-to-head diff. Both SHAs come from the event payload and are fixed for the run, so the pair matches the base-to-HEAD diff that `selection` recomputes. The workflow guard rejects a `--files` classification that reads `refs/pull/` instead of the head SHA. That ref is refreshed asynchronously, so after a force-push it can still describe the previous head, which classifies a different diff than `selection` and fails the pull request at the guard. A failed fetch or classification selects every gate. The job deliberately carries no pass/fail guards, so the Rust jobs can start before the trusted tools finish staging.

`selection` is the trust gate. It sparse-checks out the base copy with `/ci/`, `/.github/workflows/`, and `/rust-toolchain.toml`. On pull requests and merge groups it stages `base/ci`; on main pushes and dispatches it stages the checked-out head `ci/`, because main merges require `Verify` and an up-to-date branch and dispatches are owner-controlled. A direct owner push is a visible bypass, not a proved tree. It marks the staged copy read-only, records a SHA-256 manifest, and executes only from that copy. It runs gate integrity, the full tool self-test, and the baseline ratchets, then recomputes classification and compares every output with `changes`. It also compares `rust-toolchain.toml` and runs the workflow guard. The `test` job reads the inventory floor and timing script from its own trusted copy, treats the captured test log as untrusted input, and re-verifies the manifest after the test gates. The `check` job reads both ratchet baselines from the same read-only copy, so a `build.rs` cannot raise its own ceiling before Clippy runs. Moving the guards into `selection` is safe because `verify` still requires both `changes` and `selection` to succeed.

That staging is a detection boundary, not a sandbox. `cargo test` runs arbitrary repository code with write access to the runner's filesystem, so a determined change can still tamper with the process. What staging buys is that tampering after the first gate step is visible as a manifest mismatch instead of silently changing the harness. A real sandbox needs a separate container or a read-only mount, which is a Phase 3 decision.

Head-controlled process configuration is rejected rather than compared. A workflow or job `env` block can shadow `cargo` on `PATH`. A job-level `permissions` block can grant write access while the top-level block stays read. A job-level `container`, `services`, or reusable workflow moves the gate somewhere the guard cannot see. CI015 and CI006 reject all of these outright, and `persist-credentials: false` keeps the checkout token out of the git config.

`rust-toolchain.toml` is compared byte for byte with the base copy in the `selection` job. There is no `RUSTUP_TOOLCHAIN` environment override anywhere in the workflow.

On `push`, `merge_group`, and `workflow_dispatch` the guard runs with `--allow-change`, because a merged CI change must be able to run outside a pull request. `--allow-change` permits only the base-to-head step comparison. Required commands and `CI017` budget enforcement are unconditional, so the override cannot remove a gate or run it outside `ci/budget.sh`. Job-list equality and job-level wiring (`needs`, `if`, timeout, runner, outputs, strategy, `with`, and secrets) are always enforced. `workflow_dispatch` can target a feature branch, so it is owner-controlled rather than a substitute for the PR gate.

Changing a gate command is not a PR-level decision. There is no `ci-change` label. The workflow passes `--allow-change` on `push`, `merge_group`, and `workflow_dispatch`. Push and merge-group commits are protected; `workflow_dispatch` is owner-controlled and can target a feature branch. A job-list or job-wiring change is rejected on every event. A PR that changes a command, `needs`, `if`, timeout, runner, output, or strategy fails `verify` against the base workflow, by design. Step-level `working-directory`, `shell`, and every `continue-on-error` other than literal `false` are rejected too, because any of them can redirect a trusted tool or swallow a failure.

The single escape hatch is `ci-repair`, and it is a human action rather than a workflow branch:

1. The repository owner opens a PR that touches only `ci/**`, `.github/**`, or `docs/ci*.md`.
2. The PR is expected to fail `verify` at the base comparison. That failure is the record of the change.
3. Before merging, the owner runs `ci/selftest.sh`, `ci/check-ci.sh --base <base workflow> <head workflow> --allow-change`, `actionlint`, and a full local `cargo test --locked` on the same commit. The guard still enforces every required command and budget; `--allow-change` only permits the reviewed command and step changes.
4. The owner merges with an admin override, quoting the local results in the PR.
5. The `push` run on `main` is the proof: it stages the merged head `ci/` tools, runs the full self-test and workflow guard against them, selects every gate, and executes the full suite. The same proof is available on a branch through `workflow_dispatch`, which is owner-controlled.

This is visible in the PR, the merge API, and the next main run. It is not silent, and it cannot be triggered by an outside contributor.

## Rollout sequence

| Phase | Change | Files | Acceptance |
|---|---|---|---|
| 1A | Design and acceptance record | Create `.agents/parity.md`, `docs/ci.md`, `docs/ci-acceptance.md`, `scripts/check_parity_paths.py`, `scripts/check_doc_links.py`; modify `README.md` | Both checkers print `0 missing`; the EGM review record has two consecutive clean rounds |
| 1B | Trusted helpers, skills, and formal model | Create `ci/*.sh`, `ci/fixtures/**`, `ci/fmt-baseline.txt`, `ci/clippy-baseline.txt`, `ci/test-inventory-floor.txt`, `ci/formal/ci-admission/**`, `rust-toolchain.toml`, `.agents/skills/verify-baton-ci/**`, `.agents/skills/verify-baton-ci-formal/**` | `ci/selftest.sh` passes; a budget overrun exits 124; the formal gate passes locally; `cargo check`, `cargo build`, `cargo test`, and `ci/test-inventory.sh` pass |
| 1C | Full-suite workflow | Create `.github/workflows/ci.yml`; modify `docs/ci.md` | Done: run 44982816103 executed every job with `verify` green, and run 44983114852 is the first green `main` push |
| 2 | Record real timings and choose the test runner | `ci/test-timings.sh` records per-binary times; `docs/ci-acceptance.md` records the warm path and rejected runners | In progress: `cargo test` and the x64 runner retained; nextest count shards and the free ARM runner were rejected; the p50 miss remains |
| 3 | Diff-aware selection and recorded-time shards | Extend `ci/test-timings.sh` to emit per-test durations; create `ci/select-tests.sh`, `ci/plan-shards.sh`, `ci/test-groups.json`; modify `.github/workflows/ci.yml`, `ci/check-ci.sh`, `docs/ci.md` | Not started: per-test timings, selector fixtures, and shard union proof still required |
| 4 | Full format and lint plus local hooks | Create `.githooks/pre-commit`, `.githooks/pre-push`, `ci/install-hooks.sh`; modify `.github/workflows/ci.yml`, `docs/ci.md` | Format and Clippy baselines are zero and both are required; hooks run under budget |
| 5 | Last-100-run audit and runner decision | Modify `docs/ci-acceptance.md` | PR p50/p95, queue p50/p95, cache hit rate, and a paid-runner decision, all from recorded runs |

No phase may remove a passing check, widen a budget to hide a regression, or add a retry that changes a failing result into a passing one. Each phase is one small PR. Phase 1B depends on PR #86 merging first, because the debt baselines are measured from the merged `main`. Phase 1C depends on 1B, because the workflow invokes `base/ci/`.

## Phase 1 workflow contract

Triggers: `pull_request` (`opened`, `synchronize`, `reopened`, `ready_for_review`), `push` to `main`, `merge_group`, and `workflow_dispatch`. Top-level permissions are `contents: read`. Concurrency keys on `github.event.pull_request.number || github.sha` and cancels only pull-request runs.

| Job | Needs | Timeout | Runs |
|---|---|---|---|
| `changes` | none | 5 | Shallow base and head SHA fetch, trusted classifier from the base, no checkout |
| `selection` | `changes` | 5 | Gate integrity, full tool self-test, baseline ratchets, recomputes the classifier and compares outputs, compares `rust-toolchain.toml`, runs the workflow guard |
| `check` | `changes` | 10 | `cargo check --locked --all-targets` under a 120-second budget, then the format ratchet under 60 seconds, the Clippy ratchet under 300 seconds, and the debt self-test |
| `build` | `changes` | 10 | `cargo build --locked --bin baton` under a 150-second budget, then `--version` |
| `test` | `changes` | 15 | `cargo test --locked` under a 300-second budget, then `ci/test-inventory.sh` |
| `verify` | `changes`, `selection`, `check`, `build`, `test` | 2 | The required branch-protection check; strict up-to-date, with an owner-only `ci-repair` bypass |

`changes` outputs: `check_needed`, `build_needed`, `test_needed`, `debt_needed`, `full_suite_reason`, `has_tests`, `docs_only`. `full_suite_reason` records the selection class (`code`, `meta`, `unknown`, `empty`, `docs`, or the event name for non-PR events); it is not a claim that the full suite ran.

`verify` fails when:

- `changes` or `selection` did not succeed.
- A selected gate did not succeed, or an unselected gate is not `skipped`.
- `test_needed` is true and `has_tests` is not true.
- `test_needed` is true and the discovered test count is missing, not numeric, or zero.
- A non-PR event did not select `check`, `build`, and `test`, or reports `docs_only`.

`verify` does not use `paths-ignore`, does not use `continue-on-error`, and has no `if` that can skip it.

### Path classification

| Changed path class | check | build | test | debt_needed | docs_only | has_tests | full_suite_reason |
|---|---|---|---|---|---|---|---|
| Any `*.rs`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `build.rs` | true | true | true | true | false | true | `code` |
| `.github/workflows/**`, `ci/**`, `docs/ci.md`, `.agents/parity.md` | true | true | true | true | false | true | `meta` |
| `docs/**`, `*.md`, `LICENSE`, `NOTICE` only | false | false | false | false | true | false | `docs` |
| `.gitignore` or any other unknown path | true | true | true | true | false | true | `unknown` |
| Empty diff | true | true | true | true | false | true | `empty` |

`debt_needed` is still emitted for compatibility, but no job gates on it: the ratchets run inside `check` whenever `check_needed` is true, and the selection guard reads their baselines from the trusted copy.

Precedence is first-match over the classes in the order `unknown`, `code`, `meta`, docs-only. A diff that mixes classes takes the strongest match, so `docs` plus `code` selects the full suite with reason `code`.

A missing base commit, a failed `git diff`, or an unknown event selects every gate and reports reason `unknown`. Selection fails closed.

## Diff-aware selection (Phase 3)

Selection is not shipped in Phase 1, and the design is fixed now so the fixtures can be written with it.

The dependency map is generated from Cargo targets plus a reverse transitive import graph over module paths. Selection takes the reverse closure of every changed module. Selecting by direct importers only is wrong: `src/usage.rs` is imported by `src/tui/mod.rs` and indirectly by every test that drives the TUI, so a direct-importer graph would skip those tests.

Rules:

- `src/lib.rs`, `src/main.rs`, `Cargo.toml`, `Cargo.lock`, and `rust-toolchain.toml` always select the full suite.
- A changed module with no mapped importers selects the full suite.
- The generated map is committed and regenerated by a script, so a missing edge is a diff, not a silent skip.
- Every selector decision is logged as `group=<name> reason=<shared|closure|unmapped|full>` in the test job.
- The union of selected groups plus the full-suite group must equal the discovered inventory from `ci/test-inventory.sh`. A shortfall fails the job.

Fixtures required before Phase 3 lands: transitive importer, `lib.rs`, unmapped module, docs-only, and a module imported only by a test binary.

## Sharding (Phase 3)

Shards are not active. The current lib binary runs 663 tests and the nine integration binaries run 113 tests. At the cited runs the lib binary ran 326 tests in 1.73 seconds and the seven integration binaries ran 75 tests in 0.39 seconds. Sharding would duplicate the 16-second Test setup and the 13-second compile for no gain.

If a gate ceiling is missed, shards are filled from recorded per-test times, never from a count:

- Record per-test durations before designing a shard plan.
- Balance on maximum and p95, not p50.
- Give a test that is longer than the other shard work its own group until it is fixed or split.
- The catch-all group has its own budget. If it overflows, the job fails instead of stretching the budget.
- The shard count is the smallest count that meets the gate ceiling on the target runner.
- The union check from the selection section also applies to shards.

Count-based nextest shards were rejected in run 45002437721: one shard took 33 seconds and the other 61 seconds. The 61-second shard contained a single 30-second test.

Per-test timing was measured on 2026-11-25 on macOS with 18 cores and a 1-minute load of 3.48. A full `cargo nextest run` reported 776 tests and a 151-second suite summary, but the sum of the per-test `exec_time` values was 2,595.9 seconds. One test, `tui::tests::git_command_flow`, reported 45.976 seconds in that parallel run and 7.635 seconds when run alone with `-j 1`. Nextest's per-test durations therefore include scheduling and resource contention, so they cannot fill a shard plan. A serialized baseline would take about 43 minutes on the same host, which is too long to collect per change. Shards stay blocked until nextest reports a per-test cost that survives parallelism, or a serialized baseline becomes affordable.

## Formal CI admission model

`ci/formal/ci-admission/BatonCiAdmission.tla` models gate admission, not test correctness. `Gates` holds the five jobs plus the workflow-guard step (`changes`, `selection`, `workflow_guard`, `check`, `build`, `test`); `verify` is modeled as the aggregator that reads them, because it never runs a check itself.

The real configuration, `BatonCiAdmission.cfg`, runs 135,905 states and 24,125 distinct states. Six mutation configs must each fail with a named invariant:

| Config | Invariant that must break |
|---|---|
| `BatonCiAdmissionSkipSelected.cfg` | `NoFalseGreen` |
| `BatonCiAdmissionMergeSelection.cfg` | `FullSelection` |
| `BatonCiAdmissionRetryMask.cfg` | `NoRetryMasking` |
| `BatonCiAdmissionUnknownSelection.cfg` | `ChangesFailOpen` |
| `BatonCiAdmissionCommandDrift.cfg` | `CommandsMatch` |
| `BatonCiAdmissionReportWithoutStart.cfg` | `NoFalseGreen` |

What the model proves, with `CommandDriftAllowed = FALSE`: a selected gate cannot be skipped and reported green, an unselected gate cannot report success, merge events cannot use a subset, a retry cannot convert a failure, an unknown selection cannot select nothing, and a command edit cannot pass `CommandsMatch`. The one config that sets `CommandDriftAllowed = TRUE` exists only to show that `CommandsMatch` depends on that constant. The model models the rule, not the shell implementation, so `ci/selftest.sh` remains the executable proof.

What it does not prove: test inventory completeness, shard union coverage, or that the selector's dependency map is correct. Those are Phase 3 work and will be covered by the selection union check and its fixtures; they are not covered today.

`ci/formal/ci-admission/run.sh` needs a JDK and downloads `tla2tools.jar` 1.7.4 to `TLA2TOOLS_JAR` when the file is absent. That download is why the gate is local and recorded in `docs/ci-acceptance.md`, not a required PR job: a hosted job that fetches a jar from the internet is a supply-chain and flakiness risk that Phase 1 does not need. Pinning the jar as a committed artifact plus a pinned JDK action is the Phase 3 follow-up.

## Budgets

| Command | Phase 1 budget | Phase 3 ceiling |
|---|---|---|
| `budget.sh` negative self-test | 1 second, must exit 124 | unchanged |
| Classifier | 30 seconds | 15 seconds |
| Tool self-test | 60 seconds | 30 seconds |
| Workflow guard | 60 seconds | 30 seconds |
| Gate integrity | 60 seconds | 30 seconds |
| `cargo check --locked --all-targets` | 120 seconds | 90 seconds |
| `cargo build --locked --bin baton` | 150 seconds | 90 seconds |
| `cargo test --locked` | 300 seconds | 90 seconds per shard |
| Test inventory | 120 seconds | 60 seconds |
| Format ratchet | 60 seconds | 30 seconds |
| Clippy ratchet | 300 seconds | 300 seconds |
| Debt self-test | 60 seconds | 60 seconds |

Phase 1 values are three times a conservative local cold estimate. They are caps, not targets: a command that exceeds one fails the job. Phase 2 replaces each value with the measured Linux p95 plus 20 percent. That replacement is the only allowed increase; after it, no value may increase. Clippy is a separate debt ratchet, not a build or test command; its 300-second ceiling is recorded here so the 90-second warm build/test goal is not misread as a Clippy ceiling.

## Debt ratchet placement

The ratchet runs inside the `check` job. `ci/check-ci.sh` enforces that the job runs `fmt-debt.sh`,
`clippy-debt.sh`, and `debt-selftest.sh` under `ci/budget.sh`; removing any of them fails CI014.

The merge was worth doing because the ratchet was the critical path. On run 45599904468 it took 48 seconds
against Check's 20, and 22 of those 48 were cache restores against ten seconds of fmt and clippy. Two jobs
meant two checkouts, two toolchain restores, and two cargo restores to do ten seconds of checking. Check now
uses the lint toolchain and saves it, and one job runs all three checks with separate budgeted steps so a
failure still names format, clippy, or self-test.

The trusted-base rule shaped how this landed. The linter runs from the base, so the acceptance for a merged
shape had to land before the workflow that uses it, and the job-removal and `verify.needs` rules had to land
before a removal could be expressed at all.

## Debt ratchets

A count is a floor, not a quality gate: fixing one violation while adding another keeps the count flat. Phase 1 accepts that weakness because the current baseline is 550 format diffs and 61 Clippy errors, and a full gate would fail every PR on day one.

Compensating controls, shipped in Phase 1:

- `ci/gate-integrity.sh` rejects the exact configuration set in the gate-integrity table, including any Cargo config file and any blanket Clippy group suppression.
- The ratchet scripts fail when the command produces no recognizable diagnostics at all, so a broken tool cannot report zero.
- The baseline files may only be lowered. Lowering one is part of the PR that fixes the debt.

Phase 4 replaces counts with a fingerprint set: the current diagnostics must be a subset of the recorded baseline, and any new fingerprint fails. Phase 4 has not started; it needs an owner and a target date before the ratchet can become a permanent exemption.

## Measurement and acceptance

The acceptance record is [ci-acceptance.md](ci-acceptance.md). It records the runs that changed a decision and the current warm sample. It is not a complete archive of every run.

- PR feedback starts at run `createdAt` and ends when `verify` completes.
- Queue time is `startedAt - createdAt`.
- Until at least 100 runs or four weeks exist, p50 rows report n, median, and maximum; p95 rows report n and maximum. A percentile from fewer than 100 runs before four weeks exist is provisional and cannot justify a paid-runner decision.
- Report docs-only, meta, and Rust PRs separately. A docs-only or meta sample must not stand in for Rust feedback.
- The acceptance log is selective. It records the decision-changing fields in the table and points to the run for the rest. The Actions run and API supply the workflow revision, selected gates, cache key, slowest job, failure step, and budget miss. The collection commands reproduce those fields.
- Record cancelled, failed, retried, and superseded runs when they change a decision. The remainder stay in Actions history rather than being copied into the acceptance file.

## Caching

`Swatinem/rust-cache` uses `shared-key: baton-v2-<os>-<arch>-<job>` and a `key` of the `Cargo.lock`, `Cargo.toml`, and `rust-toolchain.toml` hashes. The job name is part of the key so a lint job cannot save an archive without the test build. Saves are restricted to `main`, so a PR cannot poison the cache other PRs read. The `Cargo.toml` hash is what makes the test-profile change a deliberate cold seed rather than an incompatible restore.

A cache miss changes the time, never the checks: the cold path runs the same commands under the same budgets. If a cold run regularly exceeds a Phase 1 budget, the fix is to correct the budget from measured data in Phase 2, not to skip the gate. Workflow-dispatch cache saves are limited to users with write access and are used only to seed and measure the trusted toolchain cache.

The pinned Rust toolchain uses two shared caches: `rustup-min-1.100.0-<os>-<arch>` for Check, Build, and Test, and `rustup-lint-1.100.0-<os>-<arch>` for Check's fmt and Clippy ratchets. Both cache only `~/.rustup/toolchains/1.100.0-*` and its update hash, not the full rustup home. The old full cache was about 390 MB per job; the slim caches are about 190 MB for the minimal toolchain and 199 MB for the lint toolchain. Test saves the minimal cache and Check saves the lint cache on `main` or an owner-dispatched run; Build and the other jobs only restore. A warm Test job restored the slim toolchain in 2 to 5 seconds on the measured fast runs, against 9 seconds for the old 390 MB archive. Cargo restore variance and hosted-runner allocation still dominate the total, so the total p50 has not yet been proven below 60 seconds.

## Costs and owner actions

| Item | Status | Owner action |
|---|---|---|
| GitHub Actions minutes | Public repository, standard runners are free | No paid capacity is approved |
| Monthly minute cap | Not applicable to free standard runners | Revisit only if a paid runner is approved |
| Branch protection | Configured 2026-11-23 on `main` | Require the `Verify` status check, require an up-to-date branch, block force pushes and deletions, and keep admin enforcement off so the documented `ci-repair` override remains possible. GitHub rejects user push restrictions on user-owned repositories; direct owner pushes remain a documented residual risk |
| Official merge queue | Blocked | A public repository owned by an organization is eligible; private organization repositories additionally require GitHub Enterprise Cloud. This user-owned repository must move under an organization first |
| Paid runner | Not approved | Revisit only after Rust p50 and p95 both miss across at least 100 runs or four weeks |
| `ci-repair` merge override | Owner only | Documented in the trust model; every use is quoted in its PR |

Merge-candidate proof without a merge queue: when branch protection requires branches to be up to date, the PR's tree is the tree that lands, so the full-suite `verify` run on that head is a proof of the merge candidate. The `push` run on `main` re-runs every gate on the merged commit and is the final record. A PR that bypasses "up to date" invalidates the proof, which is why that setting is part of the branch-protection owner action.

## Open decisions

- **The Debt ratchet merge into Check landed.** The separate job was the critical path at 48 seconds on run 45599904468; after the merge, Check carried the ratchets at 38 seconds and the whole run was 60 seconds. The latest main pushes on 2026-11-25 completed in 56 and 64 seconds, so the one-minute target is met on some runs but is not yet proven at p50.
- **The Test job remains the critical path.** Its 2026-11-25 samples are 44 to 57 seconds, with 23 to 29 seconds of setup and cache restore before the test command. The next measured lever is a trustworthy two-shard partition; count-based nextest shards remain rejected because their recorded per-test times did not survive parallel scheduling.

| Decision | Current state |
|---|---|
| Runner size | `ubuntu-24.04` hosted. No paid runner is approved; the free ARM runner was rejected for removing pre-merge x64 runtime coverage |
| Test runner | `cargo test`; nextest count shards were rejected as unbalanced |
| Merge queue | Blocked on repository ownership or plan |
| Formal gate in CI | Local and recorded until the jar and JDK are pinned as artifacts |
| Monthly cap | Not applicable unless a paid runner is approved |
