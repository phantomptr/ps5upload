# Bench

This directory now contains the first repo-native benchmark tooling for the `2.0` FTX2 path.

Current contents:

- `run-ftx2-upload.mjs`
  Runs a real upload benchmark against `ps5upload-engine` and stores a JSON result snapshot under `bench/results/`.
- `check-ftx2-baseline.mjs`
  Compares one result JSON against a baseline result JSON and fails on throughput or elapsed-time regression.
- `baselines/`
  Commit known-good result snapshots here when you want a stable comparison target.
- `results/`
  Local benchmark outputs written by `run-ftx2-upload.mjs`.

Example flow:

```bash
node bench/run-ftx2-upload.mjs \
  --spawn-engine \
  --ps5-addr=192.168.137.2:9113 \
  --source=/path/to/workload

cp bench/results/<new-result>.json bench/baselines/ftx2-upload-main.json

node bench/check-ftx2-baseline.mjs \
  --result=bench/results/<new-result>.json \
  --baseline=bench/baselines/ftx2-upload-main.json
```

Notes:

- The benchmark targets the `2.0` engine HTTP API, not the legacy app/desktop transfer path.
- `run-ftx2-upload.mjs` accepts either a file or a directory source.
- If the engine is already running, omit `--spawn-engine`.
- Baselines are intentionally captured from real hardware; this repo does not invent threshold numbers ahead of measurement.
