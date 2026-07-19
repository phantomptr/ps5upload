import { useEffect, useMemo, useRef, useState } from "react";
import {
  ScrollText,
  Trash2,
  Copy,
  Check,
  ChevronRight,
  Download,
} from "lucide-react";

import { invoke } from "../../lib/invokeLogged";

import {
  useLogsStore,
  type LogEntry,
  type LogLevel,
} from "../../state/logs";
import { useDiagSettingsStore, LOG_LEVELS } from "../../state/diagSettings";
import { EmptyState, Button } from "../../components";
import { useTr } from "../../state/lang";
import { writeClipboard } from "../../lib/clipboard";
import { isTauriEnv } from "../../lib/tauriEnv";
import { browserDownloadText } from "../../lib/browserDownload";

const LEVEL_ORDER: LogLevel[] = ["error", "warn", "info", "debug", "trace"];

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
  trace: {
    label: "TRC",
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

/**
 * App-side log viewer — in-memory JS events from the React app and
 * Tauri bridge (Logs/ZodErrors/Notifications/etc). Persists for the
 * session but is not network-backed.
 *
 * Designed to render as a child of the Logs tab shell — owns its own
 * action row and filter pills but not the outer page header.
 */
export default function AppLogsPanel() {
  const tr = useTr();
  const entries = useLogsStore((s) => s.entries);
  const filter = useLogsStore((s) => s.filter);
  const setFilter = useLogsStore((s) => s.setFilter);
  const clearLogs = useLogsStore((s) => s.clear);
  const logLevel = useDiagSettingsStore((s) => s.logLevel);
  const setLogLevel = useDiagSettingsStore((s) => s.setLogLevel);

  // Transient button feedback. A silent success looked identical to a silent
  // failure, which is why Copy/Download read as "not working".
  const [copyState, setCopyState] = useState<"idle" | "done" | "fail">("idle");
  const [saveState, setSaveState] = useState<"idle" | "done" | "fail">("idle");

  // Track the reset timers so we can clear them on unmount. Without
  // this, a Copy/Download click followed by a quick tab switch fires
  // setState on an unmounted component.
  const timersRef = useRef<ReturnType<typeof setTimeout>[]>([]);
  useEffect(() => {
    const timers = timersRef.current;
    return () => {
      for (const t of timers) clearTimeout(t);
    };
  }, []);
  const scheduleReset = (
    setter: (s: "idle") => void,
    ms: number,
  ) => {
    const id = setTimeout(() => setter("idle"), ms);
    timersRef.current.push(id);
  };

  const visible = useMemo(() => {
    if (filter === "all") return entries;
    return entries.filter((e) => e.level === filter);
  }, [entries, filter]);

  const counts = useMemo(() => {
    const m: Record<LogLevel, number> = {
      error: 0,
      warn: 0,
      info: 0,
      debug: 0,
      trace: 0,
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
    const ok = await writeClipboard(text);
    setCopyState(ok ? "done" : "fail");
    scheduleReset(setCopyState, 1600);
  };

  const downloadAll = async () => {
    const text = visible
      .map(
        (e) =>
          `[${new Date(e.timestamp).toISOString()}] ${e.level.toUpperCase()} ${e.source}: ${e.message}${
            e.detail ? `\n${e.detail}` : ""
          }`,
      )
      .join("\n");
    const fileName = `ps5upload-logs-${new Date()
      .toISOString()
      .replace(/[:.]/g, "-")}.txt`;
    try {
      if (!isTauriEnv()) {
        // Plain Blob + anchor download — safe here since the Tauri-webview
        // ACL interception (see the native branch below) doesn't apply in a
        // real browser.
        browserDownloadText(fileName, text);
        setSaveState("done");
        scheduleReset(setSaveState, 1600);
        return;
      }
      // Save via the native dialog + backend `save_text_file` command rather
      // than a Blob/anchor `download`. In the Tauri webview an anchor download
      // is routed to `plugin:fs|write_text_file`, which the capability ACL
      // blocks ("Command plugin:fs|write_text_file not allowed by ACL") — the
      // exact error users hit trying to save logs. The backend command writes
      // from trusted Rust and isn't fs-scope-gated. Mirrors Stats/Settings.
      const { save } = await import("@tauri-apps/plugin-dialog");
      const { writeTextFileToPath } = await import("../../lib/saveTextFile");
      const dest = await save({
        defaultPath: fileName,
        filters: [{ name: "Text", extensions: ["txt"] }],
      });
      if (!dest) return; // user cancelled
      await writeTextFileToPath(dest, text, fileName);
      setSaveState("done");
      scheduleReset(setSaveState, 1600);
    } catch (e) {
      // Surface to the in-app log so a save failure isn't silent.
      console.error(
        `Saving logs failed: ${e instanceof Error ? e.message : String(e)}`,
      );
      setSaveState("fail");
      scheduleReset(setSaveState, 2400);
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* Top action row — moved up from PageHeader.right when this
          screen was a standalone route. Keeps Copy/Download/Clear
          adjacent to the data they operate on. */}
      <div className="mb-3 flex items-center justify-end gap-1.5">
        <Button
          variant="secondary"
          size="sm"
          leftIcon={
            copyState === "done" ? <Check size={12} /> : <Copy size={12} />
          }
          onClick={copyAll}
          disabled={visible.length === 0}
        >
          {copyState === "done"
            ? tr("copied", undefined, "Copied")
            : copyState === "fail"
              ? tr("copy_failed", undefined, "Copy failed")
              : tr("copy", undefined, "Copy")}
        </Button>
        <Button
          variant="secondary"
          size="sm"
          leftIcon={
            saveState === "done" ? <Check size={12} /> : <Download size={12} />
          }
          onClick={downloadAll}
          disabled={visible.length === 0}
        >
          {saveState === "done"
            ? tr("saved", undefined, "Saved")
            : saveState === "fail"
              ? tr("save_failed", undefined, "Save failed")
              : tr("download", undefined, "Download")}
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

      {/* Persistent disk-log controls. These logs are written to
          ~/.ps5upload/logs/ so a bug report can package a time window; the
          level here gates the MINIMUM severity recorded (not what's shown
          above). */}
      <div className="mb-3 flex flex-wrap items-center gap-2 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] px-3 py-2 text-xs">
        <span className="text-[var(--color-muted)]">
          {tr(
            "logs_disk_hint",
            undefined,
            "Saved to disk for bug reports. Recording level:",
          )}
        </span>
        <select
          value={logLevel}
          onChange={(e) => setLogLevel(e.target.value as LogLevel)}
          className="rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs"
        >
          {LOG_LEVELS.map((l) => (
            <option key={l} value={l}>
              {tr(`log_level_${l}`, undefined, l[0].toUpperCase() + l.slice(1))}
            </option>
          ))}
        </select>
        {isTauriEnv() && (
          <button
            type="button"
            onClick={() => void invoke("diag_log_open_dir").catch(() => {})}
            className="ml-auto text-[var(--color-accent)] hover:underline"
          >
            {tr("logs_open_folder", undefined, "Open logs folder")}
          </button>
        )}
      </div>

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
        <div className="min-h-0 flex-1 overflow-hidden rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)]">
          <ul className="h-full divide-y divide-[var(--color-border)] overflow-y-auto">
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
          className={`mt-px shrink-0 rounded px-1.5 py-0.5 font-mono text-xs font-semibold ${meta.bg} ${meta.tone}`}
        >
          {meta.label}
        </span>
        <span className="shrink-0 font-mono tabular-nums text-[var(--color-muted)]">
          {formatTime(entry.timestamp)}
        </span>
        <span className="shrink-0 rounded border border-[var(--color-border)] px-1.5 py-0.5 font-mono text-xs text-[var(--color-muted)]">
          {entry.source}
        </span>
        <span className="min-w-0 flex-1 truncate font-mono">
          {entry.message}
        </span>
      </button>
      {expanded && entry.detail && (
        <pre className="mt-2 ml-[54px] max-h-64 overflow-auto rounded border border-[var(--color-border)] bg-[var(--color-surface)] p-2 font-mono text-xs text-[var(--color-muted)]">
          {entry.detail}
        </pre>
      )}
    </li>
  );
}
