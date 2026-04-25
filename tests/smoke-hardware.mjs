#!/usr/bin/env node
/**
 * tests2/smoke-hardware.mjs
 *
 * Real-hardware smoke test for the FTX2 engine + PS5 payload.
 * Requires a live PS5 at PS5_ADDR (default 192.168.137.2:9113) and
 * ps5upload-engine running at ENGINE_URL (default http://127.0.0.1:19113).
 *
 * Usage:
 *   node tests2/smoke-hardware.mjs [options]
 *
 * Options:
 *   --engine-url=URL       engine HTTP base URL  (default: http://127.0.0.1:19113)
 *   --ps5-addr=HOST:PORT   PS5 FTX2 address      (default: 192.168.137.2:9113)
 *   --dest-root=PATH       destination on PS5    (default: /data/ps5upload-smoke)
 *   --spawn-engine         spawn engine via cargo run if not already up
 *   --no-cleanup           skip tmp dir removal on exit
 *   --timeout-ms=N         per-job timeout in ms (default: 120000)
 *
 * Exit 0 = all checks passed.
 * Exit 1 = one or more checks failed.
 */

import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, '..');

const DEFAULT_ENGINE_URL = 'http://127.0.0.1:19113';
const DEFAULT_PS5_ADDR = '192.168.137.2:9113';
// All test uploads live under a single per-drive sandbox so the user can
// wipe them with one CLEANUP call, e.g. POST /api/ps5/cleanup
// {"path":"/data/ps5upload/tests"}. To target an M.2 or USB drive instead,
// pass --dest-root=/mnt/ext0/ps5upload/tests/smoke (payload allowlist
// matches the same shape on /mnt/{ext,usb}[0-9]+/ps5upload/tests).
const DEFAULT_DEST_ROOT = '/data/ps5upload/tests/smoke';
const DEFAULT_JOB_TIMEOUT_MS = 120_000;

// ── helpers ─────────────────────────────────────────────────────────────────

let passCount = 0;
let failCount = 0;

function log(msg) {
  process.stdout.write(`[smoke] ${msg}\n`);
}

function pass(label) {
  passCount++;
  process.stdout.write(`  \u2713 PASS  ${label}\n`);
}

function fail(label, detail) {
  failCount++;
  process.stderr.write(`  \u2717 FAIL  ${label}${detail ? `: ${detail}` : ''}\n`);
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

async function waitForHttpOk(url, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(url);
      if (r.ok) return;
    } catch {}
    await sleep(300);
  }
  throw new Error(`timed out waiting for ${url}`);
}

async function getJson(url) {
  const r = await fetch(url);
  const text = await r.text();
  const body = text ? tryParse(text) : null;
  if (!r.ok) throw new Error(`GET ${url} => ${r.status}: ${JSON.stringify(body)}`);
  return body;
}

async function postJson(url, body) {
  const r = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  const text = await r.text();
  const parsed = text ? tryParse(text) : null;
  if (!r.ok) throw new Error(`POST ${url} => ${r.status}: ${JSON.stringify(parsed)}`);
  return parsed;
}

function tryParse(text) {
  try { return JSON.parse(text); } catch { return text; }
}

function mgmtAddrFor(transferAddr) {
  const i = transferAddr.lastIndexOf(':');
  return i < 0 ? `${transferAddr}:9114` : `${transferAddr.slice(0, i)}:9114`;
}

async function pollJob(engineUrl, jobId, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const job = await getJson(`${engineUrl}/api/jobs/${jobId}`);
    if (job?.status === 'done' || job?.status === 'failed') return job;
    await sleep(1000);
  }
  throw new Error(`job ${jobId} did not finish within ${timeoutMs} ms`);
}

async function stopChild(child, name) {
  if (!child || child.exitCode != null || child.killed) return;
  child.kill('SIGTERM');
  await new Promise((resolve) => {
    const t = setTimeout(() => { try { child.kill('SIGKILL'); } catch {} resolve(); }, 5000);
    child.once('exit', () => { clearTimeout(t); resolve(); });
  });
  log(`${name} stopped`);
}

// ── fixture generation ───────────────────────────────────────────────────────

async function makeSingleFile(dir, name, sizeBytes) {
  const p = path.join(dir, name);
  const buf = crypto.randomBytes(sizeBytes);
  await fs.writeFile(p, buf);
  return p;
}

async function makeDir(dir, name, fileCount, fileSizeBytes) {
  const d = path.join(dir, name);
  await fs.mkdir(d, { recursive: true });
  for (let i = 0; i < fileCount; i++) {
    await fs.writeFile(path.join(d, `file_${String(i).padStart(4, '0')}.bin`), crypto.randomBytes(fileSizeBytes));
  }
  return d;
}

// ── arg parsing ──────────────────────────────────────────────────────────────

function parseArgs(argv) {
  const opts = {
    engineUrl: DEFAULT_ENGINE_URL,
    ps5Addr: process.env.PS5_ADDR || DEFAULT_PS5_ADDR,
    destRoot: DEFAULT_DEST_ROOT,
    spawnEngine: true,
    cleanup: true,
    jobTimeoutMs: DEFAULT_JOB_TIMEOUT_MS,
  };
  for (const arg of argv) {
    if (arg.startsWith('--engine-url=')) opts.engineUrl = arg.slice('--engine-url='.length);
    else if (arg.startsWith('--ps5-addr=')) opts.ps5Addr = arg.slice('--ps5-addr='.length);
    else if (arg.startsWith('--dest-root=')) opts.destRoot = arg.slice('--dest-root='.length);
    else if (arg === '--spawn-engine') opts.spawnEngine = true;
    else if (arg === '--no-spawn-engine') opts.spawnEngine = false;
    else if (arg === '--no-cleanup') opts.cleanup = false;
    else if (arg.startsWith('--timeout-ms=')) opts.jobTimeoutMs = Number(arg.slice('--timeout-ms='.length)) || opts.jobTimeoutMs;
  }
  return opts;
}

// ── main ─────────────────────────────────────────────────────────────────────

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  log(`engine: ${opts.engineUrl}`);
  log(`ps5:    ${opts.ps5Addr}`);
  const ps5MgmtAddr = mgmtAddrFor(opts.ps5Addr);
  log(`ps5 fs: ${ps5MgmtAddr}`);
  log(`dest:   ${opts.destRoot}`);

  const tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), 'ps5upload-smoke-'));
  log(`tmp:    ${tmpDir}`);

  let engineChild = null;

  try {
    // ── 1. Engine reachability ────────────────────────────────────────────
    if (opts.spawnEngine) {
      // Prefer the pre-built binary (fast); fall back to cargo run (slow but always works).
      const prebuilt = [
        path.join(repoRoot, 'engine', 'target', 'release', 'ps5upload-engine'),
        path.join(repoRoot, 'engine', 'target', 'debug', 'ps5upload-engine'),
      ];
      let engineBin = null;
      for (const p of prebuilt) {
        try { await fs.access(p, fs.constants?.X_OK ?? 1); engineBin = p; break; } catch {}
      }

      if (engineBin) {
        log(`spawning engine (pre-built: ${path.relative(repoRoot, engineBin)})...`);
        engineChild = spawn(engineBin, [], {
          env: { ...process.env, PS5_ADDR: opts.ps5Addr },
          stdio: ['ignore', 'pipe', 'pipe'],
        });
      } else {
        log('spawning engine (cargo run — may take a moment to compile)...');
        engineChild = spawn('cargo', ['run', '-p', 'ps5upload-engine'], {
          cwd: path.join(repoRoot, 'engine'),
          env: { ...process.env, PS5_ADDR: opts.ps5Addr },
          stdio: ['ignore', 'pipe', 'pipe'],
        });
      }
      engineChild.stdout?.on('data', (c) => process.stdout.write(String(c)));
      engineChild.stderr?.on('data', (c) => process.stderr.write(String(c)));
    }

    try {
      await waitForHttpOk(`${opts.engineUrl}/api/jobs`, 30_000);
      pass('engine reachable');
    } catch (e) {
      fail('engine reachable', e.message);
      log('cannot continue without engine — aborting');
      return;
    }

    // ── 2. PS5 status ─────────────────────────────────────────────────────
    try {
      const status = await getJson(`${opts.engineUrl}/api/ps5/status?addr=${encodeURIComponent(ps5MgmtAddr)}`);
      if (status && typeof status === 'object' && !status.error) {
        pass(`ps5 status (version: ${status.version ?? 'unknown'}, uptime: ${status.uptime_secs ?? '?'}s)`);
      } else {
        fail('ps5 status', `unexpected response: ${JSON.stringify(status)}`);
      }
    } catch (e) {
      fail('ps5 status', e.message);
      log('PS5 not reachable — file transfer tests will likely fail');
    }

    // ── 2b. PS5 volumes ──────────────────────────────────────────────────
    // Sanity check: payload enumerates at least /data. The PS5 mount table
    // is ~80 entries (system partitions + process sandboxes); we only care
    // about the "usable" subset for transfer-destination picking, so the
    // smoke output filters to non-placeholder and prints them aligned.
    try {
      const vols = await getJson(`${opts.engineUrl}/api/ps5/volumes?addr=${encodeURIComponent(ps5MgmtAddr)}`);
      const list = Array.isArray(vols?.volumes) ? vols.volumes : [];
      const usable = list.filter((v) => !v.is_placeholder);
      const placeholderCount = list.length - usable.length;
      const hasData = list.some((v) => v.path === '/data' && !v.is_placeholder);
      if (hasData) {
        pass(`ps5 volumes (${list.length} total, ${usable.length} real, ${placeholderCount} placeholders)`);
        const gib = (n) => `${(Number(n || 0) / (1024 ** 3)).toFixed(1)}G`;
        // Highlight drive-like mounts: actual user-visible storage. The PS5
        // mount table contains ~60 per-process sandbox bind mounts that are
        // noise for this report — filter by: non-placeholder, ≥ 1 GiB, and
        // not under `/mnt/sandbox/` (those are PID-namespace bind mounts).
        const drives = usable.filter((v) =>
          Number(v.total_bytes) >= 1024 ** 3 && !String(v.path).startsWith('/mnt/sandbox/'));
        if (drives.length > 0) {
          log('    drives:');
          for (const v of drives) {
            log(`      ${v.path.padEnd(18)} ${String(v.fs_type).padEnd(8)} ${gib(v.free_bytes).padStart(8)}/${gib(v.total_bytes).padStart(8)}  ${v.writable ? 'rw' : 'ro'}  <- ${v.mount_from}`);
          }
        } else {
          log('    (no /dev/-backed drives ≥ 1 GiB detected — M.2 likely not mounted)');
        }
      } else {
        fail('ps5 volumes', `/data not present in mount table`);
      }
    } catch (e) {
      fail('ps5 volumes', e.message);
    }

    // ── 3. Single-file transfer (256 KiB) ────────────────────────────────
    const singleFile = await makeSingleFile(tmpDir, 'smoke-single.bin', 256 * 1024);
    try {
      const created = await postJson(`${opts.engineUrl}/api/transfer/file`, {
        addr: opts.ps5Addr,
        dest: `${opts.destRoot}/smoke-single.bin`,
        src: singleFile,
      });
      const jobId = created?.job_id;
      if (!jobId) throw new Error(`no job_id in response: ${JSON.stringify(created)}`);

      const job = await pollJob(opts.engineUrl, jobId, opts.jobTimeoutMs);
      if (job.status === 'done') {
        pass(`single-file transfer (job ${jobId}, ${Number(job.elapsed_ms ?? 0)} ms)`);
      } else {
        fail('single-file transfer', `job ${jobId} failed: ${job.error ?? JSON.stringify(job)}`);
      }
    } catch (e) {
      fail('single-file transfer', e.message);
    }

    // ── 4. Multi-file directory transfer (8 × 32 KiB) ───────────────────
    const dirPath = await makeDir(tmpDir, 'smoke-dir', 8, 32 * 1024);
    try {
      const created = await postJson(`${opts.engineUrl}/api/transfer/dir`, {
        addr: opts.ps5Addr,
        dest_root: `${opts.destRoot}/smoke-dir`,
        src_dir: dirPath,
      });
      const jobId = created?.job_id;
      if (!jobId) throw new Error(`no job_id in response: ${JSON.stringify(created)}`);

      const job = await pollJob(opts.engineUrl, jobId, opts.jobTimeoutMs);
      if (job.status === 'done') {
        pass(`directory transfer (job ${jobId}, ${Number(job.elapsed_ms ?? 0)} ms)`);
      } else {
        fail('directory transfer', `job ${jobId} failed: ${job.error ?? JSON.stringify(job)}`);
      }
    } catch (e) {
      fail('directory transfer', e.message);
    }

    // ── 5. Multi-shard file transfer (4 MiB > default 1 MiB shard floor) ─
    const bigFile = await makeSingleFile(tmpDir, 'smoke-big.bin', 4 * 1024 * 1024);
    try {
      const created = await postJson(`${opts.engineUrl}/api/transfer/file`, {
        addr: opts.ps5Addr,
        dest: `${opts.destRoot}/smoke-big.bin`,
        src: bigFile,
      });
      const jobId = created?.job_id;
      if (!jobId) throw new Error(`no job_id in response: ${JSON.stringify(created)}`);

      const job = await pollJob(opts.engineUrl, jobId, opts.jobTimeoutMs);
      if (job.status === 'done') {
        const shards = Number(job.shards_sent ?? 0);
        pass(`multi-shard file transfer (job ${jobId}, ${shards} shards, ${Number(job.elapsed_ms ?? 0)} ms)`);
      } else {
        fail('multi-shard file transfer', `job ${jobId} failed: ${job.error ?? JSON.stringify(job)}`);
      }
    } catch (e) {
      fail('multi-shard file transfer', e.message);
    }

    // ── 6. File-list transfer (3 explicit src→dest pairs) ─────────────────
    try {
      const fileListSrcs = await Promise.all([
        makeSingleFile(tmpDir, 'fl-a.bin', 16 * 1024),
        makeSingleFile(tmpDir, 'fl-b.bin', 16 * 1024),
        makeSingleFile(tmpDir, 'fl-c.bin', 16 * 1024),
      ]);
      const created = await postJson(`${opts.engineUrl}/api/transfer/file-list`, {
        addr: opts.ps5Addr,
        dest_root: `${opts.destRoot}/smoke-filelist`,
        files: fileListSrcs.map((src, i) => ({
          src,
          dest: `file-list-${i}.bin`,
        })),
      });
      const jobId = created?.job_id;
      if (!jobId) throw new Error(`no job_id in response: ${JSON.stringify(created)}`);

      const job = await pollJob(opts.engineUrl, jobId, opts.jobTimeoutMs);
      if (job.status === 'done') {
        pass(`file-list transfer (job ${jobId}, ${Number(job.elapsed_ms ?? 0)} ms)`);
      } else {
        fail('file-list transfer', `job ${jobId} failed: ${job.error ?? JSON.stringify(job)}`);
      }
    } catch (e) {
      fail('file-list transfer', e.message);
    }

    // ── 7. Jobs list sanity ───────────────────────────────────────────────
    try {
      const jobs = await getJson(`${opts.engineUrl}/api/jobs`);
      if (Array.isArray(jobs) && jobs.length >= 1) {
        pass(`jobs list (${jobs.length} total)`);
      } else {
        fail('jobs list', `unexpected response: ${JSON.stringify(jobs)}`);
      }
    } catch (e) {
      fail('jobs list', e.message);
    }

  } finally {
    if (opts.cleanup) {
      try {
        const cleaned = await postJson(`${opts.engineUrl}/api/ps5/cleanup`, {
          addr: ps5MgmtAddr,
          path: opts.destRoot,
        });
        log(`PS5 cleanup removed ${cleaned?.removed_files ?? 0} files / ${cleaned?.removed_dirs ?? 0} dirs`);
      } catch (e) {
        log(`PS5 cleanup skipped/failed: ${e.message}`);
      }
      await fs.rm(tmpDir, { recursive: true, force: true });
    } else {
      log(`tmp dir preserved: ${tmpDir}`);
    }
    await stopChild(engineChild, 'engine');
  }

  // ── summary ──────────────────────────────────────────────────────────────
  const total = passCount + failCount;
  process.stdout.write(`\nsmoke result: ${passCount}/${total} passed`);
  if (failCount > 0) {
    process.stdout.write(`  (${failCount} FAILED)\n`);
    process.exitCode = 1;
  } else {
    process.stdout.write('  — all clear\n');
  }
}

main().catch((e) => {
  process.stderr.write(`[smoke] fatal: ${e.message}\n`);
  process.exitCode = 1;
});
