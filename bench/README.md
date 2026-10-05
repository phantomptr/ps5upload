# Bench

Repo-native benchmark and hardware-sweep tooling that drives a running `ps5upload-engine` over its HTTP API.

Current contents:

- `run-sweep.mjs`
  Runs every profile in `profiles.mjs` against a live PS5 through the engine (`make validate`, `make validate-xl`) and writes a report under `bench/reports/`.
- `profiles.mjs`
  The shared profile definitions. `scripts/gen-fixtures.mjs` builds the local trees they upload.
- `resume-test.mjs`, `multistream-hw-test.mjs`, `edge-case-sweep.mjs`
  Hardware checks for resume, parallel streams and edge cases.

Protocol benchmarks (AVA1 upload, download, copy, resume, relay and the drop-and-resend run) are `ps5upload-lab bench` scenarios; see `cargo run -p ps5upload-lab -- bench --help` from `engine/`.

Example flow:

```bash
node bench/run-sweep.mjs --spawn-engine --ps5-addr=192.168.137.2
```

Notes:

- The sweep targets the engine HTTP API. The console address is a bare host; a `:port` suffix is ignored.
- If the engine is already running, omit `--spawn-engine`.
- Results are captured from real hardware; this repo does not invent threshold numbers ahead of measurement.
