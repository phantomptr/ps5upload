import { useMemo, useState } from "react";
import { ScrollText, Trash2, Copy, ChevronRight, Download } from "lucide-react";

import {
  useLogsStore,
  type LogEntry,
  type LogLevel,
} from "../../state/logs";
import { PageHeader, EmptyState, Button } from "../../components";
import { useTr } from "../../state/lang";

const LEVEL_ORDER: LogLevel[] = ["error", "warn", "info", "debug"];

/** Per-level color / label. Keeps the visual vocabulary short — users
 *  should see "red dot = error, yellow = warn" at a glance. */
const LEVEL_META: Record<LogLevel, { label: string; tone: string; bg: string }> = {
  error: {
    label: "ERR",
    tone: "text-[var(--color-bad)]",
    bg: "bg-[var(--color-bad-soft)]",
  },
  warn: {
    label: "WRN",
    tone: "text-[var(--color-warn)]",
    bg: "bg-[var(--color-warn-soft)]",
  },
  info: {
    label: "INF",
    tone: "text-[var(--color-accent)]",
    bg: "bg-[var(--color-accent-soft)]",
  },
  debug: {
    label: "DBG",
    tone: "text-[var(--color-muted)]",
    bg: "bg-[var(--color-surface-3)]",
  },
};

function formatTime(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  const hh = pad(d.getHours());
  const mm = pad(d.getMinutes());
  const ss = pad(d.getSeconds());
  const ms3 = String(d.getMilliseconds()).padStart(3, "0");
  return `${hh}:${mm}:${ss}.${ms3}`;
}

export default function LogsScreen() {
  const tr = useTr();
  const entries = useLogsStore((s) => s.entries);
  const filter = useLogsStore((s) => s.filter);
  const setFilter = useLogsStore((s) => s.setFilter);
  const clearLogs = useLogsStore((s) => s.clear);

  const visible = useMemo(() => {
    if (filter === "all") return entries;
    return entries.filter((e) => e.level === filter);
  }, [entries, filter]);

  /** Level counts for the filter pills so users see "3 errors" at a
   *  glance without opening the filter. */
  const counts = useMemo(() => {
    const m: Record<LogLevel, number> = {
      error: 0,
      warn: 0,
      info: 0,
      debug: 0,
    };
    for (const e of entries) m[e.level] += 1;
    return m;
  }, [entries]);

  const copyAll = async () => {
    const text = visible
      .map(
        (e) =>
          `[${formatTime(e.timestamp)}] ${e.level.toUpperCase()} ${e.source}: ${e.message}${
            e.detail ? `\n${e.detail}` : ""
          }`,
      )
      .join("\n");
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // Clipboard API can fail in some embed contexts — non-fatal.
    }
  };

  const downloadAll = () => {
    const text = visible
      .map(
        (e) =>
          `[${new Date(e.timestamp).toISOString()}] ${e.level.toUpperCase()} ${e.source}: ${e.message}${
            e.detail ? `\n${e.detail}` : ""
          }`,
      )
      .join("\n");
    const blob = new Blob([text], { type: "text/plain" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `ps5upload-logs-${new Date().toISOString().replace(/[:.]/g, "-")}.txt`;
    a.click();
    URL.revokeObjectURL(url);
  };

  return (
    <div className="p-6">
      <PageHeader
        icon={ScrollText}
        title={tr("logs", undefined, "Logs")}
        count={entries.length}
        description={tr(
          "logs_description",
          undefined,
          "In-app log of errors, warnings, and notable events. Useful for bug reports — click Copy to grab a plain-text dump.",
        )}
        right={
          <div className="flex items-center gap-1.5">
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Copy size={12} />}
              onClick={copyAll}
              disabled={visible.length === 0}
            >
              {tr("copy", undefined, "Copy")}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Download size={12} />}
              onClick={downloadAll}
              disabled={visible.length === 0}
            >
              {tr("download", undefined, "Download")}
            </Button>
            <Button
              variant="danger"
              size="sm"
              leftIcon={<Trash2 size={12} />}
              onClick={clearLogs}
              disabled={entries.length === 0}
            >
              {tr("clear", undefined, "Clear")}
            </Button>
          </div>
        }
      />

      {/* Level filter pills. 'all' is the default so first-time visitors
          see everything; clicking a level scopes the view + shows the
          count so you don't need to mentally tally. */}
      <div className="mb-4 flex flex-wrap items-center gap-1.5 text-xs">
        <FilterPill
          label={tr("logs_filter_all", undefined, "All")}
          count={entries.length}
          active={filter === "all"}
          onClick={() => setFilter("all")}
        />
        {LEVEL_ORDER.map((l) => {
          const fallback = l[0].toUpperCase() + l.slice(1);
          return (
            <FilterPill
              key={l}
              label={tr(`log_level_${l}`, undefined, fallback)}
              count={counts[l]}
              active={filter === l}
              tone={LEVEL_META[l].tone}
              onClick={() => setFilter(l)}
            />
          );
        })}
      </div>

      {visible.length === 0 ? (
        <EmptyState
          icon={ScrollText}
          size="hero"
          title={tr("logs_empty_title", undefined, "Nothing logged yet")}
          message={
            entries.length === 0
              ? tr(
                  "logs_empty_message",
                  undefined,
                  "Errors and warnings will appear here as you use the app.",
                )
              : tr(
                  "logs_filter_no_matches",
                  undefined,
                  "No entries match the current filter. Try switching filters.",
                )
          }
        />
      ) : (
        <div className="overflow-hidden rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)]">
          <ul className="max-h-[calc(100vh-14rem)] divide-y divide-[var(--color-border)] overflow-y-auto">
            {visible
              .slice()
              .reverse()
              .map((entry) => (
                <LogRow key={entry.id} entry={entry} />
              ))}
          </ul>
        </div>
      )}
    </div>
  );
}

function FilterPill({
  label,
  count,
  active,
  tone,
  onClick,
}: {
  label: string;
  count: number;
  active: boolean;
  tone?: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={
        "flex items-center gap-1.5 rounded-full border px-2.5 py-1 transition-colors " +
        (active
          ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)] text-[var(--color-accent)]"
          : "border-[var(--color-border)] hover:bg-[var(--color-surface-3)]")
      }
    >
      <span className={tone ?? ""}>{label}</span>
      <span className="tabular-nums text-[var(--color-muted)]">{count}</span>
    </button>
  );
}

function LogRow({ entry }: { entry: LogEntry }) {
  const [expanded, setExpanded] = useState(false);
  const meta = LEVEL_META[entry.level];
  const hasDetail = !!entry.detail;
  return (
    <li className="px-3 py-2 text-xs">
      <button
        type="button"
        onClick={() => hasDetail && setExpanded((v) => !v)}
        className={`group flex w-full items-start gap-3 text-left ${
          hasDetail ? "cursor-pointer" : "cursor-default"
        }`}
      >
        <ChevronRight
          size={12}
          className={`mt-1 shrink-0 text-[var(--color-muted)] transition-transform ${
            expanded ? "rotate-90" : ""
          } ${hasDetail ? "" : "opacity-0"}`}
        />
        <span
          className={`mt-px shrink-0 rounded px-1.5 py-0.5 font-mono text-[10px] font-semibold ${meta.bg} ${meta.tone}`}
        >
          {meta.label}
        </span>
        <span className="shrink-0 font-mono tabular-nums text-[var(--color-muted)]">
          {formatTime(entry.timestamp)}
        </span>
        <span className="shrink-0 rounded border border-[var(--color-border)] px-1.5 py-0.5 font-mono text-[10px] text-[var(--color-muted)]">
          {entry.source}
        </span>
        <span className="min-w-0 flex-1 truncate font-mono">
          {entry.message}
        </span>
      </button>
      {expanded && entry.detail && (
        <pre className="mt-2 ml-[54px] max-h-64 overflow-auto rounded border border-[var(--color-border)] bg-[var(--color-surface)] p-2 font-mono text-[11px] text-[var(--color-muted)]">
          {entry.detail}
        </pre>
      )}
    </li>
  );
}
