#!/usr/bin/env bash
# Runs one test command over and over and writes what failed (flake-hunter.yml).
#
#   flake-hunt.sh <out-dir> <name> <rounds> <budget-seconds> -- <command...>
#
# Stops after <rounds> runs or once <budget-seconds> have gone by, whichever is first (a round the
# budget cuts off is noted, not counted as a failure). Each failed round's last 80 lines go to
# <out-dir>/failures.md; one table row per set goes to <out-dir>/summary.md. Always exits 0: a
# failure is a finding for the report, not a reason to stop the other sets.
set -uo pipefail

if [ "$#" -lt 6 ] || [ "$5" != "--" ]; then
  echo "usage: $0 <out-dir> <name> <rounds> <budget-seconds> -- <command...>" >&2
  exit 2
fi
out="$1"
name="$2"
rounds="$3"
budget="$4"
shift 5
mkdir -p "$out"
log="$out/round.log"
start=$SECONDS
runs=0
fails=0
note=""
for round in $(seq 1 "$rounds"); do
  left=$((budget - (SECONDS - start)))
  if [ "$left" -le 0 ]; then
    note="time budget used up after $runs rounds"
    break
  fi
  rc=0
  timeout --kill-after=10 "$left" "$@" > "$log" 2>&1 || rc=$?
  if { [ "$rc" = 124 ] || [ "$rc" = 137 ]; } && [ $((SECONDS - start)) -ge "$budget" ]; then
    # Cut off by the budget, not a verdict on the test (a round that hangs every night shows up
    # here as a set that never finishes a round).
    note="round $round cut off by the time budget"
    echo "$name: $note"
    break
  fi
  runs=$((runs + 1))
  if [ "$rc" = 0 ]; then
    echo "$name: round $round passed"
  else
    fails=$((fails + 1))
    what="exit $rc"
    echo "$name: round $round FAILED ($what)"
    {
      echo "### $name, round $round: $what"
      echo
      echo '<details><summary>last 80 lines</summary>'
      echo
      echo '```'
      tail -n 80 "$log"
      echo '```'
      echo
      echo '</details>'
      echo
    } >> "$out/failures.md"
  fi
done
rm -f "$log"
echo "| $name | $runs | $fails | $note |" >> "$out/summary.md"
echo "$name: $fails failed of $runs runs"
exit 0
