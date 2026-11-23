# Baton CI verification map

| Feature | Drive | Proof |
|---|---|---|
| Path classifier | `ci/selftest.sh` path fixtures | Exact seven-line output for code, docs, meta, unknown, empty, and rename-to-docs |
| Fast path | `ci/check-ci.sh --bootstrap .github/workflows/ci.yml` | `changes` contains only the shallow fetch and `classify-paths.sh`; no checkout or guard |
| Trust gate | `ci/selftest.sh` and `ci/check-ci.sh --bootstrap .github/workflows/ci.yml` | `selection` owns gate integrity, full self-test, classifier recomputation, and the workflow guard |
| Stale-base merge ref | `ci/selftest.sh` temp-repository fixture | A base-side file appears in `base..head` but not in `base..merge` |
| Repair override | `ci/selftest.sh` allow-change fixtures | `--allow-change` still rejects a missing required command (`CI014`) and an unbudgeted command (`CI017`) |
| Debt ratchets | `ci/debt-selftest.sh` | A format diff and a Clippy lint over a zero baseline both fail |
| Workflow guard | `ci/check-ci.sh --bootstrap ci/fixtures/check-ci/valid.yml` | Valid fixture passes; every invalid fixture fails its named rule |
| Command identity | `ci/check-ci.sh --base valid.yml changed-test-command.yml` | Changed test command fails without `--allow-change` and passes with it |
| Budget wrapper | `ci/budget.sh 1 sleep 2` | Exit 124 on overrun; `ci/budget.sh 5 true` exits 0 |
| Format ratchet | `ci/fmt-debt.sh . ci/fmt-baseline.txt` | Count is at or below the stored baseline |
| Clippy ratchet | `ci/clippy-debt.sh . ci/clippy-baseline.txt` | Count is at or below the stored baseline |
