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

Phase 1 does not claim the 60/120 target. It claims a hard ceiling: every job has `timeout-minutes`, every command in `check`, `build`, `test`, and `debt` runs under `ci/budget.sh`, and a budget miss exits 124 and fails the job. `verify` is a pure aggregator with no external command, and `--version` runs only after a build that is already budgeted. Phase 1 budgets are provisional because no Actions run has executed a step yet: run 44969473815 scheduled every job and stopped on an account-billing error. They must shrink in Phase 2.

Budget rule, applied at every phase: a budget starts at three times the measured cold p95 on the target runner, and may only shrink. Widening a budget needs a recorded regression explanation in `ci-acceptance.md`, not a code change.

## Current baseline

Measured on 2026-11-21 after PR #86 and PR #89 merged. The Baton baseline is `origin/main` `3b86d3c`.

| Measurement | `origin/main` `3b86d3c` |
|---|---|
| GitHub Actions workflows | None (Phase 1C adds the first one) |
| GitHub Actions run history | One run, 44969473815, which scheduled every job and executed no step because of account billing |
| Discovered test count (`ci/test-inventory.sh`) | 358 |
| `cargo fmt --check` `Diff in` lines | 890 |
| Clippy `error:` lines with `-D warnings` | 61 |
| Cold `cargo build --locked` on the developer macOS host | 29.25 seconds at `3e2d578`; not remeasured since |
| Warm `cargo build` | 0.14 seconds |
| Warm `cargo check --tests` | 0.93 seconds |
| `cargo test --lib` | 48 to 62 seconds across runs |
| `cargo nextest run`, full suite | 76.13 seconds, 350 tests at `3e2d578` |
| Slowest recorded nextest test | `orchestrate::tests::ships_completed_worker_branches_idempotently`, 62.61 seconds under contention |

`ci/fmt-baseline.txt`, `ci/clippy-baseline.txt`, and `ci/test-inventory-floor.txt` carry the same three numbers, and the workflow fails if a PR raises a baseline or lowers the floor.

These are local macOS measurements, not GitHub Actions measurements. Phase 2 records Linux x64 queue, setup, compile, test, and verify times from real runs. No shard budget is accepted before that data exists.

The formatting and Clippy counts are baseline debt, 890 format diffs and 61 Clippy errors at `3b86d3c`. Phase 1 adds ratchets that reject growth and reject the suppression that would fake a smaller count. Full `format` and `lint` gates arrive only after the debt is cleared.

## What the gate enforces

Every check below is a real command in `ci/`, and `ci/selftest.sh` proves at least one failing fixture per rule.

| Area | File | Enforcement |
|---|---|---|
| Budgets | `ci/budget.sh` | `timeout` with a Perl fallback; a miss exits 124 and fails the job |
| Path selection | `ci/classify-paths.sh` | `--event`, `--files`, first-match precedence `unknown` > `code` > `meta` > docs |
| Workflow invariants | `ci/check-ci.sh` | Rules CI001 to CI015 and CI017 (CI016 is unused) |
| Gate integrity | `ci/gate-integrity.sh` | Rules GI001 to GI006 |
| Test inventory | `ci/test-inventory.sh` | Rules TI001 to TI005 against `ci/test-inventory-floor.txt` and the captured test log |
| Format debt | `ci/fmt-debt.sh` | Count of `Diff in ` lines must not exceed the baseline |
| Clippy debt | `ci/clippy-debt.sh` | Count of `error:` lines must not exceed the baseline |
| Self-test | `ci/selftest.sh` | Runs every fixture, the budget negative test, and the inventory floor cases |

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
| GI002 | `.cargo/config.toml` that replaces the compiler, runner, or registry |
| GI003 | `[lints]` in `Cargo.toml` |
| GI004 | A blanket `allow(clippy::all)` style attribute in `src/` or `tests/` |

## Trust model

The base supplies every pass/fail script on every event. The head supplies only the measured source.

- `pull_request` uses `github.event.pull_request.base.sha`.
- `merge_group` uses `github.event.merge_group.base_sha`.
- `push` uses `github.event.before`, falling back to `HEAD~1` for a first push or a zero SHA.
- `workflow_dispatch` uses `github.event.before`, falling back to `HEAD~1`.

The base copy is sparse-checked out to `base/` with `/ci/`, `/.github/workflows/`, and `/rust-toolchain.toml`. Each job then copies `base/ci` to `$RUNNER_TEMP/trusted-ci`, marks it read-only, records a SHA-256 manifest, and executes only from that copy. The `test` job re-verifies the manifest after `cargo test` and fails if any tool changed.

That staging is a detection boundary, not a sandbox. `cargo test` runs arbitrary repository code with write access to the runner's filesystem, so a determined change can still tamper with the process. What staging buys is that tampering after the first gate step is visible as a manifest mismatch instead of silently changing the harness. A real sandbox needs a separate container or a read-only mount, which is a Phase 3 decision.

Head-controlled process configuration is rejected rather than compared. A workflow or job `env` block can shadow `cargo` on `PATH`. A job-level `permissions` block can grant write access while the top-level block stays read. A job-level `container`, `services`, or reusable workflow moves the gate somewhere the guard cannot see. CI015 and CI006 reject all of these outright, and `persist-credentials: false` keeps the checkout token out of the git config.

`rust-toolchain.toml` is compared byte for byte with the base copy in the `selection` job. There is no `RUSTUP_TOOLCHAIN` environment override anywhere in the workflow.

On `push`, `merge_group`, and `workflow_dispatch` the guard runs with `--allow-change`, because a merged CI change must be able to run on `main`; the PR run is where command drift is rejected.

Changing a gate command is not a PR-level decision. There is no `ci-change` label. The workflow passes `--allow-change` only on `push`, `merge_group`, and `workflow_dispatch`, where the commit is already on a protected branch; a `pull_request` run never does. A PR that changes a command, a `needs` edge, a gate `if`, a timeout, or a runner fails `verify` against the base workflow, by design. Step-level `working-directory`, `shell`, and every `continue-on-error` other than literal `false` are rejected too, because any of them can redirect a trusted tool or swallow a failure.

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
| 2 | Record real timings and choose the test runner | Create `ci/record-timings.sh`, `ci/test-timings.json`; add one non-gating `timings` job to the workflow; modify `docs/ci-acceptance.md` | At least 10 PR runs and 5 `main` pushes; nextest and `cargo test` compared on the same runner; Phase 1 budgets replaced with measured p95 plus 20 percent |
| 3 | Diff-aware selection and recorded-time shards | Create `ci/select-tests.sh`, `ci/plan-shards.sh`, `ci/test-groups.json`; modify `.github/workflows/ci.yml`, `ci/check-ci.sh`, `docs/ci.md` | Selector fixtures including transitive and `lib.rs` cases; shard union equals the discovered inventory; Rust PR feedback p95 under 120 seconds |
| 4 | Full format and lint plus local hooks | Create `.githooks/pre-commit`, `.githooks/pre-push`, `ci/install-hooks.sh`; modify `.github/workflows/ci.yml`, `docs/ci.md` | Format and Clippy baselines are zero and both are required; hooks run under budget |
| 5 | Last-20-run audit and runner decision | Modify `docs/ci-acceptance.md` | PR p50/p95, queue p50/p95, cache hit rate, monthly minute estimate, and a paid-runner decision, all from recorded runs |

No phase may remove a passing check, widen a budget to hide a regression, or add a retry that changes a failing result into a passing one. Each phase is one small PR. Phase 1B depends on PR #86 merging first, because the debt baselines are measured from the merged `main`. Phase 1C depends on 1B, because the workflow invokes `base/ci/`.

## Phase 1 workflow contract

Triggers: `pull_request` (`opened`, `synchronize`, `reopened`, `ready_for_review`), `push` to `main`, `merge_group`, and `workflow_dispatch`. Top-level permissions are `contents: read`. Concurrency keys on `github.event.pull_request.number || github.sha` and cancels only pull-request runs.

| Job | Needs | Timeout | Runs |
|---|---|---|---|
| `changes` | none | 5 | Classifier, tool self-test, workflow guard, gate integrity |
| `selection` | `changes` | 5 | Recomputes the classifier and compares outputs, compares `rust-toolchain.toml`, re-runs the workflow guard |
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

Shards are filled from recorded per-test times, never from a guess.

- Record per-test durations with nextest's machine output during Phase 2.
- Balance on p95 and maximum, not p50. The slowest recorded test is 62.61 seconds under contention, so a p50 balance would put it in a shard with other long tests.
- Give the slowest test its own group until it is fixed or split.
- The catch-all group has its own budget. If it overflows, the job fails instead of stretching the budget.
- The shard count is the smallest count that meets the 90-second gate ceiling on the target runner.
- The union check from the selection section also applies to shards.

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

Phase 1 values are three times a conservative local cold estimate. They are caps, not targets: a command that exceeds one fails the job. Phase 2 replaces each value with the measured Linux p95 plus 20 percent, and no value may increase after that.

## Debt ratchets

A count is a floor, not a quality gate: fixing one violation while adding another keeps the count flat. Phase 1 accepts that weakness because the baseline is 884 format diffs and 60 Clippy errors, and a full gate would fail every PR on day one.

Compensating controls, shipped in Phase 1:

- `ci/gate-integrity.sh` rejects the configuration changes that would fake a smaller count: `rustfmt.toml`, `clippy.toml`, `.cargo/config.toml` compiler or runner replacement, `[lints]` in `Cargo.toml`, and blanket `allow(clippy::all)`.
- The ratchet scripts fail when the command produces no recognizable diagnostics at all, so a broken tool cannot report zero.
- The baseline files may only be lowered. Lowering one is part of the PR that fixes the debt.

Phase 4 replaces counts with a fingerprint set: the current diagnostics must be a subset of the recorded baseline, and any new fingerprint fails. Phase 4 also has an owner and a target date, recorded in `docs/ci-acceptance.md`, so the ratchet cannot become a permanent exemption.

## Measurement and acceptance

The acceptance record is [ci-acceptance.md](ci-acceptance.md). It starts with `No recorded runs yet` and is only updated from real runs.

- PR feedback starts at run `createdAt` and ends when `verify` completes.
- Queue time is `startedAt - createdAt`.
- Report p50 and maximum until at least 100 runs or four weeks exist. A p95 from 10 to 20 samples is a maximum in disguise, so the Phase 3 acceptance requires the shard gate ceiling instead of a p95.
- Report docs-only and Rust PRs separately. A docs-only median that skips every gate must not stand in for Rust feedback.
- Record cancelled, failed, and superseded runs. Removing a slow run is a weakening.
- Record the exact commit, workflow revision, event, selected gates, cache key, slowest job, failure step, and budget miss.

## Caching

`Swatinem/rust-cache` uses `shared-key: baton-<os>-<arch>` and a `key` of the `Cargo.lock` and `rust-toolchain.toml` hashes. Saves are restricted to `main`, so a PR cannot poison the cache other PRs read.

A cache miss changes the time, never the checks: the cold path runs the same commands under the same budgets. If a cold run regularly exceeds a Phase 1 budget, the fix is to correct the budget from measured data in Phase 2, not to skip the gate.

Four jobs race to save the same key on a main run. That is acceptable because the key is content-derived and a lost race only costs the next run time. Phase 2 records which job wins so the save can be restricted to the cheapest producer.

## Costs and owner actions

| Item | Status | Owner action |
|---|---|---|
| GitHub Actions minutes for a private repository | Unknown quota | Confirm the included minutes and per-minute rate, then record them in `docs/ci-acceptance.md` before Phase 1C is merged to `main` |
| Monthly minute cap | Not set | Set one numeric cap and a pause threshold; the cap is the number in the acceptance record, not a description |
| Branch protection | Not configured | Require `verify`, block direct pushes to `main`, and record the ruleset proof |
| Official merge queue | Blocked | Requires an organization on GitHub Enterprise Cloud; until then, a merge candidate is proved by the PR `verify` run on an up-to-date branch plus the `push` run on `main` |
| Paid runner | Not evaluated | Approved only after hosted p50 or p95 misses both targets with recorded data |
| `ci-repair` merge override | Owner only | Documented in the trust model; every use is quoted in its PR |

Merge-candidate proof without a merge queue: when branch protection requires branches to be up to date, the PR's tree is the tree that lands, so the full-suite `verify` run on that head is a proof of the merge candidate. The `push` run on `main` re-runs every gate on the merged commit and is the final record. A PR that bypasses "up to date" invalidates the proof, which is why that setting is part of the branch-protection owner action.

## Open decisions

| Decision | Current state |
|---|---|
| Runner size | `ubuntu-24.04` hosted. Paid runners need measured evidence |
| Test runner | `cargo test` in Phase 1; nextest decided in Phase 2 from recorded timings |
| Merge queue | Blocked on repository ownership or plan |
| Formal gate in CI | Local and recorded until the jar and JDK are pinned as artifacts |
| Monthly cap | Unset |
