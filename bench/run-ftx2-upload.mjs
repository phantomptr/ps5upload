#!/usr/bin/env node

import fs from 'node:fs/promises';
import { existsSync } from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, '..');

const DEFAULT_ENGINE_URL = 'http://127.0.0.1:19113';
const DEFAULT_PS5_ADDR = '192.168.137.2:9113';
// Unified test sandbox — matches smoke/sweep. Wipe with a single CLEANUP:
// POST /api/ps5/cleanup {"path":"/data/ps5upload/tests"}
const DEFAULT_DEST_ROOT = '/data/ps5upload/tests/bench';
const DEFAULT_RESULTS_DIR = path.join(repoRoot, 'bench', 'results');
const DEFAULT_TIMEOUT_MS = 30 * 60 * 1000;

function log(message) {
  process.stdout.write(`[bench-ftx2-upload] ${message}\n`);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function formatBytes(bytes) {
  const value = Number(bytes || 0);
  if (!Number.isFinite(value) || value <= 0) return '0 B';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let scaled = value;
  let index = 0;
  while (scaled >= 1024 && index < units.length - 1) {
    scaled /= 1024;
    index += 1;
  }
  return `${scaled.toFixed(index === 0 ? 0 : 2)} ${units[index]}`;
}

async function sourceStats(sourcePath, stat) {
  if (stat.isFile()) {
    return { bytes: stat.size, fileCount: 1 };
  }
  if (!stat.isDirectory()) {
    return { bytes: null, fileCount: 0 };
  }

  let bytes = 0;
  let fileCount = 0;
  const stack = [sourcePath];
  while (stack.length > 0) {
    const dir = stack.pop();
    let entries;
    try {
      entries = await fs.readdir(dir, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      const p = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        stack.push(p);
      } else if (entry.isFile()) {
        try {
          const s = await fs.stat(p);
          bytes += s.size;
          fileCount += 1;
        } catch {}
      }
    }
  }
  return { bytes, fileCount };
}

function parseArgs(argv) {
  const out = {
    engineUrl: DEFAULT_ENGINE_URL,
    ps5Addr: process.env.PS5_ADDR || DEFAULT_PS5_ADDR,
    source: '',
    destRoot: DEFAULT_DEST_ROOT,
    timeoutMs: DEFAULT_TIMEOUT_MS,
    spawnEngine: false,
    resultsDir: DEFAULT_RESULTS_DIR,
    writeResult: true,
  };

  for (const arg of argv) {
    if (arg.startsWith('--engine-url=')) out.engineUrl = arg.slice('--engine-url='.length);
    else if (arg.startsWith('--ps5-addr=')) out.ps5Addr = arg.slice('--ps5-addr='.length);
    else if (arg.startsWith('--source=')) out.source = arg.slice('--source='.length);
    else if (arg.startsWith('--dest-root=')) out.destRoot = arg.slice('--dest-root='.length);
    else if (arg.startsWith('--timeout-ms=')) out.timeoutMs = Number(arg.slice('--timeout-ms='.length)) || out.timeoutMs;
    else if (arg.startsWith('--results-dir=')) out.resultsDir = path.resolve(repoRoot, arg.slice('--results-dir='.length));
    else if (arg === '--spawn-engine') out.spawnEngine = true;
    else if (arg === '--no-write-result') out.writeResult = false;
  }

  if (!out.source) {
    throw new Error('missing required --source=/path/to/file-or-dir');
  }

  return out;
}

async function waitForHttpOk(url, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {}
    await sleep(250);
  }
  throw new Error(`timed out waiting for ${url}`);
}

function spawnEngine(ps5Addr) {
  // Prefer the pre-built release binary — `cargo run` defaults to debug which
  // is dramatically slower for high-throughput transfers (the engine allocates
  // per-shard in the hot path). Fall back to release cargo build if the
  // binary isn't present.
  const releaseBin = path.join(repoRoot, 'engine', 'target', 'release', 'ps5upload-engine');
  let cmd, args;
  if (existsSync(releaseBin)) {
    cmd = releaseBin;
    args = [];
  } else {
    cmd = 'cargo';
    args = ['run', '--release', '-p', 'ps5upload-engine'];
  }
  const child = spawn(cmd, args, {
    cwd: path.join(repoRoot, 'engine'),
    env: {
      ...process.env,
      PS5_ADDR: ps5Addr,
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout?.on('data', (chunk) => process.stdout.write(String(chunk)));
  child.stderr?.on('data', (chunk) => process.stderr.write(String(chunk)));
  return child;
}

async function stopChild(child, name) {
  if (!child || child.exitCode != null || child.killed) return;
  child.kill('SIGTERM');
  await new Promise((resolve) => {
    const timer = setTimeout(() => {
      try {
        child.kill('SIGKILL');
      } catch {}
      resolve();
    }, 5000);
    child.once('exit', () => {
      clearTimeout(timer);
      resolve();
    });
  });
  log(`${name} stopped`);
}

async function postJson(url, body) {
  const response = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  const text = await response.text();
  let parsed = null;
  try {
    parsed = text ? JSON.parse(text) : null;
  } catch {
    parsed = text;
  }
  if (!response.ok) {
    throw new Error(`POST ${url} failed (${response.status}): ${typeof parsed === 'string' ? parsed : JSON.stringify(parsed)}`);
  }
  return parsed;
}

async function getJson(url) {
  const response = await fetch(url);
  const text = await response.text();
  let parsed = null;
  try {
    parsed = text ? JSON.parse(text) : null;
  } catch {
    parsed = text;
  }
  if (!response.ok) {
    throw new Error(`GET ${url} failed (${response.status}): ${typeof parsed === 'string' ? parsed : JSON.stringify(parsed)}`);
  }
  return parsed;
}

function nowIsoCompact() {
  return new Date().toISOString().replaceAll(':', '').replace(/\.\d{3}Z$/, 'Z');
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const sourcePath = path.resolve(options.source);
  const stat = await fs.stat(sourcePath);
  const sourceKind = stat.isDirectory() ? 'dir' : stat.isFile() ? 'file' : 'other';
  if (sourceKind === 'other') {
    throw new Error(`source must be a regular file or directory: ${sourcePath}`);
  }
  const sourceSummary = await sourceStats(sourcePath, stat);

  let engineChild = null;
  try {
    if (options.spawnEngine) {
      log(`spawning engine with PS5_ADDR=${options.ps5Addr}`);
      engineChild = spawnEngine(options.ps5Addr);
    }

    await waitForHttpOk(`${options.engineUrl}/api/jobs`, 30_000);
    log(`engine ready at ${options.engineUrl}`);

    const destName = path.basename(sourcePath);
    const reqBody = sourceKind === 'dir'
      ? {
          addr: options.ps5Addr,
          dest_root: options.destRoot,
          src_dir: sourcePath,
        }
      : {
          addr: options.ps5Addr,
          dest: `${options.destRoot}/${destName}`,
          src: sourcePath,
        };
    const route = sourceKind === 'dir' ? '/api/transfer/dir' : '/api/transfer/file';

    log(`starting ${sourceKind} benchmark from ${sourcePath} to ${options.destRoot}`);
    const startedAt = Date.now();
    const created = await postJson(`${options.engineUrl}${route}`, reqBody);
    const jobId = created?.job_id;
    if (!jobId) {
      throw new Error(`engine did not return job_id: ${JSON.stringify(created)}`);
    }

    let job = null;
    const deadline = Date.now() + options.timeoutMs;
    while (Date.now() < deadline) {
      job = await getJson(`${options.engineUrl}/api/jobs/${jobId}`);
      if (job?.status === 'done' || job?.status === 'failed') break;
      await sleep(1000);
    }
    if (!job || (job.status !== 'done' && job.status !== 'failed')) {
      throw new Error(`job ${jobId} did not complete within ${options.timeoutMs} ms`);
    }
    if (job.status !== 'done') {
      throw new Error(`job ${jobId} failed: ${job.error || JSON.stringify(job)}`);
    }

    const finishedAt = Date.now();
    const elapsedMs = Number(job.elapsed_ms || (finishedAt - startedAt));
    const bytesSent = Number(job.bytes_sent || 0);
    const shardsSent = Number(job.shards_sent || 0);
    const bytesPerSec = elapsedMs > 0 ? (bytesSent * 1000) / elapsedMs : 0;
    const mibPerSec = bytesPerSec / (1024 * 1024);
    const result = {
      schema_version: 1,
      benchmark: 'ftx2_upload',
      created_at: new Date().toISOString(),
      engine_url: options.engineUrl,
      ps5_addr: options.ps5Addr,
      source: {
        path: sourcePath,
        kind: sourceKind,
        bytes: sourceSummary.bytes,
        file_count: sourceSummary.fileCount,
      },
      destination: {
        root: options.destRoot,
        final_path: job.dest,
      },
      job: {
        job_id: jobId,
        tx_id_hex: job.tx_id_hex,
        shards_sent: shardsSent,
        bytes_sent: bytesSent,
        started_at_ms: Number(job.started_at_ms || 0),
        completed_at_ms: Number(job.completed_at_ms || 0),
        elapsed_ms: elapsedMs,
      },
      metrics: {
        bytes_per_sec: bytesPerSec,
        mib_per_sec: mibPerSec,
      },
    };

    log(`job ${jobId} complete`);
    log(`bytes sent: ${formatBytes(bytesSent)} in ${(elapsedMs / 1000).toFixed(2)} s`);
    log(`throughput: ${mibPerSec.toFixed(2)} MiB/s`);

    if (options.writeResult) {
      await fs.mkdir(options.resultsDir, { recursive: true });
      const outputPath = path.join(
        options.resultsDir,
        `${nowIsoCompact()}-${sourceKind}-${path.basename(sourcePath)}.json`,
      );
      await fs.writeFile(outputPath, `${JSON.stringify(result, null, 2)}\n`, 'utf8');
      log(`wrote result ${outputPath}`);
    } else {
      process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
    }
  } finally {
    await stopChild(engineChild, 'engine');
  }
}

main().catch((error) => {
  process.stderr.write(`[bench-ftx2-upload] error: ${error.message}\n`);
  process.exitCode = 1;
});
