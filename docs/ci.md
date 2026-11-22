# Baton CI/CD design

This document defines the GitHub Actions system for Baton, the measurements that justify it, and the acceptance record for rollout. The parity scope is in [parity.md](parity.md). The recorded measurement rules are in [ci-acceptance.md](ci-acceptance.md).

The design is staged. Phase 1 creates one honest full-suite gate with hard budgets and real CI data. Sharding and diff-aware selection arrive only after that data exists, because a shard plan built from macOS timings would be a guess. Formatting and Clippy ratchets prevent new debt while the full lint baseline is cleaned.

## Goals

| Metric | Target | Gated from |
|---|---|---|
| PR feedback p50, Rust PR | Under 60 seconds | Phase 3 |
| PR feedback p95, Rust PR | Under 120 seconds | Phase 3 |
| PR feedback p50, docs-only PR | Under 45 seconds | Phase 3 |
| Any single gate command's budget | Under 90 seconds | Phase 3 |
| Queue p50 | Under 10 seconds | Phase 2 |
| Queue p95 | Under 20 seconds | Phase 5, after 100 runs exist |
| Setup p95 | Under 15 seconds | Phase 2 |
| Hidden retries | Zero | Now |
| Skipped required checks | Zero | Now |
| Failed required check reported as success | Zero | Now |

Phase 1 shipped on 2026-11-22. Every job has `timeout-minutes`, every command in `check`, `build`, `test`, and `debt` runs under `ci/budget.sh`, and a budget miss exits 124 and fails the job. `verify` is a pure aggregator with no external command, and `--version` runs only after a build that is already budgeted.

Phase 2 is in progress from recorded runs. The current warm Rust PR samples are 52, 58, 60, 61, 63, 65, 68, and 70 seconds; the CI-docs PR is 59 seconds. The Rust median is 62.0 seconds, above the target. A free ARM runner was measured and rejected because it would remove x64 runtime tests from the pre-merge gate. The x64 Test job stays in place. All p50 and p95 rows remain provisional until 100 runs or four weeks exist. The acceptance record names the runs, the job breakdown, and every rejected experiment.

Budget rule, applied at every phase: Phase 1 uses three times a conservative cold estimate. Phase 2 sets each budget to the measured Linux p95 plus 20 percent. That reset is the only allowed increase. After it, a budget may only shrink. Widening a budget needs a recorded regression explanation in `ci-acceptance.md`, not a code change.

## Current baseline

Measured on 2026-11-22 from the cited runs. The Rust samples were based on `be31fc5` and the inbox slices; later slices added tests, so the executed count row is refreshed with each slice while the timing samples stay historical.

| Measurement | Recorded value | Evidence |
|---|---|---|
| Warm Rust PR feedback | 52, 58, 60, 61, 63, 65, 68, and 70 seconds; n=8, median 62.0 seconds, max 70 seconds | Runs 45035044248, 45004705055, 45009466461, 45021604178, 45011836996, 45010710576, 45037001045, and 45046974039 |
| Warm meta PR feedback, descriptive | 59 seconds, n=1 | Run 45005544007 |
| Warm Test job | 33 to 49 seconds; n=8 | Runs 45035044248, 45004705055, 45009466461, 45021604178, 45011836996, 45010710576, 45037001045, and 45046974039 |
| `cargo test --lib` | 1.67 to 3.70 seconds, 331 tests at the cited runs | Runs 45009466461 and 45011836996; the suite now has 357 lib tests |
| Integration tests | 0.40 to 0.75 seconds, 75 tests across seven binaries at the cited runs | Runs 45009466461 and 45011836996; the suite now has 90 integration tests across eight binaries, plus one Linux-only non-UTF-8 case |
| Executed test count (`ci/test-inventory.sh`) | 447 | Current local suite |
| `ci/test-inventory-floor.txt` | 447 | Current floor and executed count |
| `ci/fmt-baseline.txt` | 550 | Current debt ceiling |
| `ci/clippy-baseline.txt` | 61 | Current debt ceiling |
| Cold Rust PR | 189 seconds | Run 44982816103 |

The workflow fails if a PR raises a baseline or lowers the test inventory floor. The 447 floor is a lower bound, not a per-test lock. The run-mode check now sums every `running N tests` line from the captured log and requires `passed + ignored` to equal that total. It also requires the executed count to meet the floor and rejects ignored tests. Deleting tests above the floor still needs review in the diff.

Older local macOS measurements are historical and are not acceptance evidence. The acceptance record contains the Linux run-level data and the rejected experiments.

## Runner experiment result

The free `ubuntu-24.04-arm` runner is not adopted for Test. A temporary ARM job ran all 406 tests in 85 seconds cold and 33 seconds warm, with Test steps of 56 and 22 seconds. The 406-test x64 sample is [run 45011836996](https://github.com/billy-enrizky/baton/actions/runs/45011836996): a 47-second job with a 16-second step and a partial Cargo cache because it added `clap_complete`. The ARM run restored a full cache. The jobs differ in cache state and executed steps, so the timings are descriptive and not matched. No ARM performance advantage is claimed.

Replacing the x64 Test job would move Linux x64 runtime tests after merge, and official merge queue is unavailable for this repository. A runtime-only x64 regression could then land without a pre-merge test. That violates the rule that no phase removes a passing check. Test stays on `ubuntu-24.04`. The option is closed unless a future proposal is additive, not a replacement, keeps an independent required x64 pre-merge Test job, and provides a matched rerun with identical steps, event shape, cache policy, and command under the 100-run or four-week policy. A merge queue alone is insufficient if it runs the same ARM job.

The experiment is recorded in [ci-acceptance.md](ci-acceptance.md#rejected-arm-runner-experiment) with its runs, timings, cache sizes, and CI013 failure. The p50 miss remains at 61.0 seconds; the next levers are measured critical-path work, not architecture substitution.

## What the gate enforces

Every check below is a real command in `ci/`, and `ci/selftest.sh` proves at least one failing fixture per rule.

| Area | File | Enforcement |
|---|---|---|
| Budgets | `ci/budget.sh` | `timeout` with a Perl fallback; a miss exits 124 and fails the job |
| Path selection | `ci/classify-paths.sh` | `--event`, `--files`, first-match precedence `unknown` > `code` > `meta` > docs |
| Workflow invariants | `ci/check-ci.sh` | Rules CI001 to CI015 and CI017 (CI016 is unused) |
| Gate integrity | `ci/gate-integrity.sh` | Rules GI001 to GI006 |
| Test inventory | `ci/test-inventory.sh` | Rules TI001 to TI005. Run mode compares announced and executed totals; list mode counts discovered tests |
| Format debt | `ci/fmt-debt.sh` | Count of `Diff in ` lines must not exceed the baseline |
| Clippy debt | `ci/clippy-debt.sh` | Count of `error:` lines must not exceed the baseline |
| Self-test | `ci/selftest.sh` | Runs every fixture, the budget negative test, and the inventory floor, short-execution, and missing-total cases |

Workflow invariants:

| Rule | Rejects |
|---|---|
| CI001 | Missing trigger, or a `push` trigger that does not target `main` |
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

Gate-integrity rules:

| Rule | Rejects |
|---|---|
| GI001 | `rustfmt.toml` or `clippy.toml` in any spelling |
| GI002 | Any `.cargo/config.toml`, `.cargo/config`, or `.cargo/config.toml.example` |
| GI003 | `[lints]` in `Cargo.toml` |
| GI004 | A blanket all, pedantic, nursery, or correctness Clippy allow in `src/` or `tests/` |
| GI005 | A warning suppression or conditional Clippy suppression in tracked Rust sources |
| GI006 | A `#[rustfmt::skip]` attribute in tracked Rust sources |

## Trust model

The base supplies every pass/fail script on every event. The head supplies only the measured source.

- `pull_request` uses `github.event.pull_request.base.sha`.
- `merge_group` uses `github.event.merge_group.base_sha`.
- `push` uses `github.event.before`, falling back to `HEAD~1` for a first push or a zero SHA.
- `workflow_dispatch` uses `github.event.before`, falling back to `HEAD~1`.

The base copy is sparse-checked out to `base/` with `/ci/`, `/.github/workflows/`, and `/rust-toolchain.toml`. Each job then copies `base/ci` to `$RUNNER_TEMP/trusted-ci`, marks it read-only, records a SHA-256 manifest, and executes only from that copy. The `test` job reads the inventory floor and timing script from `$RUNNER_TEMP/trusted-ci`, treats the captured test log as untrusted input, and re-verifies the manifest after the test gates.

That staging is a detection boundary, not a sandbox. `cargo test` runs arbitrary repository code with write access to the runner's filesystem, so a determined change can still tamper with the process. What staging buys is that tampering after the first gate step is visible as a manifest mismatch instead of silently changing the harness. A real sandbox needs a separate container or a read-only mount, which is a Phase 3 decision.

Head-controlled process configuration is rejected rather than compared. A workflow or job `env` block can shadow `cargo` on `PATH`. A job-level `permissions` block can grant write access while the top-level block stays read. A job-level `container`, `services`, or reusable workflow moves the gate somewhere the guard cannot see. CI015 and CI006 reject all of these outright, and `persist-credentials: false` keeps the checkout token out of the git config.

`rust-toolchain.toml` is compared byte for byte with the base copy in the `selection` job. There is no `RUSTUP_TOOLCHAIN` environment override anywhere in the workflow.

On `push`, `merge_group`, and `workflow_dispatch` the guard runs with `--allow-change`, because a merged CI change must be able to run outside a pull request. Job-list equality is still enforced, and `workflow_dispatch` can target a feature branch, so it is owner-controlled rather than a substitute for the PR gate.

Changing a gate command is not a PR-level decision. There is no `ci-change` label. The workflow passes `--allow-change` on `push`, `merge_group`, and `workflow_dispatch`. Push and merge-group commits are protected; `workflow_dispatch` is owner-controlled and can target a feature branch. A job-list change is rejected on every event. A PR that changes a command, `needs`, `if`, timeout, runner, output, or strategy fails `verify` against the base workflow, by design. Step-level `working-directory`, `shell`, and every `continue-on-error` other than literal `false` are rejected too, because any of them can redirect a trusted tool or swallow a failure.

The single escape hatch is `ci-repair`, and it is a human action rather than a workflow branch:

1. The repository owner opens a PR that touches only `ci/**`, `.github/**`, or `docs/ci*.md`.
2. The PR is expected to fail `verify` at the base comparison. That failure is the record of the change.
3. Before merging, the owner runs `ci/selftest.sh`, `ci/check-ci.sh --base <base workflow> <head workflow> --allow-change`, `actionlint`, and a full local `cargo test --locked` on the same commit.
4. The owner merges with an admin override, quoting the local results in the PR.
5. The `push` run on `main` is the proof: it runs the new tools against the full suite with all gates selected.

This is visible in the PR, the merge API, and the next main run. It is not silent, and it cannot be triggered by an outside contributor.

## Rollout sequence

| Phase | Change | Files | Acceptance |
|---|---|---|---|
| 1A | Design and acceptance record | Create `docs/parity.md`, `docs/ci.md`, `docs/ci-acceptance.md`, `scripts/check_parity_paths.py`, `scripts/check_doc_links.py`; modify `README.md` | Both checkers print `0 missing`; the EGM review record has two consecutive clean rounds |
| 1B | Trusted helpers, skills, and formal model | Create `ci/*.sh`, `ci/fixtures/**`, `ci/fmt-baseline.txt`, `ci/clippy-baseline.txt`, `ci/test-inventory-floor.txt`, `ci/formal/ci-admission/**`, `rust-toolchain.toml`, `.agents/skills/verify-baton-ci/**`, `.agents/skills/verify-baton-ci-formal/**` | `ci/selftest.sh` passes; a budget overrun exits 124; the formal gate passes locally; `cargo check`, `cargo build`, `cargo test`, and `ci/test-inventory.sh` pass |
| 1C | Full-suite workflow | Create `.github/workflows/ci.yml`; modify `docs/ci.md` | Done: run 44982816103 executed every job with `verify` green, and run 44983114852 is the first green `main` push |
| 2 | Record real timings and choose the test runner | `ci/test-timings.sh` records per-binary times; `docs/ci-acceptance.md` records the warm path and rejected runners | In progress: `cargo test` and the x64 runner retained; nextest count shards and the free ARM runner were rejected; the p50 miss remains |
| 3 | Diff-aware selection and recorded-time shards | Create `ci/select-tests.sh`, `ci/plan-shards.sh`, `ci/test-groups.json`; modify `.github/workflows/ci.yml`, `ci/check-ci.sh`, `docs/ci.md` | Not started: selector fixtures and shard union proof still required |
| 4 | Full format and lint plus local hooks | Create `.githooks/pre-commit`, `.githooks/pre-push`, `ci/install-hooks.sh`; modify `.github/workflows/ci.yml`, `docs/ci.md` | Format and Clippy baselines are zero and both are required; hooks run under budget |
| 5 | Last-100-run audit and runner decision | Modify `docs/ci-acceptance.md` | PR p50/p95, queue p50/p95, cache hit rate, and a paid-runner decision, all from recorded runs |

No phase may remove a passing check, widen a budget to hide a regression, or add a retry that changes a failing result into a passing one. Each phase is one small PR. Phase 1B depends on PR #86 merging first, because the debt baselines are measured from the merged `main`. Phase 1C depends on 1B, because the workflow invokes `base/ci/`.

## Phase 1 workflow contract

Triggers: `pull_request` (`opened`, `synchronize`, `reopened`, `ready_for_review`), `push` to `main`, `merge_group`, and `workflow_dispatch`. Top-level permissions are `contents: read`. Concurrency keys on `github.event.pull_request.number || github.sha` and cancels only pull-request runs.

| Job | Needs | Timeout | Runs |
|---|---|---|---|
| `changes` | none | 5 | Classifier, quick tool preflight, workflow guard, gate integrity, baselines |
| `selection` | `changes` | 5 | Full tool self-test, recomputes the classifier and compares outputs, compares `rust-toolchain.toml`, re-runs the workflow guard |
| `check` | `changes` | 10 | `cargo check --locked --all-targets` under a 120-second budget |
| `build` | `changes` | 10 | `cargo build --locked --bin baton` under a 150-second budget, then `--version` |
| `test` | `changes` | 15 | `cargo test --locked` under a 300-second budget, then `ci/test-inventory.sh` |
| `debt` | `changes` | 15 | Format ratchet under 60 seconds, Clippy ratchet under 300 seconds |
| `verify` | all six | 2 | The only required branch-protection check |

`changes` outputs: `check_needed`, `build_needed`, `test_needed`, `debt_needed`, `full_suite_reason`, `has_tests`, `docs_only`. `full_suite_reason` records the selection class (`code`, `meta`, `unknown`, `empty`, `docs`, or the event name for non-PR events); it is not a claim that the full suite ran.

`verify` fails when:

- `changes` or `selection` did not succeed.
- A selected gate did not succeed, or an unselected gate is not `skipped`.
- `test_needed` is true and `has_tests` is not true.
- `test_needed` is true and the discovered test count is missing, not numeric, or zero.
- A non-PR event did not select all four gates, or reports `docs_only`.

`verify` does not use `paths-ignore`, does not use `continue-on-error`, and has no `if` that can skip it.

### Path classification

| Changed path class | check | build | test | debt | docs_only | has_tests | full_suite_reason |
|---|---|---|---|---|---|---|---|
| Any `*.rs`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `build.rs` | true | true | true | true | false | true | `code` |
| `.github/workflows/**`, `ci/**`, `docs/ci.md`, `docs/parity.md` | true | true | true | true | false | true | `meta` |
| `docs/**`, `*.md`, `LICENSE` only | false | false | false | false | true | false | `docs` |
| `.gitignore` or any other unknown path | true | true | true | true | false | true | `unknown` |
| Empty diff | true | true | true | true | false | true | `empty` |

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

Shards are not active. The current lib binary runs 357 tests and the eight integration binaries run 90 tests. At the cited runs the lib binary ran 326 tests in 1.73 seconds and the seven integration binaries ran 75 tests in 0.39 seconds. Sharding would duplicate the 16-second Test setup and the 13-second compile for no gain.

If a gate ceiling is missed, shards are filled from recorded per-test times, never from a count:

- Record per-test durations before designing a shard plan.
- Balance on maximum and p95, not p50.
- Give a test that is longer than the other shard work its own group until it is fixed or split.
- The catch-all group has its own budget. If it overflows, the job fails instead of stretching the budget.
- The shard count is the smallest count that meets the gate ceiling on the target runner.
- The union check from the selection section also applies to shards.

Count-based nextest shards were rejected in run 45002437721: one shard took 33 seconds and the other 61 seconds. The 61-second shard contained a single 30-second test.

## Formal CI admission model

`ci/formal/ci-admission/BatonCiAdmission.tla` models gate admission, not test correctness. `Gates` holds the six jobs plus the workflow-guard step (`changes`, `selection`, `workflow_guard`, `check`, `build`, `test`, `debt`); `verify` is modeled as the aggregator that reads them, because it never runs a check itself.

The real configuration, `BatonCiAdmission.cfg`, runs 583,175 states and 92,855 distinct states. Six mutation configs must each fail with a named invariant:

| Config | Invariant that must break |
|---|---|
| `BatonCiAdmissionSkipSelected.cfg` | `NoFalseGreen` |
| `BatonCiAdmissionMergeSelection.cfg` | `FullSelection` |
| `BatonCiAdmissionRetryMask.cfg` | `NoRetryMasking` |
| `BatonCiAdmissionUnknownSelection.cfg` | `ChangesFailOpen` |
| `BatonCiAdmissionCommandDrift.cfg` | `CommandsMatch` |
| `BatonCiAdmissionReportWithoutStart.cfg` | `NoFalseGreen` |

What the model proves, with `CommandDriftAllowed = FALSE`: a selected gate cannot be skipped and reported green, an unselected gate cannot report success, merge events cannot use a subset, a retry cannot convert a failure, an unknown selection cannot select nothing, and a command edit cannot pass `CommandsMatch`. The one config that sets `CommandDriftAllowed = TRUE` exists only to show that `CommandsMatch` depends on that constant. The model models the rule, not the shell implementation, so `ci/selftest.sh` remains the executable proof.

What it does not prove: test inventory completeness, shard union coverage, or that the selector's dependency map is correct. Those are covered by the selection union check and its fixtures.

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
| Clippy ratchet | 300 seconds | 180 seconds |

Phase 1 values are three times a conservative local cold estimate. They are caps, not targets: a command that exceeds one fails the job. Phase 2 replaces each value with the measured Linux p95 plus 20 percent. That replacement is the only allowed increase; after it, no value may increase.

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

`Swatinem/rust-cache` uses `shared-key: baton-v2-<os>-<arch>-<job>` and a `key` of the `Cargo.lock` and `rust-toolchain.toml` hashes. The job name is part of the key so a lint job cannot save an archive without the test build. Saves are restricted to `main`, so a PR cannot poison the cache other PRs read.

A cache miss changes the time, never the checks: the cold path runs the same commands under the same budgets. If a cold run regularly exceeds a Phase 1 budget, the fix is to correct the budget from measured data in Phase 2, not to skip the gate.

The pinned Rust toolchain is cached separately per job under `rustup-1.100.0-<os>-<arch>-<job>`. On a warm Test job the Cargo archive restored with `full match: true` and the test build reused its own cache.

## Costs and owner actions

| Item | Status | Owner action |
|---|---|---|
| GitHub Actions minutes | Public repository, standard runners are free | No paid capacity is approved |
| Monthly minute cap | Not applicable to free standard runners | Revisit only if a paid runner is approved |
| Branch protection | Not configured | Require `verify`, block direct pushes to `main`, and record the ruleset proof |
| Official merge queue | Blocked | Requires an organization on GitHub Enterprise Cloud; until then, a merge candidate is proved by the PR `verify` run on an up-to-date branch plus the `push` run on `main` |
| Paid runner | Not approved | Revisit only after Rust p50 and p95 both miss across at least 100 runs or four weeks |
| `ci-repair` merge override | Owner only | Documented in the trust model; every use is quoted in its PR |

Merge-candidate proof without a merge queue: when branch protection requires branches to be up to date, the PR's tree is the tree that lands, so the full-suite `verify` run on that head is a proof of the merge candidate. The `push` run on `main` re-runs every gate on the merged commit and is the final record. A PR that bypasses "up to date" invalidates the proof, which is why that setting is part of the branch-protection owner action.

## Open decisions

| Decision | Current state |
|---|---|
| Runner size | `ubuntu-24.04` hosted. No paid runner is approved; the free ARM runner was rejected for removing pre-merge x64 runtime coverage |
| Test runner | `cargo test`; nextest count shards were rejected as unbalanced |
| Merge queue | Blocked on repository ownership or plan |
| Formal gate in CI | Local and recorded until the jar and JDK are pinned as artifacts |
| Monthly cap | Not applicable unless a paid runner is approved |
