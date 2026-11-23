#!/usr/bin/env bash
set -euo pipefail

evidence_root=local_docs/verification/evidence
while [[ $# -gt 0 ]]; do
  case "$1" in
    --evidence-dir)
      evidence_root=${2:?--evidence-dir requires a path}
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

root=$(cd -- "$(dirname -- "$0")/../../../.." && pwd)
cd "$root"
stamp=$(date +%s)
evidence="$evidence_root/ci-probe-$stamp"
mkdir -p "$evidence"

bash ci/selftest.sh > "$evidence/selftest.log" 2>&1
bash ci/debt-selftest.sh > "$evidence/debt-selftest.log" 2>&1
bash ci/check-ci.sh --bootstrap ci/fixtures/check-ci/valid.yml > "$evidence/guard-valid.log" 2>&1
bash ci/check-ci.sh --bootstrap .github/workflows/ci.yml > "$evidence/guard-workflow.log" 2>&1
if bash ci/check-ci.sh --base ci/fixtures/check-ci/valid.yml ci/fixtures/check-ci/changed-test-command.yml > "$evidence/guard-change-rejected.log" 2>&1; then
  echo "changed command was not rejected" >&2
  exit 1
fi
bash ci/check-ci.sh --base ci/fixtures/check-ci/valid.yml ci/fixtures/check-ci/changed-test-command.yml --allow-change > "$evidence/guard-change-allowed.log" 2>&1

set +e
bash ci/budget.sh 1 sleep 2 > "$evidence/budget-overrun.log" 2>&1
code=$?
set -e
if [[ $code -ne 124 ]]; then
  echo "budget overrun returned $code instead of 124" >&2
  exit 1
fi
bash ci/budget.sh 5 true > "$evidence/budget-success.log" 2>&1

cat > "$evidence/summary.json" <<JSON
{
  "passed": true,
  "evidence": "$evidence",
  "selftest": "passed",
  "debt_selftest": "passed",
  "workflow_guard": "passed",
  "real_workflow": "passed",
  "command_change": "rejected-without-override",
  "budget_overrun_exit": 124
}
JSON
cat "$evidence/summary.json"
