/**
 * The last copy of the helper's own logs we managed to read, per console.
 *
 * Why this exists: a bug report collects the payload's logs LIVE, over the
 * payload's own RPC. When the helper is the thing that is failing, that is
 * exactly the moment it cannot answer — so the one report that needed those
 * logs shipped without them. Measured on a FW 12.70 report: the helper came up,
 * died 7.6 s later, and the report was generated in the same second, so
 * `payload_log_files` was empty and every hardware field was null, while the
 * bundle's README promised `ps5/payload-logs/` as "best for helper crashes".
 *
 * The files themselves survive the crash on the console: `stderr.log.old` is
 * the previous instance's stderr and `crash.log` its fatal breadcrumb. So a
 * read taken as soon as a NEW instance is up carries the old one's last words,
 * and keeping that read here means a later report can include it even when
 * the helper is down again by then.
 *
 * In memory only, on purpose: up to 14 files of up to 256 KB is more than
 * browser storage should hold, and the capture is retaken on every reconnect.
 */

export interface BlackBoxFile {
  name: string;
  text: string;
}

export interface BlackBoxCapture {
  host: string;
  /** ISO time of the read, so a report can say how old the copy is. */
  capturedAt: string;
  files: BlackBoxFile[];
}

const captures = new Map<string, BlackBoxCapture>();

/** Record a read. An empty read never replaces a good one: a helper that died
 *  mid-capture must not erase the evidence of the previous death. */
export function recordBlackBox(
  host: string,
  files: BlackBoxFile[],
  now: Date = new Date(),
): void {
  if (!host || files.length === 0) return;
  captures.set(host, { host, capturedAt: now.toISOString(), files });
}

export function blackBoxFor(host: string): BlackBoxCapture | null {
  return captures.get(host) ?? null;
}

/** Test seam. */
export function clearBlackBoxes(): void {
  captures.clear();
}

/** Minimum gap between captures for one console. A helper that flaps every few
 *  seconds must not be read on every edge — but each edge IS a new instance
 *  with a new predecessor to explain, so this stays short. */
export const BLACK_BOX_COOLDOWN_MS = 20_000;

const lastAttempt = new Map<string, number>();

/** True when a capture for `host` should start now, and marks the attempt. */
export function shouldCaptureBlackBox(host: string, nowMs: number): boolean {
  const last = lastAttempt.get(host);
  if (last !== undefined && nowMs - last < BLACK_BOX_COOLDOWN_MS) return false;
  lastAttempt.set(host, nowMs);
  return true;
}

/** Test seam. */
export function resetBlackBoxCooldowns(): void {
  lastAttempt.clear();
}
