#!/bin/bash
# Regression tests for the guard's retry and notification behavior.
#
#   scripts/test-openlogi-custom.sh
#
# Drives cmd_guard with stubs, in a throwaway HOME. Nothing here touches the
# installed app, launchd, the network or a repository. Runs under /bin/bash, the
# shell launchd starts the guard with.
# shellcheck disable=SC2034,SC2317,SC2329
set -uo pipefail

SCRIPT="$(cd "$(dirname "$0")" && pwd)/openlogi-custom.sh"
SANDBOX=$(mktemp -d)
trap 'rm -rf "$SANDBOX"' EXIT
export HOME="$SANDBOX"
mkdir -p "$HOME/Library/LaunchAgents"
# shellcheck source=/dev/null
source "$SCRIPT"
set +e +u

NOTES="$SANDBOX/notes"
ATTEMPTS="$SANDBOX/attempts"
failures=0

reset() {
  rm -f "$NOTES" "$ATTEMPTS" "$STATE/last-update-check" "$STATE/last-notified" "$STATE/last-error"
  : >"$NOTES"
  : >"$ATTEMPTS"
  rm -rf "$LOCK"
}
count() { echo $(($(wc -l <"$1"))); }
day_passes() { echo $(($(date +%s) - 90000)) >"$STATE/last-update-check"; }
guard_once() { (cmd_guard) 2>/dev/null; }
check() {
  if [[ "$2" == "$3" ]]; then
    echo "ok   $1"
  else
    echo "FAIL $1: expected $3, got $2"
    failures=$((failures + 1))
  fi
}

notify() { echo "$1" >>"$NOTES"; }
is_custom() { return 0; }
is_stock() { return 1; }
ensure_settings() { :; }
snapshot_settings() { :; }
restore_settings() { :; }
publish_fork() { :; }
sync_fork() { return 1; }
latest_openlogi_tag() { echo v9.9.9; }
version_of() { echo 0.1.0; }
download_release() { echo "$SANDBOX/stock"; }
APP="$SANDBOX"

# The bug this suite exists for: every guard trigger retried and notified.
reset
cmd_apply() {
  echo x >>"$ATTEMPTS"
  die "rebase conflicted"
}
for _ in 1 2 3 4 5; do guard_once; done
check "five triggers make one attempt" "$(count "$ATTEMPTS")" 1
check "five triggers make one notification" "$(count "$NOTES")" 1
check "the notification names the failure" "$(grep -c 'rebase conflicted' "$NOTES")" 1

day_passes
guard_once
check "the next day retries" "$(count "$ATTEMPTS")" 2
check "the same failure stays quiet" "$(count "$NOTES")" 1

# Two failures taking turns must not repeat either.
day_passes
cmd_apply() {
  echo x >>"$ATTEMPTS"
  die "tests failed"
}
guard_once
check "a new failure speaks" "$(count "$NOTES")" 2
day_passes
cmd_apply() {
  echo x >>"$ATTEMPTS"
  die "rebase conflicted"
}
guard_once
check "an alternating failure stays quiet" "$(count "$NOTES")" 2

# A failure that is not a die must stop the run and be reported.
reset
cmd_apply() {
  false
  echo reached >>"$ATTEMPTS"
}
guard_once
check "a failing command stops the run" "$(count "$ATTEMPTS")" 0
check "a failure without a die is still reported" "$(grep -c 'see .*openlogi-custom.log' "$NOTES")" 1

# A clean run forgets, so the same problem can speak again later.
reset
cmd_apply() { die "rebase conflicted"; }
guard_once
day_passes
latest_openlogi_tag() { echo v0.1.0; }
guard_once
check "a clean check forgets notified problems" "$([[ -f "$STATE/last-notified" ]] && echo yes || echo no)" no

# A release that cannot be downloaded yet is pending, not current.
reset
latest_openlogi_tag() { echo v9.9.9; }
download_release() { return 1; }
guard_once
check "an undownloadable release is not called current" "$(tail -1 "$LOG" | grep -c 'not installable yet')" 1
check "and notifies nothing" "$(count "$NOTES")" 0

# A killed run's lock must not silence the guard for good; a live one must.
reset
download_release() { echo "$SANDBOX/stock"; }
cmd_apply() { echo x >>"$ATTEMPTS"; }
mkdir "$LOCK"
guard_once
check "a fresh lock holds the guard off" "$(count "$ATTEMPTS")" 0
touch -t 202001010000 "$LOCK"
guard_once
check "a stale lock is cleared" "$(count "$ATTEMPTS")" 1

# The guard's copy follows the branch, and refuses a script that does not parse.
reset
fixture="$SANDBOX/repo"
mkdir -p "$fixture/scripts"
git -C "$fixture" init -q -b "$BRANCH"
git -C "$fixture" -c user.name=test -c user.email=test@example.invalid commit -q --allow-empty -m fixture
REPO="$fixture"
SELF="$STATE/openlogi-custom.sh"
echo '# old' >"$SELF"
echo '# new' >"$fixture/scripts/openlogi-custom.sh"
refresh_guard_copy 2>/dev/null
check "the guard copy follows the repository" "$(cat "$SELF")" "# new"
echo 'if then' >"$fixture/scripts/openlogi-custom.sh"
refresh_guard_copy 2>/dev/null
check "a broken script is refused" "$(cat "$SELF")" "# new"

if ((failures > 0)); then
  echo "$failures failed"
  exit 1
fi
echo "all passed"
