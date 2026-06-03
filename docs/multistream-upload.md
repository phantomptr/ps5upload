# Multi-stream upload design

## Problem

A single upload stream is pinned at ~40 MB/s on every non-Pro PS5 (Fat and Slim,
across firmwares), while the PS5 Pro reaches ~100+ MB/s. Investigation
(see the conversation that produced this doc) ruled out:

- **SSD silicon** — raw write is ~5,100 MB/s; Fat (12-channel) and Slim
  (6-channel) have *different* drives yet both cap at the same ~40 MB/s.
- **Firmware / kstuff** — high-firmware non-Pro consoles also cap at ~40.
- **Storage target** — both Pro and non-Pro write to `/data`; not an M.2 effect.
- **Our send code** — already pipelined (32 shards / 256 MiB inflight),
  `posix_fallocate`, no per-chunk `fsync`, tuned socket buffers.

The flat ~40 MB/s across diverse non-Pro hardware is the signature of a
**single-stream, single-thread, CPU/memory-bandwidth-bound write path** under the
homebrew kernel. The same Zen 2 @ 3.5 GHz + 14 GT/s memory in every non-Pro box
produces the same ceiling; the Pro's +10% CPU / +28% memory bandwidth lift its
single stream until it becomes *network*-bound near gigabit (~110 MB/s).

A per-stream ceiling is broken by **parallelism**: N independent streams aggregate
toward the gigabit wire. External proof: `manos555555/PS5-Upload-Suite` reaches
104 MB/s with 8 parallel connections (~11–14 MB/s each).

## Why this is feasible (and lower-risk than it looks)

The PS5 payload is **already concurrency-safe** for multiple transactions; the
*only* blocker is the single-threaded accept loop on the transfer port `:9113`.

- **TX table** (`runtime_state_t::tx_entries[32]`, `runtime.h`) is keyed by
  `tx_id` with two-level locking: a brief global `state_mtx` for lookups/counters
  and a per-slot `g_entry_mtx[32]` held across a handler's mutation. Distinct
  `tx_id`s take distinct per-slot mutexes → **concurrent transactions are safe**.
- **Concurrent writes to distinct files** are production-proven by the 4-worker
  pack pool (`PS5UPLOAD2_PACK_WORKERS`): no global write lock, each worker
  `open`/`write`/`close`s its own file. (Validated on 200k-file uploads.)
- **Per-tx isolation**: each `tx_entry` owns its `direct_writer` + `pack_pool`,
  so concurrent transactions share no write buffers.
- **The mgmt port `:9114` already runs up to 8 concurrent per-client handler
  threads** — the exact pattern we mirror for `:9113`.

## Architecture

Each stream is an **independent FTX2 transaction** (own connection, own `tx_id`,
own BEGIN_TX → STREAM_SHARD* → COMMIT_TX), writing a **disjoint subset of files**
to the same `dest_root`. No two streams ever touch the same file.

```
                 ┌── stream 0: conn → tx_id^0 → files[bucket 0]
 engine job ─────┼── stream 1: conn → tx_id^1 → files[bucket 1]
 (N threads)     ├── stream 2: conn → tx_id^2 → files[bucket 2]
                 └── stream 3: conn → tx_id^3 → files[bucket 3]
                          │
                          ▼  (all bump the same Arc<AtomicU64> progress counters)
                    one UI job / progress bar
```

### Engine (Rust, `ps5upload-core/src/transfer.rs`)

1. **Balanced distribution** — `distribute_balanced(weights, buckets)`: greedy
   longest-processing-time (sort files desc by size, assign each to the
   currently-lightest bucket). Pure, unit-tested. Keeps each file whole in one
   stream; packing of small files stays within a stream.
2. **Per-stream `tx_id`** — derived deterministically from a base id:
   `id[15] ^= stream_index`. Distinct per stream (XOR with distinct small index),
   **stable across retries** so per-stream `TX_FLAG_RESUME` works.
3. **Orchestrator** — `transfer_file_list_multistream(...)`:
   - `streams <= 1` or `entries.len() <= 1` → delegate to the existing
     single-stream path **unchanged** (zero behavior change when disabled).
   - else: stat sizes once, distribute, `std::thread::scope` to spawn N threads,
     each running `transfer_file_list_resumable` on its disjoint sub-slice with
     the **shared** `cfg` (progress `Arc`s shared; counters already atomic).
   - join; aggregate `shards_sent`/`bytes_sent`; on any error, set the shared
     cancel flag so siblings stop promptly, then surface the first error
     (partial streams are resumable on the job's retry).
4. **Cancel flag** — `TransferConfig::cancel: Option<Arc<AtomicBool>>`, checked in
   `PipelinedSender::send`; aborts the stream cleanly. Doubles as the long-missing
   user-facing cancel hook.

### Payload (C, `payload/src/runtime.c`)

Mirror the mgmt-port pattern on the transfer accept loop:

- On `accept()` for `:9113`, dispatch the connection to a **detached worker
  thread** running the existing `handle_binary_frame` loop with its own
  `conn_tx_ctx_t` (so interrupted-tx-on-drop still works per connection).
- Cap concurrency at `PS5UPLOAD2_MAX_TRANSFER_CONNS` (start at 4). Over the cap →
  inline-handle (back-pressure) like mgmt's fallback, never reject.
- No TX-table or write-path changes needed — already concurrency-safe.
- Track live worker count; on shutdown, stop accepting and let workers drain.

### Capability negotiation (required)

An old payload is single-client: a 2nd connection's `accept()` won't be serviced
until the first transfer finishes, so the engine would hang. The engine MUST
detect support before opening parallel connections.

- Payload advertises `max_transfer_streams` in its STATUS/HELLO response
  (mgmt `:9114`), set to `PS5UPLOAD2_MAX_TRANSFER_CONNS` on the new payload, absent
  (→ treated as 1) on old payloads.
- Engine uses `min(user_setting, payload_max, hard_cap)` streams. If the payload
  doesn't advertise it → **1 stream** (today's behavior). Safe by default.

### UI

- Settings: "Parallel upload streams" — `Auto` (use payload max, capped),
  `1` (off), `2`, `3`, `4`. Default **1** until hardware-validated, then consider
  `Auto`. Large-files benefit most; the small-file pack pool already saturates at
  4 workers, so multi-stream is gated to jobs with enough large files to matter.

## Failure modes & decisions

- **One stream fails mid-upload** → cancel siblings, surface error; the job's
  existing resume retry re-runs all streams with `TX_FLAG_RESUME` (each stream's
  stable `tx_id` lets the payload skip already-acked shards).
- **Concurrent `mkdir` of a shared parent dir** from two streams → the payload's
  mkdir-p must tolerate `EEXIST` (verify; it already does for the pack pool).
- **UFS write serialization risk** — if the kernel serialized all writes behind a
  single lock, parallel streams wouldn't scale. The pack pool's concurrent-write
  gains and the 8-connection FTP result are strong evidence it scales on `/data`.
  The per-phase timing instrumentation (below) confirms empirically.
- **Progress** — all streams bump shared `Arc<AtomicU64>`; totals are sums; the UI
  is unchanged.

## Rollout plan (incremental, each step independently safe)

1. Engine: `distribute_balanced` + tests. *(dormant)*
2. Engine: `cancel` flag + `PipelinedSender` honoring it. *(dormant)*
3. Engine: `transfer_file_list_multistream` orchestrator + tests, `streams`
   defaulting to 1 everywhere. *(dormant — no caller passes >1 yet)*
4. Payload: concurrent accept on `:9113` + `max_transfer_streams` in STATUS.
   Build via `make payload` (from repo root). **Backward-compatible**: old engines
   open 1 connection and behave exactly as before.
5. Engine: capability detection; wire orchestrator into the dir/file-list path
   behind the resolved stream count.
6. UI: the setting + plumbing.
7. **Hardware validation on Fat/Slim before enabling >1 by default or releasing.**

## Verification

- Pre-work: surface the payload's per-phase `timing_us` (`recv/write/hash`) into
  the Logs tab so a single-stream upload proves it's write-thread-bound.
- A/B on Fat: 1 vs 2 vs 4 streams, large-file game, wired gigabit. Expect
  aggregate throughput to climb from ~40 toward the wire (~90–110) and plateau
  when network- or write-lock-bound.

## Hardware validation (2026-06-02)

Tested via `bench/multistream-hw-test.mjs` against two live consoles on wired
gigabit, source on an external volume. Workload: a synthetic 4×1 GiB folder (one
large file per stream) — the case multi-stream targets. Each run re-sent the
full 4 GiB; both stream counts completed cleanly with no payload crash.

| Console            | streams=1   | streams=4   | speedup | streams=4 wall |
|--------------------|-------------|-------------|---------|----------------|
| Fat  (FW 5.10)     | 30.0 MiB/s  | 51.9 MiB/s  | 1.73×   | 79 s (vs 136 s)|
| Pro  (FW 9.60)     | 61.8 MiB/s  | 87.9 MiB/s  | 1.42×   | 47 s (vs 66 s) |

Findings:
- **Multi-stream breaks the single-stream ceiling on both consoles.** 4 concurrent
  connections confirmed (netstat + engine `multistream:` log). The Pro's smaller
  multiplier is expected — its single stream (~62–85 MiB/s) already starts near
  the ~110 MiB/s gigabit wire, leaving less headroom.
- **Stable.** Both firmwares, sustained 4-stream concurrency, repeated abrupt
  engine kills mid-transfer → payload stayed up, marked txs interrupted, cleaned
  up; `active_transactions` returned to 0. A 9.7 GB single file completed at
  29.3 MiB/s on the Fat.
- **Not a win for tiny-file folders.** A 46k-file / 1.15 GB folder (avg 26 KB) is
  metadata-bound, not bandwidth-bound — it crawls regardless of stream count.
  Multi-stream helps workloads with real bytes-per-file; the small-file pack pool
  already saturates separately.
- Peak instantaneous samples (128–384 MiB/s) are disk-cache bursts, not sustained.
