#!/usr/bin/env bash
set -euo pipefail

# CI restores a source-independent target cache, so incremental artifacts add
# compile and restore work without surviving a source change.
export CARGO_INCREMENTAL=0

if [[ $# -lt 2 ]]; then
  echo "usage: ci/budget.sh <seconds> <command...>" >&2
  exit 2
fi

seconds=$1
shift

if command -v timeout >/dev/null 2>&1; then
  exec timeout --signal=TERM --kill-after=10s "${seconds}s" "$@"
fi

# macOS does not ship GNU timeout. The Perl fallback still terminates the child.
exec perl -e '
use strict;
use warnings;
my $seconds = shift @ARGV;
my $pid = fork();
die "fork failed: $!" unless defined $pid;
if ($pid == 0) {
    exec @ARGV;
    exit 127;
}
my $timed_out = 0;
local $SIG{ALRM} = sub {
    $timed_out = 1;
    kill "TERM", $pid;
    sleep 1;
    kill "KILL", $pid;
};
alarm $seconds;
waitpid($pid, 0);
alarm 0;
exit 124 if $timed_out;
my $status = $?;
exit($status >> 8) if ($status & 127) == 0;
exit(128 + ($status & 127));
' "$seconds" "$@"
