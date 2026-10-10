import type { ActivityEntry, ActivityKind } from "../../state/activityHistory";

/** Bytes that went from this computer to a console. Every upload, including the
 *  queue's (which is how most uploads run), lands in one of these. */
const UPLOAD_KINDS: ReadonlySet<ActivityKind> = new Set<ActivityKind>([
  "upload",
  "upload-dir",
  "upload-reconcile",
  "upload-queue",
]);

/** Bytes that came from a console to this computer. */
const DOWNLOAD_KINDS: ReadonlySet<ActivityKind> = new Set<ActivityKind>([
  "download",
  "library-download",
]);

/** Whether an entry moved bytes over the network between this computer and a
 *  console. Copies and moves on the console, and installs, run at the console's
 *  disk speed, so they don't belong in a transfer-speed ranking. */
export function isNetworkTransfer(kind: ActivityKind): boolean {
  return UPLOAD_KINDS.has(kind) || DOWNLOAD_KINDS.has(kind);
}

export interface ComputedStats {
  totalOps: number;
  succeededOps: number;
  failedOps: number;
  uploadedBytes: number;
  downloadedBytes: number;
  fastestMbps: number | null;
  fastestLabel: string | null;
  averageDurationMs: number | null;
  last30Days: { date: string; count: number; bytes: number }[];
  kindCounts: Record<string, number>;
  topTransfers: { label: string; bytes: number; mbps: number; whenMs: number }[];
}

export function computeStats(
  entries: ActivityEntry[],
  now: Date = new Date(),
): ComputedStats {
  let succeededOps = 0;
  let failedOps = 0;
  let uploadedBytes = 0;
  let downloadedBytes = 0;
  let durationsSum = 0;
  let durationsCount = 0;
  const kindCounts: Record<string, number> = {};
  const topTransfersAll: ComputedStats["topTransfers"] = [];

  for (const e of entries) {
    if (e.outcome === "done") succeededOps++;
    else if (e.outcome === "failed" || e.outcome === "stopped") failedOps++;
    kindCounts[e.kind] = (kindCounts[e.kind] ?? 0) + 1;
    if (e.endedAtMs && e.startedAtMs) {
      durationsSum += e.endedAtMs - e.startedAtMs;
      durationsCount++;
    }
    const bytes = e.bytes ?? 0;
    if (UPLOAD_KINDS.has(e.kind)) uploadedBytes += bytes;
    else if (DOWNLOAD_KINDS.has(e.kind)) downloadedBytes += bytes;
    // MiB/s for finished network transfers over 1 MiB.
    if (
      isNetworkTransfer(e.kind) &&
      e.outcome === "done" &&
      bytes > 1024 * 1024 &&
      e.endedAtMs &&
      e.startedAtMs
    ) {
      const seconds = (e.endedAtMs - e.startedAtMs) / 1000;
      if (seconds > 0) {
        topTransfersAll.push({
          label: e.label,
          bytes,
          mbps: bytes / seconds / 1024 / 1024,
          whenMs: e.endedAtMs,
        });
      }
    }
  }
  topTransfersAll.sort((a, b) => b.mbps - a.mbps);
  const fastest = topTransfersAll[0];

  // Daily breakdown for the last 30 days.
  const todayStart = new Date(now);
  todayStart.setHours(0, 0, 0, 0);
  const last30Days: ComputedStats["last30Days"] = [];
  for (let i = 29; i >= 0; i--) {
    const d = new Date(todayStart);
    d.setDate(todayStart.getDate() - i);
    last30Days.push({
      date: `${d.getMonth() + 1}/${d.getDate()}`,
      count: 0,
      bytes: 0,
    });
  }
  for (const e of entries) {
    // Bucket by calendar day, matching the labels above. Snapping both ends to
    // local midnight and rounding absorbs a DST day of 23 or 25 hours.
    const entryDay = new Date(e.startedAtMs);
    entryDay.setHours(0, 0, 0, 0);
    const daysAgo = Math.round(
      (todayStart.getTime() - entryDay.getTime()) / 86_400_000,
    );
    const offset = 29 - daysAgo;
    if (offset >= 0 && offset < 30) {
      last30Days[offset].count++;
      last30Days[offset].bytes += e.bytes ?? 0;
    }
  }

  return {
    totalOps: entries.length,
    succeededOps,
    failedOps,
    uploadedBytes,
    downloadedBytes,
    fastestMbps: fastest ? fastest.mbps : null,
    fastestLabel: fastest ? fastest.label : null,
    averageDurationMs: durationsCount > 0 ? durationsSum / durationsCount : null,
    last30Days,
    kindCounts,
    topTransfers: topTransfersAll.slice(0, 5),
  };
}

/** RFC 4180 CSV escape: wrap any field containing comma, quote, or
 *  newline in double quotes; escape inner quotes by doubling. */
function csvEscape(v: string | number | null | undefined): string {
  if (v === null || v === undefined) return "";
  const s = String(v);
  if (/[",\n\r]/.test(s)) {
    return `"${s.replace(/"/g, '""')}"`;
  }
  return s;
}

/** Activity entries → RFC 4180 CSV, one row per entry. */
export function activityToCsv(entries: ActivityEntry[]): string {
  const header = [
    "id",
    "kind",
    "label",
    "outcome",
    "started_at_iso",
    "ended_at_iso",
    "duration_ms",
    "bytes",
    "files",
    "from_path",
    "to_path",
    "error",
  ].join(",");
  const rows = entries.map((e) => {
    const startedIso = new Date(e.startedAtMs).toISOString();
    const endedIso = e.endedAtMs ? new Date(e.endedAtMs).toISOString() : "";
    const duration = e.endedAtMs ? e.endedAtMs - e.startedAtMs : "";
    return [
      csvEscape(e.id),
      csvEscape(e.kind),
      csvEscape(e.label),
      csvEscape(e.outcome),
      csvEscape(startedIso),
      csvEscape(endedIso),
      csvEscape(duration),
      csvEscape(e.bytes ?? ""),
      csvEscape(e.files ?? ""),
      csvEscape(e.fromPath ?? ""),
      csvEscape(e.toPath ?? ""),
      csvEscape(e.error ?? ""),
    ].join(",");
  });
  return [header, ...rows].join("\r\n") + "\r\n";
}
