import { useMemo } from "react";
import {
  BarChart3,
  ArrowUp,
  ArrowDown,
  Clock,
  Trophy,
  Download,
} from "lucide-react";
import { useActivityHistoryStore } from "../../state/activityHistory";
import type { ActivityEntry } from "../../state/activityHistory";
import { PageHeader, Button } from "../../components";
import { useTr } from "../../state/lang";
import { formatBytes } from "../../lib/format";

/**
 * Activity stats dashboard.
 *
 * Aggregates the existing localStorage-persisted activityHistory
 * entries — no payload calls, no network. Surfaces:
 *   - Operations per day for the last 30 days (mini bar chart, ASCII)
 *   - Total bytes uploaded / downloaded (lifetime)
 *   - Fastest single transfer (peak MB/s)
 *   - Most-active types
 *   - Average operation duration
 *
 * Why this lives separately from the Activity tab: Activity shows
 * the running roster + recent events. Stats shows aggregate
 * patterns ("am I uploading more this month than last?"). Different
 * lens on the same data.
 */
export default function StatsScreen() {
  const tr = useTr();
  const entries = useActivityHistoryStore((s) => s.entries);

  const stats = useMemo(() => computeStats(entries), [entries]);

  async function exportCsv() {
    if (entries.length === 0) return;
    const { save } = await import("@tauri-apps/plugin-dialog");
    const { writeTextFile } = await import("@tauri-apps/plugin-fs");
    const dest = await save({
      defaultPath: `ps5upload-activity-${Date.now()}.csv`,
      filters: [{ name: "CSV", extensions: ["csv"] }],
    });
    if (!dest || typeof dest !== "string") return;
    const csv = activityToCsv(entries);
    await writeTextFile(dest, csv);
  }

  return (
    <div className="p-6">
      <PageHeader
        icon={BarChart3}
        title={tr("stats_title", undefined, "Activity stats")}
        description={tr(
          "stats_description",
          undefined,
          "Aggregates of every transfer + Library operation since this app was first launched. All from local history; nothing leaves your machine.",
        )}
        right={
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<Download size={12} />}
            onClick={exportCsv}
            disabled={entries.length === 0}
          >
            {tr("stats_export_csv", undefined, "Export CSV")}
          </Button>
        }
      />

      <div className="mx-auto max-w-4xl space-y-4">
        {entries.length === 0 ? (
          <div className="rounded-md border border-dashed border-[var(--color-border)] p-6 text-center text-xs text-[var(--color-muted)]">
            {tr(
              "stats_empty",
              undefined,
              "No activity recorded yet. Stats will appear after your first upload, download, or Library operation.",
            )}
          </div>
        ) : (
          <>
            <KpiRow stats={stats} />
            <DailyChart days={stats.last30Days} />
            <KindBreakdown counts={stats.kindCounts} total={entries.length} />
            {stats.topTransfers.length > 0 && (
              <TopTransfers transfers={stats.topTransfers} />
            )}
          </>
        )}
      </div>
    </div>
  );
}

interface ComputedStats {
  totalOps: number;
  succeededOps: number;
  failedOps: number;
  uploadedBytes: number;
  downloadedBytes: number;
  fastestUploadMbps: number | null;
  fastestUploadLabel: string | null;
  averageDurationMs: number | null;
  last30Days: { date: string; count: number; bytes: number }[];
  kindCounts: Record<string, number>;
  topTransfers: { label: string; bytes: number; mbps: number; whenMs: number }[];
}

function computeStats(entries: ActivityEntry[]): ComputedStats {
  let succeededOps = 0;
  let failedOps = 0;
  let uploadedBytes = 0;
  let downloadedBytes = 0;
  let durationsSum = 0;
  let durationsCount = 0;
  let fastestUploadMbps = 0;
  let fastestUploadLabel = "";
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
    if (e.kind === "upload" || e.kind === "upload-dir" || e.kind === "upload-reconcile") {
      uploadedBytes += bytes;
    } else if (e.kind === "download" || e.kind === "library-download") {
      downloadedBytes += bytes;
    }
    // Compute MB/s for finished transfers > 1 MB.
    if (
      e.outcome === "done" &&
      bytes > 1024 * 1024 &&
      e.endedAtMs &&
      e.startedAtMs
    ) {
      const seconds = (e.endedAtMs - e.startedAtMs) / 1000;
      if (seconds > 0) {
        const mbps = bytes / seconds / 1024 / 1024;
        topTransfersAll.push({
          label: e.label,
          bytes,
          mbps,
          whenMs: e.endedAtMs,
        });
        if (mbps > fastestUploadMbps) {
          fastestUploadMbps = mbps;
          fastestUploadLabel = e.label;
        }
      }
    }
  }
  topTransfersAll.sort((a, b) => b.mbps - a.mbps);

  // Daily breakdown for the last 30 days.
  const todayStart = new Date();
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
    // Bucket by CALENDAR day, matching how the labels above are built. The
    // old fixed-86,400,000-ms division drifted from local midnight whenever
    // the 30-day window crossed a DST change (a local day is 23h or 25h),
    // misattributing entries near a day boundary by ±1 day. Snapping both
    // ends to local midnight and Math.round-ing absorbs the DST hour.
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
    fastestUploadMbps: fastestUploadMbps > 0 ? fastestUploadMbps : null,
    fastestUploadLabel: fastestUploadLabel || null,
    averageDurationMs: durationsCount > 0 ? durationsSum / durationsCount : null,
    last30Days,
    kindCounts,
    topTransfers: topTransfersAll.slice(0, 5),
  };
}

function KpiRow({ stats }: { stats: ComputedStats }) {
  const tr = useTr();
  return (
    <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
      <KpiCard
        icon={<ArrowUp size={14} />}
        label={tr("stats_uploaded", undefined, "Uploaded")}
        value={formatBytes(stats.uploadedBytes)}
      />
      <KpiCard
        icon={<ArrowDown size={14} />}
        label={tr("stats_downloaded", undefined, "Downloaded")}
        value={formatBytes(stats.downloadedBytes)}
      />
      <KpiCard
        icon={<Trophy size={14} />}
        label={tr("stats_fastest", undefined, "Fastest")}
        value={
          stats.fastestUploadMbps !== null
            ? `${stats.fastestUploadMbps.toFixed(1)} MB/s`
            : "—"
        }
        sub={stats.fastestUploadLabel ?? undefined}
      />
      <KpiCard
        icon={<Clock size={14} />}
        label={tr("stats_avg_duration", undefined, "Avg duration")}
        value={
          stats.averageDurationMs !== null
            ? formatDuration(stats.averageDurationMs)
            : "—"
        }
      />
    </div>
  );
}

function KpiCard({
  icon,
  label,
  value,
  sub,
}: {
  icon: React.ReactNode;
  label: string;
  value: string;
  sub?: string;
}) {
  return (
    <div className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3">
      <div className="flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wide text-[var(--color-muted)]">
        {icon}
        {label}
      </div>
      <div className="mt-1 text-base font-semibold tabular-nums">{value}</div>
      {sub && (
        <div className="truncate text-[10px] text-[var(--color-muted)]" title={sub}>
          {sub}
        </div>
      )}
    </div>
  );
}

function DailyChart({
  days,
}: {
  days: { date: string; count: number; bytes: number }[];
}) {
  const tr = useTr();
  const maxCount = Math.max(...days.map((d) => d.count), 1);
  return (
    <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <h3 className="mb-3 text-sm font-semibold">
        {tr("stats_daily", undefined, "Operations per day (last 30)")}
      </h3>
      <div className="flex items-end gap-0.5 px-1" style={{ height: "120px" }}>
        {days.map((d) => {
          const heightPct = (d.count / maxCount) * 100;
          return (
            <div
              key={d.date}
              className="group relative flex-1"
              title={`${d.date}: ${d.count} operations · ${formatBytes(d.bytes)}`}
            >
              <div
                className="rounded-t bg-[var(--color-accent)] transition-all hover:opacity-80"
                style={{
                  height: `${Math.max(heightPct, 2)}%`,
                  minHeight: d.count > 0 ? "2px" : "0",
                }}
              />
            </div>
          );
        })}
      </div>
      <div className="mt-2 flex justify-between text-[10px] text-[var(--color-muted)]">
        <span>{days[0]?.date}</span>
        <span>{days[Math.floor(days.length / 2)]?.date}</span>
        <span>{days[days.length - 1]?.date}</span>
      </div>
    </section>
  );
}

function KindBreakdown({
  counts,
  total,
}: {
  counts: Record<string, number>;
  total: number;
}) {
  const tr = useTr();
  const sorted = Object.entries(counts).sort((a, b) => b[1] - a[1]);
  return (
    <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <h3 className="mb-3 text-sm font-semibold">
        {tr("stats_breakdown", undefined, "Operation breakdown")}
      </h3>
      <ul className="space-y-1">
        {sorted.map(([kind, count]) => {
          const pct = (count / total) * 100;
          return (
            <li key={kind} className="text-xs">
              <div className="flex items-center justify-between">
                <span className="font-mono">{kind}</span>
                <span className="tabular-nums text-[var(--color-muted)]">
                  {count} ({pct.toFixed(0)}%)
                </span>
              </div>
              <div className="mt-0.5 h-1 rounded-full bg-[var(--color-surface-3)]">
                <div
                  className="h-1 rounded-full bg-[var(--color-accent)]"
                  style={{ width: `${pct}%` }}
                />
              </div>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

function TopTransfers({
  transfers,
}: {
  transfers: ComputedStats["topTransfers"];
}) {
  const tr = useTr();
  return (
    <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <h3 className="mb-3 text-sm font-semibold">
        {tr("stats_top_transfers", undefined, "Fastest transfers")}
      </h3>
      <table className="w-full text-xs">
        <thead className="text-[var(--color-muted)]">
          <tr>
            <th className="px-1 py-0.5 text-left">{tr("stats_label", undefined, "Label")}</th>
            <th className="px-1 py-0.5 text-right">{tr("stats_size", undefined, "Size")}</th>
            <th className="px-1 py-0.5 text-right">{tr("stats_speed", undefined, "Speed")}</th>
            <th className="px-1 py-0.5 text-right">{tr("stats_when", undefined, "When")}</th>
          </tr>
        </thead>
        <tbody>
          {transfers.map((t) => (
            <tr key={`${t.label}-${t.whenMs}`} className="border-t border-[var(--color-border)]">
              <td className="truncate px-1 py-0.5">{t.label}</td>
              <td className="px-1 py-0.5 text-right tabular-nums">{formatBytes(t.bytes)}</td>
              <td className="px-1 py-0.5 text-right tabular-nums font-semibold">
                {t.mbps.toFixed(1)} MB/s
              </td>
              <td className="px-1 py-0.5 text-right text-[var(--color-muted)]">
                {new Date(t.whenMs).toLocaleDateString()}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}

// formatBytes moved to lib/format.ts (and corrected to IEC binary).

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

/** Activity entries → RFC 4180 CSV. Header row + one row per entry,
 *  with columns documented inline. Lossy on `extras` fields like
 *  fromPath/toPath — included as concatenated detail. */
function activityToCsv(entries: ActivityEntry[]): string {
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

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms.toFixed(0)} ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)} s`;
  const m = Math.floor(s / 60);
  return `${m}m ${Math.floor(s % 60)}s`;
}
