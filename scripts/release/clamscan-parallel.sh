#!/usr/bin/env bash
#
# Scans the files named on stdin (NUL-separated) with ClamAV, one clamscan per core over a
# size-balanced share of them. Exits non-zero on any hit or error (clamscan's 1 or 2) in any share.
#
#   find release -type f -print0 | scripts/release/clamscan-parallel.sh
#
# A single clamscan is single-threaded and took 5 of create-release's 6 min (28 files, 2.6 GiB,
# v9.4.1). Each process loads the signatures (~10 s, ~1.5 GB), so it is one per core, not one per
# file. Used by publish.yml's virus-scan job (each build's files as they arrive) and by
# create-release (whatever that job did not cover).

set -euo pipefail

n="$(nproc 2>/dev/null || sysctl -n hw.ncpu)"
shares="$(mktemp -d)"
trap 'rm -rf "$shares"' EXIT

# Largest first, dealt round-robin, so no share ends up with two of the biggest files.
i=0
while IFS= read -r f; do
  printf '%s\0' "$f" >> "$shares/$((i % n))"
  i=$((i + 1))
done < <(xargs -0 -r du -k | sort -rn | cut -f2-)
if [ "$i" -eq 0 ]; then
  echo "clamscan-parallel: no files to scan"
  exit 0
fi

pids=()
for share in "$shares"/*; do
  xargs -0 clamscan --infected --scan-archive=yes --max-filesize=4000M \
    --max-scansize=4000M < "$share" > "$share.log" 2>&1 &
  pids+=("$!")
done
rc=0
for pid in "${pids[@]}"; do wait "$pid" || rc=1; done
cat "$shares"/*.log
echo "clamscan-parallel: $i file(s) in ${#pids[@]} share(s), exit $rc"
exit "$rc"
