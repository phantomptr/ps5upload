# Tests

Real-hardware integration, scenario, and regression tests for `ps5upload 2.0`.

## smoke-hardware.mjs

End-to-end smoke test that exercises the full FTX2 stack on a live PS5.

**Prerequisites:**
- PS5 reachable at `PS5_ADDR` (default `192.168.137.2:9113`) with `payload/ps5upload.elf` loaded
- Engine built at least once (`make engine`) — the smoke runner auto-starts it

**Run:**
```sh
# auto-starts the engine (default)
npm run smoke:hardware

# use an engine that's already running elsewhere
node tests/smoke-hardware.mjs --no-spawn-engine

# override PS5 address
PS5_ADDR=192.168.1.50:9113 npm run smoke:hardware
```

**Options:**

| Flag | Default | Description |
|---|---|---|
| `--engine-url=URL` | `http://127.0.0.1:9114` | engine HTTP base URL |
| `--ps5-addr=HOST:PORT` | `192.168.137.2:9113` | PS5 FTX2 address |
| `--dest-root=PATH` | `/data/ps5upload/tests/smoke` | destination root on PS5 |
| `--no-spawn-engine` | off | use an already-running engine |
| `--no-cleanup` | off | keep tmp fixture files on exit |
| `--timeout-ms=N` | `120000` | per-job timeout |

The runner prefers the pre-built binary (`engine/target/release/` then `debug/`) and falls back to `cargo run` if no binary exists.

**Coverage:**

| Check | Description |
|---|---|
| engine reachable | GET /api/jobs returns 200 |
| ps5 status | GET /api/ps5/status returns non-error |
| single-file transfer | 256 KiB random file → one shard |
| directory transfer | 8 × 32 KiB files → dir on PS5 |
| multi-shard file transfer | 4 MiB file → multiple shards |
| file-list transfer | 3 explicit src→dest pairs |
| jobs list sanity | /api/jobs returns array |

**Exit codes:** `0` = all pass, `1` = one or more failed.

## Planned coverage

- transaction resume and replay after payload restart mid-transfer
- takeover lifecycle (old payload killed by new one, TX marked interrupted, replayed)
- fault injection (BLAKE3 mismatch, partial shard, connection drop mid-transfer)
