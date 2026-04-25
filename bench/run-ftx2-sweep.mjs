#!/usr/bin/env node
/**
 * bench/run-ftx2-sweep.mjs
 *
 * Runs the FTX2 upload benchmark across every profile in bench/profiles.mjs,
 * with PS5-side cleanup before each profile and after the whole run. Writes
 * one timestamped JSON + one Markdown report under bench/reports/, and prints
 * a summary table to stdout.
 *
 * Prerequisites:
 *   - PS5 payload loaded and listening at PS5_ADDR (default 192.168.137.2:9113).
 *   - Fixture trees present under bench/fixtures/ (run scripts/gen-fixtures.mjs
 *     first, or pass --gen-fixtures).
 *   - Either engine already running, or pass --spawn-engine to launch it.
 *
 * Usage:
 *   node bench/run-ftx2-sweep.mjs --spawn-engine
 *   node bench/run-ftx2-sweep.mjs --xl --spawn-engine
 *   node bench/run-ftx2-sweep.mjs --only=large-file,medium-dir --spawn-engine
 *   node bench/run-ftx2-sweep.mjs --no-cleanup   # for debugging: leave PS5 state
 */

import fs from 'node:fs/promises';
import { existsSync } from 'node:fs';
import path from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

import { selectProfiles, profileBytesTotal, profileFileCount } from './profiles.mjs';
import { profileSourcePath } from '../scripts/gen-fixtures.mjs';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, '..');

const DEFAULT_ENGINE_URL = 'http://127.0.0.1:19113';
const DEFAULT_PS5_ADDR = '192.168.137.2:9113';
// Unified test sandbox — see tests/smoke-hardware.mjs for the shape.
// All sweep dest_root entries land under this root so `rm -rf
// /data/ps5upload/tests` wipes every test artifact in one shot.
const DEFAULT_DEST_ROOT = '/data/ps5upload/tests/sweep';
const DEFAULT_FIXTURES_DIR = path.join(repoRoot, 'bench', 'fixtures');
const DEFAULT_REPORTS_DIR = path.join(repoRoot, 'bench', 'reports');
const DEFAULT_JOB_TIMEOUT_MS = 30 * 60 * 1000;

function log(msg) {
  process.stdout.write(`[sweep] ${msg}\n`);
}

function logErr(msg) {
  process.stderr.write(`[sweep] ${msg}\n`);
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

function formatBytes(bytes) {
  const v = Number(bytes || 0);
  if (!Number.isFinite(v) || v <= 0) return '0 B';
  if (v >= 1024 ** 3) return `${(v / 1024 ** 3).toFixed(2)} GiB`;
  if (v >= 1024 ** 2) return `${(v / 1024 ** 2).toFixed(2)} MiB`;
  if (v >= 1024) return `${(v / 1024).toFixed(2)} KiB`;
  return `${v.toFixed(0)} B`;
}

function parseArgs(argv) {
  const opts = {
    engineUrl: DEFAULT_ENGINE_URL,
    ps5Addr: process.env.PS5_ADDR || DEFAULT_PS5_ADDR,
    destRoot: DEFAULT_DEST_ROOT,
    fixturesDir: DEFAULT_FIXTURES_DIR,
    reportsDir: DEFAULT_REPORTS_DIR,
    only: null,
    xl: false,
    spawnEngine: false,
    genFixtures: false,
    cleanup: true,
    jobTimeoutMs: DEFAULT_JOB_TIMEOUT_MS,
  };
  for (const arg of argv) {
    if (arg.startsWith('--engine-url=')) opts.engineUrl = arg.slice('--engine-url='.length);
    else if (arg.startsWith('--ps5-addr=')) opts.ps5Addr = arg.slice('--ps5-addr='.length);
    else if (arg.startsWith('--dest-root=')) opts.destRoot = arg.slice('--dest-root='.length);
    else if (arg.startsWith('--fixtures-dir=')) opts.fixturesDir = path.resolve(repoRoot, arg.slice('--fixtures-dir='.length));
    else if (arg.startsWith('--reports-dir=')) opts.reportsDir = path.resolve(repoRoot, arg.slice('--reports-dir='.length));
    else if (arg.startsWith('--only=')) opts.only = arg.slice('--only='.length).split(',').map((s) => s.trim()).filter(Boolean);
    else if (arg === '--xl') opts.xl = true;
    else if (arg === '--spawn-engine') opts.spawnEngine = true;
    else if (arg === '--gen-fixtures') opts.genFixtures = true;
    else if (arg === '--no-cleanup') opts.cleanup = false;
    else if (arg.startsWith('--timeout-ms=')) opts.jobTimeoutMs = Number(arg.slice('--timeout-ms='.length)) || opts.jobTimeoutMs;
    else if (arg === '-h' || arg === '--help') {
      printHelp();
      process.exit(0);
    } else {
      logErr(`unknown arg: ${arg}`);
      process.exit(2);
    }
  }
  return opts;
}

function printHelp() {
  process.stdout.write(`ftx2 sweep — run every bench profile against a live PS5

Options:
  --engine-url=URL       engine HTTP base URL  (default: http://127.0.0.1:19113)
  --ps5-addr=HOST:PORT   PS5 FTX2 address      (default: 192.168.137.2:9113)
  --dest-root=PATH       dest root on PS5      (default: /data/ps5upload-sweep)
  --fixtures-dir=PATH    fixture source root   (default: bench/fixtures)
  --reports-dir=PATH     where to write report (default: bench/reports)
  --only=a,b,c           only run these profiles
  --xl                   include the 200k-file stress profile
  --spawn-engine         launch ps5upload-engine (release) for this run
  --gen-fixtures         regenerate missing fixtures before starting
  --no-cleanup           skip PS5 cleanup (for debugging only)
  --timeout-ms=N         per-profile job timeout (default: 30 min)
`);
}

// ── HTTP helpers ──────────────────────────────────────────────────────────────

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

function tryParse(t) { try { return JSON.parse(t); } catch { return t; } }

function mgmtAddrFor(transferAddr) {
  const i = transferAddr.lastIndexOf(':');
  return i < 0 ? `${transferAddr}:9114` : `${transferAddr.slice(0, i)}:9114`;
}

async function waitForHttpOk(url, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(url);
      if (r.ok) return;
    } catch {}
    await sleep(250);
  }
  throw new Error(`timed out waiting for ${url}`);
}

async function pollJob(engineUrl, jobId, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const job = await getJson(`${engineUrl}/api/jobs/${jobId}`);
    if (job?.status === 'done' || job?.status === 'failed') return job;
    await sleep(500);
  }
  throw new Error(`job ${jobId} did not finish within ${timeoutMs} ms`);
}

// ── engine lifecycle ──────────────────────────────────────────────────────────

function spawnEngine(ps5Addr) {
  const release = path.join(repoRoot, 'engine', 'target', 'release', 'ps5upload-engine');
  let cmd, args;
  if (existsSync(release)) {
    cmd = release;
    args = [];
  } else {
    cmd = 'cargo';
    args = ['run', '--release', '-p', 'ps5upload-engine'];
  }
  const child = spawn(cmd, args, {
    cwd: path.join(repoRoot, 'engine'),
    env: { ...process.env, PS5_ADDR: ps5Addr },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout?.on('data', (c) => process.stdout.write(String(c)));
  child.stderr?.on('data', (c) => process.stderr.write(String(c)));
  return child;
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

// ── sweep ─────────────────────────────────────────────────────────────────────

async function psCleanup(engineUrl, ps5Addr, p) {
  try {
    const r = await postJson(`${engineUrl}/api/ps5/cleanup`, { addr: ps5Addr, path: p });
    return { ok: true, removed_files: r.removed_files ?? 0, removed_dirs: r.removed_dirs ?? 0 };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

async function runProfile(opts, profile) {
  const src = profileSourcePath(opts.fixturesDir, profile);
  if (!existsSync(src)) {
    return {
      profile: profile.name,
      status: 'error',
      error: `fixture missing: ${src} (run --gen-fixtures)`,
    };
  }

  const destRoot = `${opts.destRoot}/${profile.name}`;

  // Pre-run cleanup — drop any leftover from a previous sweep so timings
  // don't get polluted by delete-on-overwrite.
  if (opts.cleanup) {
    const pre = await psCleanup(opts.engineUrl, opts.ps5MgmtAddr, destRoot);
    if (!pre.ok) {
      log(`  ${profile.name}: pre-cleanup failed (${pre.error}) — proceeding anyway`);
    }
  }

  const reqBody = profile.kind === 'file'
    ? { addr: opts.ps5Addr, dest: `${destRoot}/${path.basename(src)}`, src }
    : { addr: opts.ps5Addr, dest_root: destRoot, src_dir: src };
  const route = profile.kind === 'file' ? '/api/transfer/file' : '/api/transfer/dir';

  const t0 = Date.now();
  const created = await postJson(`${opts.engineUrl}${route}`, reqBody);
  const jobId = created?.job_id;
  if (!jobId) {
    return { profile: profile.name, status: 'error', error: 'no job_id in engine response' };
  }
  let job;
  try {
    job = await pollJob(opts.engineUrl, jobId, opts.jobTimeoutMs);
  } catch (e) {
    return { profile: profile.name, status: 'error', error: e.message };
  }
  const wallMs = Date.now() - t0;

  if (job.status !== 'done') {
    const result = {
      profile: profile.name,
      status: 'failed',
      error: job.error || JSON.stringify(job),
      wall_ms: wallMs,
    };
    if (opts.cleanup) {
      await psCleanup(opts.engineUrl, opts.ps5MgmtAddr, destRoot);
    }
    return result;
  }

  const bytesSent = Number(job.bytes_sent || 0);
  const shardsSent = Number(job.shards_sent || 0);
  const elapsedMs = Number(job.elapsed_ms || wallMs);
  const mibPerSec = elapsedMs > 0 ? (bytesSent * 1000) / elapsedMs / (1024 * 1024) : 0;

  const result = {
    profile: profile.name,
    status: 'ok',
    kind: profile.kind,
    file_count: profileFileCount(profile),
    bytes_total: profileBytesTotal(profile),
    bytes_sent: bytesSent,
    shards_sent: shardsSent,
    job_id: jobId,
    tx_id_hex: job.tx_id_hex,
    elapsed_ms: elapsedMs,
    wall_ms: wallMs,
    mib_per_sec: mibPerSec,
    dest_root: destRoot,
    // PS5-side COMMIT_TX_ACK (includes timing_us breakdown and pool counters)
    commit_ack: job.commit_ack ?? null,
  };

  if (opts.cleanup) {
    const post = await psCleanup(opts.engineUrl, opts.ps5MgmtAddr, destRoot);
    result.cleanup = post;
  }
  return result;
}

// ── report writer ─────────────────────────────────────────────────────────────

function nowIsoCompact() {
  return new Date().toISOString().replaceAll(':', '').replace(/\.\d{3}Z$/, 'Z');
}

async function findPreviousReport(reportsDir) {
  try {
    const files = await fs.readdir(reportsDir);
    const jsons = files.filter((f) => f.endsWith('.json')).sort();
    if (jsons.length === 0) return null;
    const latest = path.join(reportsDir, jsons[jsons.length - 1]);
    const raw = await fs.readFile(latest, 'utf8');
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

function gitCommitHash() {
  try {
    const r = spawnSync('git', ['rev-parse', '--short', 'HEAD'], { cwd: repoRoot, encoding: 'utf8' });
    if (r.status === 0) return r.stdout.trim();
  } catch {}
  return null;
}

function hostInfo() {
  return {
    platform: process.platform,
    arch: process.arch,
    node: process.version,
  };
}

function fmtDelta(cur, prev) {
  if (prev == null || !Number.isFinite(prev) || prev === 0) return '';
  const delta = cur - prev;
  const pct = (delta / prev) * 100;
  const sign = delta >= 0 ? '+' : '';
  const mark = delta >= 0 ? '🟢' : '🔴';
  return ` ${mark} ${sign}${delta.toFixed(2)} MiB/s (${sign}${pct.toFixed(1)}%)`;
}

function renderMarkdown(report, previous) {
  const prevByName = new Map();
  if (previous?.results) {
    for (const r of previous.results) if (r.status === 'ok') prevByName.set(r.profile, r);
  }
  const lines = [];
  lines.push(`# FTX2 sweep report`);
  lines.push('');
  lines.push(`- **Timestamp**: ${report.created_at}`);
  lines.push(`- **PS5 address**: ${report.ps5_addr}`);
  if (report.git_commit) lines.push(`- **Git commit**: \`${report.git_commit}\``);
  lines.push(`- **Host**: ${report.host.platform}/${report.host.arch} (node ${report.host.node})`);
  if (previous?.created_at) lines.push(`- **Previous report**: ${previous.created_at}`);
  lines.push('');
  lines.push('| Profile | Kind | Files | Bytes | Time | Shards | Throughput | Δ vs prev |');
  lines.push('| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |');
  for (const r of report.results) {
    if (r.status === 'ok') {
      const prev = prevByName.get(r.profile);
      const delta = prev ? fmtDelta(r.mib_per_sec, prev.mib_per_sec) : '';
      lines.push(
        `| \`${r.profile}\` | ${r.kind} | ${r.file_count} | ${formatBytes(r.bytes_total)} | ${(r.elapsed_ms / 1000).toFixed(2)}s | ${r.shards_sent} | **${r.mib_per_sec.toFixed(2)} MiB/s** |${delta} |`
      );
    } else {
      lines.push(`| \`${r.profile}\` | — | — | — | — | — | **${r.status.toUpperCase()}** | ${r.error ?? ''} |`);
    }
  }
  lines.push('');

  // PS5-side payload timing — only emit rows that actually had a commit_ack
  // with a timing_us block. Keeps the table narrow and meaningful.
  const withTiming = report.results.filter((r) => r.status === 'ok' && r.commit_ack?.timing_us);
  if (withTiming.length > 0) {
    lines.push('## PS5-side timing (ms; packed-path values only populated for dir profiles)');
    lines.push('');
    lines.push('| Profile | recv | write_wait | apply | pack_records | pack_open | pack_write | pack_close |');
    lines.push('| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |');
    for (const r of withTiming) {
      const t = r.commit_ack.timing_us;
      const toMs = (us) => ((Number(us || 0)) / 1000).toFixed(1);
      lines.push(
        `| \`${r.profile}\` | ${toMs(t.recv)} | ${toMs(t.write_wait)} | ${toMs(t.apply)} | ${t.pack_records ?? 0} | ${toMs(t.pack_open)} | ${toMs(t.pack_write)} | ${toMs(t.pack_close)} |`
      );
    }
    lines.push('');
  }

  // Aggregate throughput: wall-time across all ok results.
  const ok = report.results.filter((r) => r.status === 'ok');
  const fails = report.results.filter((r) => r.status !== 'ok');
  const totalBytes = ok.reduce((a, r) => a + Number(r.bytes_sent), 0);
  const totalMs = ok.reduce((a, r) => a + Number(r.elapsed_ms), 0);
  const aggMib = totalMs > 0 ? (totalBytes * 1000) / totalMs / (1024 * 1024) : 0;
  lines.push('## Aggregate');
  lines.push('');
  lines.push(`- **OK profiles**: ${ok.length} / ${report.results.length}`);
  if (fails.length) lines.push(`- **Failed**: ${fails.map((f) => f.profile).join(', ')}`);
  lines.push(`- **Total bytes**: ${formatBytes(totalBytes)}`);
  lines.push(`- **Aggregate throughput (sum bytes / sum job time)**: ${aggMib.toFixed(2)} MiB/s`);
  lines.push(`- **Sweep wall time**: ${(report.sweep_wall_ms / 1000).toFixed(2)}s`);
  lines.push('');
  return lines.join('\n') + '\n';
}

// ── main ──────────────────────────────────────────────────────────────────────

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const selected = selectProfiles({
    only: opts.only,
    include: opts.xl ? ['xl'] : [],
  });
  if (selected.length === 0) {
    logErr('no profiles selected');
    process.exit(1);
  }

  log(`engine:    ${opts.engineUrl}`);
  log(`ps5:       ${opts.ps5Addr}`);
  opts.ps5MgmtAddr = mgmtAddrFor(opts.ps5Addr);
  log(`ps5 fs:    ${opts.ps5MgmtAddr}`);
  log(`dest root: ${opts.destRoot}`);
  log(`profiles:  ${selected.map((p) => p.name).join(', ')}`);

  let engineChild = null;
  const sweepT0 = Date.now();
  const results = [];
  try {
    if (opts.genFixtures) {
      log('generating missing fixtures via gen-fixtures.mjs (--force not passed; existing fixtures kept)');
      const code = await new Promise((resolve) => {
        const child = spawn(process.execPath,
          [path.join(repoRoot, 'scripts', 'gen-fixtures.mjs'), ...(opts.xl ? ['--xl'] : []),
           ...(opts.only ? [`--only=${opts.only.join(',')}`] : []),
           `--fixtures-dir=${opts.fixturesDir}`],
          { stdio: 'inherit' });
        child.on('exit', resolve);
      });
      if (code !== 0) throw new Error(`gen-fixtures exited with ${code}`);
    }

    if (opts.spawnEngine) {
      log('spawning engine');
      engineChild = spawnEngine(opts.ps5Addr);
    }
    await waitForHttpOk(`${opts.engineUrl}/api/jobs`, 60_000);
    log('engine reachable');

    // Verify PS5 is alive before doing anything destructive.
    try {
      const status = await getJson(`${opts.engineUrl}/api/ps5/status?addr=${encodeURIComponent(opts.ps5MgmtAddr)}`);
      log(`ps5 status ok (instance_id=${status?.instance_id ?? '?'})`);
    } catch (e) {
      logErr(`PS5 status check failed: ${e.message}`);
      throw e;
    }

    for (const profile of selected) {
      log(`── profile: ${profile.name} (${profile.kind}) ──`);
      const r = await runProfile(opts, profile);
      results.push(r);
      if (r.status === 'ok') {
        log(`  ${profile.name}: ${formatBytes(r.bytes_sent)} in ${(r.elapsed_ms / 1000).toFixed(2)}s = ${r.mib_per_sec.toFixed(2)} MiB/s (${r.shards_sent} shards)`);
      } else {
        logErr(`  ${profile.name}: ${r.status.toUpperCase()} — ${r.error ?? ''}`);
      }
    }

    // Final sweep-wide cleanup: wipe the entire dest_root so nothing lingers
    // even if a profile failed midway.
    if (opts.cleanup) {
      log('final cleanup');
      const fin = await psCleanup(opts.engineUrl, opts.ps5MgmtAddr, opts.destRoot);
      if (fin.ok) {
        log(`  removed ${fin.removed_files} files / ${fin.removed_dirs} dirs`);
      } else {
        logErr(`  final cleanup error: ${fin.error}`);
      }
    }

  } finally {
    await stopChild(engineChild, 'engine');
  }

  const sweepWallMs = Date.now() - sweepT0;
  const report = {
    schema_version: 1,
    created_at: new Date().toISOString(),
    ps5_addr: opts.ps5Addr,
    engine_url: opts.engineUrl,
    host: hostInfo(),
    git_commit: gitCommitHash(),
    sweep_wall_ms: sweepWallMs,
    results,
  };

  await fs.mkdir(opts.reportsDir, { recursive: true });
  // Find the previous report BEFORE we write the new one, otherwise
  // findPreviousReport would just pick up what we're about to write and every
  // delta column would read "0.00 MiB/s (+0.0%)".
  const previous = await findPreviousReport(opts.reportsDir);
  const stamp = nowIsoCompact();
  const jsonPath = path.join(opts.reportsDir, `${stamp}-sweep.json`);
  const mdPath = path.join(opts.reportsDir, `${stamp}-sweep.md`);
  await fs.writeFile(jsonPath, JSON.stringify(report, null, 2) + '\n', 'utf8');
  await fs.writeFile(mdPath, renderMarkdown(report, previous), 'utf8');

  log(`wrote ${path.relative(repoRoot, jsonPath)}`);
  log(`wrote ${path.relative(repoRoot, mdPath)}`);
  log(`sweep done in ${(sweepWallMs / 1000).toFixed(2)}s`);

  // Print the markdown table straight to stdout too, so piping to stdout is
  // a useful preview for anyone without a markdown viewer handy.
  process.stdout.write('\n');
  process.stdout.write(renderMarkdown(report, previous));

  const failed = results.filter((r) => r.status !== 'ok');
  if (failed.length > 0) process.exitCode = 1;
}

main().catch((e) => {
  process.stderr.write(`[sweep] fatal: ${e.stack || e.message}\n`);
  process.exitCode = 1;
});
