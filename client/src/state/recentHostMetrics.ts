import { create } from "zustand";

import { hostOf } from "../lib/addr";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/**
 * Per-host upload performance history.
 *
 * After every successful upload we record the host's measured
 * throughput and (when available) per-file commit cost. The next
 * upload's pre-flight ETA banner reads from this store instead of
 * the conservative defaults baked into `lib/uploadEta`, so the
 * estimate gets sharper the more a user uses the app against a
 * given PS5.
 *
 * Keyed on the host (no port) so a PS5 that gets a different
 * payload port — or a manual override — still maps to the same
 * history record.
 *
 * Persisted to localStorage. The entire blob is rewritten on every
 * record() call; the typical user has 1-3 hosts, total payload size
 * a few hundred bytes, so we don't bother with per-key writes.
 *
 * See `.claude/ralph-design-notes.md` §2.5 (gitignored) for the full
 * rationale and the §R4 red-team finding that motivated tracking
 * commit-ms in addition to raw throughput.
 */

const STORAGE_KEY = "ps5upload.recentHostMetrics";

export interface HostMetrics {
  /** Average MiB/s observed during the most recent successful
   *  upload's bytes-on-wire phase. Smoothed across the whole
   *  transfer, not a peak. */
  throughputMibps: number;
  /** Per-file commit time observed during the most recent successful
   *  upload's apply phase, in milliseconds. Only present for hosts
   *  whose uploads reported a finalize phase (derived from the
   *  console's durable-file counts). Undefined otherwise. */
  commitMsPerFile?: number;
  /** When the measurement was taken, ms-since-epoch. Used to age out
   *  very old records — a PS5 whose USB drive got swapped out between
   *  the measurement and now would otherwise carry a stale figure
   *  forever. The aging logic lives in the consumer; this store just
   *  records the timestamp honestly. */
  measuredAtMs: number;
}

interface RecentHostMetricsState {
  /** Map keyed on `hostOf(addr)`. */
  entries: Record<string, HostMetrics>;
  /** Look up metrics for a given addr (host:port). Returns undefined
   *  if no measurement exists. The caller decides whether the record
   *  is too old to trust. */
  lookup: (addr: string) => HostMetrics | undefined;
  /** Persist the measurements from a just-finished upload. Idempotent
   *  per (host, measuredAtMs) — calling twice in a row with the same
   *  timestamp overwrites the record (no append). */
  record: (addr: string, metrics: HostMetrics) => void;
  /** Drop all stored measurements. Exposed for the Settings screen's
   *  "reset upload history" affordance, and used by the tests. */
  clear: () => void;
}

function loadInitial(): Record<string, HostMetrics> {
  if (typeof window === "undefined") return {};
  try {
    const raw = safeGetItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
      return {};
    }
    const out: Record<string, HostMetrics> = {};
    for (const [host, val] of Object.entries(parsed as Record<string, unknown>)) {
      // Defensive validation — corrupt or pre-schema records get
      // dropped silently rather than crashing the load. Worst case
      // we lose a measurement and fall back to defaults; best case
      // every old build's record migrates cleanly.
      if (val === null || typeof val !== "object") continue;
      const m = val as Partial<HostMetrics>;
      if (typeof m.throughputMibps !== "number" || !Number.isFinite(m.throughputMibps)) {
        continue;
      }
      if (typeof m.measuredAtMs !== "number" || !Number.isFinite(m.measuredAtMs)) {
        continue;
      }
      out[host] = {
        throughputMibps: m.throughputMibps,
        // commitMsPerFile is optional; only carry it forward if present
        // AND a real number.
        commitMsPerFile:
          typeof m.commitMsPerFile === "number" && Number.isFinite(m.commitMsPerFile)
            ? m.commitMsPerFile
            : undefined,
        measuredAtMs: m.measuredAtMs,
      };
    }
    return out;
  } catch {
    return {};
  }
}

function persist(entries: Record<string, HostMetrics>): void {
  if (typeof window === "undefined") return;
  try {
    safeSetItem(STORAGE_KEY, JSON.stringify(entries));
  } catch {
    // localStorage write can fail under quota / private mode. The
    // store is a nice-to-have (ETA estimator already handles
    // missing measurements), so a write failure is silent rather
    // than throwing through the upload flow.
  }
}

export const useRecentHostMetricsStore = create<RecentHostMetricsState>(
  (set, get) => ({
    entries: loadInitial(),

    lookup(addr) {
      const host = hostOf(addr);
      return get().entries[host];
    },

    record(addr, metrics) {
      const host = hostOf(addr);
      set((s) => {
        const next = { ...s.entries, [host]: metrics };
        persist(next);
        return { entries: next };
      });
    },

    clear() {
      persist({});
      set({ entries: {} });
    },
  }),
);
