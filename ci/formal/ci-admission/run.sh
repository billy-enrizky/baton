#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
JAR=${TLA2TOOLS_JAR:-${TMPDIR:-/tmp}/baton-tla2tools.jar}
URL=https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar

if [[ ! -f "$JAR" ]]; then
  curl -L --fail --silent --show-error -o "$JAR" "$URL"
fi

JAVA_BIN=${JAVA_HOME:+$JAVA_HOME/bin/java}
if [[ -z "$JAVA_BIN" || ! -x "$JAVA_BIN" ]]; then
  JAVA_BIN=$(command -v java || true)
fi
if [[ -z "$JAVA_BIN" ]] || ! "$JAVA_BIN" -version >/dev/null 2>&1; then
  echo "a working JDK is required; set JAVA_HOME or install java" >&2
  exit 1
fi

run_tlc() {
  "$JAVA_BIN" -XX:+UseParallelGC -cp "$JAR" tlc2.TLC \
    -workers 1 -nowarning -metadir "$ROOT/.tlc/$1" -config "$ROOT/$2" "$ROOT/BatonCiAdmission.tla"
}

printf '\n== CI admission ==\n'
run_tlc real BatonCiAdmission.cfg

check_bug() {
  config=$1
  invariant=$2
  output="$ROOT/.tlc/${config%.cfg}.out"
  mkdir -p "$ROOT/.tlc"
  printf '\n== %s (must fail) ==\n' "$config"
  if run_tlc "${config%.cfg}" "$config" >"$output" 2>&1; then
    echo "$config unexpectedly passed" >&2
    exit 1
  fi
  if ! grep -q "Invariant $invariant is violated" "$output"; then
    echo "$config failed for the wrong reason; see $output" >&2
    exit 1
  fi
  printf 'caught %s\n' "$invariant"
}

check_bug BatonCiAdmissionSkipSelected.cfg NoFalseGreen
check_bug BatonCiAdmissionMergeSelection.cfg FullSelection
check_bug BatonCiAdmissionRetryMask.cfg NoRetryMasking
check_bug BatonCiAdmissionUnknownSelection.cfg ChangesFailOpen
check_bug BatonCiAdmissionCommandDrift.cfg CommandsMatch
check_bug BatonCiAdmissionReportWithoutStart.cfg NoFalseGreen

printf '\nall CI admission checks passed\n'
