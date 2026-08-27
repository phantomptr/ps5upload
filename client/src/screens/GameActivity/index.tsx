import { useCallback, useEffect, useState } from "react";
import { Activity, RefreshCw, Database, Clock, TrendingUp, Trash2 } from "lucide-react";
import { PageHeader, Button, ErrorCard, ConnectionGate, EmptyState, Card, Spinner, Modal } from "../../components";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { transferAddr } from "../../lib/addr";
import { humanizePs5Error } from "../../lib/humanizeError";
import {
  activityGet,
  activityDbQuery,
  activityReset,
  type ActivityEntry,
  type ActivityDbRow,
} from "../../api/ps5";

function formatDuration(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  const mins = Math.floor(seconds / 60);
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  const remMins = mins % 60;
  return `${hours}h ${remMins}m`;
}

function formatDate(ts: number): string {
  if (!ts) return "—";
  return new Date(ts * 1000).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export default function GameActivityScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const addr = host ? transferAddr(host) : "";
  const [resetOpen, setResetOpen] = useState(false);
  const [resetting, setResetting] = useState(false);

  const [entries, setEntries] = useState<ActivityEntry[]>([]);
  const [currentTitle, setCurrentTitle] = useState("");
  const [dbRows, setDbRows] = useState<ActivityDbRow[]>([]);
  const [dbSource, setDbSource] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState<"tracked" | "recently_played" | "play_time">(
    "tracked",
  );

  const refresh = useCallback(async () => {
    if (!addr || payloadStatus !== "up") return;
    setLoading(true);
    setError(null);
    try {
      if (tab === "tracked") {
        const resp = await activityGet(addr);
        setEntries(resp.titles ?? []);
        setCurrentTitle(resp.current_title ?? "");
      } else {
        // Both database tabs render through the same row list — the rows
        // carry title_id plus an optional name and total_seconds, and the
        // renderer already shows whichever of those are present.
        const resp = await activityDbQuery(tab, addr);
        setDbRows(resp.rows ?? []);
        setDbSource(resp.source ?? "");
      }
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    } finally {
      setLoading(false);
    }
  }, [addr, payloadStatus, tab]);

  const handleReset = useCallback(async () => {
    if (!addr || payloadStatus !== "up") return;
    setResetting(true);
    setError(null);
    try {
      await activityReset(addr);
      setEntries([]);
      setCurrentTitle("");
      await refresh();
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    } finally {
      setResetting(false);
      setResetOpen(false);
    }
  }, [addr, payloadStatus, refresh]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return (
    <div className="p-6">
      <ConnectionGate>
        <PageHeader
          icon={Activity}
          title={tr("game_activity_title", undefined, "Game Activity Tracker")}
          description={tr(
            "game_activity_subtitle",
            undefined,
            "Play-time tracking and recently played titles",
          )}
          right={
            <div className="flex items-center gap-2">
              <Button
                variant="ghost"
                onClick={() => setResetOpen(true)}
                disabled={resetting || payloadStatus !== "up" || !addr}
              >
                <Trash2 size={16} />
                {tr("game_activity_reset", undefined, "Reset play time")}
              </Button>
              <Button variant="ghost" onClick={() => void refresh()} disabled={loading}>
                {loading ? <Spinner size={16} tone="inherit" /> : <RefreshCw size={16} />}
                {tr("refresh", undefined, "Refresh")}
              </Button>
            </div>
          }
        />

        {error && <div className="mb-4"><ErrorCard title={error} /></div>}

        <div className="mb-4 flex flex-wrap gap-2">
          <Button
            variant={tab === "tracked" ? "primary" : "ghost"}
            onClick={() => setTab("tracked")}
          >
            <Clock size={16} /> {tr("game_activity_tracked", undefined, "Tracked Playtime")}
          </Button>
          <Button
            variant={tab === "recently_played" ? "primary" : "ghost"}
            onClick={() => setTab("recently_played")}
          >
            <Database size={16} /> {tr("game_activity_recent", undefined, "Recently Played")}
          </Button>
          <Button
            variant={tab === "play_time" ? "primary" : "ghost"}
            onClick={() => setTab("play_time")}
          >
            <TrendingUp size={16} />{" "}
            {tr("game_activity_console_playtime", undefined, "Console Play Time")}
          </Button>
        </div>

        {loading ? (
          <div className="flex items-center justify-center py-12">
            <Spinner size={32} />
          </div>
        ) : tab === "tracked" ? (
          entries.length === 0 ? (
            <EmptyState
              icon={Clock}
              title={tr("game_activity_empty", undefined, "No tracked activity yet")}
              message={tr(
                "game_activity_empty_desc",
                undefined,
                "Launch games to start tracking play time",
              )}
            />
          ) : (
            <div className="space-y-3">
              {currentTitle && (
                <Card className="flex items-center gap-3 border-[var(--color-good)]/30 bg-[var(--color-good)]/5">
                  <TrendingUp size={20} className="text-[var(--color-good)]" />
                  <div>
                    <div className="text-sm text-[var(--color-muted)]">
                      {tr("game_activity_now_playing", undefined, "Currently playing")}
                    </div>
                    <div className="font-mono font-bold">{currentTitle}</div>
                  </div>
                </Card>
              )}
              {entries
                .slice()
                .sort((a, b) => (b.total_seconds ?? 0) - (a.total_seconds ?? 0))
                .map((e) => (
                  <Card key={e.title_id} className="flex flex-col gap-2 p-4 sm:flex-row sm:items-center sm:justify-between">
                    <div className="min-w-0 flex-1">
                      <div className="font-mono font-semibold">{e.title_id}</div>
                      <div className="text-sm text-[var(--color-muted)]">
                        {e.launches} {tr("game_activity_launches", undefined, "launches")} ·{" "}
                        {tr("game_activity_last", undefined, "Last")}: {formatDate(e.last_launch_ts)}
                      </div>
                    </div>
                    <div className="flex items-center gap-4">
                      {e.session_active && (
                        <span className="badge badge-success badge-sm">
                          {tr("game_activity_active", undefined, "Active")}
                        </span>
                      )}
                      <div className="text-right">
                        <div className="text-lg font-bold">{formatDuration(e.total_seconds)}</div>
                        <div className="text-xs text-[var(--color-muted)]">
                          {tr("game_activity_total", undefined, "total playtime")}
                        </div>
                      </div>
                    </div>
                  </Card>
                ))}
            </div>
          )
        ) : dbRows.length === 0 ? (
          <EmptyState
            icon={Database}
            title={tr("game_activity_no_recent", undefined, "No recently played data")}
            message={
              dbSource === "none"
                ? tr("game_activity_db_unavail", undefined, "Database unavailable on this firmware")
                : tr("game_activity_no_data", undefined, "No data found")
            }
          />
        ) : (
          <div className="space-y-2">
            {dbSource && (
              <div className="text-sm text-[var(--color-muted)]">
                {tr("game_activity_source", undefined, "Source")}:{" "}
                <span className="font-mono">{dbSource}</span>
              </div>
            )}
            {dbRows.map((r, i) => (
              <Card key={`${r.title_id}-${i}`} className="flex items-center justify-between p-3">
                <div className="min-w-0">
                  <span className="font-mono font-semibold">{r.title_id}</span>
                  {r.name && <span className="ml-3 text-[var(--color-muted)]">{r.name}</span>}
                </div>
                {r.total_seconds != null && (
                  <div className="shrink-0 text-sm font-bold">{formatDuration(r.total_seconds)}</div>
                )}
              </Card>
            ))}
          </div>
        )}
      
        <Modal
          open={resetOpen}
          onClose={() => setResetOpen(false)}
          title={tr("game_activity_reset_title", undefined, "Reset play time?")}
        >
          <p className="text-sm text-[var(--color-muted)]">
            {tr(
              "game_activity_reset_explain",
              undefined,
              "This permanently deletes the play time ps5upload has recorded on this console. It cannot be undone, and past sessions cannot be recovered. Your console's own records are not affected \u2014 this only clears what this screen shows.",
            )}
          </p>
          <div className="mt-4 flex justify-end gap-2">
            <Button variant="secondary" onClick={() => setResetOpen(false)}>
              {tr("cancel", undefined, "Cancel")}
            </Button>
            <Button
              variant="danger"
              disabled={resetting}
              onClick={() => void handleReset()}
            >
              {resetting
                ? tr("game_activity_resetting", undefined, "Resetting\u2026")
                : tr("game_activity_reset_confirm", undefined, "Reset")}
            </Button>
          </div>
        </Modal>
      </ConnectionGate>
    </div>
  );
}
