import { useEffect, useState } from "react";
import { Loader2, ChevronUp, ChevronDown } from "lucide-react";
import { useNavigate } from "react-router-dom";

import {
  useActivityHistoryStore,
  type ActivityEntry,
} from "../state/activityHistory";
import { formatBytes, formatDuration } from "../lib/format";
import { useTr } from "../state/lang";

/**
 * Persistent footer bar that surfaces in-flight operations across
 * every screen. Without it, a user who starts a copy on the
 * FileSystem screen and navigates to Library has no global signal
 * that work is still happening — the per-screen banner disappears
 * with the screen.
 *
 * Collapsed: a single line with the live op count + a click-through
 * to the Activity tab. Expanded: per-op summary with elapsed +
 * progress numbers + speed.
 *
 * Renders nothing when no ops are running, so the chrome stays out
 * of the way during idle.
 */
export default function OperationBar() {
  const tr = useTr();
  const navigate = useNavigate();
  const entries = useActivityHistoryStore((s) => s.entries);
  const running = entries.filter((e) => e.outcome === "running");
  const [expanded, setExpanded] = useState(false);

  if (running.length === 0) return null;

  return (
    <div className="border-t border-[var(--color-border)] bg-[var(--color-surface-2)]">
      <div className="flex w-full items-center gap-2 px-4 py-1.5 text-xs">
        <button
          type="button"
          onClick={() => setExpanded((v) => !v)}
          className="flex flex-1 items-center gap-2 truncate hover:opacity-80"
          aria-expanded={expanded}
          aria-label={tr("ops_toggle", undefined, "Toggle operations panel")}
        >
          <Loader2 size={12} className="animate-spin text-[var(--color-accent)]" />
          <span className="font-medium">
            {tr(
              "ops_in_flight",
              { count: running.length },
              `${running.length} operation${running.length === 1 ? "" : "s"} running`,
            )}
          </span>
          <span className="ml-2 truncate text-[var(--color-muted)]">
            {running[0].label}
            {running.length > 1 && ` · +${running.length - 1} more`}
          </span>
          {expanded ? <ChevronDown size={12} /> : <ChevronUp size={12} />}
        </button>
        <button
          type="button"
          onClick={() => navigate("/activity")}
          className="rounded-md border border-[var(--color-border)] px-2 py-0.5 text-[10px] text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
        >
          {tr("ops_open_activity", undefined, "View Activity")}
        </button>
      </div>

      {expanded && (
        <div className="border-t border-[var(--color-border)] px-4 py-2">
          <ul className="space-y-1.5 text-xs">
            {running.map((entry) => (
              <RunningRow key={entry.id} entry={entry} />
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

function RunningRow({ entry }: { entry: ActivityEntry }) {
  const [now, setNow] = useState(() => Date.now());
  // 1 s tick: balances responsive updates against not re-rendering
  // every screen 4× per second from the global bar.
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, []);
  const elapsedMs = Math.max(0, now - entry.startedAtMs);
  const speed =
    entry.bytes && entry.bytes > 0 && elapsedMs > 0
      ? (entry.bytes * 1000) / elapsedMs
      : 0;
  const pct =
    entry.totalBytes && entry.totalBytes > 0 && entry.bytes
      ? Math.min(100, (entry.bytes / entry.totalBytes) * 100)
      : null;

  return (
    <li className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5">
      <span className="font-medium text-[var(--color-text)]">
        {entry.label}
      </span>
      <span className="text-[var(--color-muted)]">
        {formatDuration(elapsedMs / 1000)}
        {entry.bytes !== undefined && entry.bytes > 0 && (
          <>
            {" · "}
            {formatBytes(entry.bytes)}
            {entry.totalBytes !== undefined && entry.totalBytes > 0 && (
              <> / {formatBytes(entry.totalBytes)}</>
            )}
            {pct !== null && ` (${pct.toFixed(0)}%)`}
          </>
        )}
        {speed > 0 && ` · ${formatBytes(speed)}/s`}
      </span>
      {entry.detail && (
        <span className="text-[10px] text-[var(--color-muted)]">
          {entry.detail}
        </span>
      )}
    </li>
  );
}
