import { useCallback, useEffect, useState } from "react";
import { RefreshCw, Database, Clock, TrendingUp, Trash2 } from "lucide-react";
import { PageHeader, Button, ErrorCard, ConnectionGate, EmptyState, Card, Spinner, Modal, Tabs, GameIcon, Badge } from "../../components";
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

/* Both tabs identify a title the same way: the game's name when the
 * payload could resolve one from app.db, with the title id demoted to a
 * subtitle. When there is no name -- a title that has been deleted, or a
 * console whose app.db would not open -- the title id stays the heading,
 * so those rows look exactly as they did before rather than going blank. */
function TitleHeading({ titleId, name }: { titleId: string; name?: string }) {
  if (!name) {
    return <div className="truncate font-mono font-semibold">{titleId}</div>;
  }
  return (
    <>
      <div className="truncate font-semibold">{name}</div>
      <div className="truncate font-mono text-xs text-[var(--color-muted)]">
        {titleId}
      </div>
    </>
  );
}

/** The game's icon in a small white frame, like its cover elsewhere. */
function Thumb({ host, titleId }: { host: string; titleId: string }) {
  return (
    <span className="shrink-0 rounded-2xl border border-[var(--glass-edge)] bg-[var(--color-pill)] p-1 shadow-[var(--shadow-1)]">
      <GameIcon host={host} titleId={titleId} size={44} rounded="rounded-xl" />
    </span>
  );
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
  const [tab, setTab] = useState<"tracked" | "play_time">("tracked");

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
        // Rows carry title_id plus an optional name and total_seconds; the
        // renderer shows whichever of those are present.
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
    <div className="app-page">
      <ConnectionGate>
        <PageHeader
          icon={Clock}
          title={tr("game_activity_title", undefined, "Game Activity Tracker")}
          description={tr(
            "game_activity_subtitle_v3",
            undefined,
            "What was played on this PS5 and for how long. Tracked play time is counted by the ps5upload helper on the console; Console Play Time is read from the console's own records.",
          )}
          right={
            <div className="flex items-center gap-2">
              {/* Reset clears only the helper's tracked table, so it lives on that tab. */}
              {tab === "tracked" && (
                <Button
                  variant="ghost"
                  onClick={() => setResetOpen(true)}
                  disabled={resetting || payloadStatus !== "up" || !addr}
                >
                  <Trash2 size={16} />
                  {tr("game_activity_reset", undefined, "Reset play time")}
                </Button>
              )}
              <Button variant="ghost" onClick={() => void refresh()} disabled={loading}>
                {loading ? <Spinner size={16} tone="inherit" /> : <RefreshCw size={16} />}
                {tr("refresh", undefined, "Refresh")}
              </Button>
            </div>
          }
        />

        {error && <div className="mb-4"><ErrorCard title={error} /></div>}

        <Tabs
          className="mb-5"
          ariaLabel={tr("game_activity_title", undefined, "Game Activity Tracker")}
          value={tab}
          onChange={(id) => setTab(id as "tracked" | "play_time")}
          tabs={[
            { id: "tracked", icon: Clock, label: tr("game_activity_tracked", undefined, "Tracked Playtime") },
            { id: "play_time", icon: TrendingUp, label: tr("game_activity_console_playtime", undefined, "Console Play Time") },
          ]}
        />

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
                <Card className="flex items-center gap-4">
                  <Thumb host={host} titleId={currentTitle} />
                  <div className="min-w-0">
                    <div className="flex items-center gap-1.5 text-[0.6875rem] font-semibold uppercase tracking-[0.08em] text-[var(--color-good)]">
                      <TrendingUp size={12} aria-hidden />
                      {tr("game_activity_now_playing", undefined, "Currently playing")}
                    </div>
                    <div className="min-w-0">
                      <TitleHeading
                        titleId={currentTitle}
                        name={
                          entries.find((e) => e.title_id === currentTitle)?.name
                        }
                      />
                    </div>
                  </div>
                </Card>
              )}
              {entries
                .slice()
                .sort((a, b) => (b.total_seconds ?? 0) - (a.total_seconds ?? 0))
                .map((e) => (
                  <Card key={e.title_id} className="flex flex-col gap-3 p-4 sm:flex-row sm:items-center sm:justify-between">
                    <div className="flex min-w-0 flex-1 items-center gap-4">
                      <Thumb host={host} titleId={e.title_id} />
                      <div className="min-w-0 flex-1">
                      <TitleHeading titleId={e.title_id} name={e.name} />
                      <div className="mt-1 text-sm text-[var(--color-muted)]">
                        {e.launches} {tr("game_activity_launches", undefined, "launches")} ·{" "}
                        {tr("game_activity_last", undefined, "Last")}: {formatDate(e.last_launch_ts)}
                      </div>
                      </div>
                    </div>
                    <div className="flex items-center gap-4">
                      {e.session_active && (
                        <Badge tone="good" size="sm" dot>
                          {tr("game_activity_active", undefined, "Active")}
                        </Badge>
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
            title={tr("game_activity_no_console_playtime", undefined, "No console play time")}
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
              <Card key={`${r.title_id}-${i}`} className="flex items-center justify-between gap-4 p-3">
                <div className="flex min-w-0 items-center gap-4">
                  <Thumb host={host} titleId={r.title_id} />
                  <div className="min-w-0">
                    <TitleHeading titleId={r.title_id} name={r.name} />
                  </div>
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
