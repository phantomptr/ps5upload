import { useMemo } from "react";
import { ArrowUp, ArrowDown, Clock, Trophy, Download } from "lucide-react";
import {
  ACTIVITY_HISTORY_LIMIT,
  type ActivityEntry,
} from "../../state/activityHistory";
import { Button } from "../../components";
import { useTr } from "../../state/lang";
import { formatBytes } from "../../lib/format";
import { pushNotification } from "../../state/notifications";
import { isTauriEnv } from "../../lib/tauriEnv";
import { activityToCsv, computeStats, type ComputedStats } from "./stats";
import { formatDate } from "../../lib/formatDate";

/**
 * Stats tab of the Tasks screen: aggregates of the activity history the
 * screen is showing (the selected console's, plus local-only work). Only the
 * last ACTIVITY_HISTORY_LIMIT operations are kept, so these are totals over
 * that window, not lifetime numbers. No payload calls, no network.
 */
export function StatsPanel({ entries }: { entries: ActivityEntry[] }) {
  const tr = useTr();
  const stats = useMemo(() => computeStats(entries), [entries]);

  async function exportCsv() {
    if (entries.length === 0) return;
    const fileName = `ps5upload-activity-${Date.now()}.csv`;
    const csv = activityToCsv(entries);
    // Surface write failures instead of swallowing them — a failed export must
    // not silently look like it succeeded.
    try {
      if (!isTauriEnv()) {
        const { browserDownloadText } = await import("../../lib/browserDownload");
        browserDownloadText(fileName, csv, "text/csv");
      } else {
        const { save } = await import("@tauri-apps/plugin-dialog");
        const { writeTextFileToPath } = await import("../../lib/saveTextFile");
        const dest = await save({
          defaultPath: fileName,
          filters: [{ name: "CSV", extensions: ["csv"] }],
        });
        if (!dest || typeof dest !== "string") return;
        await writeTextFileToPath(dest, csv, fileName);
      }
      pushNotification("success", "Activity exported", {
        body: `Saved ${entries.length.toLocaleString()} rows.`,
      });
    } catch (e) {
      pushNotification("error", "Couldn't export activity", {
        body: e instanceof Error ? e.message : String(e),
      });
    }
  }

  return (
    <div className="mx-auto max-w-4xl space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <p className="min-w-0 flex-1 text-xs text-[var(--color-muted)]">
          {tr(
            "stats_description_v2",
            { count: ACTIVITY_HISTORY_LIMIT },
            `Totals over the operations shown for this console. The app keeps the last ${ACTIVITY_HISTORY_LIMIT}, so older ones are not counted. Worked out from local history; nothing leaves your machine.`,
          )}
        </p>
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<Download size={12} />}
          onClick={exportCsv}
          disabled={entries.length === 0}
        >
          {tr("stats_export_csv", undefined, "Export CSV")}
        </Button>
      </div>
      {entries.length === 0 ? (
        <div className="rounded-[var(--radius-card)] border border-dashed border-[var(--color-border)] p-6 text-center text-xs text-[var(--color-muted)]">
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
  );
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
          stats.fastestMbps !== null
            ? `${stats.fastestMbps.toFixed(1)} MiB/s`
            : "—"
        }
        sub={stats.fastestLabel ?? undefined}
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
    <div className="rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3">
      <div className="flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
        {icon}
        {label}
      </div>
      <div className="mt-1 text-base font-semibold tabular-nums">{value}</div>
      {sub && (
        <div className="truncate text-xs text-[var(--color-muted)]" title={sub}>
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
    <section className="surface-panel p-5">
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
                className="rounded-t-md bg-[image:var(--accent-fill)] transition-all hover:opacity-80"
                style={{
                  height: `${Math.max(heightPct, 2)}%`,
                  minHeight: d.count > 0 ? "2px" : "0",
                }}
              />
            </div>
          );
        })}
      </div>
      <div className="mt-2 flex justify-between text-xs text-[var(--color-muted)]">
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
    <section className="surface-panel p-5">
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
    <section className="surface-panel p-5">
      <h3 className="mb-3 text-sm font-semibold">
        {tr("stats_top_transfers", undefined, "Fastest transfers")}
      </h3>
      <div className="overflow-x-auto">
      <table className="w-full min-w-[420px] text-xs">
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
                {t.mbps.toFixed(1)} MiB/s
              </td>
              <td className="px-1 py-0.5 text-right text-[var(--color-muted)]">
                {formatDate(t.whenMs, "date")}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      </div>
    </section>
  );
}

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms.toFixed(0)} ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)} s`;
  const m = Math.floor(s / 60);
  return `${m}m ${Math.floor(s % 60)}s`;
}
