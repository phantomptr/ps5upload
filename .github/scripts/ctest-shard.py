#!/usr/bin/env python3
"""Deal the ps5upload-ctest test files across CI shards by measured run time.

Usage: ctest-shard.py SHARD COUNT  (SHARD is 1-based)

Prints `--test <name>` argument pairs, one token per line, for cargo test.

Used by the rust-ctest and ps5upload-ctest-sanitize jobs in .github/workflows/engine-ci.yml. Both used
to deal the files round-robin by name, which left shard 3 with ~300 s of tests against ~170-240 s
on the others, so the slowest shard set the PR gate's time. Here the files are dealt longest
first, each to the shard with the least time so far (the classic LPT schedule), so the
shards come out within a few seconds of each other.

Every shard computes the same assignment from the same sorted file list, so the shards together
still run every file exactly once. A file missing from WEIGHTS (added after the table was
measured) still runs, under DEFAULT_WEIGHT; refresh the table when the shards drift apart.
"""

import pathlib
import sys

# Seconds per test binary (link + run) under `cargo test -- --test-threads=1`, the mean of the
# plain and the ASan + UBSan runs on ubuntu-24.04, measured from CI runs 40079393365,
# 40082659548 and 40084135077 (2026-10-24). Pruned 2026-11-16 to the files still in
# engine/crates/ps5upload-ctest/tests (the protocol-level ones moved to third_party/ava1).
# readback and smp_meta came later and are not measured yet: listed at DEFAULT_WEIGHT.
WEIGHTS = {
    "c2c": 25, "conformance": 1, "diag": 3, "download_trust": 0, "fan_map": 0, "firmware": 0,
    "job_run": 16, "job_run_trust": 0, "jobs_any_owner": 13, "lifecycle": 1, "live_notes": 2,
    "many_jobs_one_session": 13, "mgmt": 4, "mgmt_fs": 9, "mgmt_t6": 6, "mgmt_t7": 6,
    "nas_policy": 0, "nas_skip": 6, "pairing": 4, "payload_cutover": 0, "periodic_kill": 6,
    "readback": 15, "resume_after_cancel": 50, "rp_pair": 0, "sevenz": 6, "shutdown": 9,
    "single_instance": 1, "smp_meta": 15, "soak": 10, "space_resume": 49,
}
DEFAULT_WEIGHT = 15
# Each file is also its own test binary to compile and link before anything runs, which the
# measured times above leave out; charge it so the shard with the many tiny files is not overfilled.
PER_FILE_COST = 2

TESTS_DIR = pathlib.Path(__file__).resolve().parents[2] / "engine" / "crates" / "ps5upload-ctest" / "tests"


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    shard, count = int(sys.argv[1]), int(sys.argv[2])
    if not 1 <= shard <= count:
        print(f"shard {shard} is outside 1..{count}", file=sys.stderr)
        return 2

    names = sorted(p.stem for p in TESTS_DIR.glob("*.rs"))
    if not names:
        print(f"no test files under {TESTS_DIR}", file=sys.stderr)
        return 1
    unweighted = [n for n in names if n not in WEIGHTS]
    if unweighted:
        print(f"::notice::ctest-shard: no measured time for {', '.join(unweighted)}; "
              f"assuming {DEFAULT_WEIGHT} s each", file=sys.stderr)

    loads = [0] * count
    assigned = [[] for _ in range(count)]
    # Longest first; ties by name so every shard computes the same deal.
    for name in sorted(names, key=lambda n: (-WEIGHTS.get(n, DEFAULT_WEIGHT), n)):
        i = min(range(count), key=lambda k: (loads[k], k))
        loads[i] += WEIGHTS.get(name, DEFAULT_WEIGHT) + PER_FILE_COST
        assigned[i].append(name)

    mine = sorted(assigned[shard - 1])
    if not mine:
        print(f"::error::shard {shard} got no test files", file=sys.stderr)
        return 1
    print(f"shard {shard} of {count}: {len(mine)} of {len(names)} test files, "
          f"~{loads[shard - 1]} s estimated (shards: {', '.join(f'{x} s' for x in loads)})",
          file=sys.stderr)
    for name in mine:
        print("--test")
        print(name)
    return 0


if __name__ == "__main__":
    sys.exit(main())
