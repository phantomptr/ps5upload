#!/usr/bin/env node

import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, '..');

function log(message) {
  process.stdout.write(`[bench-ftx2-gate] ${message}\n`);
}

function parseArgs(argv) {
  const out = {
    result: '',
    baseline: '',
    minThroughputRatio: 0.9,
    maxElapsedRatio: 1.15,
  };

  for (const arg of argv) {
    if (arg.startsWith('--result=')) out.result = path.resolve(repoRoot, arg.slice('--result='.length));
    else if (arg.startsWith('--baseline=')) out.baseline = path.resolve(repoRoot, arg.slice('--baseline='.length));
    else if (arg.startsWith('--min-throughput-ratio=')) out.minThroughputRatio = Number(arg.slice('--min-throughput-ratio='.length)) || out.minThroughputRatio;
    else if (arg.startsWith('--max-elapsed-ratio=')) out.maxElapsedRatio = Number(arg.slice('--max-elapsed-ratio='.length)) || out.maxElapsedRatio;
  }

  if (!out.result || !out.baseline) {
    throw new Error('usage: node bench/check-ftx2-baseline.mjs --result=... --baseline=...');
  }

  return out;
}

async function readJson(filePath) {
  return JSON.parse(await fs.readFile(filePath, 'utf8'));
}

function formatMiBPerSec(value) {
  return `${Number(value || 0).toFixed(2)} MiB/s`;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const result = await readJson(options.result);
  const baseline = await readJson(options.baseline);

  const resultBps = Number(result?.metrics?.bytes_per_sec || 0);
  const baselineBps = Number(baseline?.metrics?.bytes_per_sec || 0);
  const resultElapsedMs = Number(result?.job?.elapsed_ms || 0);
  const baselineElapsedMs = Number(baseline?.job?.elapsed_ms || 0);

  if (resultBps <= 0 || baselineBps <= 0) {
    throw new Error('result and baseline must both have positive metrics.bytes_per_sec');
  }
  if (resultElapsedMs <= 0 || baselineElapsedMs <= 0) {
    throw new Error('result and baseline must both have positive job.elapsed_ms');
  }

  const throughputRatio = resultBps / baselineBps;
  const elapsedRatio = resultElapsedMs / baselineElapsedMs;

  log(`result throughput:   ${formatMiBPerSec(result?.metrics?.mib_per_sec)}`);
  log(`baseline throughput: ${formatMiBPerSec(baseline?.metrics?.mib_per_sec)}`);
  log(`throughput ratio:    ${throughputRatio.toFixed(3)}`);
  log(`elapsed ratio:       ${elapsedRatio.toFixed(3)}`);

  const failures = [];
  if (throughputRatio < options.minThroughputRatio) {
    failures.push(
      `throughput ratio ${throughputRatio.toFixed(3)} is below threshold ${options.minThroughputRatio.toFixed(3)}`,
    );
  }
  if (elapsedRatio > options.maxElapsedRatio) {
    failures.push(
      `elapsed ratio ${elapsedRatio.toFixed(3)} exceeds threshold ${options.maxElapsedRatio.toFixed(3)}`,
    );
  }

  if (failures.length) {
    for (const failure of failures) {
      log(`FAIL: ${failure}`);
    }
    process.exitCode = 1;
    return;
  }

  log('PASS');
}

main().catch((error) => {
  process.stderr.write(`[bench-ftx2-gate] error: ${error.message}\n`);
  process.exitCode = 1;
});
