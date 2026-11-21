# Baton CI verification map

| Feature | Drive | Proof |
|---|---|---|
| Path classifier | `ci/selftest.sh` path fixtures | Exact seven-line output for code, docs, meta, unknown, empty, and rename-to-docs |
| Workflow guard | `ci/check-ci.sh --bootstrap ci/fixtures/check-ci/valid.yml` | Valid fixture passes; every invalid fixture fails its named rule |
| Command identity | `ci/check-ci.sh --base valid.yml changed-test-command.yml` | Changed test command fails without `--allow-change` and passes with it |
| Budget wrapper | `ci/budget.sh 1 sleep 2` | Exit 124 on overrun; `ci/budget.sh 5 true` exits 0 |
| Format ratchet | `ci/fmt-debt.sh . ci/fmt-baseline.txt` | Count is at or below the stored baseline |
| Clippy ratchet | `ci/clippy-debt.sh . ci/clippy-baseline.txt` | Count is at or below the stored baseline |
