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
- The baseline JSON files in `bench/baselines/` were captured against the pre-2.2.0 engine which bound port `9114`. The current engine binds `19113`. The captured `engine_url` field in the older JSONs is a historical record of where the run actually executed, not a current configuration knob — `check-ftx2-baseline.mjs` compares throughput and elapsed-time, not URLs, so the drift is harmless. When refreshing a baseline against the current engine, generate a new run and overwrite the file rather than patching the URL in place. The throughput numbers themselves remain valid as a regression target across the port change.
