#!/usr/bin/env node
/**
 * scripts/gen-fixtures.mjs
 *
 * Generate local fixture trees consumed by bench/run-ftx2-sweep.mjs.
 * Profiles are declared in bench/profiles.mjs. Fixtures land under
 * bench/fixtures/<profile-name>/ (gitignored).
 *
 * Idempotent: each profile writes a `.manifest.json` describing the expected
 * shape (file count, byte total, seed). A profile is regenerated only if the
 * manifest is missing or mismatched, so re-running is cheap.
 *
 * Usage:
 *   node scripts/gen-fixtures.mjs               # default profiles only
 *   node scripts/gen-fixtures.mjs --xl          # include the 200k-file stress
 *   node scripts/gen-fixtures.mjs --force       # regenerate even if up-to-date
 *   node scripts/gen-fixtures.mjs --only=large-file,medium-dir
 *   node scripts/gen-fixtures.mjs --fixtures-dir=/path/to/elsewhere
 */

import fs from 'node:fs/promises';
import fsSync from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { PROFILES, profileBytesTotal, profileFileCount, selectProfiles, MIB } from '../bench/profiles.mjs';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, '..');

const MANIFEST_VERSION = 1;
const RANDOM_CHUNK = 8 * MIB; // generate huge files in 8 MiB chunks to bound memory

function log(msg) {
  process.stdout.write(`[gen-fixtures] ${msg}\n`);
}

function parseArgs(argv) {
  const opts = {
    xl: false,
    force: false,
    only: null,
    fixturesDir: path.join(repoRoot, 'bench', 'fixtures'),
  };
  for (const arg of argv) {
    if (arg === '--xl') opts.xl = true;
    else if (arg === '--force') opts.force = true;
    else if (arg.startsWith('--only=')) opts.only = arg.slice('--only='.length).split(',').map((s) => s.trim()).filter(Boolean);
    else if (arg.startsWith('--fixtures-dir=')) opts.fixturesDir = path.resolve(repoRoot, arg.slice('--fixtures-dir='.length));
    else if (arg === '--help' || arg === '-h') {
      printHelp();
      process.exit(0);
    } else {
      process.stderr.write(`[gen-fixtures] unknown arg: ${arg}\n`);
      process.exit(2);
    }
  }
  return opts;
}

function printHelp() {
  process.stdout.write(`gen-fixtures — create benchmark fixture trees

Options:
  --xl              include opt-in XL profiles (e.g. 200k-file stress)
  --force           regenerate even if the cached manifest is current
  --only=a,b,c      only generate these profile names
  --fixtures-dir=P  override output root (default: bench/fixtures)
  -h, --help        show this help
`);
}

function formatBytes(n) {
  if (n >= 1024 * 1024 * 1024) return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GiB`;
  if (n >= 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(2)} MiB`;
  if (n >= 1024) return `${(n / 1024).toFixed(2)} KiB`;
  return `${n} B`;
}

async function pathExists(p) {
  try {
    await fs.access(p);
    return true;
  } catch {
    return false;
  }
}

async function readManifest(dir) {
  try {
    const raw = await fs.readFile(path.join(dir, '.manifest.json'), 'utf8');
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

function profileShape(profile) {
  if (profile.kind === 'file') return { size: profile.size };
  if (profile.subdirs && profile.filesPerSubdir) {
    return {
      subdirs: profile.subdirs,
      files_per_subdir: profile.filesPerSubdir,
      file_size: profile.fileSize,
    };
  }
  return { file_count: profile.fileCount, file_size: profile.fileSize };
}

async function writeManifest(dir, profile) {
  const manifest = {
    version: MANIFEST_VERSION,
    name: profile.name,
    kind: profile.kind,
    file_count: profileFileCount(profile),
    bytes_total: profileBytesTotal(profile),
    shape: profileShape(profile),
    generated_at: new Date().toISOString(),
  };
  await fs.writeFile(path.join(dir, '.manifest.json'), JSON.stringify(manifest, null, 2) + '\n', 'utf8');
}

function manifestMatches(manifest, profile) {
  if (!manifest || manifest.version !== MANIFEST_VERSION) return false;
  if (manifest.name !== profile.name) return false;
  if (manifest.kind !== profile.kind) return false;
  if (manifest.file_count !== profileFileCount(profile)) return false;
  if (manifest.bytes_total !== profileBytesTotal(profile)) return false;
  // Shape check — every declared shape key must match; extra keys on the
  // stored manifest are OK (forward compat).
  const expected = profileShape(profile);
  for (const [k, v] of Object.entries(expected)) {
    if (manifest.shape?.[k] !== v) return false;
  }
  return true;
}

// ── fixture writers ──────────────────────────────────────────────────────────

/**
 * Stream `sizeBytes` of random data into `filePath`. For files larger than
 * RANDOM_CHUNK we refill the buffer in place, which caps peak memory at
 * ~8 MiB regardless of file size.
 */
async function writeRandomFile(filePath, sizeBytes) {
  const fh = await fs.open(filePath, 'w');
  try {
    if (sizeBytes === 0) return;
    const chunk = Buffer.allocUnsafe(Math.min(sizeBytes, RANDOM_CHUNK));
    let written = 0;
    while (written < sizeBytes) {
      const take = Math.min(sizeBytes - written, chunk.length);
      crypto.randomFillSync(chunk, 0, take);
      await fh.write(chunk, 0, take, null);
      written += take;
    }
  } finally {
    await fh.close();
  }
}

async function generateFileProfile(outDir, profile) {
  const filename = `${profile.name}.bin`;
  const target = path.join(outDir, filename);
  await writeRandomFile(target, profile.size);
  return { files: 1, bytes: profile.size };
}

/**
 * Generate `fileCount` files in `outDir` with bounded concurrency so a
 * 200k-file profile doesn't open 200k handles at once. Each file is named
 * `file_<NNNNNN>.bin`, left-padded to keep lexicographic order.
 */
/**
 * Enumerate every file the profile should emit as (relative path, size) tuples.
 * Flat dir  → `file_<NNNN>.bin`
 * Nested dir → `dir_<NN>/file_<NNNN>.bin`, balanced across subdirs.
 */
function enumerateDirFiles(profile) {
  const files = [];
  if (profile.subdirs && profile.filesPerSubdir) {
    const subWidth = String(profile.subdirs - 1).length;
    const fileWidth = String(profile.filesPerSubdir - 1).length;
    for (let s = 0; s < profile.subdirs; s++) {
      const sub = `dir_${String(s).padStart(subWidth, '0')}`;
      for (let f = 0; f < profile.filesPerSubdir; f++) {
        const name = `file_${String(f).padStart(fileWidth, '0')}.bin`;
        files.push({ rel: `${sub}/${name}`, size: profile.fileSize });
      }
    }
  } else {
    const width = String(profile.fileCount - 1).length;
    for (let i = 0; i < profile.fileCount; i++) {
      files.push({ rel: `file_${String(i).padStart(width, '0')}.bin`, size: profile.fileSize });
    }
  }
  return files;
}

async function generateDirProfile(outDir, profile, concurrency = 32) {
  const entries = enumerateDirFiles(profile);
  let next = 0;
  let writtenBytes = 0;
  const errors = [];
  const createdDirs = new Set();

  // Report progress every 5% for large dirs.
  const progressStep = Math.max(1, Math.floor(entries.length / 20));

  async function worker() {
    while (true) {
      const idx = next++;
      if (idx >= entries.length) return;
      const entry = entries[idx];
      const full = path.join(outDir, entry.rel);
      const parent = path.dirname(full);
      if (!createdDirs.has(parent)) {
        try {
          await fs.mkdir(parent, { recursive: true });
          createdDirs.add(parent);
        } catch (e) {
          errors.push({ idx, name: entry.rel, error: e.message });
          return;
        }
      }
      try {
        await writeRandomFile(full, entry.size);
        writtenBytes += entry.size;
      } catch (e) {
        errors.push({ idx, name: entry.rel, error: e.message });
        return;
      }
      if (entries.length >= 1000 && (idx + 1) % progressStep === 0) {
        const pct = Math.round(((idx + 1) / entries.length) * 100);
        log(`  ${profile.name}: ${idx + 1}/${entries.length} files (${pct}%)`);
      }
    }
  }

  await Promise.all(Array.from({ length: concurrency }, () => worker()));
  if (errors.length > 0) {
    throw new Error(`generateDirProfile ${profile.name}: ${errors.length} write errors (first: ${JSON.stringify(errors[0])})`);
  }
  return { files: entries.length, bytes: writtenBytes };
}

// ── driver ──────────────────────────────────────────────────────────────────

async function generateProfile(fixturesRoot, profile, opts) {
  const outDir = path.join(fixturesRoot, profile.name);
  const existing = await readManifest(outDir);
  if (!opts.force && manifestMatches(existing, profile)) {
    log(`skip   ${profile.name.padEnd(18)}  up-to-date (${profileFileCount(profile)} files, ${formatBytes(profileBytesTotal(profile))})`);
    return { profile, status: 'skipped' };
  }
  // Wipe and recreate — partial or stale content is a correctness hazard for
  // benchmarks, so we rebuild instead of patching.
  await fs.rm(outDir, { recursive: true, force: true });
  await fs.mkdir(outDir, { recursive: true });

  const t0 = Date.now();
  log(`build  ${profile.name.padEnd(18)}  ${profileFileCount(profile)} file(s), ${formatBytes(profileBytesTotal(profile))}`);
  let stats;
  if (profile.kind === 'file') {
    stats = await generateFileProfile(outDir, profile);
  } else {
    stats = await generateDirProfile(outDir, profile);
  }
  await writeManifest(outDir, profile);
  const secs = ((Date.now() - t0) / 1000).toFixed(2);
  log(`built  ${profile.name.padEnd(18)}  ${stats.files} file(s), ${formatBytes(stats.bytes)} in ${secs}s`);
  return { profile, status: 'generated', seconds: Number(secs) };
}

/**
 * Return the absolute source path a sweep runner should hand to the engine
 * for this profile. For `file` profiles this is the single fixture file; for
 * `dir` profiles this is the fixture directory.
 */
export function profileSourcePath(fixturesRoot, profile) {
  if (profile.kind === 'file') {
    return path.join(fixturesRoot, profile.name, `${profile.name}.bin`);
  }
  return path.join(fixturesRoot, profile.name);
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const selected = selectProfiles({
    only: opts.only,
    include: opts.xl ? ['xl'] : [],
  });
  if (selected.length === 0) {
    log('no profiles selected; check --only / --xl');
    process.exit(1);
  }
  await fs.mkdir(opts.fixturesDir, { recursive: true });
  log(`fixtures dir: ${opts.fixturesDir}`);
  log(`profiles: ${selected.map((p) => p.name).join(', ')}`);

  const results = [];
  for (const profile of selected) {
    results.push(await generateProfile(opts.fixturesDir, profile, opts));
  }

  const generated = results.filter((r) => r.status === 'generated').length;
  const skipped = results.filter((r) => r.status === 'skipped').length;
  log(`done — ${generated} generated, ${skipped} skipped, ${selected.length} total`);
}

// Allow this file to be imported by sweep runner for profileSourcePath.
if (import.meta.url === `file://${process.argv[1]}` ||
    path.resolve(process.argv[1] || '') === __filename) {
  main().catch((e) => {
    process.stderr.write(`[gen-fixtures] fatal: ${e.stack || e.message}\n`);
    process.exitCode = 1;
  });
}
