/**
 * Upload time-budget estimator.
 *
 * The Upload screen renders a pre-flight banner for large folders so
 * users see — *before* they click Upload — how long the transfer is
 * realistically going to take, including the post-100% PS5 commit
 * phase that motivated the v2.17.3/.5 fixes. The number is informative,
 * not contractual: real-world variance is large and depends on PS5
 * storage speed, network conditions, and source-disk speed. The
 * estimate's purpose is to set expectations so users don't force-quit
 * a 45-minute upload at the 30-minute mark thinking it's hung.
 *
 * The full design rationale, including why we surface a two-segment
 * breakdown instead of a single number, lives at
 * `.claude/ralph-design-notes.md` §2 (gitignored).
 */

/** Defaults used when no measurement is available yet.
 *
 *  `100 MiB/s` ≈ 80% of gigabit Ethernet's theoretical max once you
 *  account for FTX2 framing overhead, TCP windowing, and the PS5
 *  payload's per-shard processing cost. Wi-Fi typically runs 30-60.
 *  We start optimistic and self-correct downward as soon as we have
 *  one real measurement for the host.
 *
 *  `10 ms` per file is the band-midpoint for a healthy USB SSD. Sony
 *  internal NVMe is closer to 5 ms; slow USB sticks can be 25+. This
 *  is the metric most variable in practice; the per-host history
 *  store refines it after the first P3-aware upload to a host. */
export const DEFAULT_THROUGHPUT_MIBPS = 100;
export const DEFAULT_COMMIT_MS_PER_FILE = 10;

/** File-count threshold below which the banner is suppressed and the
 *  full-detail breakdown is unnecessary. The pre-flight banner is
 *  specifically for the "this is going to take a while, here's why"
 *  case — for a 200-file folder the user can just click Upload and
 *  wait a few seconds. Picked from the design's §2.4. */
export const BANNER_FILE_COUNT_THRESHOLD = 10_000;

/** Simplified banner band: folders here get a one-line "≈ N min"
 *  estimate without the segment breakdown. Above this we render the
 *  full transfer+commit table. */
export const SIMPLIFIED_BANNER_BAND_MIN = 1_000;

export interface UploadEta {
  /** Wall-clock seconds for the bytes-on-wire phase (source-read +
   *  network + payload spool, which overlap and run as one combined
   *  phase from the user's perspective). */
  transferSeconds: number;
  /** Wall-clock seconds for the post-100% PS5 commit phase, dominated
   *  by per-file inode-create + fsync cost. Strictly sequential with
   *  the transfer phase. */
  commitSeconds: number;
  /** Sum. Provided as a field rather than computed in the renderer
   *  so the math lives in one place. */
  totalSeconds: number;
  /** True if either `transferSeconds` or `commitSeconds` used a
   *  default fallback rather than a measured per-host figure. The
   *  banner uses this to show a "estimated — first upload to this
   *  PS5" qualifier so users don't read the number as a promise. */
  usedDefaults: boolean;
}

export interface UploadEtaInput {
  /** Total number of files in the source folder (post-exclude). */
  fileCount: number;
  /** Total bytes in the source folder (post-exclude). */
  totalBytes: number;
  /** Throughput observed in the most recent successful upload to
   *  this PS5 host, in MiB/sec. Falls back to `DEFAULT_THROUGHPUT_MIBPS`
   *  when undefined or non-finite. */
  throughputMibps?: number;
  /** Per-file commit cost observed on this host, in milliseconds.
   *  Falls back to `DEFAULT_COMMIT_MS_PER_FILE` when undefined or
   *  non-finite. */
  commitMsPerFile?: number;
}

/**
 * Compute the two-segment upload estimate.
 *
 * Implementation notes:
 *   - Empty folder (0 files / 0 bytes) → returns zeros. Caller should
 *     not be rendering the banner in that case anyway.
 *   - Negative inputs treated as zero (defensive — the engine has
 *     never produced these but `unknown`-typed inputs from older
 *     persisted state could).
 *   - Non-finite override values (NaN, Infinity from corrupted
 *     localStorage) trigger the default fallback for that field only.
 */
export function computeUploadEta(input: UploadEtaInput): UploadEta {
  const safeFileCount = Math.max(0, input.fileCount);
  const safeBytes = Math.max(0, input.totalBytes);

  const throughputMibps = isPositiveFinite(input.throughputMibps)
    ? input.throughputMibps!
    : DEFAULT_THROUGHPUT_MIBPS;
  const commitMsPerFile = isPositiveFinite(input.commitMsPerFile)
    ? input.commitMsPerFile!
    : DEFAULT_COMMIT_MS_PER_FILE;

  const transferSeconds =
    safeBytes === 0 ? 0 : safeBytes / 1024 / 1024 / throughputMibps;
  const commitSeconds = (safeFileCount * commitMsPerFile) / 1000;
  const totalSeconds = transferSeconds + commitSeconds;

  const usedDefaults =
    !isPositiveFinite(input.throughputMibps) ||
    !isPositiveFinite(input.commitMsPerFile);

  return {
    transferSeconds,
    commitSeconds,
    totalSeconds,
    usedDefaults,
  };
}

function isPositiveFinite(n: number | undefined): boolean {
  return typeof n === "number" && Number.isFinite(n) && n > 0;
}

/** Banner mode picker — drives which presentation to render.
 *
 *   "hidden"     : file count < SIMPLIFIED_BANNER_BAND_MIN. No banner.
 *   "simplified" : 1k ≤ file count < BANNER_FILE_COUNT_THRESHOLD.
 *                  One-liner "≈ N min" without the breakdown.
 *   "detailed"   : file count ≥ BANNER_FILE_COUNT_THRESHOLD.
 *                  Two-segment table (transfer + commit + total).
 *
 * The simplified band catches the medium-sized case where the upload
 * is non-trivial (a few minutes) but not "go make coffee" territory.
 * The detailed band is the Ghost-of-Yotei case where the user
 * absolutely needs to understand the full time budget — including
 * the structurally-fixed PS5 commit phase — before committing to it.
 */
export type BannerMode = "hidden" | "simplified" | "detailed";

export function pickBannerMode(fileCount: number): BannerMode {
  if (fileCount >= BANNER_FILE_COUNT_THRESHOLD) return "detailed";
  if (fileCount >= SIMPLIFIED_BANNER_BAND_MIN) return "simplified";
  return "hidden";
}

/** Human-readable elapsed-time formatter sympathetic to upload ETAs.
 *
 *   < 60 s          → "30 sec"
 *   < 60 min        → "12 min"
 *   < 24 h          → "1 h 23 min"
 *   else            → "26 h" (the file's-too-big-anyway band)
 *
 * Intentionally lossy at the seconds level past 1 minute — round to
 * the nearest minute. Promising "12 min 34 sec" implies precision the
 * estimate doesn't have.
 */
export function formatEtaSeconds(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "—";
  if (seconds < 60) return `${Math.round(seconds)} sec`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  const remMinutes = minutes % 60;
  if (hours < 24) {
    return remMinutes === 0 ? `${hours} h` : `${hours} h ${remMinutes} min`;
  }
  return `${hours} h`;
}
