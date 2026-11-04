import { safeGetItem, safeSetItem } from "./safeStorage";

/**
 * The browser's copy of the desktop app's resume-state file: which job id a single-file upload ran
 * under. Without it, a file re-added after a failed upload ran as a new job, and the free-space
 * check asked for room for the whole file while the kept partial still took up that room (#401).
 * Same key and lifetime as `commands/persistence.rs`.
 */
const KEY = "ps5upload.resume_txids.v1";
const TTL_MS = 7 * 24 * 60 * 60 * 1000;
const MAX_RECORDS = 256;

interface ResumeRecord {
  tx_id_hex: string;
  mode: string;
  last_seen_ms: number;
}

export interface ResumeKey {
  host: string;
  src: string;
  dest: string;
}

const keyOf = (k: ResumeKey) => `${k.host}|${k.src}|${k.dest}`;

function load(now: number): Record<string, ResumeRecord> {
  try {
    const all = JSON.parse(safeGetItem(KEY) ?? "{}") as Record<
      string,
      ResumeRecord
    >;
    const live: Record<string, ResumeRecord> = {};
    for (const [k, r] of Object.entries(all ?? {})) {
      if (typeof r?.tx_id_hex === "string" && now - r.last_seen_ms <= TTL_MS)
        live[k] = r;
    }
    return live;
  } catch {
    return {};
  }
}

function save(all: Record<string, ResumeRecord>): void {
  const newest = Object.entries(all)
    .sort((a, b) => b[1].last_seen_ms - a[1].last_seen_ms)
    .slice(0, MAX_RECORDS);
  safeSetItem(KEY, JSON.stringify(Object.fromEntries(newest)));
}

export function resumeLookup(
  k: ResumeKey,
  now = Date.now(),
): { tx_id_hex: string | null } {
  return { tx_id_hex: load(now)[keyOf(k)]?.tx_id_hex ?? null };
}

export function resumeRemember(
  k: ResumeKey & { tx_id_hex: string; mode?: string },
  now = Date.now(),
): void {
  const all = load(now);
  all[keyOf(k)] = {
    tx_id_hex: k.tx_id_hex,
    mode: k.mode ?? "file",
    last_seen_ms: now,
  };
  save(all);
}

export function resumeForget(k: ResumeKey, now = Date.now()): void {
  const all = load(now);
  delete all[keyOf(k)];
  save(all);
}
