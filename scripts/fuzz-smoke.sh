#!/usr/bin/env bash
# Build every fuzz target in engine/fuzz and run each for a short while (`make fuzz-smoke`).
#
#   scripts/fuzz-smoke.sh            every target, 20 s each
#   FUZZ_SECONDS=60 scripts/fuzz-smoke.sh exfat ufs2
#
# Needs `cargo fuzz` and a nightly toolchain; without them it says so and exits 0, so a machine
# without them can still run the rest of the checks. A crash exits 1 after every target has
# run; its input is in engine/fuzz/artifacts/<target>/ (engine/fuzz/README.md).
set -uo pipefail
cd "$(dirname "$0")/../engine/fuzz"

seconds="${FUZZ_SECONDS:-20}"

if ! cargo fuzz --version >/dev/null 2>&1; then
  echo "fuzz-smoke: skipped, cargo-fuzz is not installed (cargo install cargo-fuzz)"
  exit 0
fi
if ! cargo +nightly --version >/dev/null 2>&1; then
  echo "fuzz-smoke: skipped, no nightly toolchain (rustup toolchain install nightly)"
  exit 0
fi

if [ "$#" -gt 0 ]; then
  targets=("$@")
else
  targets=()
  while IFS= read -r t; do targets+=("$t"); done < <(cargo +nightly fuzz list)
fi

# Seeds first (stable): a fuzzer that starts from valid files gets past the magic numbers.
cargo run --quiet --release -p ps5upload-fuzz-seedgen -- corpus >/dev/null || {
  echo "fuzz-smoke: the seed generator failed"
  exit 1
}
cargo +nightly fuzz build || exit 1

failed=()
for t in "${targets[@]}"; do
  echo "== $t (${seconds}s)"
  # -rss_limit_mb: the sparse images decode to at most 512 MiB and a target may hold three.
  # Seeds are up to ~1.3 MiB, so the length cap is set above them.
  # The fuzzer's exit code, not grep's (which only trims the output to what matters).
  cargo +nightly fuzz run "$t" -- -max_total_time="$seconds" -rss_limit_mb=4096 \
    -max_len=2097152 -timeout=60 -print_final_stats=1 2>&1 |
    grep -E "^(stat::number_of_executed_units|stat::new_units_added|SUMMARY|==[0-9]+==ERROR|artifact_prefix|Base64)|panicked at"
  if [ "${PIPESTATUS[0]}" -ne 0 ]; then
    failed+=("$t")
  fi
done

if [ "${#failed[@]}" -gt 0 ]; then
  echo "fuzz-smoke: FAILED: ${failed[*]} (inputs in engine/fuzz/artifacts/)"
  exit 1
fi
echo "fuzz-smoke: ${#targets[@]} targets, ${seconds}s each, no crash"
