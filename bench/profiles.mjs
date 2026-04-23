// Shared profile definitions for the FTX2 sweep.
//
// Kept in one file so gen-fixtures and run-ftx2-sweep stay in lockstep.
// Add a profile by appending to PROFILES; give it a unique `name` and at most
// one tag in the opt-in tag set below. `bytesTotal` is derived (not declared)
// so sizes stay correct if the raw numbers change.

export const KIB = 1024;
export const MIB = 1024 * KIB;
export const GIB = 1024 * MIB;

/**
 * Profile shape:
 *   name        — URL-safe identifier, also used as the fixture subdir name
 *   kind        — 'file' or 'dir'
 *   size        — single-file byte size (kind='file')
 *   fileCount   — number of files (kind='dir')
 *   fileSize    — per-file byte size (kind='dir')
 *   tags        — optional; profiles tagged 'xl' are skipped unless --xl is passed
 *
 * Sizing note: default profiles total ~1.4 GiB of local fixture data. The XL
 * profile adds ~800 MiB. Both are generated on demand and gitignored.
 */
export const PROFILES = [
  // ── Single-file workloads ──────────────────────────────────────────────────
  {
    name: 'tiny-file',
    kind: 'file',
    size: 4 * KIB,
    note: 'latency floor; persistent-fd path below writer-thread threshold',
  },
  {
    name: 'small-file',
    kind: 'file',
    size: 1 * MIB,
    note: 'small single-shard direct-mode',
  },
  {
    name: 'medium-file',
    kind: 'file',
    size: 32 * MIB,
    note: 'exactly one shard at current shard size',
  },
  {
    name: 'large-file',
    kind: 'file',
    size: 128 * MIB,
    note: 'previous single-file baseline; 4 shards; persistent-fd win lives here',
  },
  {
    name: 'huge-file',
    kind: 'file',
    size: 1 * GIB,
    note: 'deep persistent-writer test, 32 shards',
  },

  // ── Directory workloads ───────────────────────────────────────────────────
  {
    name: 'small-dir',
    kind: 'dir',
    fileCount: 8,
    fileSize: 32 * KIB,
    note: 'spool-path smoke',
  },
  {
    name: 'medium-dir',
    kind: 'dir',
    fileCount: 32,
    fileSize: 4 * MIB,
    note: 'previous directory baseline',
  },
  {
    name: 'many-small-dir',
    kind: 'dir',
    fileCount: 5000,
    fileSize: 16 * KIB,
    note: 'flat game-dir tiny-file throughput (worst case for dir-lock contention)',
  },
  {
    name: 'nested-small-dir',
    kind: 'dir',
    subdirs: 50,
    filesPerSubdir: 100,
    fileSize: 16 * KIB,
    note: 'realistic game-dir layout (5000 files across 50 subdirs) — lets the pool parallelise across dir locks',
  },

  // ── Opt-in stress profile ─────────────────────────────────────────────────
  {
    name: 'xl-stress-dir',
    kind: 'dir',
    fileCount: 200_000,
    fileSize: 4 * KIB,
    tags: ['xl'],
    note: 'full 200k-file ambitious stress; use --xl to include',
  },
];

export const ALL_TAGS = ['xl'];

export function profileBytesTotal(p) {
  if (p.kind === 'file') return p.size;
  if (p.kind === 'dir') {
    if (p.subdirs && p.filesPerSubdir) return p.subdirs * p.filesPerSubdir * p.fileSize;
    return p.fileCount * p.fileSize;
  }
  throw new Error(`profileBytesTotal: unknown kind ${p.kind}`);
}

export function profileFileCount(p) {
  if (p.kind === 'file') return 1;
  if (p.kind === 'dir') {
    if (p.subdirs && p.filesPerSubdir) return p.subdirs * p.filesPerSubdir;
    return p.fileCount;
  }
  throw new Error(`profileFileCount: unknown kind ${p.kind}`);
}

/**
 * Filter profiles by include/exclude rules.
 *
 *   opts.only     — array of names; others dropped
 *   opts.include  — array of tags to enable (default: none; excludes 'xl')
 *   opts.exclude  — array of names to drop
 */
export function selectProfiles(opts = {}) {
  const { only = null, include = [], exclude = [] } = opts;
  const enabledTags = new Set(include);
  return PROFILES.filter((p) => {
    if (only && !only.includes(p.name)) return false;
    if (exclude.includes(p.name)) return false;
    if (Array.isArray(p.tags)) {
      // Skip profile unless at least one of its tags is explicitly enabled.
      if (!p.tags.some((t) => enabledTags.has(t))) return false;
    }
    return true;
  });
}
